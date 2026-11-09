//! The protected static-site gate. Fabric's signed hosting cookie is represented locally by a
//! verified Rayfin session, so expiry and sign-out revoke asset access too.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rl_model::{CallerResolver, bearer_token};
use serde_json::json;

use crate::ServerConfig;

const SESSION_PATH: &str = "/.rayfin-local/static-session";
const SCRIPT_PATH: &str = "/.rayfin-local/static-signin.js";
const NO_STORE: &str = "no-store, no-cache, must-revalidate";
const PAGE: &str = include_str!("static_signin.html");
const SCRIPT: &str = include_str!("static_signin.js");

pub(crate) struct StaticAccess {
    callers: Arc<dyn CallerResolver>,
    origin: String,
    cookie_name: &'static str,
    cookie_attributes: &'static str,
    page: String,
}

impl StaticAccess {
    pub(crate) fn new(config: &ServerConfig, callers: Arc<dyn CallerResolver>) -> Self {
        let secure = config.api_origin.starts_with("https://");
        let settings = json!({
            "portalUrl": config.portal_url,
            "publishableKey": config.publishable_key,
            "workspaceId": config.workspace_id,
            "itemId": config.item_id,
        })
        .to_string()
        .replace('<', "\\u003c");
        Self {
            callers,
            origin: config.api_origin.trim_end_matches('/').to_owned(),
            // __Host- requires Secure; use a different name rather than an invalid cookie over HTTP.
            cookie_name: if secure {
                "__Host-rayfin_sh_sid"
            } else {
                "rayfin_sh_sid"
            },
            cookie_attributes: if secure {
                "; HttpOnly; Secure; SameSite=None; Partitioned; Path=/"
            } else {
                "; HttpOnly; SameSite=Lax; Path=/"
            },
            page: PAGE.replace("{{CONFIG}}", &settings),
        }
    }

    pub(crate) fn check(&self, req: &Request) -> Option<Response> {
        let token = req
            .headers()
            .get_all(header::COOKIE)
            .iter()
            .find_map(|value| {
                value.to_str().ok()?.split(';').find_map(|cookie| {
                    let (name, value) = cookie.trim().split_once('=')?;
                    (name == self.cookie_name && !value.is_empty()).then_some(value)
                })
            });
        if token.is_some_and(|token| {
            self.callers
                .resolve(Some(token))
                .is_ok_and(|caller| caller.is_authenticated())
        }) {
            return None;
        }
        let mut response = if document_navigation(req.headers()) {
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                self.page.clone(),
            )
                .into_response()
        } else {
            unauthorized()
        };
        if req.method() == Method::HEAD {
            *response.body_mut() = axum::body::Body::empty();
        }
        security_headers(&mut response);
        if token.is_some() {
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_str(&format!(
                    "{}=; Max-Age=0{}",
                    self.cookie_name, self.cookie_attributes
                ))
                .expect("static cookie attributes are header-safe"),
            );
        }
        Some(response)
    }
}

pub(crate) fn routes(access: Arc<StaticAccess>) -> Router {
    Router::new()
        .route(SESSION_PATH, post(create_session))
        .route(
            SCRIPT_PATH,
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
                        (header::CACHE_CONTROL, "no-store"),
                        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                    ],
                    SCRIPT,
                )
            }),
        )
        .with_state(access)
}

async fn create_session(State(access): State<Arc<StaticAccess>>, headers: HeaderMap) -> Response {
    if headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(&access.origin) {
        return (
            StatusCode::FORBIDDEN,
            [(header::CACHE_CONTROL, NO_STORE)],
            Json(json!({ "error": "Invalid origin." })),
        )
            .into_response();
    }
    let token = bearer_token(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
    );
    let Some(token) = token.filter(|token| {
        token.len() <= 8192
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            && access
                .callers
                .resolve(Some(token))
                .is_ok_and(|caller| caller.is_authenticated())
    }) else {
        return unauthorized();
    };
    let cookie = format!(
        "{}={token}; Max-Age=3600{}",
        access.cookie_name, access.cookie_attributes
    );
    (
        StatusCode::NO_CONTENT,
        [
            (header::SET_COOKIE, cookie.as_str()),
            (header::CACHE_CONTROL, NO_STORE),
        ],
    )
        .into_response()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::CACHE_CONTROL, NO_STORE)],
        Json(json!({ "error": "Authentication required." })),
    )
        .into_response()
}

fn security_headers(response: &mut Response) {
    for (name, value) in [
        (header::CACHE_CONTROL, NO_STORE),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
}

fn document_navigation(headers: &HeaderMap) -> bool {
    let text = |name| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    };
    let dest = text("sec-fetch-dest");
    if !dest.is_empty() {
        return ["document", "iframe", "frame"]
            .iter()
            .any(|v| dest.eq_ignore_ascii_case(v));
    }
    text("sec-fetch-mode").eq_ignore_ascii_case("navigate")
        || text("accept").to_ascii_lowercase().contains("text/html")
}
