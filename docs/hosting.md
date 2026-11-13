# Runtime config and protected hosting

## Telemetry

Fabric publishes an optional, top-level `applicationInsightsConnectionString` in
`/rayfin.config.json`. It is present only when `services.telemetry.enabled` is `true` and
the provisioned project has a nonblank connection string. There is no nested telemetry object.
The SDK exposes it on `client.runtimeConfig`; it does not provision a resource or initialize
Application Insights for the app.

rayfin-local does not provision Azure resources. Supply your development resource explicitly:

```yaml
# rayfin/rayfin.yml
services:
  telemetry:
    enabled: true
```

```toml
# rayfin-local.toml
[telemetry]
application_insights_connection_string = "InstrumentationKey=...;IngestionEndpoint=https://..."
```

The value is delivered to the browser, so do not use a resource whose connection string you
do not want the app to receive. TOML values are literal, not `${VAR}` substitutions.
Disabled or absent telemetry omits the field even when a value is configured. Enabled telemetry
without a nonblank value emits an actionable warning and omits the field, rather than inventing
a resource or failing startup. The connection string is never included in the warning.

Both `--static` and the managed Vite dev server deliver this config. Vite forwards the backend's
file before `rayfinLocalDev()` runs, changing only `apiUrl` to Vite's proxy origin. Changes to
telemetry settings apply live.

Sources: BaaS `Services/StaticHosting/RuntimeConfigWriter.cs:58–66`,
`RuntimeConfigGenerator.cs:19–22,45–57`; SDK `client/src/config.ts:172–200,326–328`.

## Asset access

With `rayfin-local serve --static`, `services.staticHosting.assetAccess: protected` gates
**all** static content: the shell, SPA fallback, scripts, styles, images, and
`/rayfin.config.json`. There is no public app-shell exception.

| Request without a valid hosting cookie | Response |
| --- | --- |
| Document navigation (`Sec-Fetch-Dest: document`, `iframe`, or `frame`; otherwise `Sec-Fetch-Mode: navigate` or HTML `Accept`) | HTTP 200 sign-in interstitial at the requested URL, not a redirect and not app content. |
| Asset/config fetch, including a request without navigation headers | HTTP 401 `{"error":"Authentication required."}`. |

The interstitial signs in, exchanges its PKCE handoff, creates an
HttpOnly hosting cookie through a same-origin POST, primes the SDK's default `authSession`
store, and reloads the document. Standalone, it opens rayfin-local's sign-in popup. Framed
with `?fabricEmbedded=true` (remembered for the tab in `sessionStorage`), it asks the parent
frame for the handoff with the `fabric-auth` `auth.requestHandoff` message, as Fabric's does,
so `/.rayfin-local/portal/embed` signs it in without a popup. An Authorization header alone
does not unlock static files.
Auth/API routes, health, and the interstitial's own script are not static assets and remain
reachable under their existing guards. Public access skips the hosting-cookie check entirely.

The interstitial may be framed only when the request is embedded (`Sec-Fetch-Dest: iframe` or
`frame`; without that header, the `fabricEmbedded` query parameter): `Content-Security-Policy:
frame-ancestors https: http://localhost:* http://127.0.0.1:*`. Otherwise it sends
`frame-ancestors 'none'` and `X-Frame-Options: DENY`, as Fabric does.

A request carrying Fabric's `_fu` user hint must match the cookie's user: the hint is
base64url(SHA-256(`rayfin.sh.fu.v1|<item id>|<user id>`)[0..16]), as BaaS derives it. A hint
for someone else gates the cookie again (the interstitial for documents, 401 for assets) and
clears it. The hint can only reject a cookie, never grant access.

`public` is also the legacy serving default when `assetAccess` is absent, matching BaaS's
absent-setting fallback. New deployments to current Fabric require an explicit choice.
Changing `assetAccess` applies live; it does not rebuild the app.

### Local differences

- **Vite assets are not protected.** Vite owns its own content/HMR pipeline and remains a
  development surface. `serve` warns for `assetAccess: protected` when Vite runs. Use
  `--static` to exercise the hosting gate, including protected runtime config.
- Fabric validates `__Host-rayfin_sh_sid`, a signed, host-bound, sliding hosting session
  authorized against the Fabric item. Locally the cookie carries a verified local Rayfin
  session: its expiry and revocation also revoke asset access, with a maximum browser cookie
  lifetime of one hour, without Fabric's sliding refresh or item ACL checks.
- HTTP hosting uses `rayfin_sh_sid; HttpOnly; SameSite=Lax; Path=/`, because browsers reject
  a `__Host-` cookie without `Secure`. HTTPS uses Fabric's name and attributes
  (`HttpOnly; Secure; SameSite=None; Partitioned; Path=/`, no Domain).
- The local popup needs `services.auth.enabled` and an enabled local sign-in method. Unlike
  Fabric's independent portal authentication, turning local auth off leaves a protected
  site inaccessible; startup warns when auth is disabled. Use localhost (a secure browser
  context) or HTTPS for the PKCE bootstrap.
- Static files retain rayfin-local's development `no-cache` policy rather than Fabric's
  immutable-content cache optimization. Interstitials, denials, and runtime config are not stored.
- Fabric's `embedded.only`, tenant hosting policy, and Fabric item permissions are not
  emulated by this gate.

Sources: BaaS `StaticHostingAccessGate.cs:265–275,295–298,371–401,529–533,904–923,1282–1319`,
`StaticHostingAccessConstants.cs:32,211–226`, `StaticHostingCookieService.cs:39`.

## System errors

Service-generated function and connector 5xx errors expose only:

```json
{"error":{"code":"InternalServerError","message":"An internal execution error occured"}}
```

The spelling is the Fabric contract. The original status is retained, including local
function-host 503/retry responses, while original codes and diagnostics go to `serve` output.
Fabric's front door may also add activity/correlation details; rayfin-local does not invent them.
This differs from the OSS .NET host's `internal_server_error` envelope.

SQL connector execution failures stay HTTP 200 GraphQL responses. Internal failures use
`extensions.code: "InternalServerError"` and the same generic message, with no `extensions.detail`.
Known SQL client faults use Fabric's curated `BadRequest` messages (duplicate keys, constraints,
schema mismatch, and the GraphQL workload's other vetted SQL error numbers). Partial data,
locations, and paths survive. Validation and authorization errors are not masked.

As in BaaS's proxy workflows, app-authored HTTP responses, HTTP 200 UDF failure envelopes,
and semantic-model adapter/Power BI responses retain their upstream contract; they are not
reclassified as local system exceptions.

Diagnostic logging redacts known function secrets, local-settings values, caller and audience
tokens, SQL passwords, and common credential/JWT patterns. App-authored `console.*` output
is still the app's responsibility.

Sources: BaaS `Monitoring/BaaSCodeMarker.cs:298–377`;
`Common/.../Exceptions/ExceptionConstants.cs:25–35`; GraphQL workload
`Initialization/Exception/IErrorUtil.cs:205–336`,
`GraphQLSqlExceptionUtil.cs:51–98,231–260`; BaaS
`ConnectorGraphQLController.cs:143–158`, `ConnectorFunctionInvocationWorkflow.cs:490–505,624–677`;
shared host `FunctionInvokeWorkflow.cs:113–147`.
