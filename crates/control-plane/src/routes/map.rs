use anyhow::Context;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::json;

use crate::auth::authorize;
use crate::routes::identity::{
    identity_context_response, public_memory_payload, resolve_identity_context,
};
use crate::state::{
    ControlPlaneState, GalaxyMapQuery, GalaxyTravelArriveBody, GalaxyTravelDepartBody,
    GalaxyTravelOptionsQuery, GalaxyTravelPlanQuery, GalaxyTravelStateQuery,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::map::consequence::evaluate_arrival_consequence;
use wattetheria_kernel::map::state::{resolve_anchor_position, resolve_system_position};
use wattetheria_kernel::map::travel::{travel_options, travel_plan};

pub(crate) async fn galaxy_map(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<GalaxyMapQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let registry = state.galaxy_map_registry.lock().await;
    let map_id = query.map_id.as_deref().unwrap_or("genesis-base");
    let map = registry.get(map_id);
    drop(registry);

    let Some(map) = map else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error": "galaxy map not found"})),
        )
            .into_response();
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "map".to_string(),
        action: "map.get".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(map.map_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"systems": map.systems.len(), "routes": map.routes.len()})),
    });

    Json(map).into_response()
}

pub(crate) async fn galaxy_maps(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let registry = state.galaxy_map_registry.lock().await;
    let maps = registry.list();
    drop(registry);
    let summaries: Vec<_> = maps.into_iter().map(|map| map.summary()).collect();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "map".to_string(),
        action: "map.list".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": summaries.len()})),
    });

    Json(json!({ "maps": summaries })).into_response()
}

pub(crate) async fn galaxy_travel_options(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<GalaxyTravelOptionsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let map_id = query.map.as_deref().unwrap_or("genesis-base");
    let registry = state.galaxy_map_registry.lock().await;
    let Some(map) = registry.get(map_id) else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error": "galaxy map not found"})),
        )
            .into_response();
    };
    drop(registry);

    let context = resolve_identity_context(
        &state,
        query.public_identity.as_deref(),
        query.controller.as_deref(),
    )
    .await;
    let current_system_id = resolve_current_system_id(&state, &context).await;
    let from_system_id = query
        .from_system
        .clone()
        .or(current_system_id)
        .or_else(|| resolve_home_system_id(&map, context.profile.as_ref()))
        .unwrap_or_else(|| "genesis-prime".to_string());
    if !map
        .systems
        .iter()
        .any(|system| system.system_id == from_system_id)
    {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "unknown origin system"})),
        )
            .into_response();
    }
    let galaxy = state.galaxy_state.lock().await;
    let options = travel_options(&map, &galaxy, &from_system_id);
    drop(galaxy);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "map".to_string(),
        action: "map.travel_options".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(from_system_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "map_id": map.map_id,
            "option_count": options.len(),
        })),
    });

    Json(json!({
        "map_id": map_id,
        "from_system_id": from_system_id,
        "options": options,
    }))
    .into_response()
}

pub(crate) async fn galaxy_travel_plan(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<GalaxyTravelPlanQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let map_id = query.map.as_deref().unwrap_or("genesis-base");
    let registry = state.galaxy_map_registry.lock().await;
    let Some(map) = registry.get(map_id) else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error": "galaxy map not found"})),
        )
            .into_response();
    };
    drop(registry);

    let context = resolve_identity_context(
        &state,
        query.public_identity.as_deref(),
        query.controller.as_deref(),
    )
    .await;
    let current_system_id = resolve_current_system_id(&state, &context).await;
    let from_system_id = query
        .from_system
        .clone()
        .or(current_system_id)
        .or_else(|| resolve_home_system_id(&map, context.profile.as_ref()))
        .unwrap_or_else(|| "genesis-prime".to_string());
    let galaxy = state.galaxy_state.lock().await;
    let plan = match travel_plan(&map, &galaxy, &from_system_id, &query.destination) {
        Ok(plan) => plan,
        Err(error) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    drop(galaxy);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "map".to_string(),
        action: "map.travel_plan".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(query.destination.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "map_id": map.map_id,
            "from_system_id": from_system_id,
            "to_system_id": query.destination,
            "legs": plan.legs.len(),
        })),
    });

    Json(plan).into_response()
}

pub(crate) async fn galaxy_travel_state(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<GalaxyTravelStateQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(
        &state,
        query.public_identity.as_deref(),
        query.controller.as_deref(),
    )
    .await;
    let Some(public_id) = context
        .public_identity
        .as_ref()
        .map(|identity| identity.public_id.clone())
        .or_else(|| {
            context
                .controller_binding
                .as_ref()
                .map(|binding| binding.public_id.clone())
        })
    else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "no public identity resolved"})),
        )
            .into_response();
    };

    let record = match ensure_travel_state_record(&state, &context, &public_id).await {
        Ok(record) => record,
        Err(error) => return internal_map_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "map".to_string(),
        action: "map.travel_state.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(public_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "active_session": record.active_session.is_some(),
            "system_id": record.current_position.system_id,
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "travel_state": record,
    }))
    .into_response()
}

