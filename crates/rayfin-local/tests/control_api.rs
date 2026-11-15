//! Automatic sign-in and the control API (docs/embedding.md) of the built binary: a host runs
//! `serve --json` on the fixture app with a control token, then drives it the way Fabricator
//! does, and signs the app in through the app's dev server as its page would.
//!
//! It needs Node.js and the fixture's npm packages (`npm ci` in `tests/fixtures/apps/basic`);
//! without them it prints a note and passes.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const TOKEN: &str = "0123456789abcdef0123456789abcdef-control-api-test";

/// `tests/fixtures/apps/basic`, when its packages and Node.js are installed.
fn fixture() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/apps/basic");
    let installed = ["@microsoft/rayfin-core", "vite"].iter().all(|package| {
        let manifest = dir.join("node_modules").join(package).join("package.json");
        manifest.is_file()
    });
    if !installed {
        eprintln!(
            "skipped: the fixture's packages are not installed (run `npm ci` in {})",
            dir.display()
        );
        return None;
    }
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipped: Node.js is not on PATH");
        return None;
    }
    Some(dir)
}

/// A `serve --json` that runs until stdin closes.
struct Serve {
    child: Child,
    events: Receiver<Value>,
    seen: Vec<Value>,
    /// The Azure CLI's configuration folder: empty, so that there is no Azure sign-in.
    _no_azure: tempfile::TempDir,
}

impl Serve {
    fn start(project: &Path) -> Self {
        // No Azure sign-in, whatever this machine has: the developer identity signs in as the
        // local developer account.
        Self::start_with(
            project,
            &[
                "--no-functions",
                "--frontend-port",
                "0",
                "--token-source",
                "az",
            ],
            &[],
        )
    }

    /// `serve --json --reset --port 0` with the control API on, `args` and `env`.
    fn start_with(project: &Path, args: &[&str], env: &[(&str, &str)]) -> Self {
        let no_azure = tempfile::tempdir().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_rayfin-local"));
        command
            .args(["serve", "--json", "--reset", "--port", "0"])
            .args(args)
            .arg("-C")
            .arg(project)
            .env("RAYFIN_LOCAL_CONTROL_TOKEN", TOKEN)
            .env("AZURE_CONFIG_DIR", no_azure.path());
        for audience in ["SQL", "STORAGE", "FABRIC", "AZUREAI", "ADO", "POWERBI"] {
            command.env_remove(format!("RAYFIN_LOCAL_TOKEN_{audience}"));
        }
        command.envs(env.iter().copied());
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (sender, events) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(event) = serde_json::from_str::<Value>(&line)
                    && sender.send(event).is_err()
                {
                    break;
                }
            }
        });
        Self {
            child,
            events,
            seen: Vec::new(),
            _no_azure: no_azure,
        }
    }

    /// The next event of type `wanted`.
    fn until(&mut self, wanted: &str) -> Value {
        loop {
            let event = self
                .events
                .recv_timeout(Duration::from_secs(300))
                .unwrap_or_else(|_| panic!("no `{wanted}` event"));
            assert_ne!(
                event["type"], "stopped",
                "stopped before `{wanted}`: {event}"
            );
            self.seen.push(event.clone());
            if event["type"] == wanted {
                return event;
            }
        }
    }

    /// The events emitted so far.
    fn seen(&mut self) -> &[Value] {
        while let Ok(event) = self.events.try_recv() {
            self.seen.push(event);
        }
        &self.seen
    }

    fn stop(mut self) -> Option<i32> {
        drop(self.child.stdin.take());
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The control API of a running `serve`.
struct Host {
    client: reqwest::Client,
    api_url: String,
}

impl Host {
    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/.rayfin-local/control{path}", self.api_url);
        self.client.request(method, url).bearer_auth(TOKEN)
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = self.request(method, path);
        if let Some(body) = body {
            request = request.json(&body);
        }
        answer(request).await
    }
}

