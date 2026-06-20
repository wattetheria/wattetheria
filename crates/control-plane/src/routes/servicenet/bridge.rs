use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::state::ControlPlaneState;
use wattetheria_kernel::brain::RuntimeSessionContext;
use wattetheria_kernel::signing::verify_payload;
use wattetheria_kernel::swarm_bridge::{SwarmAgentEnvelope, SwarmSourceAgentCard};

const DEFAULT_SERVICENET_NETWORK_ID: &str = "mainnet:watt-etheria";

#[derive(Debug, Serialize)]
struct SignedAgentEnvelopePayload<'a> {
    protocol: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport_profile: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_agent_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_node_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_node_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capability: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent_card_hash: Option<&'a String>,
    message_json: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions_json: Option<&'a String>,
}

#[derive(Debug, Serialize)]
struct SignedSourceAgentCardPayload<'a> {
    agent_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<&'a String>,
    card_hash: &'a str,
    issued_at: u64,
}

pub(crate) async fn a2a_root(
    State(state): State<ControlPlaneState>,
    Json(body): Json<Value>,
) -> Response {
    handle_a2a(state, None, body).await
}

pub(crate) async fn a2a_agent(
    State(state): State<ControlPlaneState>,
    Path(agent_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    handle_a2a(state, Some(agent_id), body).await
}

async fn handle_a2a(
    state: ControlPlaneState,
    path_agent_id: Option<String>,
    body: Value,
) -> Response {
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "SendMessage" | "message/send" => send_message(state, path_agent_id, id, &body).await,
        "GetTask" | "tasks/get" => jsonrpc_error(
            &id,
            -32601,
            "GetTask is not supported by the Wattetheria ServiceNet bridge",
        ),
        _ => jsonrpc_error(&id, -32601, "unsupported A2A method"),
    }
}

async fn send_message(
    state: ControlPlaneState,
    path_agent_id: Option<String>,
    id: Value,
    body: &Value,
) -> Response {
    let params = value_at(body, &["params"]).unwrap_or(&Value::Null);
    let message = extract_message_text(params);
    if message.trim().is_empty() {
        return jsonrpc_error(&id, -32602, "A2A message text is required");
    }
    let envelope = match value_at(params, &["extensions", "agent_envelope"]) {
        Some(value) => match verified_agent_envelope(value) {
            Ok(envelope) => envelope,
            Err(message) => return jsonrpc_error(&id, -32602, &message),
        },
        None => return jsonrpc_error(&id, -32602, "A2A agent_envelope is required"),
    };
    if let Some(path_agent_id) = path_agent_id.as_deref()
        && let Some(target_agent_id) = envelope.target_agent_id.as_deref()
        && path_agent_id != target_agent_id
    {
        return jsonrpc_error(
            &id,
            -32602,
            "A2A path agent_id does not match signed agent_envelope.target_agent_id",
        );
    }
    let published_agent_id = path_agent_id
        .or_else(|| envelope.target_agent_id.clone())
        .unwrap_or_else(|| state.agent_did.clone());
    let context_id = string_at(params, &["contextId"]);
    let session_context = context_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .map(RuntimeSessionContext::precomputed)
        .or_else(|| service_session_from_envelope(&envelope, &published_agent_id));
    let prompt = bridge_prompt(params, &message, &envelope);
    let output = {
        let engine = state.brain_engine.read().await;
        match engine
            .generate_text_with_session(&prompt, session_context.as_ref())
            .await
        {
            Ok(output) => output,
            Err(error) => return jsonrpc_error(&id, -32000, &error.to_string()),
        }
    };
    let text = bridge_output_text(&output);
    let task_id = string_at(params, &["taskId"]).unwrap_or_else(|| Uuid::new_v4().to_string());
    let response_context_id = context_id.or_else(|| {
        session_context
            .as_ref()
            .map(RuntimeSessionContext::session_id)
    });
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "task": {
                "id": task_id,
                "contextId": response_context_id,
                "status": {
                    "state": "TASK_STATE_COMPLETED"
                },
                "artifacts": [
                    {
                        "parts": [
                            {
                                "kind": "text",
                                "text": text
                            }
                        ]
                    }
                ]
            }
        }
    }))
    .into_response()
}

fn service_session_from_envelope(
    envelope: &SwarmAgentEnvelope,
    published_agent_id: &str,
) -> Option<RuntimeSessionContext> {
    let caller_agent_did = envelope.source_agent_id.clone()?;
    Some(RuntimeSessionContext::servicenet(
        caller_agent_did,
        published_agent_id.to_owned(),
        DEFAULT_SERVICENET_NETWORK_ID,
    ))
}

