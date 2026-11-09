use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::post,
};
use http_body_util::BodyExt;
use rl_model::{Caller, CallerError, CallerResolver, Session};
use rl_server::{HostingOptions, ServerConfig, StaticSite, build_router_with_hosting};
use serde_json::{Value, json};
use tower::ServiceExt;

const ORIGIN: &str = "http://localhost:7171";

fn config() -> ServerConfig {
    ServerConfig {
        api_origin: ORIGIN.into(),
        publishable_key: "pk-local".into(),
        workspace_id: "workspace".into(),
        item_id: "item".into(),
        tenant_id: None,
        portal_url: format!("{ORIGIN}/.rayfin-local/portal"),
        allowed_origins: vec![],
        allowed_hosts: None,
        static_site: None,
        publishable_key_exempt: vec![],
    }
}

#[derive(Default)]
struct Callers(AtomicBool);

impl CallerResolver for Callers {
    fn resolve(&self, token: Option<&str>) -> Result<Caller, CallerError> {
        match token {
            None => Ok(Caller::anonymous()),
            Some("valid-session") if !self.0.load(Ordering::SeqCst) => {
                Ok(Caller::authenticated(Session {
                    user_id: "user".into(),
                    email: None,
                    claims: Default::default(),
                    access_token: "valid-session".into(),
                }))
            }
            _ => Err(CallerError::InvalidToken),
        }
    }
}

async fn send(
    router: &Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    (
        parts.status,
        parts.headers,
        body.collect().await.unwrap().to_bytes().to_vec(),
    )
}

#[tokio::test]
async fn telemetry_is_emitted_only_when_enabled_and_nonblank() {
    for (enabled, value, expected) in [
        (false, Some("InstrumentationKey=local"), None),
        (
            true,
            Some("InstrumentationKey=local"),
            Some("InstrumentationKey=local"),
        ),
        (true, None, None),
        (true, Some("  "), None),
    ] {
        let router = build_router_with_hosting(
            config(),
            vec![],
            HostingOptions {
                telemetry_enabled: enabled,
                application_insights_connection_string: value.map(str::to_owned),
                ..Default::default()
            },
        );
        let (status, headers, body) = send(&router, "GET", "/rayfin.config.json", &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["cache-control"], "no-store");
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["serviceMode"], "fabric");
        assert_eq!(
            body.get("applicationInsightsConnectionString")
                .and_then(Value::as_str),
            expected
        );
        assert!(body.get("telemetry").is_none());
    }
}

#[tokio::test]
async fn protected_site_gates_every_asset_and_runtime_config_but_not_backend_routes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "APP SHELL").unwrap();
    std::fs::write(dir.path().join("app.js"), "APP SCRIPT").unwrap();
    let callers = Arc::new(Callers::default());
    let router = build_router_with_hosting(
        ServerConfig {
            static_site: Some(StaticSite {
                root: dir.path().to_owned(),
                index_document: "index.html".into(),
            }),
            ..config()
        },
        vec![Router::new().route("/api/auth/v1/token", post(|| async { "AUTH" }))],
        HostingOptions {
            protected_assets: Some(callers.clone()),
            ..Default::default()
        },
    );

    for path in [
        "/",
        "/index.html",
        "/deep/link",
        "/app.js",
        "/rayfin.config.json",
    ] {
        let (status, headers, body) =
            send(&router, "GET", path, &[("sec-fetch-dest", "document")]).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            headers["cache-control"]
                .to_str()
                .unwrap()
                .contains("no-store")
        );
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains("Sign in to this app"), "{path}: {body}");
        assert!(!body.contains("APP SHELL"));

        let (status, _, body) = send(&router, "GET", path, &[("accept", "*/*")]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"error": "Authentication required."})
        );
    }
    // A bearer token alone does not authenticate browser assets in Fabric.
    let (status, _, _) = send(
        &router,
        "GET",
        "/app.js",
        &[("authorization", "Bearer valid-session")],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, body) = send(&router, "HEAD", "/app.js", &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.is_empty());
    let (status, _, _) = send(
        &router,
        "POST",
        "/api/auth/v1/token",
        &[("x-publishable-key", "pk-local")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&router, "GET", "/.rayfin-local/static-signin.js", &[]).await;
    assert_eq!(status, StatusCode::OK);

    for path in ["/", "/deep/link", "/app.js", "/rayfin.config.json"] {
        let (status, _, _) = send(
            &router,
            "GET",
            path,
            &[("cookie", "other=x; rayfin_sh_sid=valid-session")],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}");
    }
    callers.0.store(true, Ordering::SeqCst);
    let (status, headers, _) = send(
        &router,
        "GET",
        "/app.js",
        &[("cookie", "rayfin_sh_sid=valid-session")],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        headers["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
}

#[tokio::test]
async fn static_session_requires_a_verified_token_and_same_origin() {
    let router = build_router_with_hosting(
        config(),
        vec![],
        HostingOptions {
            protected_assets: Some(Arc::new(Callers::default())),
            ..Default::default()
        },
    );
    for (origin, token, expected) in [
        (
            "https://attacker.example",
            "valid-session",
            StatusCode::FORBIDDEN,
        ),
        (ORIGIN, "invalid-session", StatusCode::UNAUTHORIZED),
        (ORIGIN, "valid-session", StatusCode::NO_CONTENT),
    ] {
        let (status, headers, _) = send(
            &router,
            "POST",
            "/.rayfin-local/static-session",
            &[
                ("origin", origin),
                ("authorization", &format!("Bearer {token}")),
            ],
        )
        .await;
        assert_eq!(status, expected);
        if status == StatusCode::NO_CONTENT {
            let cookie = headers["set-cookie"].to_str().unwrap();
            assert!(cookie.starts_with("rayfin_sh_sid=valid-session;"));
            assert!(cookie.contains("HttpOnly"));
            assert!(cookie.contains("SameSite=Lax"));
            assert!(cookie.ends_with("Path=/"));
            assert!(!cookie.contains("Domain="));
        }
    }
}

#[tokio::test]
async fn secure_sites_use_fabrics_host_cookie_attributes() {
    let router = build_router_with_hosting(
        ServerConfig {
            api_origin: "https://localhost:7171".into(),
            ..config()
        },
        vec![],
        HostingOptions {
            protected_assets: Some(Arc::new(Callers::default())),
            ..Default::default()
        },
    );
    let (status, headers, _) = send(
        &router,
        "POST",
        "/.rayfin-local/static-session",
        &[
            ("origin", "https://localhost:7171"),
            ("authorization", "Bearer valid-session"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let cookie = headers["set-cookie"].to_str().unwrap();
    assert!(cookie.starts_with("__Host-rayfin_sh_sid="));
    for attribute in [
        "HttpOnly",
        "Secure",
        "SameSite=None",
        "Partitioned",
        "Path=/",
    ] {
        assert!(cookie.contains(attribute), "{cookie}");
    }
}
