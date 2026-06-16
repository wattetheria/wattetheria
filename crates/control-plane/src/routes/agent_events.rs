use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;
use watt_did::{Did, PaymentAccountBindingProof, VerifiedAgentContext};
use watt_wallet::verify_payment_account_binding_proof;
use wattetheria_kernel::brain::AgentEventResolution;
use wattetheria_kernel::civilization::missions::NetworkMissionClaimRegistry;
use wattetheria_kernel::local_db;
use wattetheria_kernel::payments::{
    PaymentAgentMessage, PaymentMessageKind, PaymentStatus, PaymentTransaction,
    source_payment_account_binding_required,
};
use wattetheria_kernel::signing::verify_payload;
use wattetheria_kernel::swarm_bridge::SwarmAgentEnvelope;
use wattetheria_social::application::friendship_service;
use wattetheria_social::domain::deferred_agent_events::DeferredAgentEvent;
use wattetheria_social::domain::friendships::FriendshipState;

pub(crate) const VERIFIED_AGENT_CONTEXT_PAYLOAD_KEY: &str = "__verified_agent_context";

use crate::diagnostics::{DiagnosticEvent, record_diagnostic};
use crate::state::{
    AgentActionCommitBody, AgentActionCommitEvent, AgentActionDecision, ControlPlaneState,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct AgentEventCallbackRequest {
    pub event: AgentEventEnvelope,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct AgentEventEnvelope {
    pub event_id: String,
    pub event_type: String,
    pub source_kind: String,
    #[serde(default)]
    pub source_node_id: Option<String>,
    #[serde(default)]
    pub target_agent_id: Option<String>,
    #[serde(default)]
    pub target_executor: Option<String>,
    #[serde(default)]
    pub agent_envelope: Option<SwarmAgentEnvelope>,
    pub payload: Value,
    #[serde(default)]
    pub requires_commit: bool,
    #[serde(default)]
    pub allowed_actions: Vec<String>,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    pub created_at: u64,
}

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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct AgentEventCallbackResponse {
    pub ok: bool,
    #[serde(default)]
    pub acked_at: Option<u64>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub decision: Option<AgentDecisionEnvelope>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct AgentDecisionEnvelope {
    pub decision_id: String,
    pub action: String,
    pub route: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

fn map_route(event_type: &str, action: &str) -> Option<&'static str> {
    if routes_to_wattetheria_commit(event_type, action) {
        return Some("wattetheria_commit");
    }

    if routes_to_noop(event_type, action) {
        return Some("noop");
    }

    None
}

fn routes_to_wattetheria_commit(event_type: &str, action: &str) -> bool {
    match event_type {
        "friend_request" => matches!(action, "accept" | "reject" | "block"),
        "dm_received" => matches!(action, "reply" | "block" | "ignore"),
        "payment_request" | "payment_update" => {
            matches!(
                action,
                "authorize" | "reject" | "submit" | "settle" | "cancel"
            )
        }
        "third_party_result" => matches!(
            action,
            "publish_mission" | "claim_mission" | "complete_mission" | "settle_mission"
        ),
        "task_claim_received" => {
            matches!(action, "claim_mission" | "reject_claim" | "human_review")
        }
        "task_claim_decision_received" => matches!(action, "complete_mission"),
        "task_result_received" => matches!(
            action,
            "complete_mission"
                | "settle_mission"
                | "reject_result"
                | "request_retry"
                | "human_review"
        ),
        "topic_message_requires_reply" => {
            matches!(action, "reply" | "complete_mission" | "settle_mission")
        }
        _ => false,
    }
}

fn routes_to_noop(event_type: &str, action: &str) -> bool {
    match event_type {
        "topic_message_requires_reply"
        | "task_claim_decision_received"
        | "task_completion_decision_received"
        | "task_settled_received" => action == "ignore",
        "third_party_result" => matches!(action, "human_review" | "continue"),
        _ => false,
    }
}

fn build_brain_event_input(state: &ControlPlaneState, event: &AgentEventEnvelope) -> Value {
    json!({
        "agent_did": state.agent_did,
        "event_id": event.event_id,
        "event_type": event.event_type,
        "source_kind": event.source_kind,
        "source_node_id": event.source_node_id,
        "target_agent_id": event.target_agent_id,
        "target_executor": event.target_executor,
        "requires_commit": event.requires_commit,
        "allowed_actions": event.allowed_actions,
        "correlation_id": event.correlation_id,
        "dedupe_key": event.dedupe_key,
        "created_at": event.created_at,
        "agent_envelope": event.agent_envelope,
        "payload": sanitize_agent_event_payload_for_brain(&event.payload),
    })
}

fn sanitize_agent_event_payload_for_brain(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut sanitized = serde_json::Map::with_capacity(object.len());
            for (key, child) in object {
                if key == "agent_envelope" {
                    continue;
                }
                sanitized.insert(key.clone(), sanitize_agent_event_payload_for_brain(child));
            }
            Value::Object(sanitized)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(sanitize_agent_event_payload_for_brain)
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn is_mission_event(event: &AgentEventEnvelope) -> bool {
    event
        .payload
        .pointer("/task_inputs/kind")
        .and_then(Value::as_str)
        == Some("wattetheria_mission")
        || event
            .payload
            .pointer("/candidate_output/kind")
            .and_then(Value::as_str)
            == Some("wattetheria_mission_result")
        || event
            .payload
            .pointer("/output/kind")
            .and_then(Value::as_str)
            == Some("mission_completed")
        || event
            .payload
            .pointer("/mission_id")
            .and_then(Value::as_str)
            .is_some()
        || event
            .payload
            .pointer("/content/mission_id")
            .and_then(Value::as_str)
            .is_some()
        || event
            .payload
            .pointer("/topic_content/mission_id")
            .and_then(Value::as_str)
            .is_some()
        || matches!(
            topic_lifecycle_kind(event),
            Some("mission_claim_approved" | "mission_completed" | "mission_settled")
        )
}

fn topic_lifecycle_kind(event: &AgentEventEnvelope) -> Option<&str> {
    mission_lifecycle_value_from_event(event, "kind").and_then(Value::as_str)
}

fn mission_lifecycle_kind(event: &AgentEventEnvelope) -> Option<&str> {
    match event.event_type.as_str() {
        "topic_message_requires_reply" => topic_lifecycle_kind(event),
        "task_claim_decision_received"
            if mission_lifecycle_value_from_event(event, "approved").and_then(Value::as_bool)
                == Some(true) =>
        {
            Some("mission_claim_approved")
        }
        "task_completion_decision_received"
            if mission_lifecycle_value_from_event(event, "approved").and_then(Value::as_bool)
                == Some(true)
                && mission_lifecycle_value_from_event(event, "retry_requested")
                    .and_then(Value::as_bool)
                    != Some(true) =>
        {
            Some("mission_completed")
        }
        "task_settled_received" => Some("mission_settled"),
        _ => None,
    }
}

fn push_allowed_action(event: &mut AgentEventEnvelope, action: &str) {
    if !event
        .allowed_actions
        .iter()
        .any(|allowed| allowed == action)
    {
        event.allowed_actions.push(action.to_owned());
    }
}

fn set_allowed_actions(event: &mut AgentEventEnvelope, actions: &[&str]) {
    event.allowed_actions = actions.iter().map(|action| (*action).to_owned()).collect();
}

fn add_mission_allowed_actions(event: &mut AgentEventEnvelope) {
    if !is_mission_event(event) {
        return;
    }
    match event.event_type.as_str() {
        "task_claim_received" => {
            set_allowed_actions(event, &["decide_claim", "reject_claim", "human_review"]);
        }
        "task_claim_decision_received" => {
            if event
                .payload
                .get("approved")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                set_allowed_actions(event, &["complete_mission", "ignore"]);
            } else {
                set_allowed_actions(event, &["ignore", "human_review"]);
            }
        }
        "task_result_received" => {
            push_allowed_action(event, "complete_mission");
            push_allowed_action(event, "settle_mission");
        }
        "task_completion_decision_received" | "task_settled_received" => {
            set_allowed_actions(event, &["ignore"]);
        }
        "topic_message_requires_reply" => match topic_lifecycle_kind(event) {
            Some("mission_claim_approved") => {
                set_allowed_actions(event, &["complete_mission", "ignore"]);
            }
            Some("mission_completed") => {
                set_allowed_actions(event, &["settle_mission", "ignore"]);
            }
            Some("mission_settled") => {
                set_allowed_actions(event, &["ignore"]);
            }
            _ => {}
        },
        _ => {}
    }
}

fn mission_id_from_event(event: &AgentEventEnvelope) -> Option<String> {
    [
        "/mission_id",
        "/task_id",
        "/content/mission_id",
        "/content/task_id",
        "/topic_content/mission_id",
        "/topic_content/task_id",
        "/task_inputs/mission_id",
        "/candidate_output/mission_id",
        "/output/mission_id",
    ]
    .into_iter()
    .find_map(|path| {
        event
            .payload
            .pointer(path)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
    })
}

fn agent_did_from_event(event: &AgentEventEnvelope) -> Option<String> {
    let paths: &[&str] = match event.event_type.as_str() {
        "task_claim_received" => &["/claimer_agent_did", "/claimer_node_id", "/agent_did"],
        "task_claim_decision_received" => &[
            "/task_inputs/agent_did",
            "/task_inputs/claimer_agent_did",
            "/claimer_agent_did",
            "/claimer_node_id",
            "/agent_did",
        ],
        "task_result_received" => &[
            "/output/agent_did",
            "/output/claimer_agent_did",
            "/candidate_output/agent_did",
            "/agent_did",
            "/claimer_agent_did",
            "/claimer_node_id",
        ],
        "task_completion_decision_received" | "task_settled_received" => &[
            "/task_inputs/agent_did",
            "/task_inputs/claimer_agent_did",
            "/agent_did",
        ],
        "topic_message_requires_reply" => &[
            "/content/claimer_agent_did",
            "/content/agent_did",
            "/topic_content/claimer_agent_did",
            "/topic_content/agent_did",
        ],
        _ => &["/agent_did"],
    };
    paths
        .iter()
        .find_map(|path| {
            event
                .payload
                .pointer(path)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
        })
        .or_else(|| event.target_agent_id.clone())
        .or_else(|| event.source_node_id.clone())
}

fn payload_bool(payload: &Value, key: &str) -> Option<bool> {
    payload
        .get(key)
        .and_then(Value::as_bool)
        .or_else(|| payload.pointer(&format!("/{key}")).and_then(Value::as_bool))
}

fn payload_value_from_event_paths(event: &AgentEventEnvelope, paths: &[&str]) -> Option<Value> {
    paths
        .iter()
        .find_map(|path| event.payload.pointer(path).cloned())
}

fn payload_string_from_event_paths(event: &AgentEventEnvelope, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        event
            .payload
            .pointer(path)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn mission_lifecycle_value_from_event<'a>(
    event: &'a AgentEventEnvelope,
    key: &str,
) -> Option<&'a Value> {
    event
        .payload
        .pointer(&format!("/content/{key}"))
        .or_else(|| event.payload.pointer(&format!("/topic_content/{key}")))
        .or_else(|| event.payload.pointer(&format!("/output/{key}")))
        .or_else(|| event.payload.pointer(&format!("/task_inputs/{key}")))
        .or_else(|| {
            event
                .agent_envelope
                .as_ref()
                .and_then(|envelope| envelope.message.get(key))
        })
        .or_else(|| {
            event
                .payload
                .pointer(&format!("/agent_envelope/message/{key}"))
        })
        .or_else(|| event.payload.get(key))
}

fn mission_lifecycle_string_from_event(event: &AgentEventEnvelope, key: &str) -> Option<String> {
    mission_lifecycle_value_from_event(event, key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn payment_event_bad_request(message: impl Into<String>) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
        .into_response()
}

fn payment_value_from_event(event: &AgentEventEnvelope) -> Option<&Value> {
    event
        .agent_envelope
        .as_ref()
        .and_then(|envelope| envelope.message.get("payment"))
        .or_else(|| event.payload.get("payment"))
        .or_else(|| event.payload.pointer("/agent_envelope/message/payment"))
}

fn payment_message_value_from_event<'a>(
    event: &'a AgentEventEnvelope,
    key: &str,
) -> Option<&'a Value> {
    event
        .agent_envelope
        .as_ref()
        .and_then(|envelope| envelope.message.get(key))
        .or_else(|| {
            event
                .payload
                .pointer(&format!("/agent_envelope/message/{key}"))
        })
        .or_else(|| event.payload.get(key))
}

fn payment_envelope_field_from_event<'a>(
    event: &'a AgentEventEnvelope,
    key: &str,
) -> Option<&'a str> {
    let top_level = event
        .agent_envelope
        .as_ref()
        .and_then(|envelope| match key {
            "source_agent_id" => envelope.source_agent_id.as_deref(),
            "target_agent_id" => envelope.target_agent_id.as_deref(),
            "source_node_id" => envelope.source_node_id.as_deref(),
            _ => None,
        });
    top_level.or_else(|| {
        event
            .payload
            .pointer(&format!("/agent_envelope/{key}"))
            .and_then(Value::as_str)
    })
}

