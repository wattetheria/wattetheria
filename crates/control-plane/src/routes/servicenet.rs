use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::{authorize, internal_error};
use crate::routes::agent_events::{
    AgentEventCallbackRequest, AgentEventCallbackResponse, AgentEventEnvelope,
};
use crate::state::{
    AgentActionCommitBody, AgentActionCommitEvent, AgentActionDecision, ControlPlaneState,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::servicenet::{
    ServiceNetClient, ServiceNetClientError, ServiceNetGetAgentTaskRequest, ServiceNetInvokeRequest,
};

const CORE_AGENT_EXECUTOR_NAME: &str = "core-agent";
const DEFAULT_AGENT_LIST_LIMIT: usize = 50;
const MAX_AGENT_LIST_LIMIT: usize = 100;

#[derive(Debug, Deserialize)]
pub(crate) struct AgentListQuery {
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Debug, Clone)]
struct ThirdPartyAgentEvent {
    event_id: String,
    event_type: String,
    source_kind: String,
    payload: Value,
    requires_commit: bool,
    allowed_actions: Vec<String>,
}

fn servicenet_client(state: &ControlPlaneState) -> Option<&ServiceNetClient> {
    state.servicenet_client.as_deref()
}

fn servicenet_unavailable_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "servicenet is not configured"})),
    )
        .into_response()
}

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

async fn apply_third_party_decision(
    state: &ControlPlaneState,
    event: &ThirdPartyAgentEvent,
    callback: AgentEventCallbackResponse,
) {
    let Some(decision) = callback.decision else {
        return;
    };
    if decision.route != "wattetheria_commit" {
        return;
    }
    let headers =
        forwarded_agent_commit_headers(&state.auth_token, &event.event_id, &decision.decision_id);
    let _ = crate::routes::core::agent_action_commit(
        State(state.clone()),
        headers,
        Json(AgentActionCommitBody {
            event: AgentActionCommitEvent {
                event_id: event.event_id.clone(),
                event_type: event.event_type.clone(),
                source_kind: event.source_kind.clone(),
                source_node_id: None,
                target_agent_id: None,
                payload: event.payload.clone(),
                requires_commit: event.requires_commit,
            },
            decision: AgentActionDecision {
                decision_id: decision.decision_id,
                action: decision.action,
                route: decision.route,
                reason: decision.reason,
                payload: decision.payload,
            },
        }),
    )
    .await;
}

async fn notify_local_agent_of_third_party_result(
    state: &ControlPlaneState,
    operation: &str,
    agent_id: &str,
    task_id: Option<&str>,
    payload: &Value,
) {
    let Some(base_url) = state.agent_event_callback_base_url.as_deref() else {
        return;
    };
    let endpoint = format!("{}/agent-events", base_url.trim_end_matches('/'));
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let dedupe_key = if let Some(task_id) = task_id {
        format!("servicenet:{operation}:{agent_id}:{task_id}:{status}")
    } else {
        format!(
            "servicenet:{operation}:{agent_id}:{}:{status}",
            payload
                .get("receipt_id")
                .and_then(Value::as_str)
                .unwrap_or("immediate")
        )
    };
    let event = ThirdPartyAgentEvent {
        event_id: Uuid::new_v4().to_string(),
        event_type: "third_party_result".to_string(),
        source_kind: "service_net_result".to_string(),
        payload: json!({
            "operation": operation,
            "agent_id": agent_id,
            "task_id": task_id,
            "response": payload,
        }),
        requires_commit: false,
        allowed_actions: vec![
            "inspect_result".to_string(),
            "continue".to_string(),
            "publish_mission".to_string(),
            "claim_mission".to_string(),
            "complete_mission".to_string(),
            "settle_mission".to_string(),
        ],
    };
    let callback = reqwest::Client::new()
        .post(endpoint)
        .json(&AgentEventCallbackRequest {
            event: AgentEventEnvelope {
                event_id: event.event_id.clone(),
                event_type: event.event_type.clone(),
                source_kind: event.source_kind.clone(),
                source_node_id: None,
                target_agent_id: None,
                target_executor: Some(CORE_AGENT_EXECUTOR_NAME.to_owned()),
                payload: event.payload.clone(),
                requires_commit: event.requires_commit,
                allowed_actions: event.allowed_actions.clone(),
                correlation_id: task_id.map(ToOwned::to_owned).or_else(|| {
                    payload
                        .get("receipt_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                }),
                dedupe_key: Some(dedupe_key),
                created_at: Utc::now().timestamp_millis().max(0).cast_unsigned(),
            },
        })
        .send()
        .await
        .ok();
    let callback = if let Some(response) = callback {
        response.json::<AgentEventCallbackResponse>().await.ok()
    } else {
        None
    };
    if let Some(callback) = callback {
        apply_third_party_decision(state, &event, callback).await;
    }
}

pub(crate) async fn list_agents(
    State(state): State<ControlPlaneState>,
    Query(query): Query<AgentListQuery>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return servicenet_unavailable_response();
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_AGENT_LIST_LIMIT)
        .clamp(1, MAX_AGENT_LIST_LIMIT);
    let offset = query.offset.unwrap_or(0);
    let agents = match client.list_agents(limit, offset).await {
        Ok(response) => response,
        Err(error) => return servicenet_error_response(&error),
    };
    let response = json!({
        "items": agents.items,
        "count": agents.count,
        "limit": agents.limit,
        "offset": agents.offset,
        "next_offset": agents.next_offset,
        "has_more": agents.has_more,
        "known_count": agents.known_count,
    });
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "servicenet".to_string(),
        action: "servicenet.agents.list".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(client.base_url().to_string()),
        capability: Some("net.outbound".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": response["count"]})),
    });
    Json(response).into_response()
}

pub(crate) async fn get_agent(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return servicenet_unavailable_response();
    };
    let response = match client.get_agent(&agent_id).await {
        Ok(response) => response,
        Err(error) => return servicenet_error_response(&error),
    };
    append_query_audit(&state, auth, "servicenet.agents.get", &agent_id);
    Json(response).into_response()
}

