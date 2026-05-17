use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;
use wattetheria_kernel::brain::AgentEventResolution;

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
        ("topic_message_requires_reply", "reply" | "ignore")
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

fn add_mission_allowed_actions(event: &mut AgentEventEnvelope) {
    if !is_mission_event(event) {
        return;
    }
    match event.event_type.as_str() {
        "task_claim_received" => push_allowed_action(event, "claim_mission"),
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
    resolution: Option<AgentEventResolution>,
    acked_at: u64,
) -> Response {
    let Some(mut resolution) = resolution else {
        return no_decision_response(state, event, acked_at);
    };
    resolution = normalize_mission_resolution(event, resolution);
    let Some(action) = resolution.action.clone() else {
        return no_action_response(state, event, &resolution, acked_at);
    };
    let Some(route) = map_route(&event.event_type, &action) else {
        return unsupported_route_response(state, event, &action, &resolution, acked_at);
    };
    if !event.allowed_actions.iter().any(|a| a == &action) {
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
    let resolution = match state
        .brain_engine
        .read()
        .await
        .decide_agent_event(&input)
        .await
    {
        Ok(resolution) => resolution,
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
    response_from_resolution(&state, &event, resolution, acked_at)
}
