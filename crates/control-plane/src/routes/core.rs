use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, extract::ws::WebSocket};
use chrono::Utc;
use serde_json::{Value, json};
use std::time::Instant;

use crate::agent_attach::{AgentAttachStatus, read_status, write_status};
use crate::auth::{authorize, internal_error, unauthorized};
use crate::autonomy::{build_brain_state, load_night_shift_report, run_autonomy_tick_once};
use crate::diagnostics::{DiagnosticEvent, record_diagnostic};
use crate::routes::identity::identity_context_value;
use crate::social_host::{SignedAgentEnvelopeArgs, build_signed_agent_envelope_for_nodes};
use crate::state::{
    ActionRequest, AgentActionCommitBody, AgentDmSendBody, AgentPaymentAuthorizeBody,
    AgentPaymentRejectBody, AgentPaymentSettleBody, AgentPaymentSubmitBody,
    AgentRelationshipActionBody, AuditQuery, AuthQuery, AutonomyTickBody, ControlPlaneState,
    EventsExportQuery, EventsQuery, HiveMessageBody, MissionClaimBody, MissionPublishBody,
    MissionSettleBody, NightShiftQuery, StreamEvent, send_stream_text,
};
use axum::extract::ws::Message;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::missions::MissionStatus;
use wattetheria_kernel::swarm_bridge::{
    SwarmAgentEnvelope, SwarmRelationshipAction, SwarmTaskClaimDecisionCommand,
    SwarmTaskCompletionDecisionCommand,
};

fn forwarded_agent_commit_headers(auth: &str, event_id: &str, decision_id: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let auth_value = format!("Bearer {auth}");
    headers.insert(
        axum::http::header::AUTHORIZATION,
        HeaderValue::from_str(&auth_value).expect("valid bearer token header"),
    );
    headers.insert(
        "x-agent-event-id",
        HeaderValue::from_str(event_id).expect("valid agent event id"),
    );
    headers.insert(
        "x-agent-decision-id",
        HeaderValue::from_str(decision_id).expect("valid agent decision id"),
    );
    headers
}

async fn task_lifecycle_envelope_for_commit(
    state: &ControlPlaneState,
    body: &AgentActionCommitBody,
    capability: &str,
    message: Value,
    target_agent_id: Option<String>,
    target_node_id: Option<String>,
) -> anyhow::Result<SwarmAgentEnvelope> {
    let source_node_id = state.swarm_bridge.local_node_id().await.ok();
    build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: state.agent_did.clone(),
            source_display_name: None,
            target_agent_id: target_agent_id.or_else(|| {
                body.event
                    .agent_envelope
                    .as_ref()
                    .and_then(|envelope| envelope.source_agent_id.clone())
            }),
            source_node_id,
            target_node_id: target_node_id.or_else(|| body.event.source_node_id.clone()),
            capability: capability.to_owned(),
            message,
            extensions: None,
        },
    )
}