pub(crate) async fn galaxy_travel_depart(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GalaxyTravelDepartBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let (context, public_id) = match resolve_travel_identity(
        &state,
        body.public_identity.as_deref(),
        body.controller.as_deref(),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    let current_record = match ensure_travel_state_record(&state, &context, &public_id).await {
        Ok(record) => record,
        Err(error) => return internal_map_error(&error),
    };
    let plan = match build_depart_plan(&state, &body, &current_record).await {
        Ok(plan) => plan,
        Err(response) => return response,
    };
    let record = match persist_departure(&state, &public_id, &context, plan).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let payload = travel_public_payload(&context, "travel_departed", &record);
    emit_travel_activity(
        &state,
        "galaxy.travel.departed",
        "GALAXY_TRAVEL_DEPARTED",
        "map.travel_depart",
        auth,
        public_id,
        payload.clone(),
    );

    Json(json!({
        "identity": identity_context_response(&context),
        "travel_state": record,
    }))
    .into_response()
}

pub(crate) async fn galaxy_travel_arrive(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GalaxyTravelArriveBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let (context, public_id) = match resolve_travel_identity(
        &state,
        body.public_identity.as_deref(),
        body.controller.as_deref(),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    let current_record = match ensure_travel_state_record(&state, &context, &public_id).await {
        Ok(record) => record,
        Err(error) => return internal_map_error(&error),
    };
    let Some(completed_session) = current_record.active_session.clone() else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "no active travel session"})),
        )
            .into_response();
    };
    let position = match resolve_arrival_position(&state, &completed_session).await {
        Ok(position) => position,
        Err(response) => return response,
    };
    let consequence =
        build_arrival_consequence(&state, &context, &public_id, &position, &completed_session)
            .await;
    let record = {
        let mut registry = state.travel_state_registry.lock().await;
        let record = match registry.arrive_with(
            &public_id,
            &context.public_memory_owner.controller,
            position,
            Some(consequence),
        ) {
            Ok(record) => record,
            Err(error) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        };
        if let Err(error) = state.local_db.save_domain(
            wattetheria_kernel::local_db::domain::TRAVEL_STATE_REGISTRY,
            &*registry,
        ) {
            return internal_map_error(&error);
        }
        record
    };
    let payload = travel_public_payload(&context, "travel_arrived", &record);
    emit_travel_activity(
        &state,
        "galaxy.travel.arrived",
        "GALAXY_TRAVEL_ARRIVED",
        "map.travel_arrive",
        auth,
        public_id,
        payload.clone(),
    );

    Json(json!({
        "identity": identity_context_response(&context),
        "travel_state": record,
    }))
    .into_response()
}

fn resolve_home_system_id(
    map: &wattetheria_kernel::map::GalaxyMap,
    profile: Option<&wattetheria_kernel::profiles::CitizenProfile>,
) -> Option<String> {
    let profile = profile?;
    resolve_anchor_position(
        map,
        profile.home_subnet_id.as_deref(),
        profile.home_zone_id.as_deref(),
    )
    .map(|position| position.system_id)
}

async fn resolve_current_system_id(
    state: &ControlPlaneState,
    context: &crate::routes::identity::IdentityContextView,
) -> Option<String> {
    let public_id = context
        .public_identity
        .as_ref()
        .map(|identity| identity.public_id.clone())
        .or_else(|| {
            context
                .controller_binding
                .as_ref()
                .map(|binding| binding.public_id.clone())
        })?;
    state
        .travel_state_registry
        .lock()
        .await
        .get(&public_id)
        .map(|record| record.current_position.system_id)
}

async fn ensure_travel_state_record(
    state: &ControlPlaneState,
    context: &crate::routes::identity::IdentityContextView,
    public_id: &str,
) -> anyhow::Result<wattetheria_kernel::map::TravelStateRecord> {
    if let Some(record) = state.travel_state_registry.lock().await.get(public_id) {
        return Ok(record);
    }
    let map = state
        .galaxy_map_registry
        .lock()
        .await
        .get("genesis-base")
        .context("default galaxy map missing")?;
    let position = resolve_anchor_position(
        &map,
        context
            .profile
            .as_ref()
            .and_then(|profile| profile.home_subnet_id.as_deref()),
        context
            .profile
            .as_ref()
            .and_then(|profile| profile.home_zone_id.as_deref()),
    )
    .context("unable to resolve travel anchor position")?;
    let mut registry = state.travel_state_registry.lock().await;
    let record =
        registry.ensure_position(public_id, &context.public_memory_owner.controller, position);
    state.local_db.save_domain(
        wattetheria_kernel::local_db::domain::TRAVEL_STATE_REGISTRY,
        &*registry,
    )?;
    Ok(record)
}

