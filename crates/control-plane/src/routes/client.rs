use anyhow::Result;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::json;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::missions::{CivilMission, MissionBoard, MissionStatus};
use wattetheria_kernel::civilization::{emergency, metrics};
use wattetheria_kernel::game::anchor::MissionAnchor;
use wattetheria_kernel::governance::GovernmentStatus;
use wattetheria_kernel::map::model::{GalaxyMap, StarSystem};
use wattetheria_kernel::map::registry::GalaxyMapRegistry;
use wattetheria_kernel::map::travel::{TravelRiskLevel, TravelWarning, travel_plan};

use crate::auth::{authorize, internal_error};
use crate::autonomy::build_operator_briefing;
use crate::routes::experience::{DashboardExperienceSignals, build_gameplay_experience};
use crate::routes::game::build_game_view;
use crate::routes::identity::{
    IdentityContextView, identity_context_response, resolve_identity_context,
};
use crate::routes::organizations::{OrganizationView, build_organization_views};
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
    travel_state: Option<wattetheria_kernel::map::TravelStateRecord>,
    organizations: Vec<OrganizationView>,
    eligible_open_count: usize,
    local_open_count: usize,
    travel_required_open_count: usize,
    active_count: usize,
}

struct GovernanceClientView {
    has_valid_license: bool,
    has_active_bond: bool,
    home_planet: Option<wattetheria_kernel::governance::SubnetPlanet>,
    governed_planets: Vec<wattetheria_kernel::governance::SubnetPlanet>,
    my_proposals: Vec<wattetheria_kernel::governance::GovernanceProposal>,
    relevant_proposals: Vec<wattetheria_kernel::governance::GovernanceProposal>,
    risks: Vec<String>,
    next_actions: Vec<String>,
    qualification_tracks: Vec<wattetheria_kernel::game::QualificationTrack>,
    organizations: Vec<OrganizationView>,
    charter_applications:
        Vec<wattetheria_kernel::civilization::organizations::OrganizationSubnetCharterApplication>,
}

#[derive(Debug, Clone, Serialize)]
struct MissionTravelView {
    requires_travel: bool,
    reachable: bool,
    from_system_id: Option<String>,
    to_system_id: String,
    total_travel_cost: u32,
    total_risk: u32,
    risk_level: TravelRiskLevel,
    warnings: Vec<TravelWarning>,
}

#[derive(Debug, Clone, Serialize)]
struct MissionClientView {
    mission: CivilMission,
    map_anchor: Option<MissionAnchor>,
    travel: Option<MissionTravelView>,
    local_to_current_position: bool,
}

