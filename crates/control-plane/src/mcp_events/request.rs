//! Optional, receiver-defined JSON bodies and request signatures.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::env_webhook::RESERVED_HEADERS;
use super::webhook::{MAX_REQUEST_BYTES, WebhookFailure};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestConfig {
    body_template: Option<Value>,
    #[serde(default)]
    headers: BTreeMap<String, Value>,
    signature: Option<Signature>,
    response: Option<ResponseMatch>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Signature {
    algorithm: Algorithm,
    secret: String,
    input: Vec<String>,
    encoding: Encoding,
    header: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Algorithm {
    Sha256,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Encoding {
    Hex,
    Base64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseMatch {
    pointer: String,
    equals: Value,
}

pub(crate) struct PreparedRequest {
    pub(crate) body: Vec<u8>,
    pub(crate) headers: HeaderMap,
}

impl RequestConfig {
    pub(crate) fn parse(raw: &str) -> Result<Self> {
        let config: Self =
            serde_json::from_str(raw).context("decode webhook request configuration")?;
        if let Some(body) = &config.body_template {
            validate_template(body)?;
        }
        for (name, value) in &config.headers {
            header_name(name)?;
            validate_template(value)?;
        }
        if let Some(signature) = &config.signature {
            let name = header_name(&signature.header)?;
            ensure!(!signature.secret.is_empty(), "signature secret is required");
            ensure!(!signature.input.is_empty(), "signature input is required");
            ensure!(
                config
                    .headers
                    .keys()
                    .all(|key| !key.eq_ignore_ascii_case(name.as_str())),
                "signature header cannot also be a template header"
            );
            for part in &signature.input {
                ensure!(
                    !part.starts_with('$')
                        || matches!(part.as_str(), "$timestamp" | "$nonce" | "$secret" | "$body"),
                    "unknown signature input variable"
                );
            }
        }
        if let Some(response) = &config.response {
            validate_pointer(&response.pointer)?;
        }
        Ok(config)
    }

    pub(crate) fn prepare(
        &self,
        body: &[u8],
        headers: &HeaderMap,
    ) -> Result<PreparedRequest, WebhookFailure> {
        self.prepare_at(
            body,
            headers,
            chrono::Utc::now().timestamp(),
            &uuid::Uuid::new_v4().to_string(),
        )
    }

    fn prepare_at(
        &self,
        body: &[u8],
        headers: &HeaderMap,
        timestamp: i64,
        nonce: &str,
    ) -> Result<PreparedRequest, WebhookFailure> {
        let event: Value =
            serde_json::from_slice(body).map_err(|_| WebhookFailure::InvalidPayload)?;
        let context = json!({"event": event, "timestamp": timestamp.to_string(), "nonce": nonce});
        let body = match &self.body_template {
            Some(template) => render(template, &context)
                .and_then(|value| Ok(serde_json::to_vec(&value)?))
                .map_err(|_| WebhookFailure::InvalidPayload)?,
            None => body.to_vec(),
        };
        if body.len() > MAX_REQUEST_BYTES {
            return Err(WebhookFailure::RequestTooLarge);
        }
        let mut headers = headers.clone();
        for (name, template) in &self.headers {
            let value = render(template, &context).map_err(|_| WebhookFailure::InvalidPayload)?;
            let mut value = value
                .as_str()
                .and_then(|value| HeaderValue::from_str(value).ok())
                .ok_or(WebhookFailure::InvalidPayload)?;
            value.set_sensitive(true);
            headers.insert(
                header_name(name).map_err(|_| WebhookFailure::InvalidPayload)?,
                value,
            );
        }
        if let Some(signature) = &self.signature {
            let mut input = Vec::new();
            for part in &signature.input {
                input.extend_from_slice(match part.as_str() {
                    "$timestamp" => context["timestamp"].as_str().unwrap_or_default().as_bytes(),
                    "$nonce" => nonce.as_bytes(),
                    "$secret" => signature.secret.as_bytes(),
                    "$body" => &body,
                    literal => literal.as_bytes(),
                });
            }
            let digest = match signature.algorithm {
                Algorithm::Sha256 => Sha256::digest(&input),
            };
            let encoded = match signature.encoding {
                Encoding::Hex => hex::encode(digest),
                Encoding::Base64 => STANDARD.encode(digest),
            };
            let mut value =
                HeaderValue::from_str(&encoded).map_err(|_| WebhookFailure::InvalidPayload)?;
            value.set_sensitive(true);
            headers.insert(
                header_name(&signature.header).map_err(|_| WebhookFailure::InvalidPayload)?,
                value,
            );
        }
        Ok(PreparedRequest { body, headers })
    }

    pub(crate) async fn check_response(
        &self,
        mut response: reqwest::Response,
    ) -> Result<u16, WebhookFailure> {
        let status = response.status().as_u16();
        let Some(expected) = self
            .response
            .as_ref()
            .filter(|_| response.status().is_success())
        else {
            return Ok(status);
        };
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| WebhookFailure::Transport)?
        {
            if body.len() + chunk.len() > MAX_REQUEST_BYTES {
                return Err(WebhookFailure::ReceiverRejected);
            }
            body.extend_from_slice(&chunk);
        }
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| WebhookFailure::ReceiverRejected)?;
        if value.pointer(&expected.pointer) != Some(&expected.equals) {
            return Err(WebhookFailure::ReceiverRejected);
        }
        Ok(status)
    }
}

fn header_name(name: &str) -> Result<HeaderName> {
    let name = HeaderName::from_bytes(name.as_bytes()).context("invalid request header name")?;
    ensure!(
        !RESERVED_HEADERS.contains(&name.as_str()),
        "header is managed by webhook transport"
    );
    Ok(name)
}

fn validate_template(template: &Value) -> Result<()> {
    match template {
        Value::Object(object) if object.contains_key("$ref") => {
            ensure!(object.len() == 1, "$ref must be the only template key");
            let pointer = object["$ref"]
                .as_str()
                .context("$ref must be a JSON Pointer string")?;
            validate_pointer(pointer)?;
        }
        Value::Object(object) if object.contains_key("$json") => {
            ensure!(object.len() == 1, "$json must be the only template key");
            validate_template(&object["$json"])?;
        }
        Value::Object(object) => {
            for value in object.values() {
                validate_template(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_template(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_pointer(pointer: &str) -> Result<()> {
    ensure!(
        pointer.is_empty() || pointer.starts_with('/'),
        "expected a JSON Pointer"
    );
    ensure!(
        pointer
            .match_indices('~')
            .all(|(index, _)| matches!(pointer.as_bytes().get(index + 1), Some(b'0' | b'1'))),
        "invalid JSON Pointer escape"
    );
    Ok(())
}

fn render(template: &Value, context: &Value) -> Result<Value> {
    match template {
        Value::Object(object) if object.contains_key("$ref") => context
            .pointer(object["$ref"].as_str().context("invalid reference")?)
            .cloned()
            .context("webhook template reference is missing"),
        Value::Object(object) if object.contains_key("$json") => Ok(Value::String(
            serde_json::to_string(&render(&object["$json"], context)?)?,
        )),
        Value::Object(object) => object
            .iter()
            .map(|(key, value)| Ok((key.clone(), render(value, context)?)))
            .collect::<Result<serde_json::Map<_, _>>>()
            .map(Value::Object),
        Value::Array(values) => values
            .iter()
            .map(|value| render(value, context))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        value => Ok(value.clone()),
    }
}

#[cfg(test)]
#[path = "../tests/mcp_events_tests/request_tests.rs"]
mod tests;
