use anyhow::Result;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::missions::{CivilMission, MissionStatus};
use wattetheria_kernel::civilization::{emergency, metrics};
use wattetheria_kernel::governance::GovernmentStatus;

use crate::auth::{authorize, internal_error};
use crate::autonomy::build_operator_briefing;
use crate::routes::identity::{
    IdentityContextView, identity_context_response, resolve_identity_context,
};
use crate::state::{
    BootstrapCatalogQuery, ControlPlaneState, DashboardHomeQuery, MyGovernanceQuery,
    MyMissionsQuery,
};

fn mission_matches_profile(
    mission: &CivilMission,
    profile: Option<&wattetheria_kernel::profiles::CitizenProfile>,
) -> bool {
    let role_ok = mission
        .required_role
        .as_ref()
        .is_none_or(|required| profile.is_some_and(|profile| &profile.role == required));
    let faction_ok = mission
        .required_faction
        .as_ref()
        .is_none_or(|required| profile.is_some_and(|profile| &profile.faction == required));
    role_ok && faction_ok
}

struct DashboardView {
    agent_stats: wattetheria_kernel::types::AgentStats,
    metrics: metrics::CivilizationScores,
    emergencies: Vec<emergency::EmergencyState>,
    briefing: serde_json::Value,
    home_planet: Option<wattetheria_kernel::governance::SubnetPlanet>,
    home_zone: Option<wattetheria_kernel::civilization::galaxy::GalaxyZone>,
    home_zone_events: Vec<wattetheria_kernel::civilization::galaxy::DynamicEvent>,
    eligible_open_count: usize,
    active_count: usize,
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

async fn build_dashboard_view(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    hours: i64,
) -> Result<DashboardView> {
    let controller_id = context.public_memory_owner.controller.clone();
    let agent_stats = resolve_agent_stats(state, &controller_id).await?;
    let missions = state.mission_board.lock().await;
    let profiles = state.citizen_registry.lock().await;
    let governance = state.governance_engine.lock().await;
    let galaxy = state.galaxy_state.lock().await;
    let metrics = metrics::compute_scores(
        &controller_id,
        &agent_stats,
        &missions,
        &profiles,
        &governance,
        &galaxy,
    );
    let emergencies =
        emergency::evaluate_emergencies(&controller_id, &profiles, &missions, &governance, &galaxy);
    let profile = context.profile.as_ref();
    let home_planet = profile
        .and_then(|profile| profile.home_subnet_id.as_deref())
        .and_then(|subnet_id| governance.planet(subnet_id).cloned());
    let home_zone = profile
        .and_then(|profile| profile.home_zone_id.as_deref())
        .and_then(|zone_id| {
            galaxy
                .zones()
                .into_iter()
                .find(|zone| zone.zone_id == zone_id)
        });
    let eligible_open_count = missions
        .list(Some(&MissionStatus::Open))
        .into_iter()
        .filter(|mission| mission_matches_profile(mission, profile))
        .count();
    let active_count = missions
        .list(None)
        .into_iter()
        .filter(|mission| {
            matches!(
                mission.status,
                MissionStatus::Claimed | MissionStatus::Completed
            ) && mission.claimed_by.as_deref() == Some(controller_id.as_str())
        })
        .count();
    let home_zone_events = profile
        .and_then(|profile| profile.home_zone_id.as_deref())
        .map_or_else(
            || galaxy.events(None),
            |zone_id| galaxy.events(Some(zone_id)),
        );
    drop(galaxy);
    drop(governance);
    drop(profiles);
    drop(missions);

    let briefing = build_operator_briefing(state, hours).await?;
    Ok(DashboardView {
        agent_stats,
        metrics,
        emergencies,
        briefing,
        home_planet,
        home_zone,
        home_zone_events,
        eligible_open_count,
        active_count,
    })
}

pub(crate) async fn client_characters(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let public_ids = {
        let registry = state.public_identity_registry.lock().await;
        let mut identities = registry.list();
        identities.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.public_id.cmp(&b.public_id))
        });
        identities
            .into_iter()
            .map(|identity| identity.public_id)
            .collect::<Vec<_>>()
    };

    let mut characters = Vec::with_capacity(public_ids.len());
    for public_id in &public_ids {
        let context = resolve_identity_context(&state, Some(public_id), None).await;
        characters.push(serde_json::to_value(&context).unwrap_or_default());
    }

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.characters.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": characters.len()})),
    });

    Json(json!({ "characters": characters })).into_response()
}

