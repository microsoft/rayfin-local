# @microsoft/rayfin-local-runtime

Experimental source package; npm publication and release signing are not configured yet.

Runs an existing Rayfin 2.0.0 project **in the Node.js process**, using the same Rust services
as `rayfin-local serve`. Project dependencies must already be installed.

```js
import { createRuntime } from '@microsoft/rayfin-local-runtime';

const runtime = createRuntime({
  projectDir: '/absolute/path/to/project',
  server: { host: '127.0.0.1', port: 0 },
  frontend: { mode: 'off' },
});
const unsubscribe = runtime.onEvent((event) => console.log(event));
try {
  const ready = await runtime.start();
  console.log(ready.apiUrl);
  console.log(await runtime.status());
  await runtime.reload();
} finally {
  await runtime.close();
  unsubscribe();
}
```

`createRuntime` validates configuration but does not start services. Paths relative to the host's
current directory are resolved at creation; the host's working directory and environment are not
changed. Helpers use `nodePath` (default `process.execPath`); Electron must supply a standalone Node
executable. The API does not install signal handlers, read stdin, open the sign-in/browser UI, or
configure a process-global logger or crypto provider. Native operations do not enable the HTTP
control API.

The default frontend is Vite (`mode: 'dev'`); `static` builds and serves the project, and `off` runs
only backend services. Backend port `0` binds an ephemeral port and returns its actual URL.
Frontend port `0` is rejected; omit it for available-port selection.

`start()` is shared by concurrent callers. An optional `start({ signal })` cancels **startup only**;
its promise settles after cleanup. After successful startup, aborting that signal has no effect.
`close()` is idempotent, including before/during startup. A stopped handle cannot restart: create
another handle. Use separate state directories and ports for simultaneous instances; a locked
state directory is rejected. Filesystems without exclusive locking are not supported in-process.

`closed` resolves to `{ reason, error, cleanupErrors }` even after failed startup or an unexpected
backend exit. Check `cleanupErrors` before assuming graceful shutdown completed. Explicitly await
`close()` before terminating the host. Garbage collection and worker-environment teardown request
best-effort cleanup; they cannot guarantee graceful shutdown on abrupt host termination.

Events use the CLI's versioned event payloads. Each instance retains at most 1,024 queued events;
on overflow logs are discarded first and an `overflow` event reports the number discarded.
`status()` remains authoritative even if delivery overflows. Listeners execute on the JS thread;
exceptions emit a `RayfinRuntimeListenerWarning` and delivery continues. Delivery is polled every
20 ms while running and keeps the event loop alive until shutdown.

Reload preserves the control API's refusal and partial-application behavior: a refused schema is
reported in the returned `schema` event, not necessarily as a rejected promise. Administrative
APIs for users, data, secrets, and storage are not yet exposed by this package.

## CLI executable without the addon

### Supervised server

```js
import { startRayfinLocal } from '@microsoft/rayfin-local-runtime/server';

const server = await startRayfinLocal({
  projectDir: '/absolute/path/to/app',
  frontend: false,
  host: '127.0.0.1',
  port: 0,
  signInAs: 'none',
  openSignIn: false,
});
try {
  console.log(server.apiUrl, server.ready);
  // Use the server here.
} finally {
  await server.stop();
}
```

This binary-only ESM/CommonJS subpath never imports the native addon. It launches this package's
exact-version `/binary` with `serve --json -C <root>` and explicit, validated options. It uses the host's Node
executable for app helpers (standalone Node required), not a globally installed Rayfin CLI.
SDK 2.0.x is required (tested 2.0.0); local support does not certify deployment compatibility.

`projectDir` must contain `rayfin/rayfin.yml` with dependencies already installed. All other
options are optional:

| Option | Type | Default |
| --- | --- | --- |
| `frontend` | boolean; whether Rayfin starts its own frontend dev server | `false` |
| `host` | `'127.0.0.1'`, `'localhost'`, or `'::1'` (loopback only) | `'127.0.0.1'` |
| `port` | integer from 0 to 65535; 0 selects an available backend port | `0` |
| `signInAs` | `'none'`, `'developer'`, or `'user:<username>'` | unset: project configuration, then CLI default (`developer`) |
| `openSignIn` | boolean; permit secretless local session issuance | `false` |
| `stateDir` | state directory, resolved relative to the host working directory | unset: CLI/environment precedence |
| `forceSchema` | boolean; allow destructive schema changes | `false` |
| `reset` | boolean; empty database/users/stored files at startup, retaining keys and configuration | `false` |
| `onEvent` | synchronous callback receiving versioned CLI events | unset |