pub(crate) async fn invoke_agent(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Json(body): Json<ServiceNetInvokeRequest>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return servicenet_unavailable_response();
    };
    let response = match client.invoke_agent(&agent_id, &body).await {
        Ok(response) => response,
        Err(error) => return servicenet_error_response(&error),
    };
    let payload = serde_json::to_value(&response).unwrap_or(Value::Null);
    notify_local_agent_of_third_party_result(&state, "invoke", &agent_id, None, &payload).await;
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "servicenet".to_string(),
        action: "servicenet.agents.invoke".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(agent_id),
        capability: Some("net.outbound".to_string()),
        reason: Some("servicenet.invoke".to_string()),
        duration_ms: None,
        details: Some(json!({
            "status": payload["status"],
            "task_id": payload["task_id"],
            "receipt_id": payload["receipt_id"],
        })),
    });
    Json(payload).into_response()
}

pub(crate) async fn invoke_agent_async(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Json(body): Json<ServiceNetInvokeRequest>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return servicenet_unavailable_response();
    };
    let response = match client.invoke_agent_async(&agent_id, &body).await {
        Ok(response) => response,
        Err(error) => return servicenet_error_response(&error),
    };
    let payload = serde_json::to_value(&response).unwrap_or(Value::Null);
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "servicenet".to_string(),
        action: "servicenet.agents.invoke_async".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(agent_id),
        capability: Some("net.outbound".to_string()),
        reason: Some("servicenet.invoke_async".to_string()),
        duration_ms: None,
        details: Some(json!({
            "status": payload["status"],
            "receipt_id": payload["receipt_id"],
        })),
    });
    Json(payload).into_response()
}

pub(crate) async fn get_agent_task(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path((agent_id, task_id)): Path<(String, String)>,
    Json(body): Json<ServiceNetGetAgentTaskRequest>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return servicenet_unavailable_response();
    };
    let response = match client.get_agent_task(&agent_id, &task_id, &body).await {
        Ok(response) => response,
        Err(error) => return servicenet_error_response(&error),
    };
    let payload = serde_json::to_value(&response).unwrap_or(Value::Null);
    notify_local_agent_of_third_party_result(
        &state,
        "task_get",
        &agent_id,
        Some(&task_id),
        &payload,
    )
    .await;
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "servicenet".to_string(),
        action: "servicenet.agents.task.get".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(agent_id),
        capability: Some("net.outbound".to_string()),
        reason: Some("servicenet.task.get".to_string()),
        duration_ms: None,
        details: Some(json!({
            "task_id": task_id,
            "status": payload["status"],
        })),
    });
    Json(payload).into_response()
}

pub(crate) async fn get_receipt(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(receipt_id): Path<uuid::Uuid>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return servicenet_unavailable_response();
    };
    let response = match client.get_receipt(&receipt_id).await {
        Ok(response) => response,
        Err(error) => return servicenet_error_response(&error),
    };
    append_query_audit(
        &state,
        auth,
        "servicenet.receipts.get",
        &receipt_id.to_string(),
    );
    Json(response).into_response()
}

fn append_query_audit(state: &ControlPlaneState, auth: String, action: &str, subject: &str) {
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: Utc::now().timestamp(),
        category: "servicenet".to_string(),
        action: action.to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(subject.to_string()),
        capability: Some("net.outbound".to_string()),
        reason: None,
        duration_ms: None,
        details: None,
    });
}

fn servicenet_error_response(error: &ServiceNetClientError) -> Response {
    if error.status().is_none() {
        return internal_error(&anyhow::anyhow!(error.to_string()));
    }
    let status = error
        .status()
        .and_then(|status| StatusCode::from_u16(status.as_u16()).ok())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    (status, Json(json!({"error": error.to_string()}))).into_response()
}
