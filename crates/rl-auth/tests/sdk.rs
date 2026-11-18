//! The real Rayfin SDK (`@microsoft/rayfin-client` 1.36.2 and 2.0.0, run by Node) against the
//! auth routes: sign-up, sign-in, refresh, JWKS validation, auth settings, sign-out and
//! 401 → refresh → retry.
//!
//! Ignored by default: it needs Node and the SDK's `node_modules`, by default the basic (1.36) and
//! `sdk-v2` (2.0) fixtures' (`RAYFIN_SDK_NODE_MODULES` and `RAYFIN_SDK_V2_NODE_MODULES` override
//! them). Run with `cargo test -p rl-auth --test sdk -- --ignored`.

use std::{path::Path, sync::Arc};

use rl_auth::{AuthService, AuthSettings};
use rl_db::Db;
use serde_json::Value;

const APP_ID: &str = "6c6f6361-6c00-4000-8000-0000000000a1";

const SCRIPT: &str = r#"
const { RayfinClient } = await import(process.env.RL_SDK_ENTRY);
const apiUrl = process.env.RL_API_URL;
const options = { baseUrl: apiUrl, publishableKey: 'pk-local-test' };
// SDK 2.0 creates the client with an explicit auth adapter; 1.36 constructs it.
const { browserSession } = Number(process.env.RL_SDK_MAJOR) >= 2
  ? await import(process.env.RL_AUTH_ENTRY)
  : {};
const newClient = browserSession
  ? () => RayfinClient.create({ ...options, auth: browserSession() })
  : () => new RayfinClient(options);
const describe = (e) => ({ name: e?.name, code: e?.code, status: e?.status, message: e?.message });
const attempt = async (run) => {
  try { return { ok: await run() }; } catch (e) { return { error: describe(e) }; }
};

const out = {};
const client = newClient();
const auth = client.auth;
const email = 'Sdk.User@Example.test';
const password = 'correct horse battery';

out.settings = await auth.getAuthSettings();
out.signUp = await auth.signUp({ email, password });
out.duplicate = await attempt(() => auth.signUp({ email: email.toLowerCase(), password }));
out.wrongPassword = await attempt(() => auth.signIn({ email, password: 'wrong horse battery' }));

const tokens = await auth.signIn({ email, password });
out.tokenType = tokens.tokenType;
out.expiresIn = tokens.expiresIn;
out.session = auth.getSession();
out.validAccessToken = await auth.validateAccessToken(tokens.accessToken);
const [header, payload, signature] = tokens.accessToken.split('.');
const forged = Buffer.from(JSON.stringify({ ...JSON.parse(atob(payload)), sub: 'someone-else' }))
  .toString('base64url');
out.validForgedToken = await auth.validateAccessToken(`${header}.${forged}.${signature}`);

const refreshed = await auth.refreshSession();
out.refreshRotated = refreshed.refreshToken !== tokens.refreshToken;
out.validRefreshedToken = await auth.validateAccessToken(refreshed.accessToken);
out.jwksKeys = (await auth.getJwks()).keys.length;

const other = newClient();
await other.auth.signIn({ email, password });
await other.auth.signOut();
out.otherAuthenticatedAfterSignOut = other.auth.getSession().isAuthenticated;

const third = newClient();
await third.auth.signIn({ email, password });

// A rejected access token: signOutAll gets a 401, refreshes the session and replays.
auth.accessToken = 'not-a-valid-token';
out.signOutAll = await attempt(() => auth.signOutAll());
out.authenticatedAfterSignOutAll = auth.getSession().isAuthenticated;

out.thirdRefresh = await attempt(() => third.auth.refreshSession());
out.thirdAuthenticated = third.auth.getSession().isAuthenticated;

for (const c of [client, other, third]) (browserSession ? c.dispose() : c.auth.destroy());
console.log('RESULT ' + JSON.stringify(out));
process.exit(0);
"#;

