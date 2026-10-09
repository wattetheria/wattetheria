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
use serde_json::{Map, Value};

use super::webhook::MAX_REQUEST_BYTES;

const MAX_TEXT_CHARS: usize = 65_536;

pub const ENV_URL: &str = "WATTETHERIA_EVENT_WEBHOOK_URL";
pub const ENV_HEADERS: &str = "WATTETHERIA_EVENT_WEBHOOK_HEADERS";
pub const ENV_SECRET: &str = "WATTETHERIA_EVENT_WEBHOOK_SECRET";
pub const ENV_TEXT: &str = "WATTETHERIA_EVENT_WEBHOOK_TEXT";

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
    pub(crate) include_text: bool,
}

impl EnvWebhookConfig {
    /// `Ok(None)` means the webhook is not configured.
    pub fn from_env() -> Result<Option<Self>> {
        let read = |key: &str| std::env::var(key).ok();
        Self::from_values(
            read(ENV_URL).as_deref(),
            read(ENV_HEADERS).as_deref(),
            read(ENV_SECRET).as_deref(),
            read(ENV_TEXT).as_deref(),
        )
    }

    pub(crate) fn from_values(
        url: Option<&str>,
        headers: Option<&str>,
        secret: Option<&str>,
        text: Option<&str>,
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
        let include_text = text
            .map(unquote_env_value)
            .filter(|value| !value.is_empty())
            .map(str::parse::<bool>)
            .transpose()
            .with_context(|| format!("{ENV_TEXT} must be true or false"))?
            .unwrap_or(false);
        Ok(Some(Self {
            url: url.to_owned(),
            headers,
            secret,
            include_text,
        }))
    }
}

