---
name: create-canvas-rayfin
description: Create or extend a GitHub Copilot canvas app with a rayfin-local backend for persistent data, local authentication, functions, or file storage. Use for Rayfin-backed canvases, local BaaS canvas apps, or shared user-and-agent data. Builds on the built-in create-canvas skill.
---

# Rayfin-backed canvas authoring

Build a normal canvas extension whose UI and agent actions use the same local Rayfin
backend. The extension owns the backend process; rayfin-local owns the database,
authentication, functions, and stored files.

## Compose with create-canvas

**Load the built-in `create-canvas` skill before implementing this workflow**, unless
it is already loaded. Follow its SDK guide, scope selection, scaffold, registration,
theme, reload, and validation instructions. This skill adds backend integration; it
does not replace those steps or modify the built-in skill.

Skill composition is an instruction to load another skill, not an `extends` frontmatter
field. If `create-canvas` is unavailable, report that prerequisite rather than inventing
its tool schemas. Do not claim extension discovery or rendering was verified when the
required tools or runtime are unavailable.

Use this skill for a local backend, not a frontend-only canvas or a hosted multi-user
service. Do not provision cloud resources merely to provide local persistence.

## Sources of truth

Before implementing, read the backend's [embedding contract](../../../docs/embedding.md)
and run the selected binary's `rayfin-local version --json`. Check its `contract` and
`sdk.supported`; pin new Rayfin packages to its tested version. The current contract
is v1 and the current tested SDK is 1.36.2. Do not assume an SDK upgrade is compatible.