fn entry_url(node_modules: &Path, package: &str) -> url::Url {
    let entry = node_modules.join(package).join("dist").join("index.js");
    url::Url::from_file_path(entry.canonicalize().unwrap()).unwrap()
}

#[tokio::test]
#[ignore = "needs Node and @microsoft/rayfin-client 1.36.2 and 2.0.0 (see the module docs)"]
async fn the_rayfin_sdk_signs_up_in_and_out() {
    for line in rl_test_support::sdk_lines() {
        let Some(node_modules) = line.with("@microsoft/rayfin-client") else {
            continue;
        };
        println!("SDK {}", line.version);
        signs_up_in_and_out(node_modules, line.major()).await;
    }
}

async fn signs_up_in_and_out(node_modules: &Path, major: u64) {
    let client_url = entry_url(node_modules, "@microsoft/rayfin-client");
    let auth_url = entry_url(node_modules, "@microsoft/rayfin-auth");

    let dir = tempfile::tempdir().unwrap();
    let mut settings = AuthSettings::new(APP_ID);
    settings.fabric_enabled = true;
    let auth = AuthService::new(
        Db::open_in_memory().unwrap(),
        dir.path().join("keys"),
        settings,
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Arc::new(auth).router())
            .await
            .unwrap();
    });

    let script = dir.path().join("sdk-auth.mjs");
    std::fs::write(&script, SCRIPT).unwrap();
    let output = tokio::process::Command::new("node")
        .arg(&script)
        .env("RL_SDK_ENTRY", client_url.as_str())
        .env("RL_AUTH_ENTRY", auth_url.as_str())
        .env("RL_SDK_MAJOR", major.to_string())
        .env("RL_API_URL", format!("http://{address}"))
        .output()
        .await
        .expect("node runs (is Node on PATH?)");
    server.abort();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "node failed:\n{stdout}\n{stderr}");
    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix("RESULT "))
        .unwrap_or_else(|| panic!("no result:\n{stdout}\n{stderr}"));
    let out: Value = serde_json::from_str(line).unwrap();
    println!("{}", serde_json::to_string_pretty(&out).unwrap());

    assert_eq!(
        out["settings"]["availableMethods"],
        serde_json::json!(["password", "fabric"])
    );
    let user_id = out["signUp"]["userId"].as_str().unwrap();
    assert_eq!(out["signUp"]["email"], "Sdk.User@Example.test");
    assert_eq!(out["signUp"]["role"], "Authenticated");
    assert_eq!(out["duplicate"]["error"]["name"], "AuthError");
    assert_eq!(
        out["duplicate"]["error"]["code"],
        "EMAIL_ALREADY_REGISTERED"
    );
    assert_eq!(
        out["duplicate"]["error"]["message"],
        "Email is already registered"
    );
    assert_eq!(out["wrongPassword"]["error"]["code"], "INVALID_GRANT");
    assert_eq!(
        out["wrongPassword"]["error"]["message"],
        "Invalid email or password"
    );
    assert_eq!(out["tokenType"], "Bearer");
    assert_eq!(out["expiresIn"], 3600);
    assert_eq!(out["session"]["user"]["id"], user_id);
    assert_eq!(out["session"]["user"]["email"], "Sdk.User@Example.test");
    assert_eq!(out["session"]["isAuthenticated"], true);
    assert_eq!(out["validAccessToken"], true);
    assert_eq!(out["validForgedToken"], false);
    assert_eq!(out["refreshRotated"], true);
    assert_eq!(out["validRefreshedToken"], true);
    assert_eq!(out["jwksKeys"], 1);
    assert_eq!(out["otherAuthenticatedAfterSignOut"], false);
    // The main session and the third client's; the other client already signed out.
    assert_eq!(out["signOutAll"]["ok"]["count"], 2);
    assert_eq!(out["authenticatedAfterSignOutAll"], false);
    assert_eq!(out["thirdRefresh"]["error"]["code"], "SESSION_EXPIRED");
    assert_eq!(out["thirdAuthenticated"], false);
}