pub(crate) fn body_with_text(body: &[u8]) -> Result<Vec<u8>> {
    let event: Map<String, Value> = serde_json::from_slice(body)?;
    // Projected data is flat. Preview its strings, never the event identity or
    // the serialized JSON itself; escaping must be included in both limits.
    for preview_chars in [None, Some(4096), Some(1024), Some(256), Some(0)] {
        let mut preview = event.clone();
        if let Some(limit) = preview_chars {
            let data = preview
                .get_mut("data")
                .and_then(Value::as_object_mut)
                .context("event data must be an object")?;
            let mut content_truncated = false;
            for (name, value) in data.iter_mut() {
                if let Some(text) = value.as_str() {
                    let shortened: String = text.chars().take(limit).collect();
                    if shortened.len() < text.len() {
                        content_truncated |= name == "content";
                        *value = Value::String(shortened);
                    }
                }
            }
            if content_truncated {
                data.insert("content_truncated".into(), Value::Bool(true));
            }
            preview.insert("text_truncated".into(), Value::Bool(true));
        }
        let text = serde_json::to_string(&preview)?;
        if text.encode_utf16().count() > MAX_TEXT_CHARS {
            continue;
        }
        let mut payload = event.clone();
        payload.insert("text".into(), Value::String(text));
        let body = serde_json::to_vec(&payload)?;
        if body.len() <= MAX_REQUEST_BYTES {
            return Ok(body);
        }
        if preview_chars.is_some() {
            payload.insert("data".into(), preview["data"].clone());
            payload.insert("data_truncated".into(), Value::Bool(true));
            let body = serde_json::to_vec(&payload)?;
            if body.len() <= MAX_REQUEST_BYTES {
                return Ok(body);
            }
        }
    }
    bail!("event metadata exceeds webhook text limits")
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
    fn text_preview_starts_only_above_the_receiver_limit() {
        for unicode in [false, true] {
            let mut event = serde_json::json!({
                "eventId": "evt_test", "name": "wattetheria.agent.topic_message_requires_reply",
                "timestamp": "2023-11-14T22:13:20.000Z", "data": {"content": ""},
            });
            let available = MAX_TEXT_CHARS
                - serde_json::to_string(&event)
                    .unwrap()
                    .encode_utf16()
                    .count();
            let content = if unicode {
                format!(
                    "{}{}",
                    "\u{1f600}".repeat(available / 2),
                    "x".repeat(available % 2)
                )
            } else {
                "x".repeat(available)
            };
            event["data"]["content"] = Value::String(content.clone());
            let encoded = serde_json::to_vec(&event).unwrap();
            let body: Value = serde_json::from_slice(&body_with_text(&encoded).unwrap()).unwrap();
            let text = body["text"].as_str().unwrap();
            assert_eq!(text.encode_utf16().count(), MAX_TEXT_CHARS);
            assert_eq!(serde_json::from_str::<Value>(text).unwrap(), event);
            assert_eq!(body["data"], event["data"]);

            event["data"]["content"] = Value::String(format!("{content}x"));
            let encoded = serde_json::to_vec(&event).unwrap();
            let body: Value = serde_json::from_slice(&body_with_text(&encoded).unwrap()).unwrap();
            let text = body["text"].as_str().unwrap();
            assert!(text.encode_utf16().count() <= MAX_TEXT_CHARS);
            let preview: Value = serde_json::from_str(text).unwrap();
            assert_eq!(preview["text_truncated"], true);
            assert_eq!(preview["data"]["content_truncated"], true);
            assert_eq!(body["data"], event["data"]);
            assert!(content.starts_with(preview["data"]["content"].as_str().unwrap()));
        }
    }

    #[test]
    fn text_preview_budgets_all_projected_strings_and_json_escaping() {
        let original = "\u{0000}".repeat(2000);
        let mut data = Map::new();
        for name in [
            "content",
            "event_id",
            "source_kind",
            "source_node_id",
            "source_agent_id",
            "target_agent_id",
            "correlation_id",
            "chosen_action",
            "route",
            "network_id",
            "feed_key",
            "message_id",
        ] {
            data.insert(name.into(), Value::String(original.clone()));
        }
        data.insert("requires_action".into(), Value::Bool(true));
        let event = serde_json::json!({
            "eventId": "evt_test", "name": "wattetheria.agent.topic_message_requires_reply",
            "timestamp": "2023-11-14T22:13:20.000Z", "data": data,
        });
        let body = body_with_text(&serde_json::to_vec(&event).unwrap()).unwrap();
        assert!(body.len() <= MAX_REQUEST_BYTES);
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["data"], event["data"]);
        let text = body["text"].as_str().unwrap();
        assert!(text.encode_utf16().count() <= MAX_TEXT_CHARS);
        let preview: Value = serde_json::from_str(text).unwrap();
        assert_eq!(preview["text_truncated"], true);
        assert_eq!(preview["data"]["content_truncated"], true);
        assert_eq!(preview["data"]["requires_action"], true);
        for (name, value) in data {
            if value.is_string() {
                assert_eq!(preview["data"][&name], "\u{0000}".repeat(256));
            }
        }
    }

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
                Some(&format!("{quote}true{quote}")),
            )
            .unwrap()
            .unwrap();
            assert_eq!(config.url, url);
            assert_eq!(config.headers["authorization"], "Bearer receiver-key");
            assert_eq!(config.headers["x-note"], "\"keep me\"");
            assert_eq!(config.secret.as_deref(), Some(secret));
            assert!(config.include_text);
            validate_secret(config.secret.as_deref().unwrap()).unwrap();
        }
        assert!(
            EnvWebhookConfig::from_values(Some("\"\""), None, None, None)
                .unwrap()
                .is_none()
        );
        assert!(
            EnvWebhookConfig::from_values(Some("''"), None, None, None)
                .unwrap()
                .is_none()
        );
        assert!(
            EnvWebhookConfig::from_values(
                Some("\"https://receiver.example/hook'"),
                None,
                None,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn unset_or_blank_url_means_not_configured() {
        for url in [None, Some(""), Some("   ")] {
            assert!(
                EnvWebhookConfig::from_values(url, Some("X: y"), None, None)
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
            let config = EnvWebhookConfig::from_values(Some(url), None, None, None)
                .unwrap()
                .unwrap();
            assert_eq!(config.url, url);
            assert!(config.headers.is_empty());
            assert!(config.secret.is_none());
            assert!(!config.include_text);
        }
        for url in [
            "ftp://example.com",
            "not a url",
            "https://user:pass@example.com/",
            "https://example.com/#frag",
        ] {
            assert!(
                EnvWebhookConfig::from_values(Some(url), None, None, None).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn text_flag_defaults_off_and_rejects_invalid_values() {
        for (text, expected) in [
            (None, false),
            (Some(""), false),
            (Some("   "), false),
            (Some("false"), false),
            (Some("\"false\""), false),
            (Some("true"), true),
            (Some("'true'"), true),
        ] {
            let config = EnvWebhookConfig::from_values(
                Some("https://receiver.example/hook"),
                None,
                None,
                text,
            )
            .unwrap()
            .unwrap();
            assert_eq!(config.include_text, expected, "{text:?}");
        }
        assert!(
            EnvWebhookConfig::from_values(
                Some("https://receiver.example/hook"),
                None,
                None,
                Some("invalid"),
            )
            .is_err()
        );
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
