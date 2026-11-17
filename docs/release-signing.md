# Official binary-signing pipeline

`.azure-pipelines/rayfin-local.official.yml` is the official binary-signing
pipeline for [powerbi/AppDev](https://dev.azure.com/powerbi/AppDev/_build).
The registered definition is
[`rayfin-local` (20870)](https://dev.azure.com/powerbi/AppDev/_build?definitionId=20870).
It follows the
[Fabricator pilot](https://github.com/microsoft/rayfin-fabricator/blob/019198cf0a11c0acc712425043d7478287c4a47d/.azure-pipelines/fabricator.official.yml).

## Current scope

Changes to root `Cargo.toml` on `main` automatically queue the pipeline.
It publishes only if the workspace version increases relative to the commit's first parent, the previous `main` tip.
PR and tag triggers are disabled; manually queued artifact builds and labelled PR alpha tests remain available.
It extends `v1/1ES.Official.PipelineTemplate.yml`, keeps the template's
security checks enabled, and signs Windows x64 on the existing `1ES-AppDev/windows-2022` infrastructure.
Linux/macOS build, signing, verification and publication paths are commented out.
They will be re-enabled in a separate PR after consulting the ESRP team about key authorization and completing validation.
The [platform section](#linux-and-apple-silicon) retains the deferred design and outstanding TODOs.

The job verifies the checkout against `Build.SourceVersion`, installs Rust 1.96.0
and Node.js 22, runs the workspace tests with the lockfile, and builds the Windows
x64 release binary with the static C runtime, matching the existing GitHub release
build. It checks the binary's version against Cargo metadata; a tag run must use
`v<package version>`. After building, `EsrpCodeSigning@6` signs only
`rayfin-local.exe` using the owner-approved OSS/component keycode `CP-231522`.
The operation requests SHA-256 Authenticode signing, an RFC 3161 SHA-256 timestamp,
and ESRP signature verification. The job then independently requires a valid
Authenticode signature from Microsoft 3rd Party Application Component and a timestamp before
executing the binary's version check or recording its hash.

The expected publisher follows the
[Windows signing-key guidance](https://eng.ms/docs/cloud-ai-platform/ciso-organization/m365-security-engineering/security-services/helios/microsoft-dciso-security-requirements/user-stories/code-security/001)
for `CP-231522`; the first signing run succeeded at ESRP but failed locally because
the initial publisher check incorrectly expected the first-party publisher
Microsoft Corporation. Keep the exact OSS/component publisher check rather than
accepting any trusted signer or changing the approved keycode.

The official job generates a checkout-local `.cargo/config.toml` that replaces
crates.io with the existing AppDev `rayfin-official` Azure Artifacts Cargo feed.
`CargoAuthenticate@0` authenticates using the pipeline's build identity; no PAT
or new service connection is needed. The Windows job likewise points npm at the
feed's npmjs upstream for the test fixtures' `npm ci`, through a generated user
`.npmrc` and `npmAuthenticate@0`: the agents' proxy to registry.npmjs.org refused
`@microsoft/rayfin-lib` 2.0.0. The config is generated only in the official
job so local development and public GitHub CI do not require internal feed access.
Normal runs preserve the lockfile; PR test runs update only the workspace package versions in their build checkout.
The vendored TDS patch remains unchanged.

The `windows-validation` artifact contains `source.json`: source commit, version,
target, toolchain, build ID, the signed binary's SHA-256, signing keycode, and signer
certificate subject/thumbprint.
After signature and version verification, the job packages the executable with `README.md` and `LICENSE` as `rayfin-local-<version>-x86_64-pc-windows-msvc.zip`.
The additional `windows-signed` pipeline artifact contains that ZIP and `windows-signing.json`, including both the executable and archive SHA-256 hashes and `distribution: pipeline-artifact`.
The official template scans the output and adds SBOM/provenance metadata.
Signing or verification failure blocks release-archive output; there is no unsigned fallback.
GitHub publication additionally waits for both the build/output checks and separate source-security stage to succeed.

Main version increases and explicit PR test requests enable a production-classified 1ES release job using the existing `github.com_christopheranderson` GitHub service connection.
Other manually queued runs only output pipeline artifacts.
No ADO credentials are copied to GitHub Actions.
Do not enable global service-connection access if publishing needs an additional resource approval.
crates.io and npm publishing remain out of scope.

An on-demand PR test release uses the `testReleasePr` queue parameter and requires the PR's explicit `release-test` label.
That mode publishes a Windows-only prerelease named `v<base version>-alpha.<ADO build ID>` directly, never marking it latest.
The current base version produces names such as `v0.3.2-alpha.21422341`.
GitHub Actions release publishing has been removed; there is no unsigned publishing path.
New releases require a verified signed Windows x64 artifact; no Unix assets are published while those paths are disabled.
Stable main releases become latest; prerelease versions and PR-test alphas never do.

The [unsigned validation run](https://dev.azure.com/powerbi/AppDev/_build/results?buildId=21383235&view=results)
succeeded for commit `e2aba0e69627faf8cfdd2be8aa83ec4e71650f27`, including the
workspace tests, release build, and official security checks. Its historical
metadata correctly records `signed: false`. Owner approval for client reuse and
`CP-231522` was confirmed separately for the signing phase.

The [signed validation run](https://dev.azure.com/powerbi/AppDev/_build/results?buildId=21422341&view=results)
succeeded for commit `bc94d0e25d7fcaaaffb4403123290dc0386db15b`.
ESRP signing, independent Windows signature/timestamp/version verification, and
the official security checks all passed.
The downloaded metadata records version `0.2.8`, `signed: true`,
`signingKeyCode: CP-231522`, the expected OSS/component publisher, and
`distribution: none`.
Its signed executable SHA-256 is
`ce22596eb0bedfdd059cae7f994f22aaabf51608dadba3f2117f031796949545`.
That historical run predates archive output and contains only validation and SBOM/provenance metadata, not the executable.
The [end-to-end alpha run](https://dev.azure.com/powerbi/AppDev/_build/results?buildId=21425056&view=results) succeeded at commit `0ba9f263052260879b34bfaf387368fb6e6ab2b0`.
It published [v0.2.8-alpha.21425056](https://github.com/microsoft/rayfin-local/releases/tag/v0.2.8-alpha.21425056).
All four public asset digests, the archive checksum, and the packaged executable hash were checked against signing metadata; stable `v0.2.8` remained latest.
Automatic main-version routing and Unix signing are subsequent changes; that alpha run verifies only the Windows signing and publishing path, not the new main trigger or Unix operations.

## Pipeline configuration and resource approvals

The existing AppDev definition uses:

| Setting | Value |
| --- | --- |
| Organization / project | `https://dev.azure.com/powerbi` / `AppDev` |
| Pipeline name | `rayfin-local` |
| Pipeline folder | `\GitHub` |
| Repository | `microsoft/rayfin-local` (GitHub) |
| YAML path | `/.azure-pipelines/rayfin-local.official.yml` |
| Triggers | Root `Cargo.toml` changes on `main`; no PR or tag triggers; no UI override |
| Windows pool / image | `1ES-AppDev` / `windows-2022` |
| Deferred Linux cross-build/verification | Commented out; `1ES-AppDev/windows-2022`, pinned Zig 0.14.1 and cargo-zigbuild 0.24.1 |
| Deferred Apple build/verification | Commented out; `Azure Pipelines/macos-15`, `hostArchitecture: amd64`; cross-compiles `aarch64-apple-darwin` |
| Deferred Unix ESRP submission | Commented out; `1ES-AppDev/windows-2022`; no signing credentials on Unix agents |
| Template repository | `1ESPipelineTemplates/1ESPipelineTemplates`, `refs/tags/release` |
| Template | `v1/1ES.Official.PipelineTemplate.yml` |
| ESRP connection | `Fabric-ESRP-Secure-AppDev` |
| Windows signing keycode | `CP-231522` (OSS/component signing) |
| Deferred Linux signing keycode | `CP-450779-Pgp` (`LinuxSign`); service denied client access in run 21430420 |
| Deferred Apple signing keycode | `CP-401337-Apple` (`MacAppDeveloperSign`, then `MacAppNotarize`); not runtime-validated |
| Published output | Signed Windows ZIP, signing metadata, checksums, `install.ps1`, and pipeline SBOM/provenance |
| PR test release | Explicit positive `testReleasePr`, same-repository open PR targeting main, exact head commit, `release-test` label; publishes Windows-only alpha |
| Main release | Version increase against first parent; creates matching tag/release; stable versions become latest |

Configure these pipeline variables from the approved AppDev ESRP registration.
The existing `rayfin-official` definition (18716) provides the configuration for
the shared client. Reuse only the approved identity and certificate references,
not unrelated publishing settings or signing keycodes.

| Variable | Purpose |
| --- | --- |
| `EsrpAppRegistrationClientId` | Authentication identity client ID |
| `EsrpAppRegistrationTenantId` | Authentication identity tenant ID |
| `EsrpClientId` | Registered ESRP client ID |
| `EsrpKeyVaultName` | Vault holding the request-signing certificate |
| `EsrpSignCertName` | Request-signing certificate name |

`UseMSIAuthentication: true` follows the existing AppDev signing configuration.
The service connection supplies authentication; no PAT, private key, or certificate
contents belong in YAML. Missing settings fail before the build. The connection
and signing keycode are fixed in the reviewed YAML rather than selected at queue
time.

Request these permissions **only for this pipeline**, not for all pipelines:

- Authorize an AppDev GitHub service connection that can read
  `microsoft/rayfin-local`. Verify repository access rather than assuming that a
  connection used by another Rayfin pipeline includes this repository.
- Authorize the template repository and grant the pipeline's build identity the
  required read access.
- Authorize `1ES-AppDev` for the pipeline, and grant the registering/queueing user
  the pool's **User / Use** permission if missing.
- When restoring Apple builds in the separate PR, if an additional grant is required, authorize `Azure Pipelines` for definition 20870 only; do not broaden the queue's existing access configuration.
  Queue 11640 already has hosted-pool authorization; its existing permissions were not changed.
- Keep the job authorization scope project-scoped where supported, granting any
  cross-project template access explicitly.
- Grant the pipeline's build identity **Feed and Upstream Reader (Collaborator)**
  on the AppDev `rayfin-official` feed if it cannot restore dependencies. Upstream
  saving requires this role; Feed Reader alone can only read already-saved crates
  and npm packages.
  Do not grant feed publishing or owner access.
- Authorize `Fabric-ESRP-Secure-AppDev` for definition 20870 only. Confirm that the
  approved identity can use the configured request-signing certificate and the
  ESRP client can use `CP-231522`.
  The separate Unix PR must establish effective access to its additional keycodes and operations before re-enabling those paths.
  Do not grant global pipeline access, broader
  vault roles, or additional keycodes to work around an authorization failure.

Pool role permissions and pipeline resource authorization are separate. Check
[AppDev agent pools](https://dev.azure.com/powerbi/AppDev/_settings/agentqueues)
under `1ES-AppDev` -> Security and Pipeline permissions. The Fabricator pilot
initially encountered a missing pool **Use** permission. The initial rayfin-local
validation and the signed validation linked above have since succeeded.
Any future resource or identity changes still require scoped authorization and
a real signing run.

Suggested approval-request text:

> Please authorize the Windows signing phase of `rayfin-local` (20870) in powerbi/AppDev using
> `microsoft/rayfin-local` and `/.azure-pipelines/rayfin-local.official.yml`.
> This is a 1ES official signing pipeline using `1ES-AppDev/windows-2022`.
> ESRP registration-owner approval has been confirmed for reusing
> `Fabric-ESRP-Secure-AppDev` with `CP-231522`.
> Please authorize that connection for this pipeline only and configure its
> approved identity/vault/certificate references.
> It builds/tests and signs rayfin-local and independently verifies Windows Authenticode.
> Its production release job publishes Windows assets automatically for version increases on main, or alpha prereleases for explicitly queued, release-test-labelled same-repository PRs.
> Linux and Apple Silicon paths are commented out and deferred to a separate PR.
> No crates.io or npm publishing permission is requested.

After authorization, queue a reviewed commit and confirm the signing task,
independent signature/version checks, official security checks, and metadata
artifact succeed. Download `windows-validation` and verify `source.json` matches
the queued commit, records `signed: true` and `signingKeyCode: CP-231522`, and has
`distribution: pipeline-artifact`.
Download `windows-signed` and check its archive and executable hashes against the metadata.
Retain the run link; do not claim signing succeeded merely
because the service connection is ready or the unsigned pilot passed.

### On-demand PR test releases

1. Add the exact `release-test` label to a trusted same-repository PR targeting `main`.
2. Queue definition 20870 manually from that PR's head branch at its current head commit, setting `testReleasePr` to the PR number.
3. The pipeline checks the live PR/label/commit before building, changes the workspace version and lockfile only in its build checkout to `<base version>-alpha.<Build.BuildId>`, and builds/tests/signs that version.
4. After Windows signature verification and all official output/source-security checks succeed, the production release job rechecks the live PR/label/commit before creating the GitHub prerelease.

There are no automatic PR triggers and no fork support.
Without the label, or if the PR is closed or its head changes while the build runs, no test release is published.
Removing the label revokes publication for queued/in-flight test builds.
The PR's committed Cargo files and existing public releases are not changed.
Existing `v0.2.8` assets are not replaced.
The prerelease includes the signed Windows ZIP, `windows-signing.json`, `SHA256SUMS.txt`, and `install.ps1`.
To install it explicitly, set `RAYFIN_LOCAL_VERSION` to its alpha version before running the appropriate installer.
There is no GitHub Actions release publisher.

### Automatic releases from main

1. Increase `[workspace.package].version` in `Cargo.toml` and run `cargo update --workspace` to update the workspace versions in `Cargo.lock`.
2. Merge the reviewed change into `main`.
3. Definition 20870 automatically queues for the root manifest change, compares versions against the first parent, and releases only a semantic-version increase.
4. After the same signing, verification, output-security and source-security checks as the alpha path, it creates the version tag and GitHub Release at the exact main commit.

No manually pushed tag is needed.
Stable versions become latest; prerelease versions never do.
Changes that leave the version unchanged or decrease it do not build/sign/publish automatically.
Keep the definition's trigger configuration sourced from YAML; do not enable a UI trigger override.
For an external failure, manually requeue the same main version-bump commit with `testReleasePr: 0`.
The create-only GitHub task refuses an existing release rather than replacing published assets.
A source correction must use a new version.
Intel macOS and Linux ARM64 are not built by this pipeline; those users must pin the existing unsigned `0.2.8` release or build from source.
While Unix releases are deferred, Apple Silicon and Linux x64 users must also pin that legacy release.

### Dependency restore failures

The [first approved run](https://dev.azure.com/powerbi/AppDev/_build/results?buildId=21360573&view=results)
reached the Windows job but failed before compilation: Cargo's request to
`index.crates.io` received a proxy `CONNECT` 403. The network-isolation log showed
required `CFSClean`, `CFSClean2`, and `CFSClean3` policies even with the pipeline's
`Permissive` base policy.

Restore through the authenticated Azure Artifacts feed instead, following
[CFS Cargo configuration](https://eng.ms/docs/coreai/devdiv/one-engineering-system-1es/1es-docs/secure-supply-chain/secure-supply-chain-analysis-task/cfs-detectors/cargo/cfs-cargo).
Do not disable the proxy or network isolation, or add public-registry exceptions.
For feed 401/403 errors, check the build identity's feed role. If a dependency is
blocked by feed policy, resolve it with the feed/security owners rather than
falling back to crates.io.

## Linux and Apple Silicon

**Deferred to a separate PR.**
The Linux cross-build steps, `BuildUnix`, `SignUnix`, `VerifyUnix`, Unix artifact inputs, and three-platform assembly/publication are commented out in the pipeline.
The templates, cross-build script, public key and verification helpers are retained for that follow-up; they are not part of the active release path.
The design below describes the deferred implementation, not currently published Unix signing guarantees.

The [Windows cross-build run](https://dev.azure.com/powerbi/AppDev/_build/results?buildId=21430420) successfully built Linux x64 on Windows and Apple Silicon on Intel macOS, and signed Windows.
ESRP `LinuxSign` then returned `FailDoNotRetry`: client `f351681f-63af-4d0a-80b2-80bc2ec5cbb2` is unauthorized for `CP-450779-Pgp`.
Apple signing/notarization, Unix signature verification and release publication were skipped.
Owner approval did not establish effective service-side key access.
Consult the ESRP team before re-enabling these paths; do not bypass the checks or broaden unrelated permissions.

`ValidateWindows` also cross-compiles Linux; `BuildUnix` and `VerifyUnix` use only the existing Intel macOS hosted queue.
`SignUnix` explicitly runs on `1ES-AppDev/windows-2022`; ESRP signing and notarization do not require Unix-hosted signing credentials.
The final Windows release job attaches only verified exact-build platform artifacts.

- [ ] TODO: In a separate PR, confirm ESRP key access with the ESRP team, restore the commented Unix paths and all-platform publication gate, and validate signing/notarization, independent platform verification and every public release asset end to end.

The user confirmed ESRP-owner approval for the additional platform signing operations.
The shared connection alone was not treated as approval.
The keys and operations follow the maintained [VS Code ESRP implementation](https://github.com/microsoft/vscode/blob/972b66fdc541e10689adfd90ade9549dea1b18bc/build/azure-pipelines/common/sign.ts).
All ESRP submissions run on the Windows signing pool with the existing managed identity and certificate references.
Native build and verification jobs share `.azure-pipelines/templates/unix-job.yml`.

Linux x64 cross-compiles in the Windows build job after its native Windows workspace tests and Authenticode packaging.
`Build-LinuxOnWindows.ps1` downloads Zig 0.14.1 from the official site, checks its pinned SHA-256 before execution, and installs cargo-zigbuild 0.24.1 through the authenticated Cargo feed.
It targets `x86_64-unknown-linux-gnu.2.28`; both Rust and bundled C dependencies (`ring` and SQLite) must compile successfully.
Python 3.13's tar writer explicitly preserves executable mode `0755`, even when packaging on Windows.
Packaging rejects non-x86-64 ELF64 files rather than attempting to execute foreign binaries.
Native Linux release-binary execution and Linux-targeted workspace tests do not run in the official Windows job; public GitHub CI's Linux tests are not a substitute for testing that exact cross-compiled artifact.
`LinuxSign` signs a scratch copy of the `.tar.gz` with `CP-450779-Pgp`.
The original archive is preserved because the detached-signature operation replaces its input with signature content; see [.NET Arcade's handling](https://github.com/dotnet/arcade/blob/28463b67ff4f7c01701c2a24fb23d36af978923c/src/Microsoft.DotNet.SignTool/src/SignTool.cs).
That response becomes `<archive>.sig`, not a replacement tarball.
The Windows verifier uses Git for Windows GPG and requires success, SHA-256 or stronger, and exactly the pinned Microsoft public-key fingerprint `BC528686B50D79E339D3721CEB3E94ADBE1229CF`.
The checked-in public key comes from [packages.microsoft.com](https://packages.microsoft.com/keys/microsoft.asc); no private key is checked in.
It also verifies the unchanged archive hash, extracted executable hash/ELF architecture, version metadata, commit and build ID.
It requires the expected archive layout and Unix executable mode, but does not execute the Linux binary.
The release includes the detached signature and `microsoft.asc`; users can import the key, check its fingerprint against the pin above, and run `gpg --verify <archive>.sig <archive>`.
The install script's checksum verification is not a substitute for this PGP check.

Apple Silicon cross-compiles on AppDev queue 11640, the existing `Azure Pipelines` pool, using the [standard Intel macOS 15 image](https://learn.microsoft.com/en-us/azure/devops/pipelines/agents/hosted) and `hostArchitecture: amd64`.
The job installs the `aarch64-apple-darwin` Rust target and uses Apple's SDK/linker from the Intel host.
It runs workspace tests for the Intel host, but does not execute the ARM64 release binary or run ARM64-targeted tests.
There is no Apple Silicon VM requirement for the ESRP signing/notarization submissions.
This change does not enable PAYG billing or broaden pool permissions.
The build produces a ZIP for ESRP `MacAppDeveloperSign` with `Hardening: --options=runtime`, followed by `MacAppNotarize`, both using `CP-401337-Apple`.
The Intel macOS verifier extracts the returned ZIP and requires ARM64, strict `codesign` verification, `Developer ID Application: Microsoft Corporation (UBF8T346G9)`, Team ID `UBF8T346G9`, hardened runtime, a timestamp, and `spctl` acceptance with `source=Notarized Developer ID`.
Only after these checks does it produce the installer-compatible `.tar.gz`, preserving the signed executable.
Linux and Apple source/signing metadata explicitly record `nativeExecutionTested: false`, and release notes disclose both deferred execution checks.
No Gatekeeper bypass or unsigned fallback is used.

The three-platform assembler requires verified platform artifacts, matches their versions/targets/commit/build IDs and archive hashes, and assembles combined checksums.
A missing platform, wrong signer, invalid signature, missing notarization, or mismatched artifact blocks publication.
The Linux key fingerprint and Apple identity above are pinned verifier expectations, not claims about a completed Unix signing run.
The main and alpha paths compile through the live official template with the checked-in job and signing templates.
Local template references use absolute repository paths with `@self`, because relative references inside the official template's parameters resolve against the 1ES template repository.
The [first cross-platform run](https://dev.azure.com/powerbi/AppDev/_build/results?buildId=21427060) failed at agent allocation, before either Unix build or ESRP submission.
`1ES-AppDev` rejected the requested `ubuntu-2204` image; a subsequent attempt used the managed Ubuntu pool.
That queue required pool-admin authorization; no global access or broader role was granted.
An attempt to use already-authorized standard hosted `Azure Pipelines/ubuntu-24.04` in [run 21429587](https://dev.azure.com/powerbi/AppDev/_build/results?buildId=21429587) started an agent but failed the mandatory 1ES pre-job check: `Using a non 1ESHostedPool with 1ES PT is not allowed`.
The deferred configuration instead performs Linux cross-compilation on the existing approved Windows pool; no official security task is disabled or downgraded.
The Apple job failed with `No agent found in pool GitHub-hosted Agents which satisfies the specified demands: Agent.Version -gtVersion 5.281.1`.
That minimum comes from the required `CodeQL3000Init` / `CodeQL3000Finalize` tasks, not an optional demand added by this repository.
That failure motivated using the standard Intel image instead of depending on Apple Silicon capacity.
The Intel job still must satisfy every official task's minimum agent version; its build succeeded in run 21430420.
The [next cross-platform run](https://dev.azure.com/powerbi/AppDev/_build/results?buildId=21427733) reached the Unix resource-authorization checkpoint but could not start those jobs.
Do not downgrade or remove security tasks to bypass agent selection.
Linux build/restore and Intel-hosted cross-compilation succeeded in run 21430420.
Unix signing/notarization assessment, ESRP archive/CLI response handling and successful public Unix assets remain unverified.
Do not treat template compilation or owner approval as evidence that those runtime operations have passed.

### Deferred native Linux testing

- [ ] TODO: Run Linux-targeted workspace tests and smoke-test the exact cross-compiled, signed release on native Linux, including `--version`, before claiming Linux runtime validation.

Until that testing is restored, retain `nativeExecutionTested: false` in Linux metadata and the release-note disclosure.
ELF architecture, archive permissions, pinned GPG identity, modern digest, source identity and hash checks remain mandatory.

### Deferred native Apple Silicon testing

- [ ] TODO: Once an approved Apple Silicon runner is available, run the workspace tests with `--target aarch64-apple-darwin` and smoke-test the final signed/notarized release executable on ARM64, including its `--version` output.

This TODO does not defer architecture, Developer ID, hardened-runtime, timestamp or notarization verification.
Those checks remain mandatory before publishing an Apple artifact.
Until native execution testing is restored, keep the metadata and release-note disclosure above.

## Subsequent phases

- Consider native Intel macOS and Linux ARM64 release builds separately.
- Define the future Node adapter's actual native artifact and packaging format
  before adding its build/signing stage. It is not part of this pilot.
- crates.io publishing remains out of scope.

Both main releases and PR-test releases currently publish only the verified signed Windows archive.
The separate Unix PR must restore the all-three-platform gate before enabling Unix publication.
The old `.github/workflows/release.yml` publisher is removed.