fn event_message_public_id(
    event: &crate::state::AgentActionCommitBody,
    key: &str,
) -> Option<String> {
    event
        .event
        .agent_envelope
        .as_ref()
        .and_then(|envelope| envelope.message.get(key))
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .event
                .payload
                .pointer(&format!("/agent_envelope/message/{key}"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            event
                .event
                .payload
                .pointer(&format!("/topic_content/agent_envelope/message/{key}"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn event_source_node_id(event: &crate::state::AgentActionCommitBody) -> Option<String> {
    event
        .event
        .agent_envelope
        .as_ref()
        .and_then(|envelope| envelope.source_node_id.as_deref())
        .or(event.event.source_node_id.as_deref())
        .or_else(|| {
            event
                .event
                .payload
                .pointer("/agent_envelope/source_node_id")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            event
                .event
                .payload
                .pointer("/topic_content/agent_envelope/source_node_id")
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn topic_reply_is_direct_message(event: &crate::state::AgentActionCommitEvent) -> bool {
    event
        .payload
        .pointer("/topic_content/kind")
        .and_then(Value::as_str)
        == Some("direct_message")
        || event.payload.pointer("/feed_key").and_then(Value::as_str) == Some("wattswarm.dm")
        || event
            .agent_envelope
            .as_ref()
            .and_then(|envelope| envelope.capability.as_deref())
            == Some("social.dm.send")
}

fn topic_reply_hive_id(body: &AgentActionCommitBody) -> Option<String> {
    payload_string(&body.decision.payload, "/hive_id")
        .or_else(|| payload_string(&body.decision.payload, "/topic_id"))
        .or_else(|| payload_string(&body.event.payload, "/hive_id"))
        .or_else(|| payload_string(&body.event.payload, "/topic_id"))
        .or_else(|| payload_string(&body.event.payload, "/topic_content/hive_id"))
        .or_else(|| payload_string(&body.event.payload, "/topic_content/topic_id"))
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
        .into_response()
}

fn required_payload_string(payload: &Value, pointer: &str) -> Option<String> {
    payload
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn optional_payload_value<T: serde::de::DeserializeOwned>(
    payload: &Value,
    key: &str,
    field_label: &str,
) -> Result<Option<T>, String> {
    payload
        .get(key)
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid {field_label}: {error}"))
}

fn payload_string(payload: &Value, pointer: &str) -> Option<String> {
    payload
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn mission_id_for_commit(body: &AgentActionCommitBody) -> Option<String> {
    required_payload_string(&body.decision.payload, "/mission_id")
        .or_else(|| payload_string(&body.event.payload, "/mission_id"))
        .or_else(|| payload_string(&body.event.payload, "/content/mission_id"))
        .or_else(|| payload_string(&body.event.payload, "/topic_content/mission_id"))
        .or_else(|| payload_string(&body.event.payload, "/output/mission_id"))
        .or_else(|| payload_string(&body.event.payload, "/candidate_output/mission_id"))
        .or_else(|| payload_string(&body.event.payload, "/task_inputs/mission_id"))
}

fn mission_agent_did_for_commit(body: &AgentActionCommitBody, default_agent_did: &str) -> String {
    payload_string(&body.decision.payload, "/agent_did")
        .or_else(|| payload_string(&body.decision.payload, "/claimer_agent_did"))
        .or_else(|| payload_string(&body.event.payload, "/agent_did"))
        .or_else(|| payload_string(&body.event.payload, "/claimer_agent_did"))
        .or_else(|| payload_string(&body.event.payload, "/content/agent_did"))
        .or_else(|| payload_string(&body.event.payload, "/content/claimer_agent_did"))
        .or_else(|| payload_string(&body.event.payload, "/topic_content/agent_did"))
        .or_else(|| payload_string(&body.event.payload, "/topic_content/claimer_agent_did"))
        .or_else(|| payload_string(&body.event.payload, "/output/agent_did"))
        .or_else(|| payload_string(&body.event.payload, "/output/claimer_agent_did"))
        .or_else(|| payload_string(&body.event.payload, "/candidate_output/agent_did"))
        .or_else(|| payload_string(&body.event.payload, "/task_inputs/agent_did"))
        .or_else(|| payload_string(&body.event.payload, "/claimer_node_id"))
        .unwrap_or_else(|| default_agent_did.to_owned())
}

fn mission_commit_string(body: &AgentActionCommitBody, field: &str) -> Option<String> {
    let pointer = format!("/{field}");
    required_payload_string(&body.decision.payload, &pointer)
        .or_else(|| payload_string(&body.event.payload, &pointer))
        .or_else(|| payload_string(&body.event.payload, &format!("/content/{field}")))
        .or_else(|| payload_string(&body.event.payload, &format!("/topic_content/{field}")))
        .or_else(|| payload_string(&body.event.payload, &format!("/output/{field}")))
        .or_else(|| payload_string(&body.event.payload, &format!("/task_inputs/{field}")))
}

fn mission_commit_result(body: &AgentActionCommitBody) -> Option<Value> {
    body.decision
        .payload
        .get("result")
        .cloned()
        .or_else(|| body.event.payload.pointer("/content/result").cloned())
        .or_else(|| body.event.payload.pointer("/topic_content/result").cloned())
        .or_else(|| body.event.payload.pointer("/output/result").cloned())
}

fn mission_claim_body_for_commit(
    body: &AgentActionCommitBody,
    mission_id: String,
    agent_did: String,
) -> MissionClaimBody {
    let mut claim_body = MissionClaimBody::local(mission_id, agent_did);
    claim_body.task_id = mission_commit_string(body, "task_id");
    claim_body.mission_feed_key = mission_commit_string(body, "mission_feed_key");
    claim_body.mission_scope_hint = mission_commit_string(body, "mission_scope_hint");
    claim_body.publisher_wattswarm_node_id =
        mission_commit_string(body, "publisher_wattswarm_node_id");
    claim_body.result = mission_commit_result(body);
    claim_body.claim_route = Some(json!({
        "agent_envelope": body.event.agent_envelope.clone(),
        "agent_event_payload": body.event.payload.clone(),
        "decision_payload": body.decision.payload.clone(),
        "task_id": claim_body.task_id,
        "mission_feed_key": claim_body.mission_feed_key,
        "mission_scope_hint": claim_body.mission_scope_hint,
        "publisher_wattswarm_node_id": claim_body.publisher_wattswarm_node_id,
    }));
    claim_body
}

fn topic_lifecycle_kind(body: &AgentActionCommitBody) -> Option<&str> {
    body.event
        .payload
        .pointer("/content/kind")
        .or_else(|| body.event.payload.pointer("/topic_content/kind"))
        .and_then(Value::as_str)
}

fn topic_mission_action_allowed(
    body: &AgentActionCommitBody,
    event_type: &str,
    action: &str,
) -> bool {
    event_type == "topic_message_requires_reply"
        && matches!(
            (topic_lifecycle_kind(body), action),
            (Some("mission_claim_approved"), "complete_mission")
                | (Some("mission_completed"), "settle_mission")
        )
}

fn agent_action_commit_object(body: &AgentActionCommitBody) -> (&'static str, Option<String>) {
    if let Some(mission_id) = mission_id_for_commit(body) {
        return ("mission", Some(mission_id));
    }
    [
        ("task", "/task_id"),
        ("task", "/task_inputs/task_id"),
        ("topic", "/topic_id"),
        ("topic", "/feed_key"),
        ("payment", "/payment_id"),
        ("message", "/message_id"),
    ]
    .into_iter()
    .find_map(|(kind, pointer)| {
        payload_string(&body.decision.payload, pointer)
            .or_else(|| payload_string(&body.event.payload, pointer))
            .map(|value| (kind, Some(value)))
    })
    .unwrap_or(("agent_event", Some(body.event.event_id.clone())))
}

struct AgentActionCommitDiagnosticContext {
    event_id: String,
    event_type: String,
    source_kind: String,
    source_node_id: Option<String>,
    requires_commit: bool,
    target_agent_id: Option<String>,
    decision_id: String,
    action: String,
    decision_route: String,
    object_kind: &'static str,
    object_id: Option<String>,
}

impl AgentActionCommitDiagnosticContext {
    fn from_body(body: &AgentActionCommitBody) -> Self {
        let (object_kind, object_id) = agent_action_commit_object(body);
        Self {
            event_id: body.event.event_id.clone(),
            event_type: body.event.event_type.clone(),
            source_kind: body.event.source_kind.clone(),
            source_node_id: body.event.source_node_id.clone(),
            requires_commit: body.event.requires_commit,
            target_agent_id: body.event.target_agent_id.clone(),
            decision_id: body.decision.decision_id.clone(),
            action: body.decision.action.clone(),
            decision_route: body.decision.route.clone(),
            object_kind,
            object_id,
        }
    }
}

struct AgentActionCommitDiagnosticEvent {
    level: &'static str,
    phase: &'static str,
    status: &'static str,
    message: String,
    route_label: Option<&'static str>,
    duration_ms: Option<u128>,
    status_code: Option<u16>,
}

fn record_agent_action_commit_diagnostic(
    state: &ControlPlaneState,
    context: &AgentActionCommitDiagnosticContext,
    event: AgentActionCommitDiagnosticEvent,
) {
    record_diagnostic(
        &state.data_dir,
        DiagnosticEvent::new(
            event.level,
            "wattetheria.event_bus",
            "agent_action_commit",
            event.phase,
            event.status,
            event.message,
        )
        .event_id(Some(context.event_id.clone()))
        .source_node_id(context.source_node_id.clone())
        .object(context.object_kind, context.object_id.clone())
        .details(json!({
            "event_id": context.event_id,
            "event_type": context.event_type,
            "source_kind": context.source_kind,
            "requires_commit": context.requires_commit,
            "target_agent_id": context.target_agent_id,
            "decision_id": context.decision_id,
            "action": context.action,
            "decision_route": context.decision_route,
            "route_label": event.route_label,
            "duration_ms": event.duration_ms,
            "status_code": event.status_code,
        })),
    );
}

fn agent_action_commit_route_label(
    body: &AgentActionCommitBody,
    event_type: &str,
    action: &str,
) -> &'static str {
    match (event_type, action) {
        ("friend_request", "accept" | "reject" | "block") => "friend_request",
        (
            "payment_request" | "payment_update",
            "authorize" | "reject" | "submit" | "settle" | "cancel",
        ) => "payment_action",
        ("topic_message_requires_reply", "reply") => "topic_reply",
        (
            "topic_message_requires_reply",
            "publish_mission" | "claim_mission" | "reject_claim" | "human_review"
            | "complete_mission" | "settle_mission",
        ) if topic_mission_action_allowed(body, event_type, action) => "mission_lifecycle_topic",
        (
            "topic_message_requires_reply",
            "publish_mission" | "claim_mission" | "reject_claim" | "human_review"
            | "complete_mission" | "settle_mission",
        ) => "unsupported_topic_mission",
        (_, "publish_mission") => "mission_publish",
        (_, "claim_mission" | "complete_mission" | "settle_mission") => "mission_transition",
        ("task_claim_received", "reject_claim" | "human_review") => "mission_claim_review",
        ("task_result_received", "reject_result" | "request_retry" | "human_review") => {
            "mission_result_review"
        }
        _ => "unsupported",
    }
}

async fn commit_friend_request(
    state: ControlPlaneState,
    commit_headers: HeaderMap,
    body: AgentActionCommitBody,
) -> Response {
    let Some(counterpart_public_id) = event_message_public_id(&body, "source_public_id") else {
        return bad_request("friend_request missing source_public_id");
    };
    let action = match body.decision.action.as_str() {
        "accept" => SwarmRelationshipAction::Accept,
        "reject" => SwarmRelationshipAction::Reject,
        "block" => SwarmRelationshipAction::Block,
        _ => unreachable!("friend_request action already matched"),
    };
    crate::routes::civilization::agent_relationship_action(
        State(state),
        commit_headers,
        Json(AgentRelationshipActionBody {
            public_id: event_message_public_id(&body, "target_public_id"),
            counterpart_public_id: Some(counterpart_public_id),
            remote_node_id: event_source_node_id(&body),
            target_agent_did: None,
            display_name: None,
            action,
            message: body.decision.payload.get("message").cloned(),
            extensions: body.decision.payload.get("extensions").cloned(),
        }),
    )
    .await
}

async fn commit_payment_action(
    state: ControlPlaneState,
    commit_headers: HeaderMap,
    body: AgentActionCommitBody,
) -> Response {
    let Some(payment_id) = required_payload_string(&body.event.payload, "/payment/payment_id")
    else {
        return bad_request("missing payment.payment_id");
    };
    match body.decision.action.as_str() {
        "authorize" => {
            crate::routes::payments::authorize_agent_payment(
                State(state),
                commit_headers,
                axum::extract::Path(payment_id),
                Json(AgentPaymentAuthorizeBody {
                    sender_address: body
                        .decision
                        .payload
                        .get("sender_address")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                }),
            )
            .await
        }
        "reject" => {
            let reject_reason = body
                .decision
                .payload
                .get("reject_reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or(body.decision.reason.clone())
                .unwrap_or_else(|| "rejected_by_agent".to_owned());
            crate::routes::payments::reject_agent_payment(
                State(state),
                commit_headers,
                axum::extract::Path(payment_id),
                Json(AgentPaymentRejectBody { reject_reason }),
            )
            .await
        }
        "submit" => {
            let settlement_receipt = body.decision.payload.get("settlement_receipt").cloned();
            crate::routes::payments::submit_agent_payment(
                State(state),
                commit_headers,
                axum::extract::Path(payment_id),
                Some(Json(AgentPaymentSubmitBody { settlement_receipt })),
            )
            .await
        }
        "settle" => {
            let Some(settlement_receipt) = body.decision.payload.get("settlement_receipt").cloned()
            else {
                return bad_request("payment settle requires settlement_receipt");
            };
            crate::routes::payments::settle_agent_payment(
                State(state),
                commit_headers,
                axum::extract::Path(payment_id),
                Json(AgentPaymentSettleBody { settlement_receipt }),
            )
            .await
        }
        "cancel" => {
            crate::routes::payments::cancel_agent_payment(
                State(state),
                commit_headers,
                axum::extract::Path(payment_id),
            )
            .await
        }
        _ => unreachable!("payment action already matched"),
    }
}

async fn commit_topic_reply(
    state: ControlPlaneState,
    commit_headers: HeaderMap,
    body: AgentActionCommitBody,
) -> Response {
    let Some(content) = body.decision.payload.get("content").cloned() else {
        return bad_request("topic reply requires content");
    };
    if topic_reply_is_direct_message(&body.event) {
        let Some(counterpart_public_id) = event_message_public_id(&body, "source_public_id") else {
            return bad_request("topic dm reply missing source_public_id");
        };
        return crate::routes::civilization::send_agent_dm_message(
            State(state),
            commit_headers,
            Json(AgentDmSendBody {
                public_id: event_message_public_id(&body, "target_public_id"),
                counterpart_public_id,
                content,
                extensions: body.decision.payload.get("extensions").cloned(),
            }),
        )
        .await;
    }
    let Some(feed_key) = required_payload_string(&body.event.payload, "/feed_key") else {
        return bad_request("missing feed_key");
    };
    let Some(scope_hint) = required_payload_string(&body.event.payload, "/scope_hint") else {
        return bad_request("missing scope_hint");
    };
    crate::routes::topics::post_hive_topic_message(
        state,
        commit_headers,
        topic_reply_hive_id(&body),
        HiveMessageBody {
            public_id: payload_string(&body.decision.payload, "/public_id"),
            network_id: payload_string(&body.decision.payload, "/network_id")
                .or_else(|| payload_string(&body.event.payload, "/network_id")),
            feed_key: Some(feed_key),
            scope_hint: Some(scope_hint),
            content,
            reply_to_message_id: body
                .decision
                .payload
                .get("reply_to_message_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| {
                    body.event
                        .payload
                        .get("message_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                }),
        },
    )
    .await
}

async fn commit_publish_mission(
    state: ControlPlaneState,
    commit_headers: HeaderMap,
    body: AgentActionCommitBody,
) -> Response {
    let Some(title) = required_payload_string(&body.decision.payload, "/title") else {
        return bad_request("missing title");
    };
    let Some(description) = required_payload_string(&body.decision.payload, "/description") else {
        return bad_request("missing description");
    };
    let publisher = body
        .decision
        .payload
        .get("publisher")
        .and_then(Value::as_str)
        .map_or_else(|| state.agent_did.clone(), ToOwned::to_owned);
    let publisher_kind =
        match optional_payload_value(&body.decision.payload, "publisher_kind", "publisher_kind") {
            Ok(Some(value)) => value,
            Ok(None) => wattetheria_kernel::civilization::missions::MissionPublisherKind::System,
            Err(error) => return bad_request(error),
        };
    let domain = match optional_payload_value(&body.decision.payload, "domain", "mission domain") {
        Ok(Some(value)) => value,
        Ok(None) => wattetheria_kernel::civilization::missions::MissionDomain::Trade,
        Err(error) => return bad_request(error),
    };
    let scope = match optional_payload_value(&body.decision.payload, "scope", "mission scope") {
        Ok(Some(value)) => value,
        Ok(None) => wattetheria_kernel::civilization::missions::MissionScope::default(),
        Err(error) => return bad_request(error),
    };
    let reward = match optional_payload_value(&body.decision.payload, "reward", "mission reward") {
        Ok(Some(value)) => value,
        Ok(None) => return bad_request("publish_mission requires reward"),
        Err(error) => return bad_request(error),
    };
    let required_role =
        match optional_payload_value(&body.decision.payload, "required_role", "required_role") {
            Ok(value) => value,
            Err(error) => return bad_request(error),
        };
    let required_faction = match optional_payload_value(
        &body.decision.payload,
        "required_faction",
        "required_faction",
    ) {
        Ok(value) => value,
        Err(error) => return bad_request(error),
    };
    crate::routes::missions::mission_publish(
        State(state),
        commit_headers,
        Json(MissionPublishBody {
            title,
            description,
            publisher,
            publisher_kind,
            domain,
            scope,
            subnet_id: body
                .decision
                .payload
                .get("subnet_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            zone_id: body
                .decision
                .payload
                .get("zone_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            required_role,
            required_faction,
            reward,
            payload: body
                .decision
                .payload
                .get("payload")
                .cloned()
                .unwrap_or(Value::Null),
            settlement_delegation: None,
        }),
    )
    .await
}

async fn commit_transition_mission(
    state: ControlPlaneState,
    commit_headers: HeaderMap,
    body: AgentActionCommitBody,
) -> Response {
    let Some(mission_id) = mission_id_for_commit(&body) else {
        return bad_request("missing mission_id");
    };
    match body.decision.action.as_str() {
        "claim_mission" => {
            let default_agent_did = state.agent_did.clone();
            let agent_did = mission_agent_did_for_commit(&body, &default_agent_did);
            let claim_body = mission_claim_body_for_commit(&body, mission_id, agent_did);
            crate::routes::missions::mission_claim(State(state), commit_headers, Json(claim_body))
                .await
        }
        "complete_mission" => {
            let default_agent_did = state.agent_did.clone();
            let agent_did = mission_agent_did_for_commit(&body, &default_agent_did);
            let claim_body = mission_claim_body_for_commit(&body, mission_id, agent_did);
            crate::routes::missions::mission_complete(
                State(state),
                commit_headers,
                Json(claim_body),
            )
            .await
        }
        "settle_mission" => {
            if body.event.event_type == "task_result_received" {
                let default_agent_did = state.agent_did.clone();
                let agent_did = mission_agent_did_for_commit(&body, &default_agent_did);
                let claim_route =
                    mission_claim_body_for_commit(&body, mission_id.clone(), agent_did.clone())
                        .claim_route;
                let task_id = payload_string(&body.event.payload, "/task_id");
                let candidate_id = payload_string(&body.event.payload, "/candidate_id");
                return commit_task_result_settle_mission(
                    state,
                    commit_headers,
                    mission_id,
                    agent_did,
                    task_id,
                    candidate_id,
                    claim_route,
                )
                .await;
            }
            let default_agent_did = state.agent_did.clone();
            let agent_did = mission_agent_did_for_commit(&body, &default_agent_did);
            let claim_route =
                mission_claim_body_for_commit(&body, mission_id.clone(), agent_did.clone())
                    .claim_route;
            crate::routes::missions::mission_settle(
                State(state),
                commit_headers,
                Json(MissionSettleBody {
                    mission_id,
                    task_id: None,
                    agent_did: Some(agent_did),
                    candidate_id: None,
                    claim_route,
                }),
            )
            .await
        }
        _ => unreachable!("mission transition action already matched"),
    }
}

#[allow(clippy::too_many_lines)]
async fn commit_mission_claim_review(
    state: ControlPlaneState,
    commit_headers: HeaderMap,
    body: AgentActionCommitBody,
) -> Response {
    let auth = match authorize(&state, &commit_headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(mission_id) = mission_id_for_commit(&body) else {
        return bad_request("missing mission_id");
    };
    let default_agent_did = state.agent_did.clone();
    let agent_did = mission_agent_did_for_commit(&body, &default_agent_did);
    let action = body.decision.action.as_str();
    let (status, stream_kind, signed_kind, audit_action) = match action {
        "reject_claim" => (
            "rejected",
            "mission.claim_rejected",
            "MISSION_CLAIM_REJECTED",
            "mission.reject_claim",
        ),
        "human_review" => (
            "human_review",
            "mission.claim_human_review",
            "MISSION_CLAIM_HUMAN_REVIEW",
            "mission.human_review",
        ),
        _ => unreachable!("mission claim review action already matched"),
    };
    let task_id = body
        .decision
        .payload
        .get("task_id")
        .and_then(Value::as_str)
        .or_else(|| body.event.payload.get("task_id").and_then(Value::as_str))
        .unwrap_or(mission_id.as_str())
        .to_owned();
    let claimer_node_id = payload_string(&body.decision.payload, "/claimer_node_id")
        .or_else(|| payload_string(&body.event.payload, "/claimer_node_id"))
        .or_else(|| body.event.source_node_id.clone());
    let Some(claimer_node_id) = claimer_node_id.clone() else {
        return bad_request("missing claimer_node_id");
    };
    let execution_id = payload_string(&body.decision.payload, "/execution_id")
        .or_else(|| payload_string(&body.event.payload, "/execution_id"))
        .unwrap_or_else(|| format!("wattetheria:{mission_id}:{agent_did}"));
    let payload = json!({
        "mission_id": mission_id,
        "task_id": task_id,
        "agent_did": agent_did,
        "claimer_node_id": claimer_node_id,
        "execution_id": execution_id,
        "status": status,
        "reason": body.decision.reason.clone(),
        "decision_payload": body.decision.payload.clone(),
        "event_id": body.event.event_id.clone(),
        "decision_id": body.decision.decision_id.clone(),
    });
    let agent_envelope = match task_lifecycle_envelope_for_commit(
        &state,
        &body,
        "mission.claim.review",
        payload.clone(),
        body.event
            .agent_envelope
            .as_ref()
            .and_then(|envelope| envelope.source_agent_id.clone()),
        Some(claimer_node_id.clone()),
    )
    .await
    {
        Ok(envelope) => envelope,
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = state
        .swarm_bridge
        .decide_task_claim(SwarmTaskClaimDecisionCommand {
            task_id: task_id.clone(),
            execution_id,
            claimer_node_id,
            approved: false,
            reason: body.decision.reason.clone(),
            agent_envelope,
        })
        .await
    {
        return internal_error(&error);
    }
    let _ = state.stream_tx.send(StreamEvent {
        kind: stream_kind.to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event(signed_kind, payload.clone());
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: audit_action.to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(mission_id),
        capability: Some(audit_action.to_string()),
        reason: payload
            .get("reason")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        duration_ms: None,
        details: Some(payload.clone()),
    });
    Json(payload).into_response()
}

#[allow(clippy::too_many_lines)]
async fn commit_mission_result_review(
    state: ControlPlaneState,
    commit_headers: HeaderMap,
    body: AgentActionCommitBody,
) -> Response {
    let auth = match authorize(&state, &commit_headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(mission_id) = mission_id_for_commit(&body) else {
        return bad_request("missing mission_id");
    };
    let default_agent_did = state.agent_did.clone();
    let agent_did = mission_agent_did_for_commit(&body, &default_agent_did);
    let action = body.decision.action.as_str();
    let (status, stream_kind, signed_kind, audit_action) = match action {
        "reject_result" => (
            "rejected",
            "mission.result_rejected",
            "MISSION_RESULT_REJECTED",
            "mission.reject_result",
        ),
        "request_retry" => (
            "retry_requested",
            "mission.result_retry_requested",
            "MISSION_RESULT_RETRY_REQUESTED",
            "mission.request_retry",
        ),
        "human_review" => (
            "human_review",
            "mission.result_human_review",
            "MISSION_RESULT_HUMAN_REVIEW",
            "mission.result_human_review",
        ),
        _ => unreachable!("mission result review action already matched"),
    };
    let task_id = payload_string(&body.decision.payload, "/task_id")
        .or_else(|| payload_string(&body.event.payload, "/task_id"))
        .unwrap_or_else(|| mission_id.clone());
    let execution_id = payload_string(&body.decision.payload, "/execution_id")
        .or_else(|| payload_string(&body.event.payload, "/execution_id"))
        .unwrap_or_else(|| format!("wattetheria:{mission_id}:{agent_did}"));
    let payload = json!({
        "mission_id": mission_id,
        "task_id": task_id,
        "candidate_id": payload_string(&body.decision.payload, "/candidate_id")
            .or_else(|| payload_string(&body.event.payload, "/candidate_id")),
        "agent_did": agent_did,
        "execution_id": execution_id,
        "status": status,
        "reason": body.decision.reason.clone(),
        "decision_payload": body.decision.payload.clone(),
        "event_id": body.event.event_id.clone(),
        "decision_id": body.decision.decision_id.clone(),
    });
    let target_node_id = payload_string(&body.event.payload, "/completed_by_node_id")
        .or_else(|| body.event.source_node_id.clone());
    let agent_envelope = match task_lifecycle_envelope_for_commit(
        &state,
        &body,
        "mission.result.review",
        payload.clone(),
        body.event
            .agent_envelope
            .as_ref()
            .and_then(|envelope| envelope.source_agent_id.clone()),
        target_node_id,
    )
    .await
    {
        Ok(envelope) => envelope,
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = state
        .swarm_bridge
        .decide_task_completion(SwarmTaskCompletionDecisionCommand {
            task_id: task_id.clone(),
            execution_id,
            approved: false,
            retry_requested: action == "request_retry",
            reason: body.decision.reason.clone(),
            agent_envelope,
        })
        .await
    {
        return internal_error(&error);
    }
    let _ = state.stream_tx.send(StreamEvent {
        kind: stream_kind.to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event(signed_kind, payload.clone());
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: audit_action.to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(mission_id),
        capability: Some(audit_action.to_string()),
        reason: payload
            .get("reason")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        duration_ms: None,
        details: Some(payload.clone()),
    });
    Json(payload).into_response()
}

async fn commit_task_result_settle_mission(
    state: ControlPlaneState,
    commit_headers: HeaderMap,
    mission_id: String,
    agent_did: String,
    task_id: Option<String>,
    candidate_id: Option<String>,
    claim_route: Option<Value>,
) -> Response {
    let status = {
        let board = state.mission_board.lock().await;
        board.get(&mission_id).map(|mission| mission.status.clone())
    };
    let Some(status) = status else {
        return bad_request("mission not found");
    };
    if status == MissionStatus::Settled {
        return Json(json!({
            "ok": true,
            "status": "settled",
            "replay": true,
            "mission_id": mission_id,
        }))
        .into_response();
    }
    // Finalize the underlying task first — this is all-or-nothing (HTTP call).
    // If it fails, no mission state has changed, so the caller can safely retry.
    if let (Some(task_id), Some(candidate_id)) = (&task_id, &candidate_id)
        && let Err(error) = state
            .swarm_bridge
            .accept_and_finalize_task(task_id, candidate_id, None)
            .await
    {
        return internal_error(&error);
    }
    // Mission transitions are idempotent via replay guards (commit_headers dedup).
    // If any step fails, a retry will skip already-completed steps.
    if status == MissionStatus::Open {
        let mut claim_body = MissionClaimBody::local(mission_id.clone(), agent_did.clone());
        claim_body.claim_route = claim_route.clone();
        let response = crate::routes::missions::mission_claim(
            State(state.clone()),
            commit_headers.clone(),
            Json(claim_body),
        )
        .await;
        if !response.status().is_success() {
            return response;
        }
    }
    if matches!(status, MissionStatus::Open | MissionStatus::Claimed) {
        let mut complete_body = MissionClaimBody::local(mission_id.clone(), agent_did.clone());
        complete_body.claim_route = claim_route.clone();
        let response = crate::routes::missions::mission_complete(
            State(state.clone()),
            commit_headers.clone(),
            Json(complete_body),
        )
        .await;
        if !response.status().is_success() {
            return response;
        }
    }
    crate::routes::missions::mission_settle(
        State(state),
        commit_headers,
        Json(MissionSettleBody {
            mission_id,
            task_id: None,
            agent_did: Some(agent_did),
            candidate_id: None,
            claim_route,
        }),
    )
    .await
}

pub(crate) async fn health(State(state): State<ControlPlaneState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "timestamp": Utc::now().timestamp(),
        "agent_did": state.agent_did,
        "uptime_sec": Utc::now().timestamp() - state.started_at,
    }))
}

async fn dispatch_agent_action_commit(
    state: ControlPlaneState,
    commit_headers: HeaderMap,
    body: AgentActionCommitBody,
    event_type: &str,
    action: &str,
) -> Response {
    match (event_type, action) {
        ("friend_request", "accept" | "reject" | "block") => {
            commit_friend_request(state, commit_headers, body).await
        }
        (
            "payment_request" | "payment_update",
            "authorize" | "reject" | "submit" | "settle" | "cancel",
        ) => commit_payment_action(state, commit_headers, body).await,
        ("topic_message_requires_reply", "reply") => {
            commit_topic_reply(state, commit_headers, body).await
        }
        (
            "topic_message_requires_reply",
            "publish_mission" | "claim_mission" | "reject_claim" | "human_review"
            | "complete_mission" | "settle_mission",
        ) if topic_mission_action_allowed(&body, event_type, action) => {
            commit_transition_mission(state, commit_headers, body).await
        }
        (
            "topic_message_requires_reply",
            "publish_mission" | "claim_mission" | "reject_claim" | "human_review"
            | "complete_mission" | "settle_mission",
        ) => bad_request("mission actions are not supported for topic messages"),
        (_, "publish_mission") => commit_publish_mission(state, commit_headers, body).await,
        ("task_claim_received", "reject_claim" | "human_review") => {
            commit_mission_claim_review(state, commit_headers, body).await
        }
        ("task_result_received", "reject_result" | "request_retry" | "human_review") => {
            commit_mission_result_review(state, commit_headers, body).await
        }
        (_, "claim_mission" | "complete_mission" | "settle_mission") => {
            commit_transition_mission(state, commit_headers, body).await
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "unsupported agent action commit",
                "event_type": event_type,
                "action": action,
            })),
        )
            .into_response(),
    }
}

pub(crate) async fn agent_action_commit(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<AgentActionCommitBody>,
) -> Response {
    let started_at = Instant::now();
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let commit_headers =
        forwarded_agent_commit_headers(&auth, &body.event.event_id, &body.decision.decision_id);
    let diagnostic_context = AgentActionCommitDiagnosticContext::from_body(&body);
    record_agent_action_commit_diagnostic(
        &state,
        &diagnostic_context,
        AgentActionCommitDiagnosticEvent {
            level: "info",
            phase: "agent_action.commit.received",
            status: "accepted",
            message: format!(
                "agent action commit received: {} -> {}",
                body.event.event_type, body.decision.action
            ),
            route_label: None,
            duration_ms: None,
            status_code: None,
        },
    );

    let event_type = body.event.event_type.clone();
    let action = body.decision.action.clone();
    let route_label = agent_action_commit_route_label(&body, &event_type, &action);
    record_agent_action_commit_diagnostic(
        &state,
        &diagnostic_context,
        AgentActionCommitDiagnosticEvent {
            level: if route_label == "unsupported" || route_label == "unsupported_topic_mission" {
                "warn"
            } else {
                "info"
            },
            phase: "agent_action.commit.routed",
            status: route_label,
            message: format!(
                "agent action commit routed: {} -> {}",
                body.event.event_type, body.decision.action
            ),
            route_label: Some(route_label),
            duration_ms: None,
            status_code: None,
        },
    );

    let response =
        dispatch_agent_action_commit(state.clone(), commit_headers, body, &event_type, &action)
            .await;
    let status_code = response.status();
    record_agent_action_commit_diagnostic(
        &state,
        &diagnostic_context,
        AgentActionCommitDiagnosticEvent {
            level: if status_code.is_success() {
                "info"
            } else {
                "error"
            },
            phase: if status_code.is_success() {
                "agent_action.commit.completed"
            } else {
                "agent_action.commit.failed"
            },
            status: if status_code.is_success() {
                "ok"
            } else {
                "error"
            },
            message: format!(
                "agent action commit {}: {} -> {}",
                if status_code.is_success() {
                    "completed"
                } else {
                    "failed"
                },
                diagnostic_context.event_type,
                diagnostic_context.action
            ),
            route_label: Some(route_label),
            duration_ms: Some(started_at.elapsed().as_millis()),
            status_code: Some(status_code.as_u16()),
        },
    );
    response
}

pub(crate) async fn state_view(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let events = match state.event_log.get_all() {
        Ok(rows) => rows,
        Err(error) => return internal_error(&error),
    };
    let pending_count = state.policy_engine.lock().await.list_pending().len();
    let identity = identity_context_value(&state, None, Some(&state.agent_did)).await;

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "state.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_did.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"events": events.len(), "pending": pending_count})),
    });

    Json(json!({
        "agent_did": state.agent_did,
        "events": events.len(),
        "pending_policy_requests": pending_count,
        "uptime_sec": Utc::now().timestamp() - state.started_at,
        "identity": identity,
    }))
    .into_response()
}

pub(crate) async fn events(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let rows = if let Some(since) = query.since {
        match state.event_log.since(since) {
            Ok(rows) => rows,
            Err(error) => return internal_error(&error),
        }
    } else {
        match state.event_log.get_all() {
            Ok(rows) => rows,
            Err(error) => return internal_error(&error),
        }
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "events.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": rows.len()})),
    });

    Json(rows).into_response()
}

pub(crate) async fn events_export(
    State(state): State<ControlPlaneState>,
    Query(query): Query<EventsExportQuery>,
) -> Response {
    let mut rows = if let Some(since) = query.since {
        match state.event_log.since(since) {
            Ok(rows) => rows,
            Err(error) => return internal_error(&error),
        }
    } else {
        match state.event_log.get_all() {
            Ok(rows) => rows,
            Err(error) => return internal_error(&error),
        }
    };

    if let Some(limit) = query.limit {
        let cap = limit.max(1);
        if rows.len() > cap {
            rows = rows.split_off(rows.len() - cap);
        }
    }

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "recovery".to_string(),
        action: "events.export".to_string(),
        status: "ok".to_string(),
        actor: Some("public".to_string()),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": rows.len()})),
    });

    Json(json!({
        "events": rows,
        "count": rows.len(),
        "generated_at": Utc::now().timestamp(),
    }))
    .into_response()
}

pub(crate) async fn night_shift(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<NightShiftQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let hours = query.hours.unwrap_or(12).max(1);
    let report = match load_night_shift_report(&state, hours) {
        Ok(report) => report,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "night_shift.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_did.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"hours": hours})),
    });

    Json(report).into_response()
}

pub(crate) async fn night_shift_narrative_payload(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<NightShiftQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let hours = query.hours.unwrap_or(12).max(1);
    let report = match load_night_shift_report(&state, hours) {
        Ok(report) => report,
        Err(error) => return internal_error(&error),
    };

    let human = match state
        .brain_engine
        .read()
        .await
        .humanize_night_shift(&report)
        .await
    {
        Ok(human) => human,
        Err(error) => return internal_error(&error),
    };

    let payload = json!({
        "hours": hours,
        "report": report,
        "human": human,
    });

    let _ = state.stream_tx.send(StreamEvent {
        kind: "brain.night_shift_narrative".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "brain".to_string(),
        action: "night_shift.narrative".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_did.clone()),
        capability: Some("model.invoke".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(json!({"hours": hours})),
    });

    Json(payload).into_response()
}

pub(crate) async fn night_shift_summary(
    state: State<ControlPlaneState>,
    headers: HeaderMap,
    query: Query<NightShiftQuery>,
) -> Response {
    night_shift(state, headers, query).await
}

pub(crate) async fn night_shift_narrative(
    state: State<ControlPlaneState>,
    headers: HeaderMap,
    query: Query<NightShiftQuery>,
) -> Response {
    night_shift_narrative_payload(state, headers, query).await
}

pub(crate) async fn brain_propose_actions(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let brain_state = match build_brain_state(&state).await {
        Ok(value) => value,
        Err(error) => return internal_error(&error),
    };

    let proposals = match state
        .brain_engine
        .read()
        .await
        .propose_actions(&brain_state)
        .await
    {
        Ok(proposals) => proposals,
        Err(error) => return internal_error(&error),
    };

    let payload = serde_json::to_value(&proposals).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: "brain.proposals".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "brain".to_string(),
        action: "brain.propose_actions".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_did.clone()),
        capability: Some("model.invoke".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": proposals.len()})),
    });

    Json(proposals).into_response()
}

pub(crate) async fn autonomy_tick(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<AutonomyTickBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let hours = body.hours.unwrap_or(12).max(1);
    let result = match run_autonomy_tick_once(&state, hours).await {
        Ok(result) => result,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "autonomy".to_string(),
        action: "autonomy.tick".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_did.clone()),
        capability: Some("model.invoke".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "hours": hours,
            "executed_actions": result["executed_actions"],
        })),
    });

    Json(result).into_response()
}

