# @microsoft/rayfin-local-runtime

Experimental source package; npm publication and release signing are not configured yet.

Runs an existing Rayfin 1.36.2 project **in the Node.js process**, using the same Rust services
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
