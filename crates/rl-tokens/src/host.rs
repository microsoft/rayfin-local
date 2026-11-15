//! The host provider (`--token-source host`): the app that runs rayfin-local (a desktop host,
//! docs/embedding.md) gets the user's tokens itself, for example from the operating system's
//! account broker, and answers them on a loopback HTTP endpoint:
//!
//! `POST <url>` with `Authorization: Bearer <secret>` and `{"scope", "tenant", "silent"}` answers
//! `{"accessToken", "expiresOn"}` (seconds since the epoch), or a 4xx/5xx `{"error", "message"}`.

use std::fmt;
use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use reqwest::Url;
use serde::Deserialize;
use serde_json::json;

use crate::{AccessToken, TokenError, az::shorten, broker::Prompt, jwt};

/// How long the host has to answer.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The shortest secret accepted.
pub const MIN_HOST_SECRET_CHARS: usize = 32;
/// The largest answer read.
const MAX_ANSWER_BYTES: usize = 1024 * 1024;

/// Where the host answers token requests, and the secret it expects.
#[derive(Clone)]
pub struct HostTokenConfig {
    url: Url,
    secret: String,
}

impl HostTokenConfig {
    /// Checks that `url` is an `http://` URL on a loopback address (`127.0.0.0/8`, `[::1]` or
    /// `localhost`), so that the secret and the tokens never leave this machine, and that
    /// `secret` has at least [`MIN_HOST_SECRET_CHARS`] characters. The error says what is wrong.
    pub fn new(url: &str, secret: &str) -> Result<Self, String> {
        let url = url.trim();
        let loopback = "an http:// URL on this machine (http://127.0.0.1:<port>/… or \
                        http://localhost:<port>/…)";
        let parsed = Url::parse(url).map_err(|_| format!("is not {loopback}: `{url}`"))?;
        let host = parsed.host_str().unwrap_or_default();
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let host_is_loopback = host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
        if parsed.scheme() != "http"
            || !host_is_loopback
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(format!("is not {loopback}: `{url}`"));
        }
        let length = secret.chars().count();
        if length < MIN_HOST_SECRET_CHARS {
            return Err(format!(
                "the secret is {length} characters long; it needs at least \
                 {MIN_HOST_SECRET_CHARS}"
            ));
        }
        Ok(Self {
            url: parsed,
            secret: secret.to_owned(),
        })
    }

    pub fn url(&self) -> &str {
        self.url.as_str()
    }
}

impl fmt::Debug for HostTokenConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostTokenConfig")
            .field("url", &self.url.as_str())
            .finish_non_exhaustive()
    }
}

pub(crate) struct HostTokens {
    config: HostTokenConfig,
    client: OnceLock<Result<reqwest::Client, String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Granted {
    access_token: String,
    /// Seconds since the epoch.
    #[serde(default)]
    expires_on: Option<f64>,
}

#[derive(Default, Deserialize)]
struct Refused {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

impl HostTokens {
    pub(crate) fn new(config: HostTokenConfig) -> Self {
        Self {
            config,
            client: OnceLock::new(),
        }
    }

