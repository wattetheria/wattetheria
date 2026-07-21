use anyhow::{Context, Result, bail};
use reqwest::Url;
use serde_json::{Map, Value, json};

use super::{
    ServiceNetClient, ServiceNetClientError, ServiceNetInvokeRequest, ServiceNetInvokeResponse,
    ServiceNetServiceAgentSignature, verify_service_agent_invoke_response,
};

pub(super) fn uses_wattetheria_direct(record: &Value) -> bool {
    record
        .pointer("/deployment/connection_mode")
        .and_then(Value::as_str)
        == Some("wattetheria_direct")
}

pub(super) async fn invoke_agent(
    client: &ServiceNetClient,
    agent_id: &str,
    request: &ServiceNetInvokeRequest,
    published_agent: &Value,
) -> std::result::Result<ServiceNetInvokeResponse, ServiceNetClientError> {
    let response = invoke_agent_inner(client, agent_id, request, published_agent)
        .await
        .map_err(|error| ServiceNetClient::client_error(&error))?;
    let expected_request_digest = request
        .agent_envelope
        .as_ref()
        .and_then(|envelope| envelope.pointer("/extensions/request_digest"))
        .and_then(Value::as_str);
    let expected_request_nonce = request
        .agent_envelope
        .as_ref()
        .and_then(|envelope| envelope.pointer("/extensions/nonce"))
        .and_then(Value::as_str);
    verify_service_agent_invoke_response(
        published_agent,
        &response,
        expected_request_digest,
        expected_request_nonce,
        true,
    )
    .map_err(|error| ServiceNetClient::client_error(&error))?;
    client
        .record_service_response_nonce(&response)
        .map_err(|error| ServiceNetClient::client_error(&error))?;
    Ok(response)
}