pub(crate) async fn actions(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(request): Json<ActionRequest>,
) -> Response {
    match authorize(&state, &headers).await {
        Ok(_token) => {}
        Err(response) => return response,
    }

    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": format!("unsupported action: {}", request.action)})),
    )
        .into_response()
}

pub(crate) async fn brain_doctor(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let provider_label = crate::routes::runtime_config::brain_provider_label(
        &state.brain_config.read().await.clone(),
    );
    let status = match state.brain_engine.read().await.doctor().await {
        Ok(detail) => {
            let status = AgentAttachStatus::connected(provider_label.clone());
            if let Err(error) = write_status(&state.data_dir, &status) {
                return internal_error(&error);
            }
            let _ = state.audit_log.append(AuditEntry {
                id: String::new(),
                timestamp: 0,
                category: "brain".to_string(),
                action: "brain.doctor".to_string(),
                status: "ok".to_string(),
                actor: Some(auth),
                subject: Some(state.agent_did.clone()),
                capability: Some("model.invoke".to_string()),
                reason: None,
                duration_ms: None,
                details: Some(json!({"detail": detail})),
            });
            status
        }
        Err(error) => {
            let message = error.to_string();
            let status = AgentAttachStatus::disconnected(provider_label.clone(), message.clone());
            if let Err(write_error) = write_status(&state.data_dir, &status) {
                return internal_error(&write_error);
            }
            let _ = state.audit_log.append(AuditEntry {
                id: String::new(),
                timestamp: 0,
                category: "brain".to_string(),
                action: "brain.doctor".to_string(),
                status: "fail".to_string(),
                actor: Some(auth),
                subject: Some(state.agent_did.clone()),
                capability: Some("model.invoke".to_string()),
                reason: None,
                duration_ms: None,
                details: Some(json!({"error": message})),
            });
            status
        }
    };

    Json(status).into_response()
}

