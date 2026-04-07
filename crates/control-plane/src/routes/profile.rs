use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error};
use crate::autonomy::build_operator_briefing;
use crate::routes::identity::{identity_context_response, resolve_identity_context};
use crate::state::{
    CitizenProfileQuery, ControlPlaneState, EmergencyQuery, MetricsQuery, NightShiftQuery,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::emergency::evaluate_emergencies;
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
    let context = resolve_identity_context(
        &state,
        query.public_id.as_deref(),
        query.agent_did.as_deref(),
    )
    .await;
    let subject = context
        .public_memory_owner
        .public
        .clone()
        .unwrap_or_else(|| context.public_memory_owner.controller.clone());

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.profile.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(subject),
        capability: None,
        reason: None,
        duration_ms: None,
        details: None,
    });

    Json(identity_context_response(&context)).into_response()
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
    let context = resolve_identity_context(
        &state,
        query.public_id.as_deref(),
        query.agent_did.as_deref(),
    )
    .await;
    let agent_did = context.public_memory_owner.controller.clone();
    let agent_stats = match state.swarm_bridge.agent_view(&agent_did).await {
        Ok(view) => view.stats,
        Err(error) => return internal_error(&error),
    };
    let missions = state.mission_board.lock().await;
    let profiles = state.citizen_registry.lock().await;
    let governance = state.governance_engine.lock().await;
    let galaxy = state.galaxy_state.lock().await;
    let scores = compute_scores(
        &agent_did,
        &agent_stats,
        &missions,
        &profiles,
        &governance,
        &galaxy,
    );
    drop(galaxy);
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
        subject: Some(
            context
                .public_memory_owner
                .public
                .clone()
                .unwrap_or_else(|| agent_did.clone()),
        ),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(serde_json::to_value(&scores).unwrap_or(Value::Null)),
    });

    Json(json!({
        "metrics": scores,
        "public_identity": context.public_identity,
        "controller_binding": context.controller_binding,
        "profile": context.profile,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
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
    let context = resolve_identity_context(
        &state,
        query.public_id.as_deref(),
        query.agent_did.as_deref(),
    )
    .await;
    let agent_did = context.public_memory_owner.controller.clone();
    let missions = state.mission_board.lock().await;
    let profiles = state.citizen_registry.lock().await;
    let governance = state.governance_engine.lock().await;
    let galaxy = state.galaxy_state.lock().await;
    let emergencies = evaluate_emergencies(&agent_did, &profiles, &missions, &governance, &galaxy);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.emergencies.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(
            context
                .public_memory_owner
                .public
                .clone()
                .unwrap_or_else(|| agent_did.clone()),
        ),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": emergencies.len()})),
    });

    Json(json!({
        "emergencies": emergencies,
        "public_identity": context.public_identity,
        "controller_binding": context.controller_binding,
        "profile": context.profile,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
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
        subject: Some(state.agent_did.clone()),
        capability: None,
        reason: Some(format!("hours={hours}")),
        duration_ms: None,
        details: Some(json!({"emergencies": briefing["emergencies"]})),
    });
    let context = resolve_identity_context(&state, None, Some(&state.agent_did)).await;
    Json(json!({
        "briefing": briefing,
        "public_identity": context.public_identity,
        "controller_binding": context.controller_binding,
        "profile": context.profile,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
}

pub(crate) async fn supervision_briefing(
    state: State<ControlPlaneState>,
    headers: HeaderMap,
    query: Query<NightShiftQuery>,
) -> Response {
    civilization_briefing(state, headers, query).await
}