pub(crate) async fn dashboard_home(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<DashboardHomeQuery>,
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
    let hours = query.hours.unwrap_or(12).max(1);
    let view = match build_dashboard_view(&state, &context, hours).await {
        Ok(view) => view,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.dashboard.home".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: context.public_memory_owner.public.clone(),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(
            json!({"eligible_open_count": view.eligible_open_count, "active_count": view.active_count}),
        ),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "agent_stats": view.agent_stats,
        "metrics": view.metrics,
        "emergencies": view.emergencies,
        "briefing": view.briefing,
        "mission_summary": {
            "eligible_open_count": view.eligible_open_count,
            "active_count": view.active_count,
        },
        "home_planet": view.home_planet,
        "home_zone": view.home_zone,
        "home_zone_events": view.home_zone_events,
    }))
    .into_response()
}

pub(crate) async fn my_missions(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<MyMissionsQuery>,
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
    let agent_id = context.public_memory_owner.controller.clone();
    let profile = context.profile.clone();
    let missions = state.mission_board.lock().await.list(None);

    let eligible_open: Vec<_> = missions
        .iter()
        .filter(|mission| mission.status == MissionStatus::Open)
        .filter(|mission| mission_matches_profile(mission, profile.as_ref()))
        .cloned()
        .collect();
    let active: Vec<_> = missions
        .iter()
        .filter(|mission| {
            matches!(
                mission.status,
                MissionStatus::Claimed | MissionStatus::Completed
            )
        })
        .filter(|mission| mission.claimed_by.as_deref() == Some(agent_id.as_str()))
        .cloned()
        .collect();
    let history: Vec<_> = missions
        .iter()
        .filter(|mission| {
            matches!(
                mission.status,
                MissionStatus::Settled | MissionStatus::Cancelled
            )
        })
        .filter(|mission| {
            mission.claimed_by.as_deref() == Some(agent_id.as_str())
                || mission.completed_by.as_deref() == Some(agent_id.as_str())
        })
        .cloned()
        .collect();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.missions.my".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: context.public_memory_owner.public.clone(),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "eligible_open_count": eligible_open.len(),
            "active_count": active.len(),
            "history_count": history.len(),
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "eligible_open": eligible_open,
        "active": active,
        "history": history,
    }))
    .into_response()
}

pub(crate) async fn my_governance(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<MyGovernanceQuery>,
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
    let agent_id = context.public_memory_owner.controller.clone();
    let profile = context.profile.clone();
    let governance = state.governance_engine.lock().await;
    let planets = governance.list_planets();
    let proposals = governance.list_proposals(None);
    let home_subnet_id = profile
        .as_ref()
        .and_then(|profile| profile.home_subnet_id.clone());
    let home_planet = home_subnet_id
        .as_deref()
        .and_then(|subnet_id| governance.planet(subnet_id).cloned());
    let governed_planets: Vec<_> = planets
        .iter()
        .filter(|planet| planet.creator == agent_id || planet.validators.contains(&agent_id))
        .cloned()
        .collect();
    let my_proposals: Vec<_> = proposals
        .iter()
        .filter(|proposal| {
            proposal.created_by == agent_id
                || proposal.votes_for.contains(&agent_id)
                || proposal.votes_against.contains(&agent_id)
        })
        .cloned()
        .collect();
    let relevant_proposals: Vec<_> = proposals
        .iter()
        .filter(|proposal| {
            home_subnet_id
                .as_deref()
                .is_some_and(|subnet_id| proposal.subnet_id == subnet_id)
        })
        .cloned()
        .collect();
    let risks = home_planet.as_ref().map_or_else(Vec::new, |planet| {
        let mut risks = Vec::new();
        if planet.government_status == GovernmentStatus::Recall {
            risks.push("recall".to_string());
        }
        if planet.government_status == GovernmentStatus::Custody {
            risks.push("custody".to_string());
        }
        if planet.stability <= 30 {
            risks.push("stability_critical".to_string());
        }
        risks
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.governance.my".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: context.public_memory_owner.public.clone(),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "governed_planets_count": governed_planets.len(),
            "my_proposals_count": my_proposals.len(),
            "relevant_proposals_count": relevant_proposals.len(),
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "eligibility": {
            "has_valid_license": governance.has_valid_license(&agent_id),
            "has_active_bond": governance.has_active_bond(&agent_id, 1),
        },
        "home_planet": home_planet,
        "governed_planets": governed_planets,
        "my_proposals": my_proposals,
        "relevant_proposals": relevant_proposals,
        "risks": risks,
    }))
    .into_response()
}

pub(crate) async fn bootstrap_catalog(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    _query: Query<BootstrapCatalogQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let zones = state.galaxy_state.lock().await.zones();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.catalog.bootstrap".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"zones": zones.len()})),
    });

    Json(json!({
        "factions": ["order", "freeport", "raider"],
        "roles": ["operator", "broker", "enforcer", "artificer"],
        "strategies": ["conservative", "balanced", "aggressive"],
        "controller_kinds": ["local_wattswarm", "external_runtime"],
        "ownership_scopes": ["local", "external"],
        "mission_domains": ["wealth", "power", "security", "trade", "culture"],
        "galaxy_zones": zones,
    }))
    .into_response()
}