async fn resolve_travel_identity(
    state: &ControlPlaneState,
    public_id: Option<&str>,
    controller_id: Option<&str>,
) -> Result<(crate::routes::identity::IdentityContextView, String), Response> {
    let context = resolve_identity_context(state, public_id, controller_id).await;
    let Some(public_id) = context
        .public_identity
        .as_ref()
        .map(|identity| identity.public_id.clone())
        .or_else(|| {
            context
                .controller_binding
                .as_ref()
                .map(|binding| binding.public_id.clone())
        })
    else {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "no public identity resolved"})),
        )
            .into_response());
    };
    Ok((context, public_id))
}

async fn build_depart_plan(
    state: &ControlPlaneState,
    body: &GalaxyTravelDepartBody,
    current_record: &wattetheria_kernel::map::TravelStateRecord,
) -> Result<wattetheria_kernel::map::TravelPlan, Response> {
    let map_id = body
        .map
        .clone()
        .unwrap_or_else(|| current_record.current_position.map_id.clone());
    let registry = state.galaxy_map_registry.lock().await;
    let Some(map) = registry.get(&map_id) else {
        return Err((
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error": "galaxy map not found"})),
        )
            .into_response());
    };
    drop(registry);
    let galaxy = state.galaxy_state.lock().await;
    travel_plan(
        &map,
        &galaxy,
        &current_record.current_position.system_id,
        &body.destination,
    )
    .map_err(|error| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": error.to_string()})),
        )
            .into_response()
    })
}

async fn persist_departure(
    state: &ControlPlaneState,
    public_id: &str,
    context: &crate::routes::identity::IdentityContextView,
    plan: wattetheria_kernel::map::TravelPlan,
) -> Result<wattetheria_kernel::map::TravelStateRecord, Response> {
    let mut registry = state.travel_state_registry.lock().await;
    let record = registry
        .depart(public_id, &context.public_memory_owner.controller, plan)
        .map_err(|error| {
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response()
        })?;
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::TRAVEL_STATE_REGISTRY,
            &*registry,
        )
        .map_err(|error| internal_map_error(&error))?;
    Ok(record)
}

async fn resolve_arrival_position(
    state: &ControlPlaneState,
    session: &wattetheria_kernel::map::TravelSession,
) -> Result<wattetheria_kernel::map::TravelPosition, Response> {
    let registry = state.galaxy_map_registry.lock().await;
    let Some(map) = registry.get(&session.map_id) else {
        return Err((
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error": "galaxy map not found"})),
        )
            .into_response());
    };
    resolve_system_position(&map, &session.to_system_id).ok_or_else(|| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "arrival system missing from map"})),
        )
            .into_response()
    })
}

async fn build_arrival_consequence(
    state: &ControlPlaneState,
    context: &crate::routes::identity::IdentityContextView,
    public_id: &str,
    position: &wattetheria_kernel::map::TravelPosition,
    session: &wattetheria_kernel::map::TravelSession,
) -> wattetheria_kernel::map::TravelConsequence {
    let map = state
        .galaxy_map_registry
        .lock()
        .await
        .get(&session.map_id)
        .expect("arrival map should exist");
    let missions = state.mission_board.lock().await;
    let governance = state.governance_engine.lock().await;
    evaluate_arrival_consequence(
        public_id,
        &map,
        position,
        session,
        context.profile.as_ref(),
        &missions,
        &governance,
    )
}

fn travel_public_payload(
    context: &crate::routes::identity::IdentityContextView,
    event: &str,
    record: &wattetheria_kernel::map::TravelStateRecord,
) -> serde_json::Value {
    public_memory_payload(
        context,
        "travel",
        json!({
            "event": event,
            "travel_state": record,
        }),
    )
}

fn emit_travel_activity(
    state: &ControlPlaneState,
    stream_kind: &str,
    event_type: &str,
    audit_action: &str,
    actor: String,
    public_id: String,
    payload: serde_json::Value,
) {
    let _ = state.stream_tx.send(crate::state::StreamEvent {
        kind: stream_kind.to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event(event_type, payload.clone());
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "map".to_string(),
        action: audit_action.to_string(),
        status: "ok".to_string(),
        actor: Some(actor),
        subject: Some(public_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });
}

fn internal_map_error(error: &anyhow::Error) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": error.to_string()})),
    )
        .into_response()
}