    fn client(&self) -> Result<&reqwest::Client, String> {
        self.client
            .get_or_init(|| {
                let _ = rustls::crypto::ring::default_provider().install_default();
                reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(TIMEOUT)
                    .build()
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    pub(crate) async fn token(
        &self,
        scope: &str,
        tenant: Option<&str>,
        prompt: Prompt,
    ) -> Result<AccessToken, TokenError> {
        let silent = prompt == Prompt::Never;
        tracing::debug!(scope, tenant, silent, "asking the host for a token");
        let client = self
            .client()
            .map_err(|error| failure(scope, format!("could not ask the host: {error}")))?;
        let request = client
            .post(self.config.url.clone())
            .bearer_auth(&self.config.secret)
            .json(&json!({ "scope": scope, "tenant": tenant, "silent": silent }))
            .send();
        let response = request.await.map_err(|error| {
            let what = if error.is_timeout() {
                format!("did not answer within {} s", TIMEOUT.as_secs())
            } else {
                "could not be reached".to_owned()
            };
            failure(
                scope,
                format!(
                    "the host's token endpoint ({}) {what}",
                    self.config.url.as_str()
                ),
            )
        })?;
        let status = response.status();
        let body = response.bytes().await.map_err(|_| {
            failure(
                scope,
                "the host's token endpoint broke off its answer".to_owned(),
            )
        })?;
        if body.len() > MAX_ANSWER_BYTES {
            return Err(failure(
                scope,
                "the host's token endpoint answered too much".to_owned(),
            ));
        }
        if !status.is_success() {
            let refused: Refused = serde_json::from_slice(&body).unwrap_or_default();
            let code = refused.error.unwrap_or_else(|| "unavailable".to_owned());
            let message = refused
                .message
                .map(|message| shorten(&message))
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            tracing::debug!(
                scope,
                status = status.as_u16(),
                code,
                "the host gave no token"
            );
            let hint = if code == "interaction_required" {
                "Sign in to the app that runs rayfin-local, then retry."
            } else {
                "Check that the app that runs rayfin-local is signed in, then retry."
            };
            return Err(TokenError {
                scope: scope.to_owned(),
                message: format!("the host could not get a token for {scope} ({code}): {message}"),
                hint: Some(hint.to_owned()),
            });
        }
        let granted: Granted = serde_json::from_slice(&body).map_err(|_| {
            failure(
                scope,
                "the host's token endpoint answered something other than \
                 {\"accessToken\", \"expiresOn\"}"
                    .to_owned(),
            )
        })?;
        let token = granted.access_token.trim().to_owned();
        if token.is_empty() {
            return Err(failure(
                scope,
                "the host answered an empty token".to_owned(),
            ));
        }
        let expires_at = granted
            .expires_on
            .filter(|seconds| *seconds > 0.0 && *seconds < 1e11)
            .map(|seconds| SystemTime::UNIX_EPOCH + Duration::from_secs_f64(seconds))
            .or_else(|| jwt::expiry(&token))
            // Unknown expiry: short enough not to hand out a stale token for long.
            .unwrap_or_else(|| SystemTime::now() + Duration::from_secs(5 * 60));
        tracing::debug!(scope, "the host answered a token");
        Ok(AccessToken { token, expires_at })
    }
}

fn failure(scope: &str, message: String) -> TokenError {
    TokenError {
        scope: scope.to_owned(),
        message,
        hint: Some(
            "Check that the app that runs rayfin-local is running and signed in, then retry."
                .to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn only_loopback_http_urls_and_long_secrets_are_accepted() {
        for url in [
            "http://127.0.0.1:4000/token",
            "http://127.1.2.3:4000/",
            "http://localhost:4000/t",
            "http://LOCALHOST/t",
            "http://[::1]:4000/t",
        ] {
            assert!(HostTokenConfig::new(url, SECRET).is_ok(), "{url}");
        }
        for url in [
            "https://127.0.0.1:4000/token",
            "http://10.0.0.1:4000/token",
            "http://example.com/token",
            "http://localhost.example.com/token",
            "http://user:pw@127.0.0.1:4000/token",
            "http://0.0.0.0:4000/token",
            "127.0.0.1:4000",
            "",
        ] {
            let error = HostTokenConfig::new(url, SECRET).unwrap_err();
            assert!(error.contains("on this machine"), "{url}: {error}");
        }
        let error = HostTokenConfig::new("http://127.0.0.1:1/", "short").unwrap_err();
        assert!(error.contains("at least 32"), "{error}");
    }

    #[test]
    fn debug_hides_the_secret() {
        let config = HostTokenConfig::new("http://127.0.0.1:1/", SECRET).unwrap();
        assert!(!format!("{config:?}").contains(SECRET));
    }
}
