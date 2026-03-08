use anyhow::Result;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::json;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::game::{
    GameComputation, bootstrap_mission_pack, bootstrap_starter_missions, catalog,
    compute_onboarding_flow, compute_onboarding_state, compute_qualifications, compute_status,
    mission_pack_set, starter_mission_set,
};

use crate::auth::{authorize, internal_error};
use crate::routes::experience::build_gameplay_experience;
use crate::routes::identity::{
    IdentityContextView, identity_context_response, public_memory_payload, resolve_identity_context,
};
use crate::routes::organizations::{OrganizationView, build_organization_views};
use crate::state::{ControlPlaneState, GameActionBody, GameStatusQuery, StreamEvent};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GameView {
    pub agent_stats: wattetheria_kernel::types::AgentStats,
    pub scores: wattetheria_kernel::metrics::CivilizationScores,
    pub status: wattetheria_kernel::game::GameStatus,
    pub onboarding: wattetheria_kernel::game::OnboardingState,
    pub onboarding_flow: wattetheria_kernel::game::OnboardingFlow,
    pub starter_missions: Option<wattetheria_kernel::game::StarterMissionSet>,
    pub mission_pack: Option<wattetheria_kernel::game::GameMissionPack>,
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
    let missions = state.mission_board.lock().await;
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
    let starter_missions = context
        .profile
        .as_ref()
        .map(|profile| starter_mission_set(&controller_id, profile, &maps, &missions));
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
    let mission_pack = context.profile.as_ref().map(|profile| {
        mission_pack_set(
            &controller_id,
            profile,
            status.stage.clone(),
            &maps,
            &galaxy,
            &missions,
        )
    });
    let onboarding = compute_onboarding_state(
        &controller_id,
        context.public_identity.as_ref(),
        &status,
        &missions,
    );
    let onboarding_flow = compute_onboarding_flow(
        context.public_identity.as_ref(),
        &status,
        onboarding.clone(),
        starter_missions.as_ref(),
        mission_pack.as_ref(),
    );
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
        onboarding,
        onboarding_flow,
        starter_missions,
        mission_pack,
        travel_state,
        organizations: organization_views,
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
        query.agent_id.as_deref(),
    )
    .await;
    let view = match build_game_view(&state, &context).await {
        Ok(view) => view,
        Err(error) => return internal_error(&error),
    };
    let experience = build_gameplay_experience(&view, None);

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
            "tier": view.status.tier,
            "total_influence": view.status.total_influence,
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "agent_stats": view.agent_stats,
        "scores": view.scores,
        "status": view.status,
        "onboarding": view.onboarding,
        "onboarding_flow": view.onboarding_flow,
        "starter_missions": view.starter_missions,
        "mission_pack": view.mission_pack,
        "travel_state": view.travel_state,
        "organizations": view.organizations,
        "experience": experience,
    }))
    .into_response()
}

pub(crate) async fn game_onboarding(
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
        query.agent_id.as_deref(),
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
        action: "game.onboarding.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: context.public_memory_owner.public.clone(),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "phase": view.onboarding.current_phase,
            "progress_pct": view.onboarding.progress_pct,
            "action_count": view.onboarding_flow.action_cards.len(),
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "status": view.status,
        "onboarding": view.onboarding,
        "onboarding_flow": view.onboarding_flow,
        "starter_missions": view.starter_missions,
        "mission_pack": view.mission_pack,
        "briefing": {
            "hours": briefing["hours"].clone(),
            "human_report": briefing["human_report"].clone(),
            "emergencies": briefing["emergencies"].clone(),
            "strategy": briefing["strategy"].clone(),
        },
    }))
    .into_response()
}

pub(crate) async fn game_mission_pack(
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
        query.agent_id.as_deref(),
    )
    .await;
    let view = match build_game_view(&state, &context).await {
        Ok(view) => view,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "game".to_string(),
        action: "game.mission_pack.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: context.public_memory_owner.public.clone(),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "stage": view.status.stage,
            "template_count": view
                .mission_pack
                .as_ref()
                .map_or(0, |pack| pack.templates.len()),
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "mission_pack": view.mission_pack,
    }))
    .into_response()
}

