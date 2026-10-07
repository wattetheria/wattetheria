//! Operator-configured event webhook.
//!
//! One URL from the deployment environment receives every agent event; the
//! receiver needs no subscription or per-event setup. Deliveries go through the
//! durable outbox with Standard Webhooks signatures and retries.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use rand::RngCore;
use reqwest::Url;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

pub const ENV_URL: &str = "WATTETHERIA_EVENT_WEBHOOK_URL";
pub const ENV_HEADERS: &str = "WATTETHERIA_EVENT_WEBHOOK_HEADERS";
pub const ENV_SECRET: &str = "WATTETHERIA_EVENT_WEBHOOK_SECRET";

/// Headers that belong to the request framing or to the signature itself.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-type",
    "content-length",
    "transfer-encoding",
    "connection",
    "webhook-id",
    "webhook-timestamp",
    "webhook-signature",
];

#[derive(Clone)]
pub struct EnvWebhookConfig {
    pub(crate) url: String,
    pub(crate) headers: HeaderMap,
    pub(crate) secret: Option<String>,
}

impl EnvWebhookConfig {
    /// `Ok(None)` means the webhook is not configured.
    pub fn from_env() -> Result<Option<Self>> {
        let read = |key: &str| std::env::var(key).ok();
        Self::from_values(
            read(ENV_URL).as_deref(),
            read(ENV_HEADERS).as_deref(),
            read(ENV_SECRET).as_deref(),
        )
    }

    pub(crate) fn from_values(
        url: Option<&str>,
        headers: Option<&str>,
        secret: Option<&str>,
    ) -> Result<Option<Self>> {
        let Some(url) = url.map(unquote_env_value).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        validate_url(url).with_context(|| format!("{ENV_URL} is invalid"))?;
        let headers = parse_headers(unquote_env_value(headers.unwrap_or_default()))
            .with_context(|| format!("{ENV_HEADERS} is invalid"))?;
        let secret = secret
            .map(unquote_env_value)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        Ok(Some(Self {
            url: url.to_owned(),
            headers,
            secret,
        }))
    }
}

fn unquote_env_value(value: &str) -> &str {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        })
        .unwrap_or(value)
        .trim()
}

/// Syntax-only check: operator URLs may be http or private addresses.
pub(crate) fn validate_url(raw: &str) -> Result<()> {
    let url = Url::parse(raw).map_err(|_| anyhow!("not a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("scheme must be http or https");
    }
    if url.host_str().is_none() {
        bail!("host is required");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("credentials in the URL are not supported; use a header or a query parameter");
    }
    if url.fragment().is_some() {
        bail!("fragments are not supported");
    }
    Ok(())
}

/// `Name: value` entries separated by a newline, a literal `\n` or `;`.
/// Values therefore cannot contain `;`.
pub(crate) fn parse_headers(raw: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for entry in raw.replace("\\n", "\n").split(['\n', ';']) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (name, value) = entry
            .split_once(':')
            .ok_or_else(|| anyhow!("header entry must look like `Name: value`"))?;
        let name = HeaderName::from_bytes(name.trim().as_bytes())
            .map_err(|_| anyhow!("invalid header name"))?;
        if RESERVED_HEADERS.contains(&name.as_str()) {
            bail!("header {name} is managed by the delivery and cannot be set");
        }
        let mut value = HeaderValue::from_str(value.trim())
            .map_err(|_| anyhow!("invalid value for header {name}"))?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(headers)
}

pub(crate) fn generate_secret() -> String {
    let mut key = [0_u8; 32];
    rand::rng().fill_bytes(&mut key);
    format!(
        "whsec_{}",
        base64::engine::general_purpose::STANDARD.encode(key)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_events::webhook::validate_secret;

    #[test]
    fn webhook_values_accept_paired_outer_quotes() {
        let url = "https://receiver.example/hook?a=1&b=2";
        let headers = "Authorization: Bearer receiver-key;X-Note: \"keep me\"";
        let secret = "whsec_AAECAwQFBgcICQoLDA0ODxAREhMUFRYX";
        for quote in ["", "\"", "'"] {
            let config = EnvWebhookConfig::from_values(
                Some(&format!("{quote}{url}{quote}")),
                Some(&format!("{quote}{headers}{quote}")),
                Some(&format!("{quote}{secret}{quote}")),
            )
            .unwrap()
            .unwrap();
            assert_eq!(config.url, url);
            assert_eq!(config.headers["authorization"], "Bearer receiver-key");
            assert_eq!(config.headers["x-note"], "\"keep me\"");
            assert_eq!(config.secret.as_deref(), Some(secret));
            validate_secret(config.secret.as_deref().unwrap()).unwrap();
        }
        assert!(
            EnvWebhookConfig::from_values(Some("\"\""), None, None)
                .unwrap()
                .is_none()
        );
        assert!(
            EnvWebhookConfig::from_values(Some("''"), None, None)
                .unwrap()
                .is_none()
        );
        assert!(
            EnvWebhookConfig::from_values(Some("\"https://receiver.example/hook'"), None, None)
                .is_err()
        );
    }

    #[test]
    fn unset_or_blank_url_means_not_configured() {
        for url in [None, Some(""), Some("   ")] {
            assert!(
                EnvWebhookConfig::from_values(url, Some("X: y"), None)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn url_keeps_query_secret_and_allows_private_http() {
        for url in [
            "http://127.0.0.1:9000/hook",
            "http://10.0.0.5/hook?key=abc",
            "https://example.com/wake?token=t%2Fx",
        ] {
            let config = EnvWebhookConfig::from_values(Some(url), None, None)
                .unwrap()
                .unwrap();
            assert_eq!(config.url, url);
            assert!(config.headers.is_empty());
            assert!(config.secret.is_none());
        }
        for url in [
            "ftp://example.com",
            "not a url",
            "https://user:pass@example.com/",
            "https://example.com/#frag",
        ] {
            assert!(
                EnvWebhookConfig::from_values(Some(url), None, None).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn headers_accept_newline_literal_newline_and_semicolon_separators() {
        for raw in [
            "Authorization: Bearer abc\nX-Api-Key: k1",
            "Authorization: Bearer abc\\nX-Api-Key: k1",
            "Authorization: Bearer abc; X-Api-Key: k1;",
        ] {
            let headers = parse_headers(raw).unwrap();
            assert_eq!(headers.len(), 2, "{raw}");
            assert_eq!(headers["authorization"], "Bearer abc");
            assert_eq!(headers["x-api-key"], "k1");
            assert!(headers["authorization"].is_sensitive());
        }
        assert!(parse_headers("").unwrap().is_empty());
    }

    #[test]
    fn headers_reject_malformed_and_reserved_entries() {
        for raw in [
            "no-colon",
            "Bad Name: v",
            ": value",
            "Content-Type: text/plain",
            "Webhook-Signature: forged",
            "Host: elsewhere",
            "X-Bad: line\u{7f}",
        ] {
            assert!(parse_headers(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn generated_secret_is_a_valid_signing_secret() {
        let first = generate_secret();
        validate_secret(&first).unwrap();
        assert_ne!(first, generate_secret());
    }
}
