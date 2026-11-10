//! The protected static-site gate. Fabric's signed hosting cookie is represented locally by a
//! verified Rayfin session, so expiry and sign-out revoke asset access too. As in Fabric, a
//! request whose `?_fu=` user hint names another user than the cookie's is gated again (the hint
//! can only reject a cookie), and the sign-in page may be framed when the app is embedded.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rl_model::{CallerResolver, FABRIC_USER_HINT_PARAM, bearer_token, fabric_user_hint};
use serde_json::json;

use crate::ServerConfig;

const SESSION_PATH: &str = "/.rayfin-local/static-session";
const SCRIPT_PATH: &str = "/.rayfin-local/static-signin.js";
const NO_STORE: &str = "no-store, no-cache, must-revalidate";
const PAGE: &str = include_str!("static_signin.html");
const SCRIPT: &str = include_str!("static_signin.js");
/// What marks a request as embedded when the browser sends no `Sec-Fetch-Dest`.
const EMBEDDED_PARAM: &str = "fabricEmbedded";
/// Fabric's `frame-ancestors` for gate pages in the portal frame (`EmbeddedFrameAncestors`).
const EMBEDDED_FRAME_ANCESTORS: &str = "https: http://localhost:* http://127.0.0.1:*";

pub(crate) struct StaticAccess {
    callers: Arc<dyn CallerResolver>,
    /// The app's item id, which user hints are derived for.
    project_id: String,
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
            project_id: config.item_id.clone(),
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
        let hint = query_value(req, FABRIC_USER_HINT_PARAM).filter(|hint| !hint.is_empty());
        if token.is_some_and(|token| {
            self.callers.resolve(Some(token)).is_ok_and(|caller| {
                caller.session.is_some_and(|session| {
                    hint.as_deref().is_none_or(|hint| {
                        fabric_user_hint(&self.project_id, &session.user_id).as_deref()
                            == Some(hint)
                    })
                })
            })
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
        security_headers(&mut response, embedded_request(req));
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

/// The gate's headers. Standalone, nothing may frame its page; embedded, Fabric's portal chain
/// (and a local one) may.
fn security_headers(response: &mut Response, embedded: bool) {
    let frame_ancestors = if embedded {
        EMBEDDED_FRAME_ANCESTORS
    } else {
        "'none'"
    };
    let csp = format!(
        "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors {frame_ancestors}"
    );
    let headers = response.headers_mut();
    for (name, value) in [
        (header::CACHE_CONTROL, NO_STORE),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&csp).expect("the gate's CSP is header-safe"),
    );
    if !embedded {
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    }
}

/// Fabric's `IsEmbeddedRequest`: loaded in a frame per `Sec-Fetch-Dest`, or, from browsers that
/// send no `Sec-Fetch-Dest`, marked with `fabricEmbedded`.
fn embedded_request(req: &Request) -> bool {
    match req
        .headers()
        .get("sec-fetch-dest")
        .and_then(|v| v.to_str().ok())
        .filter(|dest| !dest.is_empty())
    {
        Some(dest) => dest.eq_ignore_ascii_case("iframe") || dest.eq_ignore_ascii_case("frame"),
        None => query_value(req, EMBEDDED_PARAM).is_some(),
    }
}

fn query_value(req: &Request, name: &str) -> Option<String> {
    url::form_urlencoded::parse(req.uri().query()?.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
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