Use the integration patterns below for subprocess supervision, readiness, private
control-token handling, authorized GraphQL calls, idempotent opens, same-origin
configuration, brokered local sign-in, pagination, and lifecycle validation.
Adapt paths, canvas IDs, entities, identity, and lifetime to the requested app.
If this skill was installed outside this checkout, obtain the embedding contract from
[microsoft/rayfin-local](https://github.com/microsoft/rayfin-local); do not assume
repository-relative files exist there. Read the installed Rayfin SDK's types for
exact client APIs and wire formats.

## 1. Choose app scope and data scope separately

Follow `create-canvas` for extension installation scope. Also establish who owns the
backend data and how long it must live; ask when the request leaves that ambiguous.

| Data ownership | Suggested state directory |
| --- | --- |
| One Rayfin project or checkout | `<app-project>/.rayfin-local/` |
| One conversation | A stable app subdirectory in that session's storage |
| User-global app | `$COPILOT_HOME/extensions/<name>/artifacts/backend/<app-id>/` |
| A particular board or document | A validated stable domain ID under the chosen scope |

`instanceId` identifies a panel, **never persistent data**. Two panels for the same
logical app must share its data. A new panel or rehydrated provider must reopen that
data, not create an empty store. Validate any domain ID used to build a filesystem
path; keep it under the selected owning directory.

Use `--state-dir` explicitly. Honor an intentional `RAYFIN_LOCAL_STATE_DIR` override.
Do not write backend state into source files, a synced build directory, or arbitrary
working-directory-relative paths. Never reset or force a destructive schema migration
on ordinary open, close, reload, or startup.

## 2. Create or reuse a real Rayfin app

rayfin-local runs a Rayfin project, not arbitrary HTML plus a schemaless database.
The app needs `package.json`, `rayfin/rayfin.yml`, entity definitions and analyzer
configuration, and a frontend build. Reuse an existing app when appropriate; use
`create-rayfin-app` if available when scaffolding a new one.

Enable only the services the user needs. For a data-only prototype, enable auth and
data, define entity permissions, and leave functions, storage, and connectors off.
If functions or file storage are requested, include their real code/schema and
packages and test them through the SDK. Do not leave `--no-functions` in a launcher
for an app that needs functions.

Keep `extension.mjs` focused on SDK wiring; put supervision and domain operations in
sibling modules. A portable source layout can put the Rayfin project in `app/`
inside the extension folder. A project-specific example may live elsewhere in the
repository, but then document that dependency explicitly.

Resolve paths relative to the extension module or an explicit, validated project
location, not the session's incidental cwd. The Rayfin app needs its own npm
dependencies; the extension's Copilot SDK is still supplied by the host. Do not add
`@github/copilot-sdk` to the app to work around extension loading.

## 3. Supervise rayfin-local from the extension

Use a compatible installed binary, an explicit `RAYFIN_LOCAL_BIN`, or a verified local
build. In this repository, `cargo build -p rayfin-local` produces the debug binary;
other repositories should use the documented installation process. Do not silently
download or run a binary as a side effect of opening a canvas.

For a same-origin static canvas, launch the equivalent of:

```text
rayfin-local serve --json -C <app-project> --state-dir <stable-state-dir>
  --host 127.0.0.1 --static --open-sign-in --sign-in-as user:<local-canvas-user>
```

This is an argument list for a subprocess, not a shell string. Use `spawn` with
`stdio: ['pipe', 'pipe', 'pipe']` and no shell:

1. Generate `randomBytes(32).toString('hex')` in the provider process and pass it as
   `RAYFIN_LOCAL_CONTROL_TOKEN` to the backend. Keep the value private and in memory.
2. Keep the child's stdin open for its lifetime. Consume stdout as NDJSON; never
   inherit it into the extension's JSON-RPC stdout. Handle stderr without exposing
   credentials. Ignore unknown additive event types and fields.
3. Wait for a compatible `ready` event with the required services and non-null
   `appUrl`/`apiUrl`. Validate the URLs before making authenticated requests: the
   default static arrangement requires same-origin HTTP loopback URLs without
   embedded credentials. Do not trust arbitrary origins from project configuration.
4. Confirm the backend answers an HTTP request, such as `/rayfin.config.json`,
   before returning `ready.appUrl` from the canvas's `open` handler.
5. Reject malformed events, fatal errors, early exits, and bounded startup timeouts
   with actionable `CanvasError`s. Stop partially started children on failure.
   Surface relevant warnings and failures without logging tokens or identity claims.
6. On last-panel close and provider shutdown, close stdin and await process exit.
   Use bounded termination escalation for a stuck child. Do not leave a detached
   backend that outlives its owning extension.

Deduplicate concurrent startup with one in-flight promise per canonical app/state
pair. Track open panels as a set, so reopening the same `instanceId` does not add a
reference. Serialize open/close and operations that would race shutdown. Unexpected
backend exit must invalidate readiness; subsequent actions must fail visibly or use
an explicit restart path.

The backend's state lock prevents two processes from serving the same state directory.
For separate provider processes, either implement authenticated ownership/attachment
or report `already-serving`. Do not evade the lock with a new state directory and
make the user's records appear lost.

`--static` builds the app on startup. Use `--reuse-build` only with a valid build
and runtime configuration; it must not serve stale source or baked-in ports.
A Vite development arrangement is also supported, but keep its separate-origin
authentication/proxy flow intact. Do not add a second unused renderer server from
the base scaffold when rayfin-local already serves the UI.

## 4. Keep UI auth and host administration separate

For the static arrangement above:

- Fetch `/rayfin.config.json` at runtime for the publishable key; use the page's
  same-origin backend URL. Do not bake run-specific URLs, keys, or tokens into assets.
- Get the local app session from `GET /.rayfin-local/dev/session-token`, then install
  it using `signInWithBrokeredToken` from the supported Rayfin Fabric auth provider.
  This needs `--open-sign-in`; it is not a control API call.
- Prefer `authStorage: false` for a simple static canvas. If persisting sessions,
  handle the session stamp/epoch and invalidation after reset or identity change.
  Support SDK token refresh and bounded recovery from stale sessions, not an
  unbounded retry loop or a new session token on every poll.
- Use entity roles and policies. A shared local account is acceptable for an
  explicitly single-user app; `authenticated` access alone is not user isolation.
  Use ownership policies if the requested app has separate users.

`--open-sign-in` intentionally makes an app-user session available to same-origin
pages and local processes. Keep the service loopback-only, explain that boundary,
and do not expose or forward its ports to untrusted clients. Local Rayfin sign-in
is not GitHub authentication. Azure connectors/functions, when enabled, can use
the developer's Azure identity and are not sandboxed.

## 5. Route agent actions through the app's permissions

Expose typed, domain-specific canvas actions such as `list_items`, `add_item`, and
`update_item`, with JSON Schema input validation. Both user controls and agent
actions should execute the same business rules.

For host-side app calls:

1. Read the publishable key from `GET {apiUrl}/rayfin.config.json`.
2. Obtain a token for the **same app user the UI uses** with
   `POST {apiUrl}/.rayfin-local/control/token`, body
   `{"user":"<local-canvas-user>","create":true}`, and the private control bearer token.
   Do not default to the developer identity when the UI uses a named local user.
3. Call normal `/graphql`, `/functions/<name>/invoke`, or the SDK storage routes
   using that app-user bearer token and `X-Publishable-Key`.

The control API rejects browser origins and is administrative. Never send its token
to an iframe, frontend environment, URL, log, or action result. Do not use its
permission-bypassing entity/storage administration endpoints for ordinary app actions.
Keep resets, schema forcing, and identity administration separate from domain actions.

Use GraphQL variables, check both HTTP status and GraphQL `errors`, and return domain
data only. Follow pagination in both the UI and agent actions; do not present the
first page as a complete list. Return raw action results and throw `CanvasError`
on failure, as the base skill requires.

After agent writes, refresh the visible UI from the backend. A small app can poll
while visible with one cancellable timer, bounded requests, and explicit error/retry
states. A larger app can use a deliberate notification mechanism. Do not maintain
a separate in-memory or localStorage "database" for either actor.

## 6. Validate the integration, not just the screenshot

Run the base skill's discovery/open/action/input-validation checks, then prove:

- The real app builds and its SDK talks to the real backend.
- A UI-created record is visible to an agent action; an agent update appears in the
  UI without losing active form input or keyboard focus.
- Pagination works past one page. Invalid inputs and denied access are visible.
- Two panels for one app share a process/data scope. Reopening a panel is idempotent.
- Last-panel close shuts down the listener; opening a fresh panel restores records.
- Reloading the provider with a panel open rehydrates from the same state.
- Startup failure/timeout cleans up children and a later open can retry.
- Anonymous access matches the intended policies; the browser cannot use the control
  API; no private token is returned to the agent or embedded in the frontend.
- Any requested function or storage operation works through the app's authorization.

Use isolated temporary state for tests; never reset the user's live board to test
persistence. Follow the host theme and inspect narrow layouts under the base skill.
If runtime validation is unavailable, report precisely what remains unverified.

## 7. Document prerequisites and shipping

Ship the app sources, extension, dependency lockfiles, and instructions for obtaining
a compatible backend binary, installing app dependencies, and building the app.
Opening a canvas is not permission to perform installations or cloud sign-in.

Project scope can rely on a documented repository layout. User/session scope must
carry or explicitly locate the app rather than assume this repo's `examples/` exists.
Source sharing through a gist or repo folder does not provision the backend or npm
dependencies: gist sharing excludes binaries, `node_modules/`, `dist/`, and `build/`.
Do not claim an extension-only gist is a complete installable Rayfin app.

State where data lives, when the process stops, which identity is used, and which
services are enabled. Local persistence is not cloud sync or multi-machine
collaboration. A remote execution host's localhost is not the desktop's localhost;
require an approved forwarding integration rather than binding to `0.0.0.0` or
assuming remote URLs can be embedded.