pub(crate) async fn game_starter_missions(
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
        query.agent_id.as_deref(),
    )
    .await;
    let Some(profile) = context.profile.as_ref() else {
        return Json(json!({"error": "profile not found"})).into_response();
    };
    let controller_id = context.public_memory_owner.controller.clone();
    let board = state.mission_board.lock().await;
    let maps = state.galaxy_map_registry.lock().await;
    let starter_set = starter_mission_set(&controller_id, profile, &maps, &board);
    drop(maps);
    drop(board);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "game".to_string(),
        action: "game.starter_missions.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: context.public_memory_owner.public.clone(),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "template_count": starter_set.templates.len(),
            "existing_count": starter_set.existing.len(),
            "missing_count": starter_set.missing_template_ids.len(),
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "starter_missions": starter_set,
    }))
    .into_response()
}

pub(crate) async fn bootstrap_starter_missions_route(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GameActionBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context =
        resolve_identity_context(&state, body.public_id.as_deref(), body.agent_id.as_deref()).await;
    let Some(profile) = context.profile.as_ref() else {
        return Json(json!({"error": "profile not found"})).into_response();
    };
    let controller_id = context.public_memory_owner.controller.clone();
    let created = {
        let mut board = state.mission_board.lock().await;
        let maps = state.galaxy_map_registry.lock().await;
        let created = bootstrap_starter_missions(&controller_id, profile, &maps, &mut board);
        drop(maps);
        if let Err(error) = board.persist(&state.mission_board_state_path) {
            return internal_error(&error);
        }
        created
    };

    let payload = public_memory_payload(
        &context,
        "game.starter_missions.bootstrap",
        json!({
            "created_count": created.len(),
            "missions": created.clone(),
        }),
    );
    let _ = state.stream_tx.send(StreamEvent {
        kind: "game.starter_missions.bootstrapped".to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.event_log.append_signed(
        "GAME_STARTER_MISSIONS_BOOTSTRAPPED",
        payload.clone(),
        &state.identity,
    );
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "game".to_string(),
        action: "game.starter_missions.bootstrap".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: context.public_memory_owner.public.clone(),
        capability: Some("game.starter_missions.bootstrap".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "created": created,
    }))
    .into_response()
}

pub(crate) async fn bootstrap_mission_pack_route(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GameActionBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context =
        resolve_identity_context(&state, body.public_id.as_deref(), body.agent_id.as_deref()).await;
    let Some(profile) = context.profile.as_ref() else {
        return Json(json!({"error": "profile not found"})).into_response();
    };
    let view = match build_game_view(&state, &context).await {
        Ok(view) => view,
        Err(error) => return internal_error(&error),
    };
    let controller_id = context.public_memory_owner.controller.clone();
    let created = {
        let mut board = state.mission_board.lock().await;
        let maps = state.galaxy_map_registry.lock().await;
        let galaxy = state.galaxy_state.lock().await;
        let created = bootstrap_mission_pack(
            &controller_id,
            profile,
            &view.status.stage,
            &maps,
            &galaxy,
            &mut board,
        );
        drop(galaxy);
        drop(maps);
        if let Err(error) = board.persist(&state.mission_board_state_path) {
            return internal_error(&error);
        }
        created
    };

    let payload = public_memory_payload(
        &context,
        "game.mission_pack.bootstrap",
        json!({
            "created_count": created.len(),
            "stage": view.status.stage,
            "missions": created.clone(),
        }),
    );
    let _ = state.stream_tx.send(StreamEvent {
        kind: "game.mission_pack.bootstrapped".to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.event_log.append_signed(
        "GAME_MISSION_PACK_BOOTSTRAPPED",
        payload.clone(),
        &state.identity,
    );
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "game".to_string(),
        action: "game.mission_pack.bootstrap".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: context.public_memory_owner.public.clone(),
        capability: Some("game.mission_pack.bootstrap".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "created": created,
    }))
    .into_response()
}