async fn invoke_agent_inner(
    client: &ServiceNetClient,
    agent_id: &str,
    request: &ServiceNetInvokeRequest,
    published_agent: &Value,
) -> Result<ServiceNetInvokeResponse> {
    let direct_url = published_agent
        .pointer("/invoke/direct_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("published wattetheria_direct agent is missing invoke.direct_url")?;
    let direct_url = Url::parse(direct_url).context("parse published Adapter URL")?;
    if request.auth_context_id.is_some() && request.auth_token.is_none() {
        bail!(
            "wattetheria_direct invocation cannot resolve a ServiceNet auth_context_id; provide auth_token directly"
        );
    }
    let mut builder = client
        .http_client
        .post(direct_url.clone())
        .header("A2A-Version", "1.0");
    if let Some(token) = request.auth_token.as_deref() {
        builder = builder.bearer_auth(token);
    }
    let response = builder
        .json(&json!({
            "jsonrpc": "2.0",
            "id": uuid::Uuid::new_v4().to_string(),
            "method": "SendMessage",
            "params": build_send_message_params(request),
        }))
        .send()
        .await
        .with_context(|| format!("request Wattetheria Adapter {direct_url}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("read Wattetheria Adapter response")?;
    if !status.is_success() {
        bail!("Wattetheria Adapter {direct_url} returned {status}: {body}");
    }
    let raw: Value = serde_json::from_str(&body)
        .with_context(|| format!("parse Wattetheria Adapter response from {direct_url}"))?;
    if let Some(error) = raw.get("error") {
        bail!("Wattetheria Adapter rejected invocation: {error}");
    }
    build_direct_response(agent_id, request, raw)
}

fn build_send_message_params(request: &ServiceNetInvokeRequest) -> Value {
    let mut parts = Vec::new();
    if let Some(message) = invoke_message_text(request) {
        parts.push(json!({"kind": "text", "text": message}));
    }
    if !request.input.is_null() {
        parts.push(json!({"kind": "data", "data": request.input}));
    }
    if parts.is_empty() {
        parts.push(json!({"kind": "data", "data": Value::Null}));
    }
    let mut params = Map::new();
    if let Some(task_id) = request.task_id.as_ref() {
        params.insert("taskId".to_owned(), Value::String(task_id.clone()));
    }
    if let Some(context_id) = request.context_id.as_ref() {
        params.insert("contextId".to_owned(), Value::String(context_id.clone()));
    }
    if let Some(skill_id) = request.skill_id.as_ref() {
        params.insert("skillId".to_owned(), Value::String(skill_id.clone()));
    }
    params.insert(
        "message".to_owned(),
        json!({"role": "user", "parts": parts}),
    );
    let mut extensions = Map::new();
    if let Some(settlement) = request.settlement.as_ref() {
        extensions.insert(
            "settlement".to_owned(),
            normalize_settlement(serde_json::to_value(settlement).unwrap_or(Value::Null)),
        );
    }
    if let Some(envelope) = request.agent_envelope.as_ref() {
        extensions.insert("agent_envelope".to_owned(), envelope.clone());
    }
    if !extensions.is_empty() {
        params.insert("extensions".to_owned(), Value::Object(extensions));
    }
    Value::Object(params)
}

fn normalize_settlement(mut settlement: Value) -> Value {
    if settlement.get("layer").is_none() {
        settlement["layer"] = Value::String("web3".to_owned());
    }
    if let Some(rail) = settlement
        .get("rail")
        .and_then(Value::as_str)
        .map(|rail| rail.trim().to_ascii_lowercase())
    {
        settlement["rail"] = Value::String(rail);
    }
    if settlement.get("rail").and_then(Value::as_str) == Some("x402") {
        match settlement.get_mut("request") {
            Some(Value::Object(request)) => {
                request
                    .entry("protocol")
                    .or_insert_with(|| Value::String("x402".to_owned()));
            }
            Some(request @ Value::Null) => *request = json!({"protocol": "x402"}),
            None => settlement["request"] = json!({"protocol": "x402"}),
            Some(_) => {}
        }
    }
    settlement
}

fn invoke_message_text(request: &ServiceNetInvokeRequest) -> Option<String> {
    request
        .message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            request
                .input
                .as_object()
                .and_then(|input| {
                    ["message", "text", "query", "prompt"]
                        .into_iter()
                        .find_map(|key| input.get(key).and_then(Value::as_str))
                })
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn build_direct_response(
    agent_id: &str,
    request: &ServiceNetInvokeRequest,
    raw: Value,
) -> Result<ServiceNetInvokeResponse> {
    let result = raw
        .get("result")
        .context("A2A response is missing result")?;
    let task = result.get("task");
    let message = result.get("message");
    let service_signature = raw
        .pointer("/extensions/service_agent_signature")
        .cloned()
        .map(serde_json::from_value::<ServiceNetServiceAgentSignature>)
        .transpose()
        .context("parse Service Agent response signature")?;
    Ok(ServiceNetInvokeResponse {
        agent_id: agent_id.to_owned(),
        status: task
            .and_then(|task| task.pointer("/status/state"))
            .and_then(Value::as_str)
            .unwrap_or("MESSAGE_COMPLETED")
            .to_owned(),
        receipt_id: None,
        task_id: task
            .and_then(|task| task.get("id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        context_id: task
            .and_then(|task| task.get("contextId"))
            .or_else(|| message.and_then(|message| message.get("contextId")))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        message: response_text(result),
        output: response_output(result),
        settlement: request.settlement.clone(),
        payment_receipt: None,
        service_signature,
        raw,
    })
}

fn response_text(result: &Value) -> Option<String> {
    let parts = result.pointer("/message/parts").or_else(|| {
        result
            .pointer("/task/messages")
            .and_then(Value::as_array)
            .and_then(|messages| messages.last())
            .and_then(|message| message.get("parts"))
    });
    parts
        .and_then(Value::as_array)
        .and_then(|parts| {
            parts
                .iter()
                .find_map(|part| part.get("text").and_then(Value::as_str))
        })
        .map(ToOwned::to_owned)
}

fn response_output(result: &Value) -> Option<Value> {
    result
        .pointer("/task/artifacts/0/parts/0")
        .and_then(|part| {
            part.get("data")
                .cloned()
                .or_else(|| part.get("text").cloned())
        })
        .or_else(|| result.get("message").cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, routing::post};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    use crate::servicenet::SERVICE_RESPONSE_SIGNATURE_PROTOCOL;

    #[derive(Clone)]
    struct SignedAdapterState {
        signing_key: Arc<SigningKey>,
        service_did: String,
    }

    async fn signed_adapter_response(
        State(state): State<SignedAdapterState>,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        let result = json!({
            "message": {
                "role": "agent",
                "contextId": "signed-direct-context",
                "parts": [{"kind": "text", "text": "signed direct response"}]
            }
        });
        let request_digest = request
            .pointer("/params/extensions/agent_envelope/extensions/request_digest")
            .and_then(Value::as_str)
            .expect("request digest");
        let request_nonce = request
            .pointer("/params/extensions/agent_envelope/extensions/nonce")
            .and_then(Value::as_str)
            .expect("request nonce");
        let public_key_multibase = state
            .service_did
            .strip_prefix("did:key:")
            .expect("did:key prefix");
        let verification_method = format!("{}#{public_key_multibase}", state.service_did);
        let result_digest = format!(
            "sha256:{:x}",
            Sha256::digest(serde_jcs::to_vec(&result).expect("result should canonicalize"))
        );
        let issued_at_ms = chrono::Utc::now().timestamp_millis().max(0).cast_unsigned();
        let payload = json!({
            "protocol": SERVICE_RESPONSE_SIGNATURE_PROTOCOL,
            "service_did": state.service_did,
            "agent_id": "direct-agent",
            "verification_method": verification_method,
            "request_digest": request_digest,
            "request_nonce": request_nonce,
            "result_digest": result_digest,
            "nonce": "signed-direct-response-nonce",
            "issued_at_ms": issued_at_ms,
        });
        let signature = STANDARD.encode(
            state
                .signing_key
                .sign(&serde_jcs::to_vec(&payload).expect("payload should canonicalize"))
                .to_bytes(),
        );
        Json(json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": result,
            "extensions": {
                "service_agent_signature": {
                    "protocol": SERVICE_RESPONSE_SIGNATURE_PROTOCOL,
                    "service_did": state.service_did,
                    "agent_id": "direct-agent",
                    "verification_method": verification_method,
                    "request_digest": request_digest,
                    "request_nonce": request_nonce,
                    "result_digest": result_digest,
                    "nonce": "signed-direct-response-nonce",
                    "issued_at_ms": issued_at_ms,
                    "signature": signature,
                }
            }
        }))
    }

    async fn capture_adapter_response(
        State(captured): State<Arc<Mutex<Option<Value>>>>,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        *captured.lock().await = Some(request.clone());
        Json(json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {
                "message": {
                    "role": "agent",
                    "contextId": "direct-context",
                    "parts": [{"kind": "text", "text": "direct response"}]
                }
            }
        }))
    }

    #[test]
    fn direct_params_keep_target_envelope_and_business_input() {
        let request = ServiceNetInvokeRequest {
            message: Some("book a ride".to_owned()),
            input: json!({"pickup": "airport"}),
            skill_id: Some("rides.book".to_owned()),
            agent_envelope: Some(json!({"target_agent_id": "ride-agent"})),
            ..ServiceNetInvokeRequest::default()
        };

        let params = build_send_message_params(&request);

        assert_eq!(params["skillId"], "rides.book");
        assert_eq!(params["message"]["parts"][0]["text"], "book a ride");
        assert_eq!(params["message"]["parts"][1]["data"]["pickup"], "airport");
        assert_eq!(
            params["extensions"]["agent_envelope"]["target_agent_id"],
            "ride-agent"
        );
    }

    #[test]
    fn direct_params_normalize_x402_like_the_adapter() {
        let request = ServiceNetInvokeRequest {
            settlement: Some(
                serde_json::from_value(json!({
                    "layer": "web3",
                    "rail": "X402",
                    "request": null
                }))
                .unwrap(),
            ),
            ..ServiceNetInvokeRequest::default()
        };

        let params = build_send_message_params(&request);

        assert_eq!(params["extensions"]["settlement"]["rail"], "x402");
        assert_eq!(
            params["extensions"]["settlement"]["request"]["protocol"],
            "x402"
        );
    }

    #[test]
    fn connection_mode_selects_direct_transport() {
        assert!(uses_wattetheria_direct(&json!({
            "deployment": {"connection_mode": "wattetheria_direct"}
        })));
        assert!(!uses_wattetheria_direct(&json!({
            "deployment": {"connection_mode": "servicenet_relay"}
        })));
    }

    #[tokio::test]
    async fn direct_transport_posts_to_exact_published_adapter_url() {
        let captured = Arc::new(Mutex::new(None::<Value>));
        let app = Router::new()
            .route("/provider/custom-adapter", post(capture_adapter_response))
            .with_state(Arc::clone(&captured));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let address = listener.local_addr().expect("listener address");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock Adapter should run");
        });
        let client = ServiceNetClient::new("http://127.0.0.1:1").expect("client should build");
        let request = ServiceNetInvokeRequest {
            message: Some("hello direct".to_owned()),
            agent_envelope: Some(json!({"target_agent_id": "direct-agent"})),
            ..ServiceNetInvokeRequest::default()
        };
        let published = json!({
            "invoke": {
                "direct_url": format!("http://{address}/provider/custom-adapter")
            }
        });

        let response = invoke_agent_inner(&client, "direct-agent", &request, &published)
            .await
            .expect("direct Adapter invocation should succeed");

        assert_eq!(response.message.as_deref(), Some("direct response"));
        let request = captured
            .lock()
            .await
            .clone()
            .expect("Adapter should receive request");
        assert_eq!(request["method"], "SendMessage");
        assert_eq!(
            request["params"]["extensions"]["agent_envelope"]["target_agent_id"],
            "direct-agent"
        );
    }

    #[tokio::test]
    async fn client_selects_direct_transport_and_verifies_adapter_signature() {
        let signing_key = Arc::new(SigningKey::from_bytes(&[71u8; 32]));
        let multicodec = [
            [0xed, 0x01].as_slice(),
            signing_key.verifying_key().as_bytes(),
        ]
        .concat();
        let service_did = format!("did:key:z{}", bs58::encode(multicodec).into_string());
        let adapter = Router::new()
            .route("/custom-adapter", post(signed_adapter_response))
            .with_state(SignedAdapterState {
                signing_key,
                service_did: service_did.clone(),
            });
        let adapter_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Adapter listener should bind");
        let adapter_address = adapter_listener.local_addr().expect("Adapter address");
        tokio::spawn(async move {
            axum::serve(adapter_listener, adapter)
                .await
                .expect("signed Adapter should run");
        });

        let published = json!({
            "agent_id": "direct-agent",
            "service_did": service_did,
            "deployment": {"connection_mode": "wattetheria_direct"},
            "invoke": {
                "direct_url": format!("http://{adapter_address}/custom-adapter")
            }
        });
        let registry = Router::new().route(
            "/v1/agents/direct-agent",
            axum::routing::get(move || {
                let published = published.clone();
                async move { Json(published) }
            }),
        );
        let registry_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("registry listener should bind");
        let registry_address = registry_listener.local_addr().expect("registry address");
        tokio::spawn(async move {
            axum::serve(registry_listener, registry)
                .await
                .expect("mock registry should run");
        });

        let request_digest = "sha256:direct-request";
        let request_nonce = "direct-request-nonce";
        let request = ServiceNetInvokeRequest {
            message: Some("hello signed direct".to_owned()),
            agent_envelope: Some(json!({
                "target_agent_id": "direct-agent",
                "extensions": {
                    "request_digest": request_digest,
                    "nonce": request_nonce
                }
            })),
            ..ServiceNetInvokeRequest::default()
        };
        let client = ServiceNetClient::new(format!("http://{registry_address}"))
            .expect("client should build");

        let response = client
            .invoke_agent("direct-agent", &request)
            .await
            .expect("signed direct invocation should verify");

        assert_eq!(response.message.as_deref(), Some("signed direct response"));
        assert_eq!(
            response
                .service_signature
                .as_ref()
                .and_then(|signature| signature.request_nonce.as_deref()),
            Some(request_nonce)
        );
        assert!(response.receipt_id.is_none());
    }
}