fn payment_message_kind_from_str(value: &str) -> Option<PaymentMessageKind> {
    match value {
        "payment_request" => Some(PaymentMessageKind::Request),
        "payment_authorized" => Some(PaymentMessageKind::Authorized),
        "payment_submitted" => Some(PaymentMessageKind::Submitted),
        "payment_settled" => Some(PaymentMessageKind::Settled),
        "payment_rejected" => Some(PaymentMessageKind::Rejected),
        "payment_cancelled" => Some(PaymentMessageKind::Cancelled),
        _ => None,
    }
}

fn payment_message_kind_from_status(status: &PaymentStatus) -> Option<PaymentMessageKind> {
    match status {
        PaymentStatus::Proposed => Some(PaymentMessageKind::Request),
        PaymentStatus::Authorized => Some(PaymentMessageKind::Authorized),
        PaymentStatus::Submitted => Some(PaymentMessageKind::Submitted),
        PaymentStatus::Settled => Some(PaymentMessageKind::Settled),
        PaymentStatus::Rejected => Some(PaymentMessageKind::Rejected),
        PaymentStatus::Cancelled => Some(PaymentMessageKind::Cancelled),
        PaymentStatus::Expired => None,
    }
}

fn payment_message_kind_from_event(
    event: &AgentEventEnvelope,
    payment: &PaymentTransaction,
) -> Option<PaymentMessageKind> {
    payment_message_value_from_event(event, "message_kind")
        .and_then(Value::as_str)
        .and_then(payment_message_kind_from_str)
        .or_else(|| {
            if event.event_type == "payment_request" {
                Some(PaymentMessageKind::Request)
            } else {
                payment_message_kind_from_status(&payment.status)
            }
        })
}