Unknown options and arbitrary CLI arguments are rejected. Omitting `signInAs` preserves
`[auth] sign_in_as` in `rayfin-local.toml`, including live identity changes. An explicit
`signInAs` overrides that configuration. Inherited `RAYFIN_LOCAL_OPEN_SIGN_IN` cannot enable open sign-in.
Explicit `developer` may use the developer's Entra identity when open sign-in is off, following
the CLI contract. No caller identity is hard-coded.

The result is `{ready, apiUrl, closed, stop}`. `ready` is the initial versioned Rayfin readiness descriptor;
`apiUrl` is its validated direct HTTP loopback origin. Startup verifies the selected frontend,
port, explicit identity and open-sign-in policy against readiness, and requires a supported SDK. Analysis/schema/function build
failures reject instead of resolving with a stale backend. Startup is limited to 120 seconds.
Errors have name `RayfinServerError` and a stable `code`, including upstream fatal codes.
Compiler output and application stderr are not forwarded. `onEvent` receives structured CLI events
as data (including diagnostic fields); the wrapper does not log those events.
Run the app's compiler separately for detailed diagnostics.

`stop()` is idempotent (the same promise), closes stdin and waits for child exit and project-lock
release before resolving. Startup rejection also waits for cleanup. An unsuccessful exit rejects;
after 30 seconds shutdown requests forced termination and rejects rather than claiming success.
Call and await `stop()` before starting a replacement. A second backend for the same project is
rejected by the upstream lock; state persists under `<app>/.rayfin-local` by default, with no automatic reset.
`stateDir` overrides inherited `RAYFIN_LOCAL_STATE_DIR`; when omitted, the environment still applies.
`forceSchema` and `reset` are destructive, explicit opt-ins. Inherited admin credentials are not passed to the backend.

`closed` settles after backend exit: it resolves on a clean requested stop and rejects with a
`RayfinServerError` on an unexpected exit or fatal error, including `code`, `exitCode` and `signal`.
It is safe to leave unobserved (no unhandled rejection), but hosts should observe it to react to
backend failure. `stop()` returns the same exit outcome.

SIGINT/SIGTERM request cleanup for owned children, preserving existing host signal handlers.
With no other handler, the host exits with the conventional signal exit code after cleanup.
Normal parent exit closes child stdin; abrupt host termination relies on OS pipe closure and
the CLI's existing EOF supervision. Awaited `stop()` is the deterministic release boundary.
Watchers remain enabled. `onEvent` receives analysis/schema/function reload failures and
`config-changed` events with `restartRequired`, so hosts can present failures or stop and recreate
the backend. These nonfatal events do not reject `closed`. Later `ready` events expose updated
configuration; the returned `ready` remains the startup snapshot. Throwing callbacks issue a
warning without interrupting event delivery. Runtime failures use `closed`, not process warnings.

**Open sign-in is opt-in and a local development trust boundary, not caller isolation.** When enabled, any local process
can obtain a full access/refresh session from `GET /.rayfin-local/dev/session-token`; the
`session-stamp` endpoint exposes the app/epoch stamp. Foreign Origin and Host checks remain.
A page on another origin must use a trusted server-side broker (omit Origin), not cross-origin
browser bootstrap. Session refresh stays on the backend's public auth API.

`npm run test:server` runs lifecycle unit tests. `scripts/test-npm.mjs` also validates the
actual installed tarballs: no addon load, sessions/refresh, lock contention, startup failure
recovery, restart and parent-exit cleanup, plus both TypeScript export forms.

See `examples/server.mjs` for a standalone Node program that runs until Enter is pressed and
awaits cleanup. It requires no application host integration.

### Direct binary access

```js
import { binaryPath } from '@microsoft/rayfin-local-runtime/binary';
import { spawn } from 'node:child_process';

const child = spawn(binaryPath, ['serve', '-C', '/path/to/project'], { stdio: 'inherit' });
```

The `/binary` export works with ESM and CommonJS and never loads the `.node` addon. It resolves
only the matching, exact-version platform package: no network downloads, source compilation,
postinstall hooks, or fallback to `PATH`. Keep optional dependencies enabled during installation.

Initial build targets: macOS arm64/x64, glibc Linux arm64/x64, and Windows x64. Node.js 20+ with
Node-API 8 is required. Other JavaScript hosts and Electron are not yet certified.

Both the executable and addon are native artifacts and must be signed independently when release
signing is configured. Application distributors may need to re-sign the addon for their macOS Team
ID and notarize the final application. npm provenance does not replace OS signing.