struct MissionClientBuckets {
    eligible_open: Vec<MissionClientView>,
    local_open: Vec<MissionClientView>,
    travel_required_open: Vec<MissionClientView>,
    active: Vec<MissionClientView>,
    history: Vec<MissionClientView>,
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

fn resolve_mission_anchor(
    mission: &CivilMission,
    maps: &GalaxyMapRegistry,
) -> Option<MissionAnchor> {
    mission
        .payload
        .get("map_anchor")
        .and_then(|value| serde_json::from_value::<MissionAnchor>(value.clone()).ok())
        .or_else(|| {
            maps.list()
                .into_iter()
                .find_map(|map| mission_anchor_from_map(&map, mission))
        })
}

fn mission_anchor_from_map(map: &GalaxyMap, mission: &CivilMission) -> Option<MissionAnchor> {
    if let Some(subnet_id) = mission.subnet_id.as_deref() {
        for system in &map.systems {
            if let Some(planet) = system
                .planets
                .iter()
                .find(|planet| planet.subnet_id.as_deref() == Some(subnet_id))
            {
                return Some(mission_anchor_for_planet(map, system, planet));
            }
        }
    }

    if let Some(zone_id) = mission.zone_id.as_deref() {
        for system in &map.systems {
            if let Some(planet) = system
                .planets
                .iter()
                .find(|planet| planet.zone_id == zone_id)
            {
                return Some(mission_anchor_for_planet(map, system, planet));
            }
        }
    }

    None
}

fn mission_anchor_for_planet(
    map: &GalaxyMap,
    system: &StarSystem,
    planet: &wattetheria_kernel::map::model::PlanetNode,
) -> MissionAnchor {
    MissionAnchor {
        map_id: map.map_id.clone(),
        map_name: map.name.clone(),
        system_id: system.system_id.clone(),
        system_name: system.name.clone(),
        planet_id: Some(planet.planet_id.clone()),
        planet_name: Some(planet.name.clone()),
        route_id: None,
    }
}

fn mission_travel_view(
    anchor: &MissionAnchor,
    travel_state: Option<&wattetheria_kernel::map::TravelStateRecord>,
    maps: &GalaxyMapRegistry,
    galaxy: &wattetheria_kernel::civilization::galaxy::GalaxyState,
) -> Option<MissionTravelView> {
    let travel_state = travel_state?;
    let current_position = &travel_state.current_position;

    if current_position.map_id != anchor.map_id {
        return Some(MissionTravelView {
            requires_travel: true,
            reachable: false,
            from_system_id: Some(current_position.system_id.clone()),
            to_system_id: anchor.system_id.clone(),
            total_travel_cost: 0,
            total_risk: 0,
            risk_level: TravelRiskLevel::Volatile,
            warnings: vec![TravelWarning {
                code: "different_map".to_string(),
                title: "Mission anchor is on a different galaxy map".to_string(),
                severity: 9,
            }],
        });
    }

    if current_position.system_id == anchor.system_id {
        return Some(MissionTravelView {
            requires_travel: false,
            reachable: true,
            from_system_id: Some(current_position.system_id.clone()),
            to_system_id: anchor.system_id.clone(),
            total_travel_cost: 0,
            total_risk: 0,
            risk_level: TravelRiskLevel::Stable,
            warnings: Vec::new(),
        });
    }

    let map = maps.get(&anchor.map_id)?;
    match travel_plan(&map, galaxy, &current_position.system_id, &anchor.system_id) {
        Ok(plan) => Some(MissionTravelView {
            requires_travel: true,
            reachable: true,
            from_system_id: Some(current_position.system_id.clone()),
            to_system_id: anchor.system_id.clone(),
            total_travel_cost: plan.total_travel_cost,
            total_risk: plan.total_risk,
            risk_level: plan.risk_level,
            warnings: plan.warnings,
        }),
        Err(_) => Some(MissionTravelView {
            requires_travel: true,
            reachable: false,
            from_system_id: Some(current_position.system_id.clone()),
            to_system_id: anchor.system_id.clone(),
            total_travel_cost: 0,
            total_risk: 0,
            risk_level: TravelRiskLevel::Volatile,
            warnings: vec![TravelWarning {
                code: "no_route".to_string(),
                title: "No active route to mission anchor".to_string(),
                severity: 8,
            }],
        }),
    }
}

fn build_mission_client_view(
    mission: &CivilMission,
    travel_state: Option<&wattetheria_kernel::map::TravelStateRecord>,
    maps: &GalaxyMapRegistry,
    galaxy: &wattetheria_kernel::civilization::galaxy::GalaxyState,
) -> MissionClientView {
    let map_anchor = resolve_mission_anchor(mission, maps);
    let travel = map_anchor
        .as_ref()
        .and_then(|anchor| mission_travel_view(anchor, travel_state, maps, galaxy));
    let local_to_current_position = travel
        .as_ref()
        .is_some_and(|travel| travel.reachable && !travel.requires_travel);
    MissionClientView {
        mission: mission.clone(),
        map_anchor,
        travel,
        local_to_current_position,
    }
}

fn build_mission_client_buckets(
    missions: &[CivilMission],
    agent_id: &str,
    profile: Option<&wattetheria_kernel::profiles::CitizenProfile>,
    travel_state: Option<&wattetheria_kernel::map::TravelStateRecord>,
    maps: &GalaxyMapRegistry,
    galaxy: &wattetheria_kernel::civilization::galaxy::GalaxyState,
) -> MissionClientBuckets {
    let eligible_open = missions
        .iter()
        .filter(|mission| mission.status == MissionStatus::Open)
        .filter(|mission| mission_matches_profile(mission, profile))
        .map(|mission| build_mission_client_view(mission, travel_state, maps, galaxy))
        .collect::<Vec<_>>();
    let local_open = eligible_open
        .iter()
        .filter(|mission| mission.local_to_current_position)
        .cloned()
        .collect::<Vec<_>>();
    let travel_required_open = eligible_open
        .iter()
        .filter(|mission| {
            mission
                .travel
                .as_ref()
                .is_some_and(|travel| travel.requires_travel)
        })
        .cloned()
        .collect::<Vec<_>>();
    let active = missions
        .iter()
        .filter(|mission| {
            matches!(
                mission.status,
                MissionStatus::Claimed | MissionStatus::Completed
            )
        })
        .filter(|mission| mission.claimed_by.as_deref() == Some(agent_id))
        .map(|mission| build_mission_client_view(mission, travel_state, maps, galaxy))
        .collect::<Vec<_>>();
    let history = missions
        .iter()
        .filter(|mission| {
            matches!(
                mission.status,
                MissionStatus::Settled | MissionStatus::Cancelled
            )
        })
        .filter(|mission| {
            mission.claimed_by.as_deref() == Some(agent_id)
                || mission.completed_by.as_deref() == Some(agent_id)
        })
        .map(|mission| build_mission_client_view(mission, travel_state, maps, galaxy))
        .collect::<Vec<_>>();

    MissionClientBuckets {
        eligible_open,
        local_open,
        travel_required_open,
        active,
        history,
    }
}

fn dashboard_open_mission_counts(
    missions: &MissionBoard,
    profile: Option<&wattetheria_kernel::profiles::CitizenProfile>,
    travel_state: Option<&wattetheria_kernel::map::TravelStateRecord>,
    maps: &GalaxyMapRegistry,
    galaxy: &wattetheria_kernel::civilization::galaxy::GalaxyState,
) -> (usize, usize, usize) {
    let eligible_open_views = missions
        .list(Some(&MissionStatus::Open))
        .into_iter()
        .filter(|mission| mission_matches_profile(mission, profile))
        .map(|mission| build_mission_client_view(&mission, travel_state, maps, galaxy))
        .collect::<Vec<_>>();
    let eligible_open_count = eligible_open_views.len();
    let local_open_count = eligible_open_views
        .iter()
        .filter(|view| view.local_to_current_position)
        .count();
    let travel_required_open_count = eligible_open_views
        .iter()
        .filter(|view| {
            view.travel
                .as_ref()
                .is_some_and(|travel| travel.requires_travel)
        })
        .count();

    (
        eligible_open_count,
        local_open_count,
        travel_required_open_count,
    )
}

fn dashboard_active_mission_count(missions: &MissionBoard, controller_id: &str) -> usize {
    missions
        .list(None)
        .into_iter()
        .filter(|mission| {
            matches!(
                mission.status,
                MissionStatus::Claimed | MissionStatus::Completed
            ) && mission.claimed_by.as_deref() == Some(controller_id)
        })
        .count()
}

async fn build_dashboard_view(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    hours: i64,
) -> Result<DashboardView> {
    let controller_id = context.public_memory_owner.controller.clone();
    let agent_stats = resolve_agent_stats(state, &controller_id).await?;
    let travel_state = if let Some(identity) = context.public_identity.as_ref() {
        state
            .travel_state_registry
            .lock()
            .await
            .get(&identity.public_id)
    } else {
        None
    };
    let maps = state.galaxy_map_registry.lock().await;
    let missions = state.mission_board.lock().await;
    let profiles = state.citizen_registry.lock().await;
    let governance = state.governance_engine.lock().await;
    let galaxy = state.galaxy_state.lock().await;
    let organizations = state.organization_registry.lock().await;
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
    let (eligible_open_count, local_open_count, travel_required_open_count) =
        dashboard_open_mission_counts(&missions, profile, travel_state.as_ref(), &maps, &galaxy);
    let active_count = dashboard_active_mission_count(&missions, &controller_id);
    let home_zone_events = profile
        .and_then(|profile| profile.home_zone_id.as_deref())
        .map_or_else(
            || galaxy.events(None),
            |zone_id| galaxy.events(Some(zone_id)),
        );
    let organization_views = context
        .public_identity
        .as_ref()
        .map(|identity| {
            build_organization_views(&organizations, &missions, &governance, &identity.public_id)
        })
        .unwrap_or_default();
    drop(organizations);
    drop(galaxy);
    drop(governance);
    drop(profiles);
    drop(missions);
    drop(maps);

    let briefing = build_operator_briefing(state, hours).await?;
    Ok(DashboardView {
        agent_stats,
        metrics,
        emergencies,
        briefing,
        home_planet,
        home_zone,
        home_zone_events,
        travel_state,
        organizations: organization_views,
        eligible_open_count,
        local_open_count,
        travel_required_open_count,
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
        let travel_state = state.travel_state_registry.lock().await.get(public_id);
        let organizations = {
            let organizations = state.organization_registry.lock().await;
            let missions = state.mission_board.lock().await;
            let governance = state.governance_engine.lock().await;
            build_organization_views(&organizations, &missions, &governance, public_id)
        };
        characters.push(json!({
            "identity": context,
            "travel_state": travel_state,
            "organizations": organizations,
        }));
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
    let game = match build_game_view(&state, &context).await {
        Ok(game) => game,
        Err(error) => return internal_error(&error),
    };
    let experience = build_gameplay_experience(
        &game,
        Some(DashboardExperienceSignals {
            emergencies: &view.emergencies,
            eligible_open_count: view.eligible_open_count,
            local_open_count: view.local_open_count,
            travel_required_open_count: view.travel_required_open_count,
        }),
    );

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
        details: Some(json!({
            "eligible_open_count": view.eligible_open_count,
            "local_open_count": view.local_open_count,
            "travel_required_open_count": view.travel_required_open_count,
            "active_count": view.active_count
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "agent_stats": view.agent_stats,
        "metrics": view.metrics,
        "emergencies": view.emergencies,
        "briefing": view.briefing,
        "game": game,
        "experience": experience,
        "mission_summary": {
            "eligible_open_count": view.eligible_open_count,
            "local_open_count": view.local_open_count,
            "travel_required_open_count": view.travel_required_open_count,
            "active_count": view.active_count,
        },
        "home_planet": view.home_planet,
        "home_zone": view.home_zone,
        "home_zone_events": view.home_zone_events,
        "travel_state": view.travel_state,
        "organizations": view.organizations,
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
    let travel_state = if let Some(identity) = context.public_identity.as_ref() {
        state
            .travel_state_registry
            .lock()
            .await
            .get(&identity.public_id)
    } else {
        None
    };
    let maps = state.galaxy_map_registry.lock().await;
    let galaxy = state.galaxy_state.lock().await;
    let missions = state.mission_board.lock().await.list(None);
    let buckets = build_mission_client_buckets(
        &missions,
        &agent_id,
        profile.as_ref(),
        travel_state.as_ref(),
        &maps,
        &galaxy,
    );
    drop(galaxy);
    drop(maps);

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
            "eligible_open_count": buckets.eligible_open.len(),
            "local_open_count": buckets.local_open.len(),
            "travel_required_open_count": buckets.travel_required_open.len(),
            "active_count": buckets.active.len(),
            "history_count": buckets.history.len(),
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "eligible_open": buckets.eligible_open,
        "local_open": buckets.local_open,
        "travel_required_open": buckets.travel_required_open,
        "active": buckets.active,
        "history": buckets.history,
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
    let game = match build_game_view(&state, &context).await {
        Ok(game) => game,
        Err(error) => return internal_error(&error),
    };
    let view = build_governance_client_view(&state, &context, &game).await;

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
            "governed_planets_count": view.governed_planets.len(),
            "my_proposals_count": view.my_proposals.len(),
            "relevant_proposals_count": view.relevant_proposals.len(),
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "eligibility": {
            "has_valid_license": view.has_valid_license,
            "has_active_bond": view.has_active_bond,
        },
        "journey": game.status.governance_journey,
        "qualification_tracks": view.qualification_tracks,
        "next_actions": view.next_actions,
        "home_planet": view.home_planet,
        "governed_planets": view.governed_planets,
        "my_proposals": view.my_proposals,
        "relevant_proposals": view.relevant_proposals,
        "organizations": view.organizations,
        "charter_applications": view.charter_applications,
        "risks": view.risks,
    }))
    .into_response()
}

async fn build_governance_client_view(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    game: &crate::routes::game::GameView,
) -> GovernanceClientView {
    let agent_id = context.public_memory_owner.controller.clone();
    let profile = context.profile.clone();
    let governance = state.governance_engine.lock().await;
    let planets = governance.list_planets();
    let proposals = governance.list_proposals(None);
    let has_valid_license = governance.has_valid_license(&agent_id);
    let has_active_bond = governance.has_active_bond(&agent_id, 1);
    let home_subnet_id = profile
        .as_ref()
        .and_then(|profile| profile.home_subnet_id.clone());
    let home_planet = home_subnet_id
        .as_deref()
        .and_then(|subnet_id| governance.planet(subnet_id).cloned());
    let governed_planets = planets
        .iter()
        .filter(|planet| planet.creator == agent_id || planet.validators.contains(&agent_id))
        .cloned()
        .collect::<Vec<_>>();
    let my_proposals = proposals
        .iter()
        .filter(|proposal| {
            proposal.created_by == agent_id
                || proposal.votes_for.contains(&agent_id)
                || proposal.votes_against.contains(&agent_id)
        })
        .cloned()
        .collect::<Vec<_>>();
    let relevant_proposals = proposals
        .iter()
        .filter(|proposal| {
            home_subnet_id
                .as_deref()
                .is_some_and(|subnet_id| proposal.subnet_id == subnet_id)
        })
        .cloned()
        .collect::<Vec<_>>();
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
    let qualification_tracks = game
        .status
        .qualifications
        .iter()
        .filter(|track| track.key == "civic_governance" || track.key == "expansion_planning")
        .cloned()
        .collect::<Vec<_>>();
    let organizations = game.organizations.clone();
    let charter_applications = organizations
        .iter()
        .filter_map(|organization| {
            organization
                .governance_summary
                .latest_charter_application
                .clone()
        })
        .collect::<Vec<_>>();
    GovernanceClientView {
        has_valid_license,
        has_active_bond,
        home_planet,
        governed_planets,
        my_proposals,
        relevant_proposals,
        risks,
        next_actions: governance_next_actions(&game.status.governance_journey),
        qualification_tracks,
        organizations,
        charter_applications,
    }
}

fn governance_next_actions(journey: &wattetheria_kernel::game::GovernanceJourney) -> Vec<String> {
    journey
        .gates
        .iter()
        .filter(|gate| !gate.complete)
        .map(|gate| gate.title.clone())
        .take(3)
        .collect()
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
        "organization_kinds": ["guild", "consortium", "fleet", "civic_union"],
        "organization_roles": ["founder", "officer", "member"],
        "organization_permissions": ["manage_members", "manage_treasury", "publish_missions", "manage_governance"],
        "organization_proposal_kinds": ["subnet_charter"],
        "controller_kinds": ["local_wattswarm", "external_runtime"],
        "ownership_scopes": ["local", "external"],
        "mission_domains": ["wealth", "power", "security", "trade", "culture"],
        "travel_risk_levels": ["stable", "guarded", "contested", "volatile"],
        "galaxy_zones": zones,
    }))
    .into_response()
}