fn payment_event_source_agent_did(event: &AgentEventEnvelope) -> Option<String> {
    payment_envelope_field_from_event(event, "source_agent_id")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn payment_event_target_agent_did(event: &AgentEventEnvelope) -> Option<String> {
    payment_envelope_field_from_event(event, "target_agent_id")
        .or(event.target_agent_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn looks_like_full_payment_payload(value: &Value) -> bool {
    value.get("sender_did").is_some()
        || value.get("recipient_did").is_some()
        || value.get("status").is_some()
}

fn payment_event_source_node_id(event: &AgentEventEnvelope) -> Option<String> {
    payment_envelope_field_from_event(event, "source_node_id")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[allow(clippy::result_large_err)]
fn extract_verified_agent_context(
    event: &AgentEventEnvelope,
) -> Result<Option<VerifiedAgentContext>, Response> {
    let Some(value) = event.payload.get(VERIFIED_AGENT_CONTEXT_PAYLOAD_KEY) else {
        return Ok(None);
    };
    serde_json::from_value::<VerifiedAgentContext>(value.clone())
        .map(Some)
        .map_err(|error| {
            payment_event_bad_request(format!(
                "invalid {VERIFIED_AGENT_CONTEXT_PAYLOAD_KEY} payload: {error}"
            ))
        })
}

fn remote_event_requires_signed_agent_envelope(event: &AgentEventEnvelope) -> bool {
    matches!(
        event.event_type.as_str(),
        "payment_request"
            | "payment_update"
            | "friend_request"
            | "dm_received"
            | "task_claim_received"
            | "task_claim_decision_received"
            | "task_result_received"
            | "task_completion_decision_received"
            | "task_settled_received"
            | "topic_message_requires_reply"
    )
}

#[allow(clippy::result_large_err)]
fn verify_agent_event_signed_envelope(
    event: &AgentEventEnvelope,
) -> Result<Option<VerifiedAgentContext>, Response> {
    let Some(envelope) = event.agent_envelope.as_ref() else {
        return Ok(None);
    };
    let signature = envelope
        .signature
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| payment_event_bad_request("agent_envelope.signature is required"))?;
    let source_agent_id = envelope
        .source_agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| payment_event_bad_request("agent_envelope.source_agent_id is required"))?;
    let source_node_id = envelope
        .source_node_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| payment_event_bad_request("agent_envelope.source_node_id is required"))?;
    if let Some(event_source_node_id) = event.source_node_id.as_deref()
        && event_source_node_id != source_node_id
    {
        return Err(payment_event_bad_request(
            "event.source_node_id does not match signed agent_envelope.source_node_id",
        ));
    }
    if let Some(event_target_agent_id) = event.target_agent_id.as_deref()
        && let Some(envelope_target_agent_id) = envelope.target_agent_id.as_deref()
        && event_target_agent_id != envelope_target_agent_id
    {
        return Err(payment_event_bad_request(
            "event.target_agent_id does not match signed agent_envelope.target_agent_id",
        ));
    }
    verify_payload_agent_envelope_matches_signed(event, envelope)?;
    let message_json = serde_json::to_string(&envelope.message).map_err(|error| {
        payment_event_bad_request(format!("invalid agent_envelope.message: {error}"))
    })?;
    let extensions_json = envelope
        .extensions
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| {
            payment_event_bad_request(format!("invalid agent_envelope.extensions: {error}"))
        })?;
    let signed_payload = SignedAgentEnvelopePayload {
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
    let verified =
        verify_payload(&signed_payload, signature, source_agent_id).map_err(|error| {
            payment_event_bad_request(format!(
                "agent_envelope signature verification failed: {error}"
            ))
        })?;
    if !verified {
        return Err(payment_event_bad_request(
            "agent_envelope signature verification failed",
        ));
    }
    let context = VerifiedAgentContext {
        agent_did: Did::parse(source_agent_id).map_err(|error| {
            payment_event_bad_request(format!("invalid source agent DID: {error}"))
        })?,
        controller_node_id: source_node_id.to_owned(),
        source_node_id: Some(source_node_id.to_owned()),
        envelope_verified: true,
        source_node_verified: true,
        controller_binding_verified: false,
        controller_binding_proof: None,
        payment_account_binding: None,
        verified_at_ms: Utc::now().timestamp_millis().max(0).cast_unsigned(),
        expires_at_ms: None,
    };
    context
        .validate_basic()
        .map_err(|error| payment_event_bad_request(format!("invalid verified context: {error}")))?;
    Ok(Some(context))
}

#[allow(clippy::result_large_err)]
fn verify_payload_agent_envelope_matches_signed(
    event: &AgentEventEnvelope,
    signed_envelope: &SwarmAgentEnvelope,
) -> Result<(), Response> {
    let Some(payload_envelope_value) = event.payload.get("agent_envelope") else {
        return Ok(());
    };
    let payload_envelope =
        serde_json::from_value::<SwarmAgentEnvelope>(payload_envelope_value.clone()).map_err(
            |error| payment_event_bad_request(format!("invalid payload agent_envelope: {error}")),
        )?;
    if payload_envelope.source_agent_id != signed_envelope.source_agent_id
        || payload_envelope.target_agent_id != signed_envelope.target_agent_id
        || payload_envelope.source_node_id != signed_envelope.source_node_id
        || payload_envelope.target_node_id != signed_envelope.target_node_id
        || payload_envelope.capability != signed_envelope.capability
        || payload_envelope.message != signed_envelope.message
    {
        return Err(payment_event_bad_request(
            "payload agent_envelope does not match signed agent_envelope",
        ));
    }
    Ok(())
}

#[allow(clippy::result_large_err)]
fn verified_context_for_event(
    event: &AgentEventEnvelope,
) -> Result<Option<VerifiedAgentContext>, Response> {
    let signed_context = verify_agent_event_signed_envelope(event)?;
    let payload_context = extract_verified_agent_context(event)?;
    match (signed_context, payload_context) {
        (Some(context), Some(payload_context)) => {
            verify_context_against_payment_event(
                &payload_context,
                &context.agent_did.to_string(),
                context.source_node_id.as_deref(),
            )?;
            Ok(Some(context))
        }
        (Some(context), None) => Ok(Some(context)),
        (None, Some(_)) => Err(payment_event_bad_request(
            "signed agent_envelope is required for verified agent context",
        )),
        (None, None) => Ok(None),
    }
}

#[allow(clippy::result_large_err)]
fn verify_context_against_payment_event(
    context: &VerifiedAgentContext,
    source_agent_did: &str,
    envelope_source_node_id: Option<&str>,
) -> Result<(), Response> {
    if !context.envelope_verified || !context.source_node_verified {
        return Err(payment_event_bad_request(
            "verified agent context must have envelope and source node verified",
        ));
    }
    if context.agent_did.to_string() != source_agent_did {
        return Err(payment_event_bad_request(
            "verified agent context agent_did does not match agent_envelope.source_agent_id",
        ));
    }
    if let (Some(context_node), Some(envelope_node)) =
        (context.source_node_id.as_deref(), envelope_source_node_id)
        && context_node != envelope_node
    {
        return Err(payment_event_bad_request(
            "verified agent context source_node_id does not match agent_envelope.source_node_id",
        ));
    }
    Ok(())
}

#[allow(clippy::result_large_err)]
fn extract_payment_account_binding(
    event: &AgentEventEnvelope,
) -> Result<Option<PaymentAccountBindingProof>, Response> {
    let Some(value) = event
        .agent_envelope
        .as_ref()
        .and_then(|envelope| envelope.message.get("payment_account_binding"))
        .or_else(|| {
            event
                .payload
                .pointer("/agent_envelope/message/payment_account_binding")
        })
    else {
        return Ok(None);
    };
    serde_json::from_value::<PaymentAccountBindingProof>(value.clone())
        .map(Some)
        .map_err(|error| {
            payment_event_bad_request(format!("invalid payment_account_binding payload: {error}"))
        })
}

#[allow(clippy::result_large_err)]
fn verify_inbound_payment_account_binding(
    proof: &PaymentAccountBindingProof,
    source_agent_did: &str,
    payment: &PaymentTransaction,
    require_sender_binding: bool,
) -> Result<(), Response> {
    if proof.agent_did.to_string() != source_agent_did {
        return Err(payment_event_bad_request(
            "payment_account_binding agent_did does not match agent_envelope.source_agent_id",
        ));
    }
    verify_payment_account_binding_proof(proof)
        .map_err(|error| payment_event_bad_request(format!("payment_account_binding: {error}")))?;
    if !require_sender_binding {
        return Ok(());
    }
    if !proof.can_sign || proof.receive_only || proof.payment_account_proof.is_none() {
        return Err(payment_event_bad_request(
            "payment_account_binding must prove a signing payment account",
        ));
    }
    let Some(sender_address) = payment
        .sender_address
        .as_deref()
        .map(str::trim)
        .filter(|address| !address.is_empty())
    else {
        return Err(payment_event_bad_request(
            "payment sender_address is required when sender account binding is required",
        ));
    };
    if !proof.payment_address.eq_ignore_ascii_case(sender_address) {
        return Err(payment_event_bad_request(
            "payment_account_binding payment_address does not match payment.sender_address",
        ));
    }
    if !proof.rail.trim().eq_ignore_ascii_case(payment.rail.trim()) {
        return Err(payment_event_bad_request(
            "payment_account_binding rail does not match payment.rail",
        ));
    }
    if let Some(payment_network) = payment
        .network
        .as_deref()
        .map(str::trim)
        .filter(|network| !network.is_empty())
    {
        let proof_network = proof
            .network
            .as_deref()
            .map(str::trim)
            .filter(|network| !network.is_empty());
        if proof_network.is_none_or(|network| !network.eq_ignore_ascii_case(payment_network)) {
            return Err(payment_event_bad_request(
                "payment_account_binding network does not match payment.network",
            ));
        }
    }
    Ok(())
}

async fn sync_payment_event_to_ledger(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    verified_context: Option<&VerifiedAgentContext>,
) -> Result<(), Response> {
    if !matches!(
        event.event_type.as_str(),
        "payment_request" | "payment_update"
    ) {
        return Ok(());
    }
    let Some(payment_value) = payment_value_from_event(event) else {
        return Ok(());
    };
    let payment = match serde_json::from_value::<PaymentTransaction>(payment_value.clone()) {
        Ok(payment) => payment,
        Err(error) if looks_like_full_payment_payload(payment_value) => {
            return Err(payment_event_bad_request(format!(
                "invalid payment payload: {error}"
            )));
        }
        Err(_) => return Ok(()),
    };
    if let Some(target_agent_did) = payment_event_target_agent_did(event)
        && target_agent_did != state.agent_did
    {
        return Err(payment_event_bad_request(
            "payment event target_agent_id does not match local agent",
        ));
    }
    if payment.sender_did != state.agent_did && payment.recipient_did != state.agent_did {
        return Err(payment_event_bad_request(
            "payment event does not include the local agent as a participant",
        ));
    }
    let Some(kind) = payment_message_kind_from_event(event, &payment) else {
        return Err(payment_event_bad_request(
            "payment event has unsupported payment status",
        ));
    };
    let Some(source_agent_did) = payment_event_source_agent_did(event) else {
        return Err(payment_event_bad_request(
            "payment event missing agent_envelope.source_agent_id",
        ));
    };
    let Some(context) = verified_context else {
        return Err(payment_event_bad_request(
            "signed agent_envelope is required for payment event",
        ));
    };
    verify_context_against_payment_event(
        context,
        &source_agent_did,
        payment_event_source_node_id(event).as_deref(),
    )?;
    let require_sender_binding =
        source_payment_account_binding_required(&kind, &payment, &source_agent_did);
    if let Some(binding) = extract_payment_account_binding(event)? {
        verify_inbound_payment_account_binding(
            &binding,
            &source_agent_did,
            &payment,
            require_sender_binding,
        )?;
    } else if require_sender_binding {
        return Err(payment_event_bad_request(
            "payment_account_binding is required for sender-signed payment state",
        ));
    }
    let message = PaymentAgentMessage {
        kind,
        payment,
        emitted_at: event.created_at.try_into().unwrap_or(i64::MAX),
    };
    let mut ledger = state.payment_ledger.lock().await;
    ledger
        .merge_remote_agent_message(message, &source_agent_did)
        .map_err(|error| payment_event_bad_request(error.to_string()))?;
    state
        .local_db
        .save_domain(local_db::domain::PAYMENT_LEDGER, &*ledger)
        .map_err(|error| payment_event_bad_request(error.to_string()))?;
    Ok(())
}

fn mission_lifecycle_status(kind: &str) -> Option<&'static str> {
    match kind {
        "mission_claim_approved" => Some("claimed"),
        "mission_completed" => Some("completed"),
        "mission_settled" => Some("settled"),
        _ => None,
    }
}

