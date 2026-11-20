# Developing rayfin-local

This page covers building, testing and releasing rayfin-local.
[`AGENTS.md`](../AGENTS.md) has the conventions in full, and [architecture.md](architecture.md) explains how the crates fit together.

## Prerequisites

- Rust 1.96 (`rustup toolchain install 1.96.0`), with `rustfmt` and `clippy`.
- Node.js 20 or later (CI uses 22) and npm.
- Optional, for the tests that use them:
  - the reference app, `fabric-apps/apps/explorer`, checked out next to this repository with its packages installed (`npm ci`);
  - Playwright's Chromium (`npx playwright install chromium`), or an installed Microsoft Edge or Chrome;
  - a signed-in Azure CLI (`az login`).

No C toolchain beyond the platform's usual one is needed: TLS is rustls with ring, and SQLite is bundled.

## Build and run

```sh
cargo build --workspace
cargo run -p rayfin-local -- serve -C <project>
```

To try it on the fixture app:

```sh
cd tests/fixtures/apps/basic
npm ci
npm ci --prefix rayfin/functions
cd ../../../..
cargo run -p rayfin-local -- serve -C tests/fixtures/apps/basic
```

For SDK 2.0, use `tests/fixtures/apps/sdk-v2` instead; its `npm ci` also installs the Functions workspace.
It uses explicit auth adapters and service factories and pins the SDK and worker to `2.0.0`.

`-v` shows rayfin-local's debug logs, and `RUST_LOG` (for example `rl_graphql=trace`) replaces the default filters.

### Native npm package

`rl-runtime` owns shared service orchestration; the executable supplies CLI integration, and
`rl-napi` supplies the private Node-API bridge. The supported JS API and binary-only export live in
[`npm/rayfin-local-runtime`](../npm/rayfin-local-runtime/README.md). Nothing is published automatically.

Build both artifacts and assemble their exact-version platform package. For example, on macOS arm64:

```sh
cargo build -p rayfin-local -p rl-napi
node scripts/assemble-npm.mjs --target aarch64-apple-darwin \
  --binary target/debug/rayfin-local --addon target/debug/librl_napi.dylib
npm ci --prefix tests/fixtures/apps/basic --ignore-scripts
npm ci --prefix tests/fixtures/apps/basic/rayfin/functions --ignore-scripts
node scripts/test-npm.mjs \
  --platform .artifacts/npm/rayfin-local-runtime-darwin-arm64 \
  --project tests/fixtures/apps/basic
```

Use the matching target and artifact paths on other systems: `librl_napi.so` on Linux, and
`rayfin-local.exe` plus `rl_napi.dll` on Windows. The assembler supports macOS arm64/x64,
glibc Linux arm64/x64, and Windows x64. On Windows, build the executable with
`RUSTFLAGS="-C target-feature=+crt-static"` separately; do **not** apply that flag to the addon.

The smoke runner packs and installs both packages offline with scripts disabled, exercises their
public exports against actual native artifacts, and checks TypeScript ESM/CommonJS consumers.
It requires the installed fixture and never silently skips missing SDK or functions dependencies.
Coverage includes concurrent instances, state locks, cancellation, reload, worker teardown,
normal host exit, Vite/functions shutdown, and real SDK data/auth/function requests.
CI builds and runs this on all five target architectures. Local validation on one machine does
not certify the other platforms.

The assembler copies the final artifacts unchanged. When release signing is configured, sign
and verify **both** the executable and addon before assembly, then hash/package/publish.
npm publication, provenance and OS signing identities are intentionally not configured yet.

## Lint and format

CI runs these on every push to `main` and every pull request, with Rust 1.96.0:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

While other work is in progress, lint and format one crate: `cargo clippy -p <crate> --all-targets -- -D warnings` and `cargo fmt -p <crate>`.

## Tests

`scripts/check.mjs` runs every check CI runs, the same way on Windows, Linux and macOS:

