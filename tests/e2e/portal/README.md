# Sign-in portal end-to-end test

Drives rayfin-local's sign-in portal (the popup `ensureSignedInWithFabric` opens, and the local
Fabric host for apps using `initEmbeddedAuth`) with a real browser and the real Rayfin SDK, on
both lines rayfin-local serves (1.36.2 and 2.0.0). `crates/rl-auth/tests/portal_e2e.rs` starts the
auth routes behind `rl-server` and runs `run.mjs` once per line, which:

1. bundles `app.js` (a Sign in button calling `ensureSignedInWithFabric`, or `initEmbeddedAuth`
   on load when framed with `fabricEmbedded=true`) with esbuild, taking
   `@microsoft/rayfin-client`, `@microsoft/rayfin-auth` and `@microsoft/rayfin-auth-provider-fabric`
   from an app's `node_modules` (on 2.0 it creates the client with `RayfinClient.create` and a
   `browserSession()`, on 1.36 with `new RayfinClient`);
2. serves it from its own origin, as Vite serves an app next to the backend;
3. clicks Sign in in Chromium (Playwright) and, in the popup: signs in with a wrong then the
   right password, signs up, uses "Continue as" (with a fake developer token), cancels, and
   closes the popup; it also opens the portal without an opener;
4. opens the app in the local Fabric host (`/.rayfin-local/portal/embed?app=…`), where the app
   signs in with the real `initEmbeddedAuth`: signs in on the host, reloads the page and the
   app, switches to a new account, and checks that a request the host does not support fails
   at once;
5. prints `RESULT <json>` with what the app saw (session, refresh, errors), which the Rust test
   checks.

## Run it

```powershell
cd tests/e2e/portal
npm install
npx playwright install chromium   # optional: without it, the installed Microsoft Edge is used
cd ../../..
cargo test -p rl-auth --test portal_e2e -- --ignored --nocapture
```

The SDK 1.36 comes from the basic fixture's `node_modules` and the SDK 2.0 from the `sdk-v2`
fixture's (`node scripts/check.mjs setup` installs both); `RAYFIN_SDK_NODE_MODULES` and
`RAYFIN_SDK_V2_NODE_MODULES` override them. The test prints `skipped` and passes when Playwright or
an SDK line is missing, and fails instead under `RAYFIN_LOCAL_TEST_STRICT`.

`node_modules/` and the `.build/` bundle are not committed.

## The Entra exchange with a real token

`crates/rl-auth/tests/entra_smoke.rs` mints your Power BI token with the Azure CLI (`az login`
first), exchanges it at `POST /api/auth/v1/brokered/token`, which checks it against Microsoft's
published signing keys, and asks the sign-in page who "Continue as" would sign in. It prints no
token and no identity:

```powershell
cargo test -p rl-auth --test entra_smoke -- --ignored --nocapture
```