async fn sync_mission_lifecycle_event_to_state(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    verified_context: Option<&VerifiedAgentContext>,
) -> Result<(), Response> {
    let Some(kind) = mission_lifecycle_kind(event) else {
        return Ok(());
    };
    let Some(status) = mission_lifecycle_status(kind) else {
        return Ok(());
    };
    if let Some(target_agent_id) = event.target_agent_id.as_deref()
        && target_agent_id != state.agent_did
    {
        return Err(payment_event_bad_request(
            "mission lifecycle event target_agent_id does not match local agent",
        ));
    }
    if verified_context.is_none() {
        return Err(payment_event_bad_request(
            "signed agent_envelope is required for mission lifecycle event",
        ));
    }
    let Some(mission_id) = mission_lifecycle_string_from_event(event, "mission_id") else {
        return Ok(());
    };
    let mut registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(local_db::domain::NETWORK_MISSION_CLAIMS)
        .map_err(|error| payment_event_bad_request(error.to_string()))?;
    let claim_exists = registry.contains_mission(&mission_id);
    let board_exists = {
        let board = state.mission_board.lock().await;
        board.get(&mission_id).is_some()
    };
    match (board_exists, claim_exists) {
        (true, true) => {
            record_agent_event_diagnostic(
                state,
                event,
                "warn",
                "mission_lifecycle.sync.skipped",
                "ambiguous_state_owner",
                "mission lifecycle sync skipped because mission exists in both local tables",
                &json!({
                    "mission_id": mission_id,
                    "kind": kind,
                    "status": status,
                }),
            );
            Ok(())
        }
        (true, false) => sync_mission_lifecycle_to_board(state, event, &mission_id, kind).await,
        (false, true) => {
            let updated = registry.update_status_by_mission(&mission_id, status);
            if updated.is_some() {
                state
                    .local_db
                    .save_domain(local_db::domain::NETWORK_MISSION_CLAIMS, &registry)
                    .map_err(|error| payment_event_bad_request(error.to_string()))?;
            }
            Ok(())
        }
        (false, false) => {
            record_agent_event_diagnostic(
                state,
                event,
                "info",
                "mission_lifecycle.sync.skipped",
                "unknown_mission",
                "mission lifecycle sync skipped because mission is not tracked locally",
                &json!({
                    "mission_id": mission_id,
                    "kind": kind,
                    "status": status,
                }),
            );
            Ok(())
        }
    }
}

