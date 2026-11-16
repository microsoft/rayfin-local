# Official binary-signing pipeline

`.azure-pipelines/rayfin-local.official.yml` is the cross-platform official signing
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
security checks enabled, and uses `1ES-AppDev` for Windows/Linux and the existing `GitHub-hosted Agents` pool for Apple Silicon.
The [platform section](#linux-and-apple-silicon) describes the additional keys, native verification and validation status.

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
or new service connection is needed. The config is generated only in the official
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
That mode publishes a three-platform prerelease named `v<base version>-alpha.<ADO build ID>` directly, never marking it latest.
The current base version produces names such as `v0.2.8-alpha.21422341`.
GitHub Actions release publishing has been removed; there is no unsigned publishing path.
New releases require Windows x64, Linux x64 and Apple Silicon artifacts; a failed platform blocks the whole release.
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
| Linux pool / image | `1ES-AppDev` / `ubuntu-2204` |
| Apple Silicon pool / image | `GitHub-hosted Agents` / `macos-26-arm64`, `hostArchitecture: arm64` |
| Template repository | `1ESPipelineTemplates/1ESPipelineTemplates`, `refs/tags/release` |
| Template | `v1/1ES.Official.PipelineTemplate.yml` |
| ESRP connection | `Fabric-ESRP-Secure-AppDev` |
| Windows signing keycode | `CP-231522` (OSS/component signing) |
| Linux signing keycode | `CP-450779-Pgp` (`LinuxSign`) |
| Apple signing keycode | `CP-401337-Apple` (`MacAppDeveloperSign`, then `MacAppNotarize`) |
| Published output | Windows ZIP, Linux/Apple Silicon tarballs, signatures, signing metadata, and SBOM/provenance |
| PR test release | Explicit positive `testReleasePr`, same-repository open PR targeting main, exact head commit, `release-test` label; publishes three-platform alpha |
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
- If an additional grant is required, authorize `GitHub-hosted Agents` for definition 20870 only; do not broaden the queue's existing access configuration.
- Keep the job authorization scope project-scoped where supported, granting any
  cross-project template access explicitly.
- Grant the pipeline's build identity **Feed and Upstream Reader (Collaborator)**
  on the AppDev `rayfin-official` feed if it cannot restore dependencies. Upstream
  saving requires this role; Feed Reader alone can only read already-saved crates.
  Do not grant feed publishing or owner access.
- Authorize `Fabric-ESRP-Secure-AppDev` for definition 20870 only. Confirm that the
  approved identity can use the configured request-signing certificate and the
  ESRP client can use the three owner-approved keycodes and operations listed above.
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

> Please authorize the cross-platform signing phase of `rayfin-local` (20870) in powerbi/AppDev using
> `microsoft/rayfin-local` and `/.azure-pipelines/rayfin-local.official.yml`.
> This is a 1ES official signing pipeline using `1ES-AppDev` for Windows/Linux and
> the existing `GitHub-hosted Agents/macos-26-arm64` pool for native Apple Silicon builds and verification.
> ESRP registration-owner approval has been confirmed for reusing
> `Fabric-ESRP-Secure-AppDev` with `CP-231522`, `CP-450779-Pgp` and `CP-401337-Apple`, including Apple notarization.
> Please authorize that connection for this pipeline only and configure its
> approved identity/vault/certificate references.
> It builds/tests and signs rayfin-local and independently verifies all three platforms.
> Its production release job publishes all three platforms automatically for version increases on main, or alpha prereleases for explicitly queued, release-test-labelled same-repository PRs.
> No crates.io or npm publishing permission is requested.

After authorization, queue a reviewed commit and confirm the signing task,
independent signature/version checks, official security checks, and metadata
artifact succeed. Download `windows-validation` and verify `source.json` matches
the queued commit, records `signed: true` and `signingKeyCode: CP-231522`, and has
`distribution: pipeline-artifact`.
Download `windows-signed` and check its archive and executable hashes against the metadata.
Also require `linux-signed` and `macos-signed`, and verify their detached signature or notarization as described below.
Retain the run link; do not claim signing succeeded merely
because the service connection is ready or the unsigned pilot passed.

### On-demand PR test releases

1. Add the exact `release-test` label to a trusted same-repository PR targeting `main`.
2. Queue definition 20870 manually from that PR's head branch at its current head commit, setting `testReleasePr` to the PR number.
3. The pipeline checks the live PR/label/commit before building, changes the workspace version and lockfile only in its build checkout to `<base version>-alpha.<Build.BuildId>`, and builds/tests/signs that version.
4. After all three platform verifiers and the separate source-security stage succeed, the production release job rechecks the live PR/label/commit before creating the GitHub prerelease.

There are no automatic PR triggers and no fork support.
Without the label, or if the PR is closed or its head changes while the build runs, no test release is published.
Removing the label revokes publication for queued/in-flight test builds.
The PR's committed Cargo files and existing public releases are not changed.
Existing `v0.2.8` assets are not replaced.
The prerelease includes all three archives, three `*-signing.json` files, the Linux `.sig` and public key, combined `SHA256SUMS.txt`, `install.sh`, and `install.ps1`.
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

The user confirmed ESRP-owner approval for the additional platform signing operations.
The shared connection alone was not treated as approval.
The keys and operations follow the maintained [VS Code ESRP implementation](https://github.com/microsoft/vscode/blob/972b66fdc541e10689adfd90ade9549dea1b18bc/build/azure-pipelines/common/sign.ts).
All ESRP submissions run on the Windows signing pool with the existing managed identity and certificate references.
Native build and verification jobs share `.azure-pipelines/templates/unix-job.yml`.

Linux x64 builds and tests on `1ES-AppDev/ubuntu-2204`, using the same authenticated Cargo feed as Windows.
`LinuxSign` signs a scratch copy of the `.tar.gz` with `CP-450779-Pgp`.
The original archive is preserved because the detached-signature operation replaces its input with signature content; see [.NET Arcade's handling](https://github.com/dotnet/arcade/blob/28463b67ff4f7c01701c2a24fb23d36af978923c/src/Microsoft.DotNet.SignTool/src/SignTool.cs).
That response becomes `<archive>.sig`, not a replacement tarball.
The native Linux verifier requires GPG success, SHA-256 or stronger, and exactly the pinned Microsoft public-key fingerprint `BC528686B50D79E339D3721CEB3E94ADBE1229CF`.
The checked-in public key comes from [packages.microsoft.com](https://packages.microsoft.com/keys/microsoft.asc); no private key is checked in.
It also verifies the unchanged archive hash, extracted executable hash, version, commit and build ID.
The release includes the detached signature and `microsoft.asc`; users can import the key, check its fingerprint against the pin above, and run `gpg --verify <archive>.sig <archive>`.
The install script's checksum verification is not a substitute for this PGP check.

Apple Silicon builds/tests and verifies natively on AppDev queue 22798, the existing `GitHub-hosted Agents` pool, with `vmImage: macos-26-arm64` and `hostArchitecture: arm64`.
This is the documented [PAYG Apple Silicon image](https://learn.microsoft.com/en-us/azure/devops/pipelines/agents/github-hosted); usage is billed per minute, with no free tier.
The pool already exists; this change does not enable billing or broaden its permissions.
The build produces a ZIP for ESRP `MacAppDeveloperSign` with `Hardening: --options=runtime`, followed by `MacAppNotarize`, both using `CP-401337-Apple`.
The native verifier extracts the returned ZIP and requires ARM64, strict `codesign` verification, `Developer ID Application: Microsoft Corporation (UBF8T346G9)`, Team ID `UBF8T346G9`, hardened runtime, a timestamp, and `spctl` acceptance with `source=Notarized Developer ID`.
Only after these checks does it produce the installer-compatible `.tar.gz`, preserving the signed executable.
No Gatekeeper bypass or unsigned fallback is used.

The final release job downloads only verified platform artifacts, matches their versions/targets/commit/build IDs and archive hashes, and assembles combined checksums.
A missing platform, wrong signer, invalid signature, missing notarization, or mismatched artifact blocks publication.
The Linux key fingerprint and Apple identity above are pinned verifier expectations, not claims about a completed Unix signing run.
The main and alpha paths compile through the live official template with the checked-in job and signing templates.
Local template references use absolute repository paths with `@self`, because relative references inside the official template's parameters resolve against the 1ES template repository.
The first actual cross-platform run is still required to establish Linux image capacity, Apple native tool availability, ESRP archive/CLI handling, and successful public assets.
Do not treat template compilation or owner approval as evidence that those runtime operations have passed.

## Subsequent phases

- Consider native Intel macOS and Linux ARM64 release builds separately.
- Define the future Node adapter's actual native artifact and packaging format
  before adding its build/signing stage. It is not part of this pilot.
- crates.io publishing remains out of scope.

Both main releases and PR-test releases require all official signed platform archives.
The old `.github/workflows/release.yml` publisher is removed.
