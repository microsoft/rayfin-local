//! The developer's Azure tokens as `serve` hands them to the functions (`ctx.Tokens`) and the
//! connectors, which can't work without them: the broker's, with each audience whose token can't
//! be had reported once to hosts (`azure-sign-in`). The auth service gets the broker itself,
//! since it signs in as the local developer account without a token.

use std::collections::HashSet;
use std::ffi::OsString;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use anyhow::anyhow;
use rl_tokens::{
    AccessToken, Audience, BoxFuture, HostTokenConfig, TokenBroker, TokenError, TokenSource,
};
use serde_json::json;

use crate::errors::coded;
use crate::events::{self, ErrorCode, WarningCode};

/// Where the `host` token source asks for tokens.
pub const TOKEN_URL_VAR: &str = "RAYFIN_LOCAL_TOKEN_URL";
/// The secret the `host` token source sends the host.
pub const TOKEN_SECRET_VAR: &str = "RAYFIN_LOCAL_TOKEN_SECRET";

/// `RAYFIN_LOCAL_TOKEN_SECRET` as `rayfin-local` started ([`take_token_secret`]).
static TOKEN_SECRET: OnceLock<Option<OsString>> = OnceLock::new();

/// Takes `RAYFIN_LOCAL_TOKEN_SECRET` out of the environment, so that no child process (Vite, the
/// function host, the app's code) can ask the host for the user's tokens. Call it first in
/// `main`, while no other thread runs.
pub fn take_token_secret() {
    let secret = std::env::var_os(TOKEN_SECRET_VAR);
    if secret.is_some() {
        // SAFETY: `main` calls this before it starts any thread.
        unsafe { std::env::remove_var(TOKEN_SECRET_VAR) };
    }
    let _ = TOKEN_SECRET.set(secret);
}

/// The `host` token source's endpoint: `RAYFIN_LOCAL_TOKEN_URL`, an `http://` URL on loopback,
/// and `RAYFIN_LOCAL_TOKEN_SECRET`, at least 32 characters. A `config-invalid` error otherwise.
pub fn host_token_config() -> anyhow::Result<HostTokenConfig> {
    let invalid = |message: String| {
        coded(
            ErrorCode::ConfigInvalid,
            events::no_details(),
            anyhow!(message),
        )
    };
    let url = std::env::var(TOKEN_URL_VAR).unwrap_or_default();
    let secret = TOKEN_SECRET
        .get_or_init(|| std::env::var_os(TOKEN_SECRET_VAR))
        .as_ref()
        .map(|secret| secret.to_string_lossy().into_owned())
        .unwrap_or_default();
    if url.trim().is_empty() || secret.is_empty() {
        return Err(invalid(format!(
            "The host token source (--token-source host) needs {TOKEN_URL_VAR} and \
             {TOKEN_SECRET_VAR}: the endpoint of the app that runs rayfin-local, and its secret."
        )));
    }
    HostTokenConfig::new(&url, &secret).map_err(|error| {
        invalid(if error.contains("secret") {
            format!("{TOKEN_SECRET_VAR} is too short for the host token source: {error}.")
        } else {
            format!("{TOKEN_URL_VAR} {error}.")
        })
    })
}

/// [`TokenBroker`]'s tokens, reporting failures as `azure-sign-in` warnings.
pub fn reporting(broker: Arc<TokenBroker>) -> Arc<dyn TokenSource> {
    Arc::new(ReportingTokens { broker })
}

struct ReportingTokens {
    broker: Arc<TokenBroker>,
}

impl ReportingTokens {
    /// The audience a scope belongs to: its default scope, or the one configured for it.
    fn audience(&self, scope: &str) -> Option<Audience> {
        let scope = scope.trim();
        Audience::ALL.into_iter().find(|audience| {
            audience.default_scope() == scope || self.broker.scope_for(*audience) == scope
        })
    }
}

impl TokenSource for ReportingTokens {
    fn token<'a>(&'a self, scope: &'a str) -> BoxFuture<'a, Result<AccessToken, TokenError>> {
        Box::pin(async move {
            let result = self.broker.token(scope).await;
            if let Err(err) = &result {
                report(self.audience(scope), scope, err);
            }
            result
        })
    }

    fn token_silently<'a>(
        &'a self,
        scope: &'a str,
    ) -> BoxFuture<'a, Result<AccessToken, TokenError>> {
        Box::pin(async move {
            let result = self.broker.token_silently(scope).await;
            if let Err(err) = &result {
                report(self.audience(scope), scope, err);
            }
            result
        })
    }

    /// Not reported: the caller has a token, which a service turned down, and says what to do.
    fn another_token<'a>(
        &'a self,
        scope: &'a str,
        refused: &'a str,
    ) -> BoxFuture<'a, Option<Result<AccessToken, TokenError>>> {
        self.broker.another_token(scope, refused)
    }
}

/// An `azure-sign-in` warning, the first time an audience (or another scope) fails in this run.
fn report(audience: Option<Audience>, scope: &str, err: &TokenError) {
    static REPORTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let name = audience.map(Audience::udf_name);
    let key = name.unwrap_or(scope).to_owned();
    let first = REPORTED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key);
    if !first {
        return;
    }
    let what = name.map_or_else(|| format!("the scope {scope}"), str::to_owned);
    events::warning(
        WarningCode::AzureSignIn,
        format!(
            "No Azure token for {what}: {}.",
            err.message.trim_end_matches('.')
        ),
        err.hint.clone(),
        json!({ "audience": name, "scope": scope }),
    );
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    /// Set for the copy of this test binary that takes the secret.
    const TAKING: &str = "RAYFIN_LOCAL_TEST_TAKES_THE_TOKEN_SECRET";
    const SECRET: &str = "a-token-secret-of-more-than-32-characters";

    /// In a process of its own (this test binary again), since it changes the environment.
    #[test]
    fn the_token_secret_is_taken_out_of_the_environment() {
        let name = "tokens::tests::the_token_secret_is_taken_out_of_the_environment";
        if std::env::var_os(TAKING).is_some() {
            take_token_secret();
            assert_eq!(std::env::var_os(TOKEN_SECRET_VAR), None);
            let config = host_token_config().unwrap();
            assert_eq!(config.url(), "http://127.0.0.1:4000/token");
            // Nor do the processes it starts see it.
            let mut child = if cfg!(windows) {
                let mut cmd = Command::new("cmd");
                cmd.args([
                    "/C",
                    "if defined RAYFIN_LOCAL_TOKEN_SECRET (exit 1) else (exit 0)",
                ]);
                cmd
            } else {
                let mut sh = Command::new("sh");
                sh.args(["-c", "test -z \"${RAYFIN_LOCAL_TOKEN_SECRET+set}\""]);
                sh
            };
            assert!(child.status().unwrap().success());
            return;
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([name, "--exact", "--test-threads=1"])
            .env(TAKING, "1")
            .env(TOKEN_URL_VAR, "http://127.0.0.1:4000/token")
            .env(TOKEN_SECRET_VAR, SECRET)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(stdout.contains("1 passed"), "{stdout}");
    }
}