pub(crate) async fn agent_attach_status(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let status = match read_status(&state.data_dir) {
        Ok(Some(status)) => status,
        Ok(None) => {
            AgentAttachStatus::unknown(Some(crate::routes::runtime_config::brain_provider_label(
                &state.brain_config.read().await.clone(),
            )))
        }
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "agent.attach_status".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_did.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"attach_status": status.status})),
    });

    Json(status).into_response()
}

pub(crate) async fn audit_recent(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let limit = query.limit.unwrap_or(50).max(1);
    let rows = match state.audit_log.list_recent(limit) {
        Ok(rows) => rows,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "audit.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"limit": limit})),
    });

    Json(rows).into_response()
}

pub(crate) async fn stream(
    State(state): State<ControlPlaneState>,
    Query(query): Query<AuthQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    if query.token != state.auth_token {
        return unauthorized();
    }

    ws.on_upgrade(move |socket| handle_ws(socket, state))
        .into_response()
}

async fn handle_ws(mut socket: WebSocket, state: ControlPlaneState) {
    let mut receiver = state.stream_tx.subscribe();

    let hello = json!({
        "kind": "hello",
        "timestamp": Utc::now().timestamp(),
        "agent_did": state.agent_did,
    });

    if !send_stream_text(&mut socket, hello.to_string()).await {
        return;
    }

    while let Ok(event) = receiver.recv().await {
        let Ok(payload) = serde_json::to_string(&event) else {
            continue;
        };

        if socket.send(Message::Text(payload.into())).await.is_err() {
            break;
        }
    }
}
