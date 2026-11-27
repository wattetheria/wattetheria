use anyhow::Result;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::game::{
    BootstrapFlow, BootstrapState, GameComputation, catalog, compute_bootstrap_flow,
    compute_bootstrap_state, compute_qualifications, compute_status,
};

use crate::auth::{authorize, internal_error};
use crate::routes::identity::{
    IdentityContextView, identity_context_response, resolve_identity_context,
};
use crate::routes::organizations::{OrganizationView, build_organization_views};
use crate::routes::supervision::build_supervision_view;
use crate::state::{ControlPlaneState, GameStatusQuery};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GameView {
    pub agent_stats: wattetheria_kernel::types::AgentStats,
    pub scores: wattetheria_kernel::metrics::CivilizationScores,
    pub status: wattetheria_kernel::game::GameStatus,
    pub bootstrap: BootstrapState,
    pub bootstrap_flow: BootstrapFlow,
    pub travel_state: Option<wattetheria_kernel::map::TravelStateRecord>,
    pub organizations: Vec<OrganizationView>,
}

async fn resolve_agent_stats(
    state: &ControlPlaneState,
    controller_id: &str,
) -> Result<wattetheria_kernel::types::AgentStats> {
    state
        .swarm_bridge
        .agent_view(controller_id)
        .await
        .map(|view| view.stats)
}

pub(crate) async fn build_game_view(
    state: &ControlPlaneState,
    context: &IdentityContextView,
) -> Result<GameView> {
    let controller_id = context.public_memory_owner.controller.clone();
    let agent_stats = resolve_agent_stats(state, &controller_id).await?;
    let missions = crate::routes::gateway_missions::board_view(state).await?;
    let profiles = state.citizen_registry.lock().await;
    let governance = state.governance_engine.lock().await;
    let galaxy = state.galaxy_state.lock().await;
    let maps = state.galaxy_map_registry.lock().await;
    let organizations = state.organization_registry.lock().await;
    let scores = wattetheria_kernel::metrics::compute_scores(
        &controller_id,
        &agent_stats,
        &missions,
        &profiles,
        &governance,
        &galaxy,
    );
    let qualifications = compute_qualifications(
        &controller_id,
        context.profile.as_ref(),
        &scores,
        &missions,
        &governance,
    );
    let status = compute_status(
        &controller_id,
        context.profile.as_ref(),
        GameComputation {
            stats: &agent_stats,
            scores: &scores,
            missions: &missions,
            governance: &governance,
            maps: &maps,
            qualifications,
        },
    );
    let bootstrap = compute_bootstrap_state(context.public_identity.as_ref(), &status);
    let bootstrap_flow = compute_bootstrap_flow(&status, bootstrap.clone());
    let travel_state = if let Some(identity) = context.public_identity.as_ref() {
        state
            .travel_state_registry
            .lock()
            .await
            .get(&identity.public_id)
    } else {
        None
    };
    let organization_views = context
        .public_identity
        .as_ref()
        .map(|identity| {
            build_organization_views(&organizations, &missions, &governance, &identity.public_id)
        })
        .unwrap_or_default();
    Ok(GameView {
        agent_stats,
        scores,
        status,
        bootstrap,
        bootstrap_flow,
        travel_state,
        organizations: organization_views,
    })
}

#[must_use]
pub(crate) fn game_view_payload(view: &GameView) -> Value {
    json!({
        "agent_stats": view.agent_stats,
        "scores": view.scores,
        "status": view.status,
        "bootstrap": view.bootstrap.clone(),
        "bootstrap_flow": view.bootstrap_flow.clone(),
        "travel_state": view.travel_state,
        "organizations": view.organizations,
    })
}

pub(crate) async fn game_catalog(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let payload = catalog();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "game".to_string(),
        action: "game.catalog.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "roles": payload.roles.len(),
            "factions": payload.factions.len(),
            "stages": payload.stages.len(),
        })),
    });

    Json(payload).into_response()
}

pub(crate) async fn game_status(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<GameStatusQuery>,
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
    let view = match build_game_view(&state, &context).await {
        Ok(view) => view,
        Err(error) => return internal_error(&error),
    };
    let supervision = build_supervision_view(&view, None);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "game".to_string(),
        action: "game.status.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: context.public_memory_owner.public.clone(),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "stage": view.status.stage,
            "total_influence": view.status.total_influence,
        })),
    });

    let mut payload = game_view_payload(&view)
        .as_object()
        .cloned()
        .unwrap_or_default();
    payload.insert(
        "identity".to_string(),
        json!(identity_context_response(&context)),
    );
    payload.insert("supervision".to_string(), json!(supervision));
    Json(payload).into_response()
}

pub(crate) async fn game_bootstrap_payload(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<GameStatusQuery>,
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
    let view = match build_game_view(&state, &context).await {
        Ok(view) => view,
        Err(error) => return internal_error(&error),
    };
    let briefing = match crate::autonomy::build_operator_briefing(&state, 12).await {
        Ok(briefing) => briefing,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "game".to_string(),
        action: "game.bootstrap.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: context.public_memory_owner.public.clone(),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "phase": view.bootstrap.current_phase,
            "progress_pct": view.bootstrap.progress_pct,
            "action_count": view.bootstrap_flow.action_cards.len(),
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "status": view.status,
        "bootstrap": view.bootstrap,
        "bootstrap_flow": view.bootstrap_flow,
        "briefing": {
            "hours": briefing["hours"].clone(),
            "human_report": briefing["human_report"].clone(),
            "emergencies": briefing["emergencies"].clone(),
            "strategy": briefing["strategy"].clone(),
        },
    }))
    .into_response()
}

pub(crate) async fn game_bootstrap(
    state: State<ControlPlaneState>,
    headers: HeaderMap,
    query: Query<GameStatusQuery>,
) -> Response {
    game_bootstrap_payload(state, headers, query).await
}
