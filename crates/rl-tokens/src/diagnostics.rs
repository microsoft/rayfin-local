use std::sync::LazyLock;

use regex::Regex;

static CREDENTIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)((?:password|pwd|access[_-]?token|refresh[_-]?token|client[_-]?secret|secret|authorization|x-ms-rayfin-secrets)["']?\s*[:=]\s*)(?:"(?:\\.|[^"\\])*"|'[^']*'|[^\s,;}\r\n]+)"#,
    )
    .expect("credential pattern is valid")
});
static BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(bearer\s+)[^\s"',;}\r\n]+"#).expect("bearer pattern is valid")
});
static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+")
        .expect("JWT pattern is valid")
});
static URL_CREDENTIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([a-z][a-z0-9+.-]*://)[^/\s@]+@").expect("URL pattern is valid")
});

/// Keeps diagnostics useful without printing credentials. Callers must supply the actual secrets
/// and opaque tokens they passed upstream; pattern redaction alone cannot recognize every secret.
pub fn redact_diagnostic<'a>(
    message: &str,
    sensitive_values: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut values: Vec<&str> = sensitive_values
        .into_iter()
        .filter(|v| !v.is_empty())
        .collect();
    values.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
    let mut safe = message.to_owned();
    for value in values {
        safe = safe.replace(value, "<redacted>");
        if let Ok(escaped) = serde_json::to_string(value) {
            safe = safe.replace(&escaped[1..escaped.len() - 1], "<redacted>");
        }
    }
    safe = BEARER.replace_all(&safe, "${1}<redacted>").into_owned();
    safe = CREDENTIAL.replace_all(&safe, "${1}<redacted>").into_owned();
    safe = JWT.replace_all(&safe, "<redacted>").into_owned();
    URL_CREDENTIAL
        .replace_all(&safe, "${1}<redacted>@")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_and_structured_credentials_without_losing_the_failure() {
        let safe = redact_diagnostic(
            r#"query failed: opaque-value password="p4ss" Pwd=sqlpw; {"accessToken":"token"} Bearer abc.xyz https://user:pw@example.com eyJhbGciOiJYIn0.eyJzdWIiOiIxIn0.signature"#,
            ["opaque-value"],
        );
        assert!(safe.starts_with("query failed:"));
        for secret in [
            "opaque-value",
            "p4ss",
            "sqlpw",
            "\"token\"",
            "abc.xyz",
            "user:pw",
            "eyJ",
        ] {
            assert!(!safe.contains(secret), "{safe}");
        }
    }

    #[test]
    fn redacts_json_escaped_secrets_and_overlapping_values() {
        let value = "long\nsecret";
        assert_eq!(
            redact_diagnostic(r#"long\nsecret long"#, ["long", value]),
            "<redacted> <redacted>"
        );
        assert_eq!(
            redact_diagnostic("timeout on query", []),
            "timeout on query"
        );
    }

    #[test]
    fn redacts_quoted_credentials_with_spaces_and_authorization_headers() {
        let safe = redact_diagnostic(
            r#"Password="two word password"; {"refreshToken":"two word token"} Authorization: Bearer opaque-token"#,
            [],
        );
        for secret in ["two word", "opaque-token"] {
            assert!(!safe.contains(secret), "{safe}");
        }
    }
}