async fn sync_mission_lifecycle_to_board(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    mission_id: &str,
    kind: &str,
) -> Result<(), Response> {
    let agent_did = mission_lifecycle_string_from_event(event, "claimer_agent_did")
        .or_else(|| mission_lifecycle_string_from_event(event, "agent_did"))
        .unwrap_or_default();
    let result = mission_lifecycle_value_from_event(event, "result").cloned();
    let updated = {
        let mut board = state.mission_board.lock().await;
        let updated = match kind {
            "mission_claim_approved" => board.apply_remote_claim_approved(mission_id, &agent_did),
            "mission_completed" => board.apply_remote_completed(mission_id, &agent_did, result),
            "mission_settled" => {
                board.apply_remote_settled(mission_id, Some(agent_did.as_str()), result)
            }
            _ => None,
        };
        if updated.is_some() {
            state
                .local_db
                .save_domain(local_db::domain::MISSION_BOARD, &*board)
                .map_err(|error| payment_event_bad_request(error.to_string()))?;
        }
        updated
    };
    if updated.is_none() {
        record_agent_event_diagnostic(
            state,
            event,
            "info",
            "mission_lifecycle.sync.skipped",
            "unknown_mission",
            "mission lifecycle sync skipped because mission is not tracked in mission board",
            &json!({
                "mission_id": mission_id,
                "kind": kind,
            }),
        );
    }
    Ok(())
}

fn ensure_mission_payload_fields(
    event: &AgentEventEnvelope,
    resolution: &mut AgentEventResolution,
) {
    if !resolution.payload.is_object() {
        resolution.payload = json!({});
    }
    let Some(payload) = resolution.payload.as_object_mut() else {
        return;
    };
    if !payload.contains_key("mission_id")
        && let Some(mission_id) = mission_id_from_event(event)
    {
        payload.insert("mission_id".to_string(), Value::String(mission_id));
    }
    if !payload.contains_key("agent_did")
        && let Some(agent_did) = agent_did_from_event(event)
    {
        payload.insert("agent_did".to_string(), Value::String(agent_did));
    }
    for (field, paths) in [
        (
            "task_id",
            &[
                "/task_id",
                "/content/task_id",
                "/topic_content/task_id",
                "/task_inputs/task_id",
                "/output/task_id",
            ][..],
        ),
        (
            "mission_feed_key",
            &[
                "/mission_feed_key",
                "/content/mission_feed_key",
                "/topic_content/mission_feed_key",
                "/output/mission_feed_key",
                "/task_inputs/mission_feed_key",
            ][..],
        ),
        (
            "mission_scope_hint",
            &[
                "/mission_scope_hint",
                "/content/mission_scope_hint",
                "/topic_content/mission_scope_hint",
                "/output/mission_scope_hint",
                "/task_inputs/mission_scope_hint",
            ][..],
        ),
        (
            "publisher_wattswarm_node_id",
            &[
                "/publisher_wattswarm_node_id",
                "/content/publisher_wattswarm_node_id",
                "/topic_content/publisher_wattswarm_node_id",
                "/output/publisher_wattswarm_node_id",
                "/task_inputs/publisher_wattswarm_node_id",
            ][..],
        ),
        (
            "execution_id",
            &[
                "/execution_id",
                "/content/execution_id",
                "/topic_content/execution_id",
                "/output/execution_id",
            ][..],
        ),
        (
            "claimer_node_id",
            &[
                "/claimer_node_id",
                "/content/claimer_node_id",
                "/topic_content/claimer_node_id",
                "/output/claimer_node_id",
            ][..],
        ),
    ] {
        if !payload.contains_key(field)
            && let Some(value) = payload_string_from_event_paths(event, paths)
        {
            payload.insert(field.to_string(), Value::String(value));
        }
    }
    if !payload.contains_key("result")
        && let Some(result) = payload_value_from_event_paths(
            event,
            &[
                "/result",
                "/content/result",
                "/topic_content/result",
                "/output/result",
            ],
        )
    {
        payload.insert("result".to_string(), result);
    }
}

fn normalize_mission_resolution(
    event: &AgentEventEnvelope,
    mut resolution: AgentEventResolution,
) -> AgentEventResolution {
    if !is_mission_event(event) {
        return resolution;
    }
    match (event.event_type.as_str(), resolution.action.as_deref()) {
        ("task_claim_received", Some("decide_claim"))
            if payload_bool(&resolution.payload, "approved") == Some(true) =>
        {
            resolution.action = Some("claim_mission".to_string());
            ensure_mission_payload_fields(event, &mut resolution);
        }
        ("task_claim_received", Some("decide_claim"))
            if payload_bool(&resolution.payload, "approved") == Some(false) =>
        {
            resolution.action = Some("reject_claim".to_string());
            ensure_mission_payload_fields(event, &mut resolution);
        }
        ("task_result_received", Some("accept_result")) => {
            resolution.action = Some("settle_mission".to_string());
            ensure_mission_payload_fields(event, &mut resolution);
        }
        ("task_claim_decision_received", Some("complete_mission"))
        | ("task_claim_received", Some("claim_mission"))
        | ("task_result_received", Some("complete_mission" | "settle_mission")) => {
            ensure_mission_payload_fields(event, &mut resolution);
        }
        ("topic_message_requires_reply", Some("complete_mission"))
            if topic_lifecycle_kind(event) == Some("mission_claim_approved") =>
        {
            ensure_mission_payload_fields(event, &mut resolution);
        }
        ("topic_message_requires_reply", Some("settle_mission"))
            if topic_lifecycle_kind(event) == Some("mission_completed") =>
        {
            ensure_mission_payload_fields(event, &mut resolution);
        }
        _ => {}
    }
    resolution
}

