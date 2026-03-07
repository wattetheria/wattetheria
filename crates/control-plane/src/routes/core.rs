use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, extract::ws::WebSocket};
use chrono::Utc;
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error, unauthorized};
use crate::autonomy::{
    build_brain_state, load_night_shift_report, run_autonomy_tick_once, run_demo_market_task,
};
use crate::state::{
    ActionRequest, AuditQuery, AuthQuery, AutonomyTickBody, BrainPlansQuery, ControlPlaneState,
    EventsExportQuery, EventsQuery, NightShiftQuery, StreamEvent, send_stream_text,
};
use axum::extract::ws::Message;
use wattetheria_kernel::audit::AuditEntry;

pub(crate) async fn health(State(state): State<ControlPlaneState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "timestamp": Utc::now().timestamp(),
        "agent_id": state.agent_id,
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

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "state.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"events": events.len(), "pending": pending_count})),
    });

    Json(json!({
        "agent_id": state.agent_id,
        "events": events.len(),
        "pending_policy_requests": pending_count,
        "uptime_sec": Utc::now().timestamp() - state.started_at,
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
        subject: Some(state.agent_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"hours": hours})),
    });

    Json(report).into_response()
}

pub(crate) async fn night_shift_humanized(
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
        kind: "brain.humanized_night_shift".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "brain".to_string(),
        action: "night_shift.humanized".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_id.clone()),
        capability: Some("model.invoke".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(json!({"hours": hours})),
    });

    Json(payload).into_response()
}

pub(crate) async fn brain_propose_actions(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let brain_state = match build_brain_state(&state, state.autonomy_skill_planner_enabled).await {
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
        subject: Some(state.agent_id.clone()),
        capability: Some("model.invoke".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": proposals.len()})),
    });

    Json(proposals).into_response()
}

pub(crate) async fn brain_plan_skill_calls(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<BrainPlansQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let enable_skill_planner = query.enable.unwrap_or(state.autonomy_skill_planner_enabled);
    let brain_state = match build_brain_state(&state, enable_skill_planner).await {
        Ok(value) => value,
        Err(error) => return internal_error(&error),
    };

    let plans = match state.brain_engine.plan_skill_calls(&brain_state).await {
        Ok(plans) => plans,
        Err(error) => return internal_error(&error),
    };

    let payload = serde_json::to_value(&plans).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: "brain.skill_plans".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "brain".to_string(),
        action: "brain.plan_skill_calls".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_id.clone()),
        capability: Some("model.invoke".to_string()),
        reason: Some(format!("skill_planner_enabled={enable_skill_planner}")),
        duration_ms: None,
        details: Some(json!({"count": plans.len()})),
    });

    Json(plans).into_response()
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
    let enable_skill_planner = body
        .enable_skill_planner
        .unwrap_or(state.autonomy_skill_planner_enabled);

    let result = match run_autonomy_tick_once(&state, hours, enable_skill_planner).await {
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
        subject: Some(state.agent_id.clone()),
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
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let result = match request.action.as_str() {
        "task.run_demo_market" => match run_demo_market_task(&state).await {
            Ok(payload) => payload,
            Err(error) => return internal_error(&error),
        },
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("unsupported action: {}", request.action)})),
            )
                .into_response();
        }
    };

    let _ = state.stream_tx.send(StreamEvent {
        kind: "action.result".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: result.clone(),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "action.exec".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_id.clone()),
        capability: Some("task.run_demo_market".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(result.clone()),
    });

    Json(result).into_response()
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
        "agent_id": state.agent_id,
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
