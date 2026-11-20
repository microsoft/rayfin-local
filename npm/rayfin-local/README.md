# @microsoft/rayfin-local

Run an existing Rayfin project from Node.js using the bundled `rayfin-local` executable.
Requires Node.js 20+ and the project's dependencies already installed. No Rust toolchain, global
CLI install, native Node addon, or install-time download is needed.

```sh
npm install @microsoft/rayfin-local@preview
```

```js
import { startRayfinLocal } from '@microsoft/rayfin-local';

const server = await startRayfinLocal({ projectDir: '/path/to/app' });
try {
  console.log(server.apiUrl);
  // Use the server here.
} finally {
  await server.stop();
}
```

The client defaults to a backend-only listener on an available loopback port, with open sign-in off.
Unless you set `signInAs`, automatic sign-in follows the project's `rayfin-local.toml`, then the
CLI default (`developer`). It waits for the [versioned `ready` event](https://github.com/microsoft/rayfin-local/blob/main/docs/embedding.md#ready)
and gracefully stops the child by closing stdin. Use `frontend: true` to run the app's frontend too.
The child uses the host's standalone Node executable. CommonJS and TypeScript are supported.
`@microsoft/rayfin-local/binary` exports `binaryPath` for hosts managing their own child process.

The tarball includes Windows x64, macOS Apple Silicon, and Linux x64 (glibc 2.28+) executables.
On Windows and macOS the executable retains its OS signature. For Linux, the official pipeline
verifies Microsoft's detached signature against the original native archive before packaging its
unchanged executable; that signature doesn't cover the npm tarball. `native-build.json` identifies
the source commit, ADO build and SHA-256 digest of each bundled executable.
