use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error};
use crate::routes::identity::{public_memory_payload, resolve_identity_context};
use crate::state::{
    ControlPlaneState, GalaxyEventBody, GalaxyEventsQuery, GalaxyGenerateBody, StreamEvent,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::emergency::generate_system_galaxy_events;

pub(crate) async fn galaxy_zones(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let zones = state.galaxy_state.lock().await.zones();
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "galaxy".to_string(),
        action: "galaxy.zones.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": zones.len()})),
    });
    Json(zones).into_response()
}

pub(crate) async fn galaxy_events(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<GalaxyEventsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let events = state
        .galaxy_state
        .lock()
        .await
        .events(query.zone_id.as_deref());
    let context = resolve_identity_context(&state, None, Some(&state.agent_did)).await;
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "galaxy".to_string(),
        action: "galaxy.events.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: query.zone_id,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": events.len()})),
    });
    Json(json!({
        "events": events,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
}

pub(crate) async fn galaxy_event_publish(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GalaxyEventBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let mut galaxy = state.galaxy_state.lock().await;
    let event = match galaxy.publish_event(
        body.category,
        &body.zone_id,
        &body.title,
        &body.description,
        body.severity,
        body.expires_at,
        body.tags,
    ) {
        Ok(event) => event,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    if let Err(error) = state
        .local_db
        .save_domain(wattetheria_kernel::local_db::domain::GALAXY_STATE, &*galaxy)
    {
        return internal_error(&error);
    }
    drop(galaxy);

    let context = resolve_identity_context(&state, None, Some(&state.agent_did)).await;
    let payload = public_memory_payload(
        &context,
        "galaxy",
        serde_json::to_value(&event).unwrap_or(Value::Null),
    );
    let _ = state.stream_tx.send(StreamEvent {
        kind: "galaxy.event.published".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event("GALAXY_EVENT_PUBLISHED", payload.clone());

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "galaxy".to_string(),
        action: "galaxy.event.publish".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.zone_id),
        capability: Some("galaxy.event.publish".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    Json(json!({
        "event": event,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
}

pub(crate) async fn galaxy_event_generate(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GalaxyGenerateBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let missions = state.mission_board.lock().await;
    let governance = state.governance_engine.lock().await;
    let mut galaxy = state.galaxy_state.lock().await;
    let generated = match generate_system_galaxy_events(
        &mut galaxy,
        &governance,
        &missions,
        body.max_events.unwrap_or(5).max(1),
    ) {
        Ok(events) => events,
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = state
        .local_db
        .save_domain(wattetheria_kernel::local_db::domain::GALAXY_STATE, &*galaxy)
    {
        return internal_error(&error);
    }
    drop(galaxy);
    drop(governance);
    drop(missions);

    let context = resolve_identity_context(&state, None, Some(&state.agent_did)).await;
    let payload = public_memory_payload(
        &context,
        "galaxy",
        serde_json::to_value(&generated).unwrap_or(Value::Null),
    );
    let _ = state.stream_tx.send(StreamEvent {
        kind: "galaxy.events.generated".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event("GALAXY_EVENTS_GENERATED", payload.clone());
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "galaxy".to_string(),
        action: "galaxy.events.generate".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: Some("galaxy.events.generate".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    Json(json!({
        "events": generated,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
}
