use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error};
use crate::autonomy::build_operator_briefing;
use crate::state::{
    CitizenProfileBody, CitizenProfileQuery, ControlPlaneState, EmergencyQuery, MetricsQuery,
    NightShiftQuery, StreamEvent, WorldEventBody, WorldEventsQuery, WorldGenerateBody,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::emergency::{evaluate_emergencies, generate_system_world_events};
use wattetheria_kernel::metrics::compute_scores;

pub(crate) async fn citizen_profile(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<CitizenProfileQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let agent_id = query.agent_id.unwrap_or_else(|| state.agent_id.clone());
    let registry = state.citizen_registry.lock().await;
    let profile = registry.profile(&agent_id);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.profile.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(agent_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: None,
    });

    Json(json!({"profile": profile})).into_response()
}

pub(crate) async fn citizen_profile_upsert(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<CitizenProfileBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let mut registry = state.citizen_registry.lock().await;
    let profile = registry.set_profile(
        &body.agent_id,
        body.faction,
        body.role,
        body.strategy,
        body.home_subnet_id,
        body.home_zone_id,
    );
    if let Err(error) = registry.persist(&state.citizen_registry_state_path) {
        return internal_error(&error);
    }
    drop(registry);

    let payload = serde_json::to_value(&profile).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: "civilization.profile.updated".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.event_log.append_signed(
        "CIVILIZATION_PROFILE_UPDATED",
        payload.clone(),
        &state.identity,
    );

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.profile.update".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.agent_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    Json(profile).into_response()
}

pub(crate) async fn civilization_metrics(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<MetricsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let agent_id = query.agent_id.unwrap_or_else(|| state.agent_id.clone());
    let agent_stats = match state.swarm_bridge.agent_view(&agent_id).await {
        Ok(view) => view.stats,
        Err(error) => return internal_error(&error),
    };
    let missions = state.mission_board.lock().await;
    let profiles = state.citizen_registry.lock().await;
    let governance = state.governance_engine.lock().await;
    let world = state.world_state.lock().await;
    let scores = compute_scores(
        &agent_id,
        &agent_stats,
        &missions,
        &profiles,
        &governance,
        &world,
    );
    drop(world);
    drop(governance);
    drop(profiles);
    drop(missions);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.metrics.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(agent_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(serde_json::to_value(&scores).unwrap_or(Value::Null)),
    });

    Json(scores).into_response()
}

pub(crate) async fn world_zones(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let zones = state.world_state.lock().await.zones();
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "world".to_string(),
        action: "world.zones.query".to_string(),
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

pub(crate) async fn world_events(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<WorldEventsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let events = state
        .world_state
        .lock()
        .await
        .events(query.zone_id.as_deref());
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "world".to_string(),
        action: "world.events.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: query.zone_id,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": events.len()})),
    });
    Json(events).into_response()
}

pub(crate) async fn world_event_publish(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<WorldEventBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let mut world = state.world_state.lock().await;
    let event = match world.publish_event(
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
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    if let Err(error) = world.persist(&state.world_state_path) {
        return internal_error(&error);
    }
    drop(world);

    let payload = serde_json::to_value(&event).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: "world.event.published".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ =
        state
            .event_log
            .append_signed("WORLD_EVENT_PUBLISHED", payload.clone(), &state.identity);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "world".to_string(),
        action: "world.event.publish".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.zone_id),
        capability: Some("world.event.publish".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    Json(event).into_response()
}

pub(crate) async fn world_event_generate(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<WorldGenerateBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let missions = state.mission_board.lock().await;
    let governance = state.governance_engine.lock().await;
    let mut world = state.world_state.lock().await;
    let generated = match generate_system_world_events(
        &mut world,
        &governance,
        &missions,
        body.max_events.unwrap_or(5).max(1),
    ) {
        Ok(events) => events,
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = world.persist(&state.world_state_path) {
        return internal_error(&error);
    }
    drop(world);
    drop(governance);
    drop(missions);

    let payload = serde_json::to_value(&generated).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: "world.events.generated".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ =
        state
            .event_log
            .append_signed("WORLD_EVENTS_GENERATED", payload.clone(), &state.identity);
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "world".to_string(),
        action: "world.events.generate".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: Some("world.events.generate".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    Json(generated).into_response()
}

pub(crate) async fn civilization_emergencies(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<EmergencyQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let agent_id = query.agent_id.unwrap_or_else(|| state.agent_id.clone());
    let missions = state.mission_board.lock().await;
    let profiles = state.citizen_registry.lock().await;
    let governance = state.governance_engine.lock().await;
    let world = state.world_state.lock().await;
    let emergencies = evaluate_emergencies(&agent_id, &profiles, &missions, &governance, &world);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.emergencies.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(agent_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": emergencies.len()})),
    });

    Json(emergencies).into_response()
}

pub(crate) async fn civilization_briefing(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<NightShiftQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let hours = query.hours.unwrap_or(12).max(1);
    let briefing = match build_operator_briefing(&state, hours).await {
        Ok(briefing) => briefing,
        Err(error) => return internal_error(&error),
    };
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.briefing.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_id.clone()),
        capability: None,
        reason: Some(format!("hours={hours}")),
        duration_ms: None,
        details: Some(json!({"emergencies": briefing["emergencies"]})),
    });
    Json(briefing).into_response()
}
