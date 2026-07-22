use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use wattetheria_kernel::servicenet::SERVICENET_A2A_V1_PROTOCOL;

use crate::social_host::{
    SignedAgentEnvelopeArgs, build_signed_agent_envelope_for_nodes_with_protocol, public_agent_id,
    resolve_social_local_context,
};
use crate::state::ControlPlaneState;

const SERVICENET_INVOCATION_ENVELOPE_TTL_MS: u64 = 5 * 60 * 1000;

pub(crate) async fn servicenet_invoke_agent_envelope(
    state: &ControlPlaneState,
    agent_id: &str,
    body: &Value,
) -> Result<Value> {
    let source_node_id = state.swarm_bridge.local_node_id().await.ok();
    let local = resolve_social_local_context(state, None).await;
    let message = servicenet_invoke_envelope_message(body);
    let issued_at_ms = Utc::now().timestamp_millis().max(0).cast_unsigned();
    let expires_at_ms = issued_at_ms.saturating_add(SERVICENET_INVOCATION_ENVELOPE_TTL_MS);
    let envelope = build_signed_agent_envelope_for_nodes_with_protocol(
        state,
        SERVICENET_A2A_V1_PROTOCOL,
        SignedAgentEnvelopeArgs {
            source_agent_id: local.agent_id,
            source_public_id: public_agent_id(&local.public_id),
            source_display_name: local.display_name,
            target_agent_id: Some(agent_id.to_owned()),
            source_node_id,
            target_node_id: None,
            capability: "servicenet.agents.invoke".to_owned(),
            message: message.clone(),
            extensions: Some(json!({
                "caller_public_id": local.public_id,
                "nonce": Uuid::new_v4().to_string(),
                "issued_at_ms": issued_at_ms,
                "expires_at_ms": expires_at_ms,
                "request_digest": servicenet_invoke_request_digest(&message)?,
            })),
        },
    )?;
    Ok(serde_json::to_value(envelope)?)
}

pub(crate) async fn servicenet_task_agent_envelope(
    state: &ControlPlaneState,
    agent_id: &str,
    capability: &str,
    message: Value,
) -> Result<Value> {
    let source_node_id = state.swarm_bridge.local_node_id().await.ok();
    let local = resolve_social_local_context(state, None).await;
    let issued_at_ms = Utc::now().timestamp_millis().max(0).cast_unsigned();
    let expires_at_ms = issued_at_ms.saturating_add(SERVICENET_INVOCATION_ENVELOPE_TTL_MS);
    let envelope = build_signed_agent_envelope_for_nodes_with_protocol(
        state,
        SERVICENET_A2A_V1_PROTOCOL,
        SignedAgentEnvelopeArgs {
            source_agent_id: local.agent_id,
            source_public_id: public_agent_id(&local.public_id),
            source_display_name: local.display_name,
            target_agent_id: Some(agent_id.to_owned()),
            source_node_id,
            target_node_id: None,
            capability: capability.to_owned(),
            message: message.clone(),
            extensions: Some(json!({
                "caller_public_id": local.public_id,
                "nonce": Uuid::new_v4().to_string(),
                "issued_at_ms": issued_at_ms,
                "expires_at_ms": expires_at_ms,
                "request_digest": servicenet_invoke_request_digest(&message)?,
            })),
        },
    )?;
    Ok(serde_json::to_value(envelope)?)
}

pub(crate) fn normalize_servicenet_invoke_body(body: &mut Value, arguments: &Value) {
    let message = body
        .as_object()
        .and_then(servicenet_invoke_message_text_from_object)
        .or_else(|| {
            arguments
                .as_object()
                .and_then(servicenet_invoke_message_text_from_object)
        });
    let Some(message) = message else {
        return;
    };
    let Some(object) = body.as_object_mut() else {
        return;
    };
    object.insert("message".to_owned(), Value::String(message));
}

fn servicenet_invoke_envelope_message(body: &Value) -> Value {
    let mut message = body.clone();
    if let Some(object) = message.as_object_mut() {
        object.remove("auth_token");
        object.remove("auth_context_id");
        object.remove("agent_envelope");
    }
    message
}

fn servicenet_invoke_message_text_from_object(object: &Map<String, Value>) -> Option<String> {
    if let Some(message) = object
        .get("message")
        .and_then(Value::as_str)
        .and_then(non_empty_text)
    {
        return Some(message);
    }
    if let Some(message) = object
        .get("message")
        .and_then(servicenet_invoke_a2a_message_text)
    {
        return Some(message);
    }
    for key in ["text", "query", "prompt"] {
        if let Some(message) = object
            .get(key)
            .and_then(Value::as_str)
            .and_then(non_empty_text)
        {
            return Some(message);
        }
    }
    object
        .get("input")
        .and_then(servicenet_invoke_message_text_from_value)
}

fn servicenet_invoke_message_text_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(message) => non_empty_text(message),
        Value::Object(object) => servicenet_invoke_message_text_from_object(object)
            .or_else(|| servicenet_invoke_a2a_message_text(value)),
        _ => None,
    }
}

fn servicenet_invoke_a2a_message_text(value: &Value) -> Option<String> {
    value
        .get("parts")
        .and_then(Value::as_array)
        .and_then(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .find_map(non_empty_text)
        })
}

fn non_empty_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn servicenet_invoke_request_digest(message: &Value) -> Result<String> {
    let bytes = serde_jcs::to_vec(message)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}
