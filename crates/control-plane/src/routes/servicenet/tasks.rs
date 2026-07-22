use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use wattetheria_kernel::servicenet::{
    ServiceNetCancelAgentTaskRequest, ServiceNetGetAgentTaskRequest,
    ServiceNetListAgentTasksRequest, ServiceNetSubscribeAgentTaskRequest,
    cancel_agent_task_envelope_message, get_agent_task_envelope_message,
    list_agent_tasks_envelope_message, subscribe_agent_task_envelope_message,
};

use super::envelope::servicenet_task_agent_envelope;
use super::{
    append_query_audit, notify_local_agent_of_third_party_result, servicenet_client,
    servicenet_error_response, servicenet_unavailable_response,
};
use crate::{auth::authorize, state::ControlPlaneState};

pub(crate) async fn get_agent_task(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path((agent_id, task_id)): Path<(String, String)>,
    Json(mut body): Json<ServiceNetGetAgentTaskRequest>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return servicenet_unavailable_response();
    };
    if body.agent_envelope.is_none() {
        body.agent_envelope = match servicenet_task_agent_envelope(
            &state,
            &agent_id,
            "servicenet.agents.tasks.get",
            get_agent_task_envelope_message(&task_id, &body),
        )
        .await
        {
            Ok(envelope) => Some(envelope),
            Err(error) => return crate::auth::internal_error(&error),
        };
    }
    let response = match client
        .get_service_agent_task(&agent_id, &task_id, &body)
        .await
    {
        Ok(response) => response,
        Err(error) => return servicenet_error_response(&error),
    };
    let payload = serde_json::to_value(&response).unwrap_or(Value::Null);
    Box::pin(notify_local_agent_of_third_party_result(
        &state,
        "task_get",
        &agent_id,
        Some(&task_id),
        &payload,
        body.agent_envelope.as_ref(),
    ))
    .await;
    append_query_audit(&state, auth, "servicenet.agents.tasks.get", &agent_id);
    Json(payload).into_response()
}

pub(crate) async fn list_agent_tasks(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Json(mut body): Json<ServiceNetListAgentTasksRequest>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return servicenet_unavailable_response();
    };
    if body.agent_envelope.is_none() {
        body.agent_envelope = match servicenet_task_agent_envelope(
            &state,
            &agent_id,
            "servicenet.agents.tasks.list",
            list_agent_tasks_envelope_message(&body),
        )
        .await
        {
            Ok(envelope) => Some(envelope),
            Err(error) => return crate::auth::internal_error(&error),
        };
    }
    let response = match client.list_agent_tasks(&agent_id, &body).await {
        Ok(response) => response,
        Err(error) => return servicenet_error_response(&error),
    };
    append_query_audit(&state, auth, "servicenet.agents.tasks.list", &agent_id);
    Json(serde_json::to_value(response).unwrap_or(Value::Null)).into_response()
}

pub(crate) async fn cancel_agent_task(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path((agent_id, task_id)): Path<(String, String)>,
    Json(mut body): Json<ServiceNetCancelAgentTaskRequest>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return servicenet_unavailable_response();
    };
    if body.agent_envelope.is_none() {
        body.agent_envelope = match servicenet_task_agent_envelope(
            &state,
            &agent_id,
            "servicenet.agents.tasks.cancel",
            cancel_agent_task_envelope_message(&task_id),
        )
        .await
        {
            Ok(envelope) => Some(envelope),
            Err(error) => return crate::auth::internal_error(&error),
        };
    }
    let response = match client.cancel_agent_task(&agent_id, &task_id, &body).await {
        Ok(response) => response,
        Err(error) => return servicenet_error_response(&error),
    };
    append_query_audit(&state, auth, "servicenet.agents.tasks.cancel", &agent_id);
    Json(serde_json::to_value(response).unwrap_or(Value::Null)).into_response()
}

pub(crate) async fn subscribe_agent_task(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path((agent_id, task_id)): Path<(String, String)>,
    Json(mut body): Json<ServiceNetSubscribeAgentTaskRequest>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return servicenet_unavailable_response();
    };
    if body.agent_envelope.is_none() {
        body.agent_envelope = match servicenet_task_agent_envelope(
            &state,
            &agent_id,
            "servicenet.agents.tasks.subscribe",
            subscribe_agent_task_envelope_message(&task_id),
        )
        .await
        {
            Ok(envelope) => Some(envelope),
            Err(error) => return crate::auth::internal_error(&error),
        };
    }
    let response = match client
        .subscribe_agent_task(&agent_id, &task_id, &body)
        .await
    {
        Ok(response) => response,
        Err(error) => return servicenet_error_response(&error),
    };
    append_query_audit(&state, auth, "servicenet.agents.tasks.subscribe", &agent_id);
    Json(json!(response)).into_response()
}
