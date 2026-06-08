use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;
use watt_did::{Did, PaymentAccountBindingProof, VerifiedAgentContext};
use watt_wallet::verify_payment_account_binding_proof;
use wattetheria_kernel::brain::AgentEventResolution;
use wattetheria_kernel::local_db;
use wattetheria_kernel::payments::{
    PaymentAgentMessage, PaymentMessageKind, PaymentStatus, PaymentTransaction,
    source_payment_account_binding_required,
};
use wattetheria_kernel::signing::verify_payload;
use wattetheria_kernel::swarm_bridge::SwarmAgentEnvelope;

pub(crate) const VERIFIED_AGENT_CONTEXT_PAYLOAD_KEY: &str = "__verified_agent_context";

use crate::diagnostics::{DiagnosticEvent, record_diagnostic};
use crate::state::ControlPlaneState;

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
    match (event_type, action) {
        ("friend_request", "accept" | "reject" | "block")
        | ("dm_received", "reply" | "block" | "ignore")
        | (
            "payment_request" | "payment_update",
            "authorize" | "reject" | "submit" | "settle" | "cancel",
        )
        | (
            "third_party_result",
            "publish_mission" | "claim_mission" | "complete_mission" | "settle_mission",
        )
        | ("task_claim_received", "claim_mission")
        | ("task_result_received", "complete_mission" | "settle_mission") => {
            Some("wattetheria_commit")
        }
        ("topic_message_requires_reply", "reply") => Some("wattetheria_commit"),
        ("topic_message_requires_reply", "ignore")
        | ("task_claim_received", "decide_claim" | "inspect_task")
        | (
            "task_result_received",
            "accept_result" | "reject_result" | "request_retry" | "inspect_task",
        ) => Some("wattswarm_direct"),
        ("third_party_result", "inspect_result" | "continue") => Some("noop"),
        _ => None,
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
        "payload": event.payload,
    })
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
            .pointer("/mission_id")
            .and_then(Value::as_str)
            .is_some()
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
        "task_claim_received" => set_allowed_actions(event, &["decide_claim"]),
        "task_result_received" => {
            push_allowed_action(event, "complete_mission");
            push_allowed_action(event, "settle_mission");
        }
        _ => {}
    }
}

fn mission_id_from_event(event: &AgentEventEnvelope) -> Option<String> {
    [
        "/mission_id",
        "/task_id",
        "/task_inputs/mission_id",
        "/candidate_output/mission_id",
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
        "task_result_received" => &[
            "/candidate_output/agent_did",
            "/agent_did",
            "/claimer_agent_did",
            "/claimer_node_id",
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
        .or_else(|| event.source_node_id.clone())
}

fn payload_bool(payload: &Value, key: &str) -> Option<bool> {
    payload
        .get(key)
        .and_then(Value::as_bool)
        .or_else(|| payload.pointer(&format!("/{key}")).and_then(Value::as_bool))
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
            | "task_result_received"
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
        ("task_result_received", Some("accept_result")) => {
            resolution.action = Some("settle_mission".to_string());
            ensure_mission_payload_fields(event, &mut resolution);
        }
        ("task_claim_received", Some("claim_mission"))
        | ("task_result_received", Some("complete_mission" | "settle_mission")) => {
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
    is_mission_event(event)
        && matches!(
            (
                event.event_type.as_str(),
                original_action,
                normalized_action
            ),
            ("task_claim_received", Some("decide_claim"), "claim_mission")
                | (
                    "task_result_received",
                    Some("accept_result"),
                    "settle_mission"
                )
        )
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

fn selected_action_response(
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

fn response_from_resolution(
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
    selected_action_response(state, event, &action, route, &resolution, acked_at)
}

pub(crate) async fn callback(
    State(state): State<ControlPlaneState>,
    Json(body): Json<AgentEventCallbackRequest>,
) -> Response {
    let acked_at = Utc::now().timestamp_millis().max(0).cast_unsigned();
    let mut event = body.event;
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
    let callback_request = json!({ "event": &event });
    add_mission_allowed_actions(&mut event);
    let input = build_brain_event_input(&state, &event);
    record_agent_event_diagnostic(
        &state,
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
                &state,
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
            record_agent_callback_responded(&state, &event, &response);
            return Json(response).into_response();
        }
    };
    record_agent_brain_response_diagnostic(&state, &event, &decision.diagnostics);
    response_from_resolution(
        &state,
        &event,
        verified_context.as_ref(),
        decision.resolution,
        acked_at,
    )
}
