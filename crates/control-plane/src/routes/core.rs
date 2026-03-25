use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, extract::ws::WebSocket};
use chrono::Utc;
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error, unauthorized};
use crate::autonomy::{build_brain_state, load_night_shift_report, run_autonomy_tick_once};
use crate::routes::identity::identity_context_value;
use crate::state::{
    ActionRequest, AuditQuery, AuthQuery, AutonomyTickBody, ControlPlaneState, EventsExportQuery,
    EventsQuery, NightShiftQuery, StreamEvent, send_stream_text,
};
use axum::extract::ws::Message;
use wattetheria_kernel::audit::AuditEntry;

pub(crate) async fn health(State(state): State<ControlPlaneState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "timestamp": Utc::now().timestamp(),
        "agent_did": state.agent_did,
        "uptime_sec": Utc::now().timestamp() - state.started_at,
    }))
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

    let human = match state.brain_engine.humanize_night_shift(&report).await {
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

    let proposals = match state.brain_engine.propose_actions(&brain_state).await {
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
