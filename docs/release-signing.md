# Official binary-signing pipeline

`.azure-pipelines/rayfin-local.official.yml` is the initial Windows-only validation
pipeline for [powerbi/AppDev](https://dev.azure.com/powerbi/AppDev/_build).
The registered definition is
[`rayfin-local` (20870)](https://dev.azure.com/powerbi/AppDev/_build?definitionId=20870).
It establishes the official build definition and resource approvals before ESRP
signing is enabled. It follows the
[Fabricator pilot](https://github.com/microsoft/rayfin-fabricator/blob/019198cf0a11c0acc712425043d7478287c4a47d/.azure-pipelines/fabricator.official.yml).

## Initial scope

CI and PR triggers are disabled. Queue the pipeline manually for a reviewed source
commit. It extends `v1/1ES.Official.PipelineTemplate.yml`, keeps the template's
security checks enabled, and uses `1ES-AppDev` with the `windows-2022` image.

The job verifies the checkout against `Build.SourceVersion`, installs Rust 1.96.0
and Node.js 22, runs the workspace tests with the lockfile, and builds the Windows
x64 release binary with the static C runtime, matching the existing GitHub release
build. It checks the binary's version against Cargo metadata; a tag run must use
`v<package version>`.

The official job generates a checkout-local `.cargo/config.toml` that replaces
crates.io with the existing AppDev `rayfin-official` Azure Artifacts Cargo feed.
`CargoAuthenticate@0` authenticates using the pipeline's build identity; no PAT
or new service connection is needed. The config is generated only in the official
job so local development and public GitHub CI do not require internal feed access.
The lockfile and vendored TDS patch remain unchanged.

The only published artifact is `windows-validation/source.json`: source commit,
version, target, toolchain, build ID, and the unsigned binary's SHA-256. The binary
stays on the agent. This pipeline does not call ESRP, access signing credentials,
publish executables or GitHub releases, or publish crates or npm packages.

## Register and request admin approval

Make the YAML available on the branch used for registration before creating the
definition. An AppDev pipeline administrator can then register:

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
| Published output | Validation JSON only; no executables |

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

Pool role permissions and pipeline resource authorization are separate. Check
[AppDev agent pools](https://dev.azure.com/powerbi/AppDev/_settings/agentqueues)
under `1ES-AppDev` -> Security and Pipeline permissions. The Fabricator pilot
initially encountered a missing pool **Use** permission; no successful run of
this new pipeline should be assumed until its own approval and validation finish.

Suggested approval-request text:

> Please register/authorize `rayfin-local` in powerbi/AppDev using
> `microsoft/rayfin-local` and `/.azure-pipelines/rayfin-local.official.yml`.
> This is a manual-only 1ES official Windows validation pipeline using
> `1ES-AppDev/windows-2022` and the 1ES official template repository.
> It builds/tests rayfin-local but publishes only validation JSON, never unsigned
> binaries. Please grant the necessary pool Use permission and authorize the
> GitHub connection, template repository and pool for this pipeline only.
> No ESRP connection, signing-key access, package publishing or release publishing
> permission is requested in this phase.

After authorization, queue a reviewed commit and confirm the build, template
security checks and metadata artifact succeed. Retain the run link for the
subsequent signing approval.

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

## Subsequent signing phases

The initial definition deliberately has no placeholder credentials or signing
task that could produce an unsigned release on failure.

- Confirm registration-owner approval to reuse the existing AppDev ESRP client
  and `Fabric-ESRP-Secure-AppDev` connection. Authorize that connection for this
  pipeline separately.
- Confirm the binary's signing classification and approved keycode with the ESRP
  owners: product signing versus separately distributed component signing must
  not be inferred merely from the repository being Microsoft-owned or OSS.
- Add Windows ESRP signing after the build, verify the signature, and only then
  package the executable and calculate distribution checksums. Signing failures
  must block release output.
- Establish an approved macOS build pool and the required Apple signing and
  notarization operations before adding macOS output.
- Define the future Node adapter's actual native artifact and packaging format
  before adding its build/signing stage. It is not part of this pilot.
- Decide the protected distribution path for signed archives separately.
  crates.io publishing remains out of scope.

The existing `.github/workflows/release.yml` is unchanged: version tags still
trigger its current GitHub binary-release workflow. This pilot neither signs
those artifacts nor replaces or gates that workflow. Do not treat a successful
validation run as a signed release.
