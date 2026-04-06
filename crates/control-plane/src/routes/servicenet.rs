use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::servicenet::{
    ServiceNetClient, ServiceNetClientError, ServiceNetGetAgentTaskRequest, ServiceNetInvokeRequest,
};

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

pub(crate) async fn list_agents(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return servicenet_unavailable_response();
    };
    let items = match client.list_agents().await {
        Ok(items) => items,
        Err(error) => return servicenet_error_response(&error),
    };
    let response = json!({
        "items": items,
        "count": items.len(),
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