fn internal_mission_action_allowed(
    event: &AgentEventEnvelope,
    original_action: Option<&str>,
    normalized_action: &str,
) -> bool {
    if !is_mission_event(event) {
        return false;
    }
    match (
        event.event_type.as_str(),
        original_action,
        normalized_action,
    ) {
        ("task_claim_received", Some("decide_claim"), "claim_mission")
        | ("task_result_received", Some("accept_result"), "settle_mission")
        | ("task_claim_decision_received", Some("complete_mission"), "complete_mission") => true,
        ("topic_message_requires_reply", Some("complete_mission"), "complete_mission") => {
            topic_lifecycle_kind(event) == Some("mission_claim_approved")
        }
        ("topic_message_requires_reply", Some("settle_mission"), "settle_mission") => {
            topic_lifecycle_kind(event) == Some("mission_completed")
        }
        _ => false,
    }
}

fn agent_event_object_id(event: &AgentEventEnvelope) -> Option<String> {
    [
        "task_id",
        "topic_id",
        "message_id",
        "mission_id",
        "payment_id",
    ]
    .into_iter()
    .find_map(|field| {
        event
            .payload
            .get(field)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn record_agent_event_diagnostic(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    level: &'static str,
    phase: &'static str,
    status: &'static str,
    message: impl Into<String>,
    details: &Value,
) {
    record_diagnostic(
        &state.data_dir,
        DiagnosticEvent::new(
            level,
            "wattetheria.control_plane",
            "agent_event",
            phase,
            status,
            message,
        )
        .event_id(Some(event.event_id.clone()))
        .correlation_id(event.correlation_id.clone())
        .source_node_id(event.source_node_id.clone())
        .object("agent_event", agent_event_object_id(event))
        .details(json!({
            "event_type": event.event_type,
            "source_kind": event.source_kind,
            "target_agent_id": event.target_agent_id,
            "target_executor": event.target_executor,
            "requires_commit": event.requires_commit,
            "allowed_actions": event.allowed_actions,
            "dedupe_key": event.dedupe_key,
            "payload": details,
        })),
    );
}

fn record_agent_callback_responded(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    response: &AgentEventCallbackResponse,
) {
    let detail = response
        .detail
        .as_deref()
        .unwrap_or("agent event callback response");
    record_agent_event_diagnostic(
        state,
        event,
        if response.ok { "info" } else { "warn" },
        "callback.responded",
        if response.ok { "ok" } else { "error" },
        format!("agent event callback responded: {}", event.event_type),
        &json!({
            "ok": response.ok,
            "detail": detail,
            "event_type": event.event_type,
            "requires_commit": event.requires_commit,
            "decision": response.decision.as_ref().map(|decision| json!({
                "decision_id": decision.decision_id,
                "action": decision.action,
                "route": decision.route,
                "reason": decision.reason,
                "payload": decision.payload,
            })),
            "callback_response": response,
        }),
    );
}

fn no_decision_response(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    acked_at: u64,
) -> Response {
    record_agent_event_diagnostic(
        state,
        event,
        "info",
        "decision.empty",
        "noop",
        format!("no decision for {}", event.event_type),
        &event.payload,
    );
    let detail = format!("no decision for {}", event.event_type);
    let response = AgentEventCallbackResponse {
        ok: true,
        acked_at: Some(acked_at),
        detail: Some(detail),
        decision: None,
    };
    record_agent_callback_responded(state, event, &response);
    Json(response).into_response()
}

fn record_agent_brain_response_diagnostic(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    diagnostics: &Value,
) {
    if diagnostics
        .as_object()
        .is_none_or(serde_json::Map::is_empty)
    {
        return;
    }
    record_agent_event_diagnostic(
        state,
        event,
        "info",
        "decision.brain_response",
        "observed",
        format!("agent event brain response observed: {}", event.event_type),
        diagnostics,
    );
}

#[derive(Debug, Clone)]
struct DeferredDmAgentEventContext {
    local_public: String,
    remote_public: String,
    remote_node: Option<String>,
    source_agent: Option<String>,
}

fn trimmed_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn topic_dm_agent_event_context(event: &AgentEventEnvelope) -> Option<DeferredDmAgentEventContext> {
    if event.event_type != "topic_message_requires_reply" {
        return None;
    }
    let envelope = event.agent_envelope.as_ref()?;
    let capability_is_dm = envelope.capability.as_deref() == Some("social.dm.send");
    let feed_is_dm = event.payload.get("feed_key").and_then(Value::as_str) == Some("wattswarm.dm");
    let content_is_dm = event
        .payload
        .pointer("/topic_content/kind")
        .and_then(Value::as_str)
        == Some("direct_message");
    if !capability_is_dm && !feed_is_dm && !content_is_dm {
        return None;
    }
    let local_public_id = trimmed_string(envelope.message.get("target_public_id"))?;
    let remote_public_id = trimmed_string(envelope.message.get("source_public_id"))?;
    if local_public_id == remote_public_id {
        return None;
    }
    Some(DeferredDmAgentEventContext {
        local_public: local_public_id,
        remote_public: remote_public_id,
        remote_node: envelope
            .source_node_id
            .clone()
            .or_else(|| event.source_node_id.clone()),
        source_agent: envelope.source_agent_id.clone(),
    })
}

fn has_active_friendship(
    state: &ControlPlaneState,
    local_public_id: &str,
    remote_public_id: &str,
) -> bool {
    friendship_service::list_friendships(&*state.social_store, local_public_id)
        .unwrap_or_default()
        .into_iter()
        .any(|friendship| {
            friendship.remote_public_id == remote_public_id
                && friendship.state == FriendshipState::Active
        })
}

fn deferred_dm_response(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    context: &DeferredDmAgentEventContext,
    acked_at: u64,
) -> Response {
    let now = Utc::now().timestamp_millis();
    let deferred = DeferredAgentEvent {
        event_id: event.event_id.clone(),
        local_public_id: context.local_public.clone(),
        remote_public_id: context.remote_public.clone(),
        remote_node_id: context.remote_node.clone(),
        source_agent_id: context.source_agent.clone(),
        status: "waiting_for_friendship".to_owned(),
        event_json: serde_json::to_value(event).unwrap_or(Value::Null),
        reason: Some("waiting_for_friendship".to_owned()),
        created_at: now,
        updated_at: now,
        replayed_at: None,
    };
    let response = match state.social_store.defer_agent_event(&deferred) {
        Ok(()) => AgentEventCallbackResponse {
            ok: true,
            acked_at: Some(acked_at),
            detail: Some("deferred until friendship is active".to_owned()),
            decision: None,
        },
        Err(error) => AgentEventCallbackResponse {
            ok: false,
            acked_at: Some(acked_at),
            detail: Some(format!("defer agent event failed: {error}")),
            decision: None,
        },
    };
    record_agent_event_diagnostic(
        state,
        event,
        if response.ok { "info" } else { "error" },
        "callback.deferred",
        if response.ok {
            "waiting_for_friendship"
        } else {
            "error"
        },
        format!("deferred DM agent event: {}", event.event_id),
        &json!({
            "local_public_id": context.local_public,
            "remote_public_id": context.remote_public,
            "remote_node_id": context.remote_node,
            "source_agent_id": context.source_agent,
            "callback_response": response,
        }),
    );
    record_agent_callback_responded(state, event, &response);
    Json(response).into_response()
}

fn defer_unfriended_dm_agent_event(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    acked_at: u64,
) -> Option<Response> {
    let context = topic_dm_agent_event_context(event)?;
    if has_active_friendship(state, &context.local_public, &context.remote_public) {
        return None;
    }
    Some(deferred_dm_response(state, event, &context, acked_at))
}

fn no_action_response(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    resolution: &AgentEventResolution,
    acked_at: u64,
) -> Response {
    let details = json!({
        "payload": event.payload,
        "reason": resolution.reason,
    });
    record_agent_event_diagnostic(
        state,
        event,
        "info",
        "decision.no_action",
        "noop",
        format!("no action selected for {}", event.event_type),
        &details,
    );
    let detail = format!("no action selected for {}", event.event_type);
    let response = AgentEventCallbackResponse {
        ok: true,
        acked_at: Some(acked_at),
        detail: Some(detail),
        decision: None,
    };
    record_agent_callback_responded(state, event, &response);
    Json(response).into_response()
}

fn unsupported_route_response(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    action: &str,
    resolution: &AgentEventResolution,
    acked_at: u64,
) -> Response {
    let details = json!({
        "action": action,
        "reason": resolution.reason,
        "payload": event.payload,
    });
    record_agent_event_diagnostic(
        state,
        event,
        "warn",
        "decision.route",
        "unsupported",
        format!("unsupported action {action} for {}", event.event_type),
        &details,
    );
    let detail = format!(
        "unsupported action {action} for event_type {}",
        event.event_type
    );
    let response = AgentEventCallbackResponse {
        ok: false,
        acked_at: Some(acked_at),
        detail: Some(detail),
        decision: None,
    };
    record_agent_callback_responded(state, event, &response);
    Json(response).into_response()
}

fn disallowed_action_response(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    action: &str,
    resolution: &AgentEventResolution,
    acked_at: u64,
) -> Response {
    let details = json!({
        "action": action,
        "allowed_actions": event.allowed_actions,
        "reason": resolution.reason,
        "payload": event.payload,
    });
    record_agent_event_diagnostic(
        state,
        event,
        "warn",
        "decision.policy",
        "rejected",
        format!("action {action} not allowed for {}", event.event_type),
        &details,
    );
    let detail = format!(
        "action {action} not in allowed_actions for {}",
        event.event_type
    );
    let response = AgentEventCallbackResponse {
        ok: false,
        acked_at: Some(acked_at),
        detail: Some(detail),
        decision: None,
    };
    record_agent_callback_responded(state, event, &response);
    Json(response).into_response()
}

async fn commit_wattetheria_decision(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    decision: &AgentDecisionEnvelope,
) -> axum::http::StatusCode {
    let mut headers = HeaderMap::new();
    let auth_value = format!("Bearer {}", state.auth_token);
    headers.insert(
        axum::http::header::AUTHORIZATION,
        HeaderValue::from_str(&auth_value).expect("valid bearer token header"),
    );
    let response = Box::pin(crate::routes::core::agent_action_commit(
        State(state.clone()),
        headers,
        Json(AgentActionCommitBody {
            event: AgentActionCommitEvent {
                event_id: event.event_id.clone(),
                event_type: event.event_type.clone(),
                source_kind: event.source_kind.clone(),
                source_node_id: event.source_node_id.clone(),
                target_agent_id: event.target_agent_id.clone(),
                agent_envelope: event.agent_envelope.clone(),
                payload: event.payload.clone(),
                requires_commit: event.requires_commit,
            },
            decision: AgentActionDecision {
                decision_id: decision.decision_id.clone(),
                action: decision.action.clone(),
                route: decision.route.clone(),
                reason: decision.reason.clone(),
                payload: decision.payload.clone(),
            },
        }),
    ))
    .await;
    response.status()
}

async fn selected_action_response(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    action: &str,
    route: &str,
    resolution: &AgentEventResolution,
    acked_at: u64,
) -> Response {
    let details = json!({
        "action": action,
        "route": route,
        "reason": resolution.reason,
        "payload": resolution.payload,
    });
    record_agent_event_diagnostic(
        state,
        event,
        "info",
        "decision.selected",
        "ok",
        format!("selected {action} for {}", event.event_type),
        &details,
    );
    let decision = AgentDecisionEnvelope {
        decision_id: Uuid::new_v4().to_string(),
        action: action.to_string(),
        route: route.to_owned(),
        reason: resolution.reason.clone(),
        payload: resolution.payload.clone(),
    };
    let commit_status = if route == "wattetheria_commit" {
        Some(commit_wattetheria_decision(state, event, &decision).await)
    } else {
        None
    };
    if let Some(status) = commit_status {
        record_agent_event_diagnostic(
            state,
            event,
            if status.is_success() { "info" } else { "error" },
            "decision.commit",
            if status.is_success() { "ok" } else { "error" },
            format!(
                "agent event decision commit {}: {} -> {}",
                if status.is_success() {
                    "completed"
                } else {
                    "failed"
                },
                event.event_type,
                action
            ),
            &json!({
                "decision_id": &decision.decision_id,
                "action": action,
                "route": route,
                "status_code": status.as_u16(),
            }),
        );
    }
    let detail = format!("selected {action} for {}", event.event_type);
    let response = AgentEventCallbackResponse {
        ok: true,
        acked_at: Some(acked_at),
        detail: Some(detail),
        decision: Some(decision),
    };
    record_agent_callback_responded(state, event, &response);
    Json(response).into_response()
}

async fn response_from_resolution(
    state: &ControlPlaneState,
    event: &AgentEventEnvelope,
    verified_context: Option<&VerifiedAgentContext>,
    resolution: Option<AgentEventResolution>,
    acked_at: u64,
) -> Response {
    let Some(mut resolution) = resolution else {
        return no_decision_response(state, event, acked_at);
    };
    let original_action = resolution.action.clone();
    resolution = normalize_mission_resolution(event, resolution);
    let Some(action) = resolution.action.clone() else {
        return no_action_response(state, event, &resolution, acked_at);
    };
    let Some(route) = map_route(&event.event_type, &action) else {
        return unsupported_route_response(state, event, &action, &resolution, acked_at);
    };
    if route == "wattetheria_commit"
        && remote_event_requires_signed_agent_envelope(event)
        && verified_context.is_none()
    {
        return payment_event_bad_request(
            "signed agent_envelope is required for remote commit event",
        );
    }
    if !event.allowed_actions.iter().any(|a| a == &action)
        && !internal_mission_action_allowed(event, original_action.as_deref(), &action)
    {
        return disallowed_action_response(state, event, &action, &resolution, acked_at);
    }
    selected_action_response(state, event, &action, route, &resolution, acked_at).await
}

async fn process_agent_event_decision(
    state: &ControlPlaneState,
    mut event: AgentEventEnvelope,
    verified_context: Option<&VerifiedAgentContext>,
    acked_at: u64,
) -> Response {
    let callback_request = json!({ "event": &event });
    add_mission_allowed_actions(&mut event);
    let input = build_brain_event_input(state, &event);
    record_agent_event_diagnostic(
        state,
        &event,
        "info",
        "callback.received",
        "accepted",
        format!("agent event callback received: {}", event.event_type),
        &json!({
            "callback_request": callback_request,
            "normalized_event": &event,
            "brain_input": &input,
        }),
    );
    let decision = match state
        .brain_engine
        .read()
        .await
        .decide_agent_event_with_diagnostics(&input)
        .await
    {
        Ok(decision) => decision,
        Err(error) => {
            let detail = format!("agent event decision failed: {error:#}");
            let response = AgentEventCallbackResponse {
                ok: false,
                acked_at: Some(acked_at),
                detail: Some(detail.clone()),
                decision: None,
            };
            record_agent_event_diagnostic(
                state,
                &event,
                "error",
                "decision.failed",
                "error",
                detail.clone(),
                &json!({
                    "error": detail,
                    "brain_input": input,
                    "callback_response": response,
                }),
            );
            record_agent_callback_responded(state, &event, &response);
            return Json(response).into_response();
        }
    };
    record_agent_brain_response_diagnostic(state, &event, &decision.diagnostics);
    response_from_resolution(
        state,
        &event,
        verified_context,
        decision.resolution,
        acked_at,
    )
    .await
}

pub(crate) async fn replay_deferred_dm_agent_events_for_friendship(
    state: &ControlPlaneState,
    local_public_id: &str,
    remote_public_id: &str,
) -> anyhow::Result<usize> {
    if !has_active_friendship(state, local_public_id, remote_public_id) {
        return Ok(0);
    }
    let deferred_events = state
        .social_store
        .list_waiting_deferred_agent_events(local_public_id, remote_public_id, 50)
        .map_err(anyhow::Error::msg)?;
    let mut replayed = 0usize;
    for deferred in deferred_events {
        let event: AgentEventEnvelope =
            serde_json::from_value(deferred.event_json).map_err(anyhow::Error::msg)?;
        let Ok(verified_context) = verified_context_for_event(&event) else {
            continue;
        };
        if remote_event_requires_signed_agent_envelope(&event) && verified_context.is_none() {
            continue;
        }
        if let Err(_response) =
            sync_payment_event_to_ledger(state, &event, verified_context.as_ref()).await
        {
            continue;
        }
        if let Err(_response) =
            sync_mission_lifecycle_event_to_state(state, &event, verified_context.as_ref()).await
        {
            continue;
        }
        let acked_at = Utc::now().timestamp_millis().max(0).cast_unsigned();
        let _ =
            process_agent_event_decision(state, event, verified_context.as_ref(), acked_at).await;
        state
            .social_store
            .mark_deferred_agent_event_replayed(&deferred.event_id, Utc::now().timestamp_millis())
            .map_err(anyhow::Error::msg)?;
        replayed += 1;
    }
    Ok(replayed)
}

pub(crate) async fn callback(
    State(state): State<ControlPlaneState>,
    Json(body): Json<AgentEventCallbackRequest>,
) -> Response {
    let acked_at = Utc::now().timestamp_millis().max(0).cast_unsigned();
    let event = body.event;
    let verified_context = match verified_context_for_event(&event) {
        Ok(context) => context,
        Err(response) => return response,
    };
    if remote_event_requires_signed_agent_envelope(&event) && verified_context.is_none() {
        return payment_event_bad_request(
            "signed agent_envelope is required for remote agent event",
        );
    }
    if let Err(response) =
        sync_payment_event_to_ledger(&state, &event, verified_context.as_ref()).await
    {
        return response;
    }
    if let Err(response) =
        sync_mission_lifecycle_event_to_state(&state, &event, verified_context.as_ref()).await
    {
        return response;
    }
    if let Some(response) = defer_unfriended_dm_agent_event(&state, &event, acked_at) {
        return response;
    }
    process_agent_event_decision(&state, event, verified_context.as_ref(), acked_at).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_event(event_type: &str, payload: Value) -> AgentEventEnvelope {
        AgentEventEnvelope {
            event_id: "evt-test".to_owned(),
            event_type: event_type.to_owned(),
            source_kind: "task_lifecycle".to_owned(),
            source_node_id: Some("node-a".to_owned()),
            target_agent_id: Some("agent-target".to_owned()),
            target_executor: None,
            agent_envelope: None,
            payload,
            requires_commit: false,
            allowed_actions: Vec::new(),
            correlation_id: None,
            dedupe_key: None,
            created_at: 1,
        }
    }

    #[test]
    fn claim_decision_complete_mission_resolution_gets_mission_fields() {
        let event = test_event(
            "task_claim_decision_received",
            json!({
                "task_id": "mission-1",
                "execution_id": "exec-1",
                "approved": true,
                "task_inputs": {
                    "kind": "wattetheria_mission",
                    "mission_id": "mission-1",
                    "agent_did": "claimer-agent",
                    "mission_feed_key": "wattetheria.missions",
                    "mission_scope_hint": "group:mission-1",
                    "publisher_wattswarm_node_id": "publisher-node"
                }
            }),
        );
        let resolution = normalize_mission_resolution(
            &event,
            AgentEventResolution {
                action: Some("complete_mission".to_owned()),
                reason: None,
                payload: json!({"result": {"ok": true}}),
            },
        );

        assert_eq!(resolution.action.as_deref(), Some("complete_mission"));
        assert_eq!(resolution.payload["mission_id"].as_str(), Some("mission-1"));
        assert_eq!(
            resolution.payload["agent_did"].as_str(),
            Some("claimer-agent")
        );
        assert_eq!(resolution.payload["execution_id"].as_str(), Some("exec-1"));
        assert_eq!(
            resolution.payload["publisher_wattswarm_node_id"].as_str(),
            Some("publisher-node")
        );
    }

    #[test]
    fn task_completed_output_is_mission_result_source() {
        let event = test_event(
            "task_result_received",
            json!({
                "task_id": "mission-2",
                "execution_id": "exec-2",
                "event_kind": "task_completed",
                "output": {
                    "kind": "mission_completed",
                    "mission_id": "mission-2",
                    "agent_did": "claimer-agent",
                    "result": {"score": 1},
                    "mission_feed_key": "wattetheria.missions",
                    "mission_scope_hint": "group:mission-2"
                }
            }),
        );
        assert!(is_mission_event(&event));
        let resolution = normalize_mission_resolution(
            &event,
            AgentEventResolution {
                action: Some("accept_result".to_owned()),
                reason: None,
                payload: json!({}),
            },
        );

        assert_eq!(resolution.action.as_deref(), Some("settle_mission"));
        assert_eq!(resolution.payload["mission_id"].as_str(), Some("mission-2"));
        assert_eq!(
            resolution.payload["agent_did"].as_str(),
            Some("claimer-agent")
        );
        assert_eq!(resolution.payload["execution_id"].as_str(), Some("exec-2"));
        assert_eq!(resolution.payload["result"]["score"].as_i64(), Some(1));
    }
}