async fn answer(request: reqwest::RequestBuilder) -> (StatusCode, Value) {
    let response = request.send().await.unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// The stamp of the backend's sessions that the dev server puts in the app's page.
async fn page_stamp(client: &reqwest::Client, app_url: &str) -> String {
    let page = client
        .get(format!("{app_url}/"))
        .header("accept", "text/html")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let before = "(window.localStorage,\"";
    let start = page
        .find(before)
        .unwrap_or_else(|| panic!("no session stamp in the page:\n{page}"))
        + before.len();
    let end = start + page[start..].find('"').unwrap();
    page[start..end].to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_drives_the_app_and_signs_it_in_as_anyone() {
    let Some(project) = fixture() else {
        return;
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut serve = Serve::start(&project);
    let ready = serve.until("ready");
    assert_eq!(ready["signInAs"], json!({ "kind": "developer" }));
    let api_url = ready["apiUrl"].as_str().unwrap().to_owned();
    let app_url = ready["appUrl"]
        .as_str()
        .unwrap()
        .trim_end_matches('/')
        .to_owned();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let host = Host {
        client: client.clone(),
        api_url: api_url.clone(),
    };

    let (status, body) = host.call(Method::GET, "/status", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "running");
    assert_eq!(body["apiUrl"], api_url);
    assert_eq!(body["schema"]["type"], "schema");
    let control = format!("{api_url}/.rayfin-local/control/status");
    let (status, _) = answer(client.get(&control)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let from_a_page = client
        .get(&control)
        .bearer_auth(TOKEN)
        .header("origin", app_url.as_str());
    let (status, _) = answer(from_a_page).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) =
        answer(client.get(format!("{api_url}/.rayfin-local/dev/session-token"))).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "only the dev server signs in"
    );

    // The page asks its dev server, as fetchRayfinLocalSessionToken() does: a same-origin GET.
    let sign_in = || {
        client
            .get(format!("{app_url}/.rayfin/dev/session-token"))
            .header("accept", "application/json")
            .header("sec-fetch-site", "same-origin")
            .header("sec-fetch-mode", "cors")
            .header("referer", format!("{app_url}/"))
    };
    let (status, developer) = answer(sign_in()).await;
    assert_eq!(status, StatusCode::OK, "{developer}");
    assert_eq!(developer["tokenType"], "Bearer");
    assert!(developer["refreshToken"].is_string());
    let (_, identity) = host.call(Method::GET, "/identity", None).await;
    assert_eq!(identity["user"]["username"], "developer@example.com");
    assert_eq!(identity["user"]["developer"], true);
    // Without an Azure sign-in, the developer is the local account, which the log says; no
    // `azure-sign-in` warning, which is for what can't work without Azure.
    let seen = serve.seen();
    assert!(
        seen.iter().any(|event| event["type"] == "log"
            && event["level"] == "info"
            && event["line"]
                .as_str()
                .is_some_and(|line| line.starts_with("Signing in as the local developer account"))),
        "{seen:?}"
    );
    assert!(
        !seen.iter().any(|event| event["code"] == "azure-sign-in"),
        "{seen:?}"
    );
    let stamp = page_stamp(&client, &app_url).await;
    assert_eq!(page_stamp(&client, &app_url).await, stamp);

    let ana = json!({ "username": "ana@example.com", "password": "secret-1" });
    let (status, ana) = host.call(Method::POST, "/users", Some(ana)).await;
    assert_eq!(status, StatusCode::CREATED, "{ana}");
    let as_ana = json!({ "kind": "user", "user": "ana@example.com" });
    let (status, identity) = host.call(Method::PUT, "/identity", Some(as_ana)).await;
    assert_eq!(status, StatusCode::OK, "{identity}");
    assert_eq!(identity["user"]["id"], ana["id"]);
    // Pages forget the sessions the switch signed out.
    let switched = page_stamp(&client, &app_url).await;
    assert_ne!(switched, stamp);
    let (status, signed_in) = answer(sign_in()).await;
    assert_eq!(status, StatusCode::OK, "{signed_in}");
    let access_token = signed_in["accessToken"].as_str().unwrap().to_owned();
    let (_, status_now) = host.call(Method::GET, "/status", None).await;
    assert_eq!(
        status_now["signInAs"],
        json!({ "kind": "user", "username": "ana@example.com" })
    );

    // The app's own routes, as the signed-in user.
    let config: Value = client
        .get(format!("{api_url}/rayfin.config.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let key = config["publishableKey"].as_str().unwrap().to_owned();
    let graphql = |query: &str, token: &str| {
        client
            .post(format!("{api_url}/graphql"))
            .bearer_auth(token)
            .header("x-publishable-key", key.as_str())
            .json(&json!({ "query": query }))
    };
    let create = "mutation { createCategory(item: { name: \"From a host\", sortOrder: 3 }) \
                  { id name } }";
    let (status, created) = answer(graphql(create, &access_token)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["data"]["createCategory"]["name"], "From a host");

    let (_, entities) = host.call(Method::GET, "/entities", None).await;
    let entities = entities["entities"].as_array().unwrap().clone();
    assert!(entities.contains(&json!({ "name": "Category", "rows": 1 })));
    let (status, rows) = host
        .call(
            Method::GET,
            "/entities/Category/rows?limit=10&orderBy=name",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    assert_eq!(rows["total"], 1);
    assert_eq!(rows["rows"][0]["name"], "From a host");
    assert_eq!(rows["rows"][0]["sortOrder"], 3);

    let (status, token) = host
        .call(
            Method::POST,
            "/token",
            Some(json!({ "user": "bob", "create": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{token}");
    let bob_token = token["accessToken"].as_str().unwrap();
    let (status, listed) = answer(graphql("{ categories { items { name } } }", bob_token)).await;
    assert_eq!(status, StatusCode::OK, "{listed}");

    let nobody = json!({ "kind": "none" });
    host.call(Method::PUT, "/identity", Some(nobody)).await;
    let (status, refused) = answer(sign_in()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(refused["error"], "Automatic sign-in is off.");
    let (status, _) = answer(graphql("{ categories { items { name } } }", &access_token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "switching signed ana out");

    let (status, reload) = host.call(Method::POST, "/reload", Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{reload}");
    assert_eq!(reload["schema"]["result"], "unchanged", "{reload}");
    assert_eq!(reload["config"], Value::Null, "{reload}");

    let before_reset = page_stamp(&client, &app_url).await;
    let (status, reset) = host.call(Method::POST, "/data/reset", None).await;
    assert_eq!((status, reset), (StatusCode::OK, json!({ "ok": true })));
    assert_ne!(page_stamp(&client, &app_url).await, before_reset);
    let (_, entities) = host.call(Method::GET, "/entities", None).await;
    assert!(
        entities["entities"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entity| entity["rows"] == 0)
    );
    let (_, users) = host.call(Method::GET, "/users", None).await;
    assert_eq!(users, json!({ "users": [] }));

    assert_eq!(serve.stop(), Some(0));
}

/// The secret a desktop host shares with `serve --token-source host`.
const HOST_SECRET: &str = "the-desktop-host's-token-secret-of-32-characters";

/// A desktop host's token endpoint: answers every scope with a token of Ada's (a fake JWT) with
/// the scope's audience; keeps the requests' bodies.
async fn fake_token_host() -> (String, Arc<Mutex<Vec<Value>>>) {
    use axum::http::HeaderMap;
    use base64::Engine as _;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let encode = |value: Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string().as_bytes())
    };
    let router = axum::Router::new().route(
        "/token",
        axum::routing::post({
            let seen = Arc::clone(&seen);
            move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                seen.lock().unwrap().push(body.clone());
                let expected = format!("Bearer {HOST_SECRET}");
                if headers
                    .get("authorization")
                    .is_none_or(|value| *value != *expected)
                {
                    let denied = json!({ "error": "denied", "message": "Wrong secret." });
                    return (axum::http::StatusCode::UNAUTHORIZED, axum::Json(denied));
                }
                let exp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    + 3600;
                let audience = body["scope"]
                    .as_str()
                    .unwrap()
                    .trim_end_matches("/.default");
                let token = format!(
                    "{}.{}.signature",
                    encode(json!({ "alg": "RS256", "typ": "JWT" })),
                    encode(json!({
                        "aud": audience,
                        "iss": "https://sts.windows.net/72f988bf-86f1-41af-91ab-2d7cd011db47/",
                        "exp": exp, "name": "Ada Lovelace", "scp": "user_impersonation",
                        "oid": "00000000-0000-4000-8000-00000000ada0",
                        "tid": "72f988bf-86f1-41af-91ab-2d7cd011db47",
                        "upn": "ada@contoso.com",
                    }))
                );
                let granted = json!({ "accessToken": token, "expiresOn": exp });
                (axum::http::StatusCode::OK, axum::Json(granted))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router).await });
    (format!("http://127.0.0.1:{port}/token"), seen)
}

/// A desktop host: the app's build (`--static`, no Vite), the user's tokens from the host
/// (`--token-source host`), and sessions from the control API, with neither the Azure CLI nor
/// open sign-in.
#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_host_signs_the_app_in_as_its_user_with_its_tokens() {
    let Some(project) = fixture() else {
        return;
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (token_url, seen) = fake_token_host().await;
    let state = tempfile::tempdir().unwrap();
    let state_dir = state.path().to_str().unwrap();
    let mut serve = Serve::start_with(
        &project,
        &[
            "--static",
            "--token-source",
            "host",
            "--state-dir",
            state_dir,
        ],
        &[
            ("RAYFIN_LOCAL_TOKEN_URL", &token_url),
            ("RAYFIN_LOCAL_TOKEN_SECRET", HOST_SECRET),
        ],
    );
    let ready = serve.until("ready");
    assert_eq!(ready["signInAs"], json!({ "kind": "developer" }));
    assert_eq!(ready["openSignIn"], false);
    assert_eq!(ready["features"]["functions"]["enabled"], true, "{ready}");
    let api_url = ready["apiUrl"].as_str().unwrap().to_owned();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let host = Host {
        client: client.clone(),
        api_url: api_url.clone(),
    };

    let (status, session) = host.call(Method::POST, "/session", Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    assert_eq!(session["tokenType"], "Bearer");
    assert!(session["refreshToken"].is_string(), "{session}");
    let user = &session["user"];
    assert_eq!(user["email"], "ada@contoso.com", "{session}");
    assert_eq!(user["developer"], true);
    assert!(
        user["signIn"].as_array().unwrap().contains(&json!("entra")),
        "{user}"
    );
    let stamp = session["stamp"].as_str().unwrap();
    assert!(stamp.contains(':'), "{stamp}");
    let (_, identity) = host.call(Method::GET, "/identity", None).await;
    assert_eq!(identity["user"]["id"], user["id"]);
    let power_bi = seen
        .lock()
        .unwrap()
        .iter()
        .find(|body| body["scope"] == "https://analysis.windows.net/powerbi/api/.default")
        .cloned();
    assert_eq!(
        power_bi.as_ref().map(|body| &body["silent"]),
        Some(&json!(true)),
        "{power_bi:?}"
    );
    // The page's session works with the app's own routes.
    let config: Value = client
        .get(format!("{api_url}/rayfin.config.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let key = config["publishableKey"].as_str().unwrap().to_owned();
    let (status, listed) = answer(
        client
            .post(format!("{api_url}/graphql"))
            .bearer_auth(session["accessToken"].as_str().unwrap())
            .header("x-publishable-key", key.as_str())
            .json(&json!({ "query": "{ categories { items { name } } }" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");

    // A function's `ctx.Tokens` come from the host.
    let (status, invoked) = answer(
        client
            .post(format!("{api_url}/functions/tokenInfo/invoke"))
            .bearer_auth(session["accessToken"].as_str().unwrap())
            .header("x-publishable-key", key.as_str())
            .json(&json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{invoked}");
    assert!(
        invoked["output"]["fabricTokenLength"].as_u64().unwrap() > 0,
        "{invoked}"
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|body| body["scope"] == "https://api.fabric.microsoft.com/.default"),
        "{seen:?}"
    );
    let events = serve.seen();
    assert!(
        !events.iter().any(|event| event["code"] == "azure-sign-in"),
        "{events:?}"
    );

    assert_eq!(serve.stop(), Some(0));
}