fn bridge_prompt(params: &Value, message: &str, envelope: &SwarmAgentEnvelope) -> String {
    let caller = envelope
        .source_agent_id
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let caller_public_id = envelope
        .extensions
        .as_ref()
        .and_then(|value| string_at(value, &["caller_public_id"]))
        .unwrap_or_else(|| "unknown".to_owned());
    let input = value_at(params, &["message", "parts"])
        .cloned()
        .unwrap_or(Value::Null);
    format!(
        "You are the published Wattetheria ServiceNet agent. Return strict JSON object {{\"message\":\"...\"}}. Caller agent DID: {caller}. Caller public id: {caller_public_id}. User message: {message}. A2A parts: {input}"
    )
}

fn verified_agent_envelope(value: &Value) -> Result<SwarmAgentEnvelope, String> {
    let envelope: SwarmAgentEnvelope = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid A2A agent_envelope: {error}"))?;
    let signature = envelope
        .signature
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "A2A agent_envelope.signature is required".to_owned())?;
    let source_agent_id = envelope
        .source_agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "A2A agent_envelope.source_agent_id is required".to_owned())?;
    envelope
        .source_node_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "A2A agent_envelope.source_node_id is required".to_owned())?;
    if let Some(card) = envelope.source_agent_card.as_ref() {
        verify_source_agent_card(card, source_agent_id)?;
    }
    let message_json = serde_json::to_string(&envelope.message)
        .map_err(|error| format!("invalid A2A agent_envelope.message: {error}"))?;
    let extensions_json = envelope
        .extensions
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| format!("invalid A2A agent_envelope.extensions: {error}"))?;
    let payload = SignedAgentEnvelopePayload {
        protocol: &envelope.protocol,
        transport_profile: envelope.transport_profile.as_ref(),
        source_agent_id: envelope.source_agent_id.as_ref(),
        target_agent_id: envelope.target_agent_id.as_ref(),
        source_node_id: envelope.source_node_id.as_ref(),
        target_node_id: envelope.target_node_id.as_ref(),
        capability: envelope.capability.as_ref(),
        source_agent_card_hash: envelope
            .source_agent_card
            .as_ref()
            .map(|card| &card.card_hash),
        message_json: &message_json,
        extensions_json: extensions_json.as_ref(),
    };
    let verified = verify_payload(&payload, signature, source_agent_id)
        .map_err(|error| format!("A2A agent_envelope signature verification failed: {error}"))?;
    if !verified {
        return Err("A2A agent_envelope signature verification failed".to_owned());
    }
    Ok(envelope)
}

fn verify_source_agent_card(
    card: &SwarmSourceAgentCard,
    source_agent_id: &str,
) -> Result<(), String> {
    if card.agent_id != source_agent_id {
        return Err("A2A source_agent_card.agent_id must match source_agent_id".to_owned());
    }
    let computed_hash = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(
            serde_jcs::to_string(&card.card)
                .map_err(|error| format!("canonicalize A2A source_agent_card failed: {error}"))?
                .as_bytes()
        ))
    );
    if card.card_hash != computed_hash {
        return Err("A2A source_agent_card.card_hash does not match card".to_owned());
    }
    let signature = card
        .signature
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "A2A source_agent_card.signature is required".to_owned())?;
    let payload = SignedSourceAgentCardPayload {
        agent_id: &card.agent_id,
        node_id: card.node_id.as_ref(),
        card_hash: &card.card_hash,
        issued_at: card.issued_at,
    };
    let verified = verify_payload(&payload, signature, source_agent_id)
        .map_err(|error| format!("A2A source_agent_card signature verification failed: {error}"))?;
    if !verified {
        return Err("A2A source_agent_card signature verification failed".to_owned());
    }
    Ok(())
}

fn extract_message_text(params: &Value) -> String {
    let Some(parts) = value_at(params, &["message", "parts"]).and_then(Value::as_array) else {
        return string_at(params, &["message", "text"]).unwrap_or_default();
    };
    parts
        .iter()
        .filter_map(|part| string_at(part, &["text"]))
        .collect::<Vec<_>>()
        .join("\n")
}

fn bridge_output_text(output: &str) -> String {
    serde_json::from_str::<Value>(output)
        .ok()
        .and_then(|value| {
            string_at(&value, &["message"])
                .or_else(|| string_at(&value, &["answer"]))
                .or_else(|| string_at(&value, &["response"]))
        })
        .unwrap_or_else(|| output.to_owned())
}

fn jsonrpc_error(id: &Value, code: i64, message: &str) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message,
            }
        })),
    )
        .into_response()
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    value_at(value, path)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(segment)?;
    }
    Some(current)
}
