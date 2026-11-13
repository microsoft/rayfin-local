# Official binary-signing pipeline

`.azure-pipelines/rayfin-local.official.yml` is the Windows-only signing-validation
pipeline for [powerbi/AppDev](https://dev.azure.com/powerbi/AppDev/_build).
The registered definition is
[`rayfin-local` (20870)](https://dev.azure.com/powerbi/AppDev/_build?definitionId=20870).
It follows the
[Fabricator pilot](https://github.com/microsoft/rayfin-fabricator/blob/019198cf0a11c0acc712425043d7478287c4a47d/.azure-pipelines/fabricator.official.yml).

## Current scope

CI and PR triggers are disabled. Queue the pipeline manually for a reviewed source
commit. It extends `v1/1ES.Official.PipelineTemplate.yml`, keeps the template's
security checks enabled, and uses `1ES-AppDev` with the `windows-2022` image.

The job verifies the checkout against `Build.SourceVersion`, installs Rust 1.96.0
and Node.js 22, runs the workspace tests with the lockfile, and builds the Windows
x64 release binary with the static C runtime, matching the existing GitHub release
build. It checks the binary's version against Cargo metadata; a tag run must use
`v<package version>`. After building, `EsrpCodeSigning@6` signs only
`rayfin-local.exe` using the owner-approved OSS/component keycode `CP-231522`.
The operation requests SHA-256 Authenticode signing, an RFC 3161 SHA-256 timestamp,
and ESRP signature verification. The job then independently requires a valid
Authenticode signature from Microsoft Corporation and a timestamp before
executing the binary's version check or recording its hash.

The official job generates a checkout-local `.cargo/config.toml` that replaces
crates.io with the existing AppDev `rayfin-official` Azure Artifacts Cargo feed.
`CargoAuthenticate@0` authenticates using the pipeline's build identity; no PAT
or new service connection is needed. The config is generated only in the official
job so local development and public GitHub CI do not require internal feed access.
The lockfile and vendored TDS patch remain unchanged.

The `windows-validation` artifact contains `source.json`: source commit, version,
target, toolchain, build ID, the signed binary's SHA-256, signing keycode, and signer
certificate subject/thumbprint. The official template also adds SBOM/provenance
metadata. The binary stays on the agent. Signing or verification failure blocks
metadata publication; there is no unsigned fallback. This pipeline does not
publish executables or GitHub releases, or publish crates or npm packages.

The [unsigned validation run](https://dev.azure.com/powerbi/AppDev/_build/results?buildId=21383235&view=results)
succeeded for commit `e2aba0e69627faf8cfdd2be8aa83ec4e71650f27`, including the
workspace tests, release build, and official security checks. Its historical
metadata correctly records `signed: false`. Owner approval for client reuse and
`CP-231522` was confirmed for the next signing phase; that approval is separate
from a successful signing run.

## Pipeline configuration and resource approvals

The existing AppDev definition uses:

| Setting | Value |
| --- | --- |
| Organization / project | `https://dev.azure.com/powerbi` / `AppDev` |
| Pipeline name | `rayfin-local` |
| Pipeline folder | `\GitHub` |
| Repository | `microsoft/rayfin-local` (GitHub) |
| YAML path | `/.azure-pipelines/rayfin-local.official.yml` |
| Triggers | Manual only; no CI or PR override |
| Agent pool / image | `1ES-AppDev` / `windows-2022` |
| Template repository | `1ESPipelineTemplates/1ESPipelineTemplates`, `refs/tags/release` |
| Template | `v1/1ES.Official.PipelineTemplate.yml` |
| ESRP connection | `Fabric-ESRP-Secure-AppDev` |
| Windows signing keycode | `CP-231522` (OSS/component signing) |
| Published output | Validation and SBOM/provenance metadata only; no executables |

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
- Keep the job authorization scope project-scoped where supported, granting any
  cross-project template access explicitly.
- Grant the pipeline's build identity **Feed and Upstream Reader (Collaborator)**
  on the AppDev `rayfin-official` feed if it cannot restore dependencies. Upstream
  saving requires this role; Feed Reader alone can only read already-saved crates.
  Do not grant feed publishing or owner access.
- Authorize `Fabric-ESRP-Secure-AppDev` for definition 20870 only. Confirm that the
  approved identity can use the configured request-signing certificate and the
  ESRP client can use `CP-231522`. Do not grant global pipeline access, broader
  vault roles, or additional keycodes to work around an authorization failure.

Pool role permissions and pipeline resource authorization are separate. Check
[AppDev agent pools](https://dev.azure.com/powerbi/AppDev/_settings/agentqueues)
under `1ES-AppDev` -> Security and Pipeline permissions. The Fabricator pilot
initially encountered a missing pool **Use** permission. The initial rayfin-local
validation has since succeeded, but signing resource authorization and a real
signing run must be verified separately.

Suggested approval-request text:

> Please authorize the Windows signing phase of `rayfin-local` (20870) in powerbi/AppDev using
> `microsoft/rayfin-local` and `/.azure-pipelines/rayfin-local.official.yml`.
> This is a manual-only 1ES official Windows signing-validation pipeline using
> `1ES-AppDev/windows-2022` and the 1ES official template repository.
> ESRP registration-owner approval has been confirmed for reusing
> `Fabric-ESRP-Secure-AppDev` with OSS/component keycode `CP-231522`.
> Please authorize that connection for this pipeline only and configure its
> approved identity/vault/certificate references.
> It builds/tests and signs rayfin-local but publishes metadata only, never
> executables. No package publishing or public release permission is requested.

After authorization, queue a reviewed commit and confirm the signing task,
independent signature/version checks, official security checks, and metadata
artifact succeed. Download `windows-validation` and verify `source.json` matches
the queued commit, records `signed: true` and `signingKeyCode: CP-231522`, and has
`distribution: none`. Retain the run link; do not claim signing succeeded merely
because the service connection is ready or the unsigned pilot passed.

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

## Subsequent phases

Windows signing validation deliberately does not enable binary distribution.

- Decide the protected distribution path for signed archives separately.
  Package the executable and calculate distribution checksums only after
  signature verification. Signing failures must block release output.
- Establish an approved macOS build pool and the required Apple signing and
  notarization operations before adding macOS output.
- Define the future Node adapter's actual native artifact and packaging format
  before adding its build/signing stage. It is not part of this pilot.
- crates.io publishing remains out of scope.

The existing `.github/workflows/release.yml` is unchanged: version tags still
trigger its current GitHub binary-release workflow. This pilot neither signs
those artifacts nor replaces or gates that workflow. Do not treat a successful
validation run as a distributed release.