```sh
node scripts/check.mjs              # everything: setup, lint, rust, js, e2e
node scripts/check.mjs rust js      # some steps
node scripts/check.mjs e2e --no-setup
node scripts/check.mjs --help
```

| Step | What it runs | CI job |
| --- | --- | --- |
| `setup` | `npm ci` in each test fixture and harness (again only when its `package-lock.json` changed), builds `tests/fixtures/functions/basic`, and for `e2e` downloads Playwright's Chromium (not with `PLAYWRIGHT_CHANNEL`). The other steps run it first unless `--no-setup`. | all but `lint` |
| `lint` | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`. | `lint` |
| `rust` | `cargo test --workspace --locked` and the SDK auth test (`rl-auth --test sdk -- --ignored`), in strict mode. | `test` (Windows, Linux, macOS) |
| `js` | `node --test` on each `js/test/*.test.mjs`. | `test` |
| `e2e` | The Playwright `fixture` and `static` suites ([below](#end-to-end-tests)), and the sign-in portal test (`rl-auth --test portal_e2e -- --ignored`). | `e2e` (Linux) |

Cargo runs with `--locked`, as the official release build does, so a `Cargo.lock` that `Cargo.toml` has moved on from fails the check: after bumping the version or changing a dependency, run `cargo build` (or `cargo update -w`) and commit the lockfile with it.
The `test` job also runs `.github/scripts/*.test.mjs`. `npm-registry.test.mjs` checks that every committed `package-lock.json` resolves its packages from the anonymous 1es-public feed, and that a `.npmrc` next to it sets that registry: the official build's network isolation blocks registry.npmjs.org. The `.npmrc` keeps a regenerated lockfile on the feed whatever your own npm registry is.

While working on one crate, `cargo test -p <crate>` is quicker; run the check before you push.
Unit tests sit next to the code (`#[cfg(test)]`); integration tests are in `crates/<crate>/tests/`.

**Strict mode.** The check sets `RAYFIN_LOCAL_TEST_STRICT=1`, and points `RAYFIN_SDK_NODE_MODULES` at the basic fixture's `node_modules` (SDK 1.36) and `RAYFIN_SDK_V2_NODE_MODULES` at the sdk-v2 fixture's (SDK 2.0).
In strict mode a test that needs Node.js, a fixture's packages or the SDK fails when they are missing, instead of skipping, so a broken setup can't pass for a green run.
The tests that need what CI doesn't have still skip: an app outside this repository (the variables below), a SQL Server (`RAYFIN_LOCAL_TEST_MSSQL`; CI's `sql-connectors` job has one), and a signed-in Azure CLI.
Skips go through `rl_test_support::skip` (`crates/rl-test-support`); a new test that needs an optional dependency should too.

### Tests that use Node.js and the real SDK

Several integration tests drive the real Rayfin SDKs 1.36.2 and 2.0.0, or the project's own packages, with Node.js against rayfin-local's routes.
Outside strict mode they print a `skipped: …` note and pass when Node.js or the packages they need are missing, so a plain `cargo test --workspace` works on any machine.

| Test | What it checks | Needs |
| --- | --- | --- |
| `rl-graphql` `tests/sdk.rs` | The SDK's data client against the GraphQL API, signed in through `rl-auth`, on the reference app's model. | `npm ci` in `tests/fixtures/apps/basic` |
| `rl-connectors` `tests/sdk_decode.rs` | The SDK's `fabricSemanticModel()` through rayfin-local, compared with its direct path to the same mock Power BI, on both SDK lines. | `npm ci` in `tests/fixtures/apps/basic` and `tests/fixtures/apps/sdk-v2` |
| `rl-storage` `tests/sdk.rs` | The SDK's `StorageFolderClient` against the storage routes. | `npm install` in `crates/rl-storage/tests/sdk` |
| `rl-model` `tests/analyze_js.rs` | Golden 1.36 analysis, plus SDK 2.0 data, storage and mapped SQL-connector permissions. | `npm ci` in `tests/fixtures/apps/basic`, `tests/fixtures/apps/connectors` and `tests/fixtures/apps/sdk-v2` |
| `rl-connectors` `tests/sql_sdk.rs` | The SDK's Category A connector client (`createConnectorsApi` with the entity classes, for a SQL database, a Warehouse and a SQL analytics endpoint) through rayfin-local on a real SQL Server, on both SDK lines. | `npm ci` in `tests/fixtures/apps/connectors` and `tests/fixtures/apps/sdk-v2`, and [SQL Server](#sql-server-tests) |
| `rl-functions` `tests/host.rs`, `tests/invoke.rs` | The function host and the invoke routes on `tests/fixtures/functions/basic`, which they install and build on first use; also the reference app's functions when present. | Node.js and npm |
| `rl-frontend` `tests/vite.rs` | The Vite dev server and the static build, on copies of the reference app and small Vite apps; automatic sign-in answered before `rayfinLocalDev()`'s, on the fixture app and the apps in `RAYFIN_LOCAL_TEST_VITE_APPS`. | The reference app; `npm ci` in `tests/fixtures/apps/basic` |
| `rayfin-local` `tests/host_contract.rs` | `serve --json` of the fixture app ([embedding.md](embedding.md)): its events from `starting` to `ready`, the app and the backend answering, and a clean stop when stdin closes; with a `--state-dir` outside the project, a backup while it serves, a refused restore, and a restore into a new state folder that `serve` then runs on. Also the composed SDK 2.0 client (`tests/fixtures/apps/sdk-v2`) using auth, data, functions and storage through Vite. | `npm ci` in `tests/fixtures/apps/basic` and `tests/fixtures/apps/sdk-v2` |
| `rayfin-local` `tests/control_api.rs` | The control API and automatic sign-in of the fixture app under `serve --json`: status, users, identities, the dev server's session token used on `/graphql`, rows, a reload and a data reset. | `npm ci` in `tests/fixtures/apps/basic` |
| `rayfin-local` `tests/live_reload.rs` | `serve --json` of a copy of the fixture app (its `node_modules` linked): `rayfin.yml` edits applied on the same ports, `secret set` restarting only the function host, Vite started again after it is killed, and the control API, `POST /reload` (refused and forced schema changes, invalid files) and automatic sign-in across changes. | `npm ci` in `tests/fixtures/apps/basic` and its `rayfin/functions` |

These variables point the tests at what they need:

| Variable | Default |
| --- | --- |
| `RAYFIN_SDK_NODE_MODULES` | `tests/fixtures/apps/basic/node_modules`: a `node_modules` with the Rayfin SDK 1.36.2. |
| `RAYFIN_SDK_V2_NODE_MODULES` | `tests/fixtures/apps/sdk-v2/node_modules`: a `node_modules` with the Rayfin SDK 2.0.0. The SDK auth, sign-in portal and semantic model tests run on both; other SDK 2.0 tests use `tests/fixtures/apps/sdk-v2` directly. |
| `RAYFIN_LOCAL_EXPLORER` | `../fabric-apps/apps/explorer`: the reference app, for `rl-frontend`. |
| `RAYFIN_LOCAL_EXPLORER_FUNCTIONS` | `../fabric-apps/apps/explorer/rayfin/functions`: the reference app's functions, for `rl-functions`. |
| `RAYFIN_LOCAL_TYPESCRIPT_DIR` | `tests/fixtures/functions/basic` (its TypeScript devDependency), else the reference app's functions: where `rl-functions`' metadata tests resolve `typescript` from. |
| `RAYFIN_APPS_DIR` | `../fabric-apps/apps`: every app's `rayfin.yml`, for `rl-project`'s ignored test. |
| `RAYFIN_LOCAL_TEST_VITE_APPS` | None: more apps with their packages installed (a list of folders, separated as in `PATH`), for `rl-frontend`'s automatic sign-in test, such as an app on another Vite version. |
| `RAYFIN_LOCAL_NODE` | `node` on `PATH`. |

The defaults are relative to this repository's folder: they expect `fabric-apps` checked out beside it.
The `test` job's `node scripts/check.mjs rust js` installs both SDK fixtures and runs these tests strict on Linux, Windows and macOS.

### SQL Server tests

`rl-connectors`' `sql_server.rs` and `sql_sdk.rs` run the SQL connectors against a real SQL Server, and print a `skipped: …` note and pass without one.
Each test creates its own database and drops it at the end.
Start SQL Server 2022 in Docker, then point the tests at it with `RAYFIN_LOCAL_TEST_MSSQL` (`host,port;user;password`):

```sh
docker run -d --name rayfin-local-mssql -p 127.0.0.1:14330:1433 \
  -e ACCEPT_EULA=Y -e "MSSQL_SA_PASSWORD=<a strong password>" \
  mcr.microsoft.com/mssql/server:2022-latest
RAYFIN_LOCAL_TEST_MSSQL="127.0.0.1,14330;sa;<the password>" \
  cargo test -p rl-connectors --test sql_server --test sql_sdk
docker rm -f rayfin-local-mssql
```

SQL Server takes a few seconds to accept logins after the container starts.
`sql_sdk.rs` also needs the SDK fixtures' packages (`npm ci` in `tests/fixtures/apps/connectors` for 1.36 and `tests/fixtures/apps/sdk-v2` for 2.0); it compiles the `connectors` fixture's entities against each line.
CI's `sql-connectors` job runs both on Linux, with a SQL Server service container, in strict mode.

### Ignored tests

Tests that need a signed-in Azure CLI, the network or a browser are `#[ignore]`d.
Run them explicitly; they print no tokens and no identity.

| Command | What it does |
| --- | --- |
| `cargo test -p rl-auth --test sdk -- --ignored` | The SDK's auth flows: sign-up, sign-in, refresh, JWKS, sign-out, 401 then refresh then retry. |
| `cargo test -p rl-auth --test portal_e2e -- --ignored --nocapture` | The sign-in popup in a real browser with the SDK (see [`tests/e2e/portal/README.md`](../tests/e2e/portal/README.md)). |
| `cargo test -p rl-auth --test entra_smoke -- --ignored --nocapture` | The Entra exchange with your real Power BI token. |
| `cargo test -p rl-tokens --test smoke -- --ignored --nocapture` | Tokens from your Azure CLI. |
| `cargo test -p rl-connectors --test real_power_bi -- --ignored --nocapture` | A query against the reference app's semantic model in the real Power BI, as you. |
| `cargo test -p rl-connectors --test real_fabric_sql -- --ignored --nocapture` | A read-only query of `INFORMATION_SCHEMA.TABLES` in a real Fabric SQL database, as you; `RAYFIN_LOCAL_REAL_SQL="<workspace id>;<item id>[;<connector type>]"` picks the item. |
| `cargo test -p rl-functions --test invoke -- --ignored` | The reference app's functions with your tokens. |
| `cargo test -p rl-project --test rayfin_yml -- --ignored --nocapture` | Parsing every app's `rayfin.yml` in the `fabric-apps` repository (`RAYFIN_APPS_DIR` points at its `apps` folder). |

### JS helper tests

`js/test/` tests the helpers with Node's test runner.
The session stamp's tests need nothing else: `node --test js/test/session-stamp.test.mjs`.
The function host's tests need the fixture functions package installed and built first:

```sh
cd tests/fixtures/functions/basic
npm ci
npm run build
cd ../../../..
node --test js/test/function-host.test.mjs
```

### End-to-end tests

`tests/e2e/` runs `rayfin-local serve` on real apps and drives them with Playwright.
`node scripts/check.mjs e2e` runs what CI runs; to run one suite:

```sh
cd tests/e2e
npm ci
npx playwright install chromium   # or set PLAYWRIGHT_CHANNEL=msedge (or chrome)
npm run test:fixture              # the fixture app, with fake tokens and a mock Power BI
npm run test:static               # the fixture app's static build with protected hosting
npm run test:explorer             # the reference app, with your real tokens (az login)
npx playwright test --project=fixture -g "fn-ping"   # one test
```

- The run builds rayfin-local first (`cargo build -p rayfin-local`); `RAYFIN_LOCAL_BIN` uses another binary, and `RAYFIN_LOCAL_SKIP_BUILD=1` skips the build.
- Each suite starts `serve` with `--reset` on free ports, and installs the fixture app's packages when they are missing.
- The reference app comes from `EXPLORER_DIR`, else `../fabric-apps/apps/explorer`; run `npm ci` in it first.
  Its suite puts back the app's own `.rayfin-local/` and checks that git sees no new changes in the app.
- `HEADED=1` shows the browser.
- Logs go to `test-results/logs/`, and traces and screenshots of failures to `test-results/`; CI's `e2e` job uploads `test-results/` as the `e2e-results` artifact when it fails.
- A change to what a browser sees needs a test here. [`tests/e2e/README.md`](../tests/e2e/README.md) lists each test and how the harness works.

### Fixtures

| Fixture | Used for |
| --- | --- |
| `tests/fixtures/apps/basic` | The end-to-end app: every service, with one button per scenario. Its [README](../tests/fixtures/apps/basic/README.md) lists the scenarios and how to regenerate its generated files. |
| `tests/fixtures/apps/connectors` | An app with one SQL connector, `inventory`: entity classes, `metadata.json` and `@role` policies, for the connector analysis and the SQL Server tests. |
| `tests/fixtures/functions/basic` | A functions package for the function host's tests. |
| `crates/rl-model/tests/fixtures/*.analysis.json` | Golden analyses of the fixture app and the reference app; `basic.v2.analysis.json` is the fixture app under `@microsoft/rayfin-core` 2.0.0 (binary enum and range CHECKs), for the DDL tests. |
| `crates/rl-model/tests/fixtures/inventory.connector.json` | The golden connector analysis of the `connectors` fixture app. |
| `crates/rl-functions/tests/fixtures/` | Small functions packages for build and host edge cases. |

## Conventions

The essentials of [`AGENTS.md`](../AGENTS.md):

- **Rust:** edition 2024, Rust 1.96.
  Shared dependency versions live in the root `[workspace.dependencies]`, every crate has `[lints] workspace = true`, and `cargo fmt` (100 columns) and `cargo clippy -D warnings` must pass.
- **TLS** is rustls with ring only, never aws-lc-rs.
  Production HTTP clients use `rl_http::client_builder()`, which selects ring and platform trust
  explicitly without changing the embedding host's global provider.
  Tests that build a `reqwest` client install the provider first: `let _ = rustls::crypto::ring::default_provider().install_default();`.
- **Crypto:** `ring` for ES256, RS256 verification, SHA-256, HMAC and random numbers; `argon2` for passwords.
- **Crate contracts:** feature crates take `Arc<dyn CallerResolver>` and `Arc<dyn TokenSource>`, and expose an `axum::Router` with its state applied.
- **Errors are actionable:** say what failed and what to do.
- **Privacy:** never log tokens, passwords, secrets or Entra tokens, and keep identity claims out of info-level logs.
- **Local state:** everything rayfin-local writes goes under the state folder: `<project>/.rayfin-local/`, or `--state-dir`.
- **Rayfin behavior:** check the version-locked SDK dist and matching source tags, not memory; `AGENTS.md` lists the sources of truth.
- **Tests:** focused tests for the behavior you add; never test constants or framework behavior.
- **JS helpers:** plain ESM with no dependencies of their own; see [`js/README.md`](../js/README.md).
- **Docs:** one sentence per line.

## Releasing

The [official ADO pipeline](release-signing.md) automatically releases version increases merged into `main`.
It builds and signs Windows x64 on the existing `1ES-AppDev/windows-2022` infrastructure, cross-compiles Linux x64 there, and builds Apple Silicon on hosted Intel macOS; ESRP signs Linux and signs and notarizes macOS.
Publication requires the verified `windows-signed`, `macos-signed` and `linux-signed` artifacts and all official output/source-security checks; there is no unsigned fallback.
The [signing runbook](release-signing.md#linux-and-apple-silicon) describes the Linux and macOS paths and the remaining validation TODOs.
To publish a signed test prerelease, label its PR `release-test` and queue that PR's exact head commit with `testReleasePr` set to the PR number.
It publishes `v<base version>-alpha.<ADO build ID>`, never latest; see the [test-release runbook](release-signing.md#on-demand-pr-test-releases).
GitHub Actions release publishing is removed: there is no unsigned fallback.
Intel macOS and Linux ARM64 users must pin the existing unsigned `0.2.8` release or build from source.

1. Increase `[workspace.package].version` in the root `Cargo.toml`, and update `Cargo.lock` with `cargo update --workspace`.
2. Merge that change into `main`; no tag push or manual queue is required.

The official pipeline then:

- compares the version with the commit's first parent (the previous `main` tip); unchanged or decreased versions do not release;
- tests and builds `rayfin-local` with Rust 1.96.0 for `x86_64-pc-windows-msvc`, cross-compiles `x86_64-unknown-linux-gnu` on Windows and `aarch64-apple-darwin` on Intel macOS;
- signs through ESRP and independently verifies Windows Authenticode, timestamp and executable version;
- signs and notarizes the macOS executable through ESRP, then independently verifies its Developer ID, hardened runtime, timestamp and notarization;
- signs the Linux archive with a detached PGP signature through ESRP, then independently verifies it against the pinned Microsoft key;
- packages each executable with `README.md` and `LICENSE` (a ZIP for Windows, a `.tar.gz` for macOS and Linux);
- waits for verification, build/output and source-security checks, then creates `v<version>` and its GitHub Release at the exact source commit;
- attaches the three archives, the Linux `.sig` and `microsoft.asc`, each platform's `*-signing.json`, a combined `SHA256SUMS.txt`, `install.ps1` and `install.sh`; stable versions become latest, while prereleases never do.

Native release-binary execution tests for Linux and Apple Silicon are deferred; the signing runbook tracks [Linux](release-signing.md#deferred-native-linux-testing) and [Apple Silicon](release-signing.md#deferred-native-apple-silicon-testing) TODOs.
The retained Unix helpers require `nativeExecutionTested: false`; signature verification is not a native execution test.

The README's one-line installers fetch `install.sh` and `install.ps1` from the `main` branch, and those install the latest release (prereleases excluded) unless `RAYFIN_LOCAL_VERSION` is set.
Keep the scripts compatible with the releases they may install: they expect the archive names above and `SHA256SUMS.txt`.

If a run fails, manually queue the same version-bump commit from `main` after fixing the external failure.
Existing releases are never edited or overwritten; a source fix needs a new version bump.
The pipeline compares semantic versions, including prerelease precedence, and rejects malformed versions.

To try the scripts before a release, run them against an existing one: `RAYFIN_LOCAL_VERSION=<version> RAYFIN_LOCAL_INSTALL_DIR=/tmp/rl sh install.sh`, or in PowerShell set `$env:RAYFIN_LOCAL_VERSION`, `$env:RAYFIN_LOCAL_INSTALL_DIR` and `$env:RAYFIN_LOCAL_NO_MODIFY_PATH = '1'`, then run `./install.ps1`.
Lint them with `shellcheck --shell=sh install.sh` and [actionlint](https://github.com/rhysd/actionlint) for the workflows.
