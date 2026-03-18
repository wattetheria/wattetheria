use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{TimeZone, Utc};
use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::metrics::{CivilizationScores, compute_scores};
use wattetheria_kernel::civilization::missions::{
    MissionDomain, MissionPublisherKind, MissionStatus,
};
use wattetheria_kernel::identities::{ControllerBinding, PublicIdentity};
use wattetheria_kernel::signing::sign_payload;
use wattetheria_kernel::storage::event_log::EventRecord;
use wattetheria_kernel::types::AgentStats;

use crate::auth::{authorize, internal_error};
use crate::routes::identity::resolve_identity_context;
use crate::routes::network::{derived_distance_km, derived_geo};
use crate::state::{
    ClientExportQuery, ClientIdentityQuery, ClientLeaderboardQuery, ClientListQuery,
    ClientRpcLogsQuery, ControlPlaneState,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicClientSnapshot {
    pub generated_at: i64,
    pub node_id: String,
    pub public_key: String,
    pub network_status: Value,
    pub peers: Vec<Value>,
    pub operator: Value,
    pub rpc_logs: Vec<Value>,
    pub tasks: Vec<Value>,
    pub organizations: Vec<Value>,
    pub leaderboard: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPublicClientSnapshot {
    pub payload: PublicClientSnapshot,
    pub signature: String,
    pub signer_agent_id: String,
}

async fn build_client_network_status_payload(state: &ControlPlaneState) -> anyhow::Result<Value> {
    let network = state.swarm_bridge.network_status().await?;
    let peers = state.swarm_bridge.peers().await?;
    let total_nodes = peers.len() + 1;
    let active_nodes = if network.running { total_nodes } else { 0 };
    let health_percent = if total_nodes == 0 {
        0
    } else {
        ((active_nodes * 100) / total_nodes) as u64
    };
    Ok(json!({
        "total_nodes": total_nodes,
        "active_nodes": active_nodes,
        "health_percent": health_percent,
        "avg_latency_ms": 0,
    }))
}

async fn build_client_peers_payload(
    state: &ControlPlaneState,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    Ok(state
        .swarm_bridge
        .peers()
        .await?
        .into_iter()
        .take(limit)
        .map(|peer| {
            let (lat, lng) = derived_geo(&peer.node_id);
            json!({
                "id": peer.node_id,
                "distance_km": derived_distance_km(&peer.node_id),
                "latency_ms": 0,
                "status": "online",
                "lat": lat,
                "lng": lng,
            })
        })
        .collect())
}

async fn build_client_self_payload(
    state: &ControlPlaneState,
    query: &ClientIdentityQuery,
) -> anyhow::Result<Value> {
    let context =
        resolve_identity_context(state, query.public_id.as_deref(), query.agent_id.as_deref())
            .await;
    let controller_id = context.public_memory_owner.controller.clone();
    let agent_stats = state.swarm_bridge.agent_view(&controller_id).await?.stats;
    let public_id = context.public_identity.as_ref().map_or_else(
        || controller_id.clone(),
        |identity| identity.public_id.clone(),
    );
    let display_name = context.public_identity.as_ref().map_or_else(
        || public_id.clone(),
        |identity| identity.display_name.clone(),
    );
    let (lat, lng) = derived_geo(&public_id);
    Ok(json!({
        "id": public_id,
        "display_name": display_name,
        "watt_balance": agent_stats.watt,
        "status": "online",
        "lat": lat,
        "lng": lng,
        "controller_id": controller_id,
    }))
}

fn build_client_rpc_logs_payload(
    state: &ControlPlaneState,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    Ok(state
        .event_log
        .get_all()?
        .into_iter()
        .rev()
        .take(limit)
        .map(|row| {
            json!({
                "timestamp": timestamp_to_rfc3339(row.timestamp),
                "message": rpc_log_message(&row),
                "level": rpc_log_level(&row.event_type),
            })
        })
        .collect())
}

async fn build_client_tasks_payload(state: &ControlPlaneState, limit: usize) -> Vec<Value> {
    let mut missions = state.mission_board.lock().await.list(None);
    missions.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.mission_id.cmp(&right.mission_id))
    });
    missions
        .into_iter()
        .filter(|mission| mission.status != MissionStatus::Cancelled)
        .take(limit)
        .map(|mission| {
            json!({
                "id": mission.mission_id,
                "title": mission.title,
                "domain": mission_domain_label(&mission.domain),
                "reward_watt": mission.reward.agent_watt,
                "status": client_task_status(&mission.status),
                "publisher_id": mission.publisher,
                "claimer_id": mission.claimed_by,
                "created_at": timestamp_to_rfc3339(mission.created_at),
            })
        })
        .collect()
}

async fn build_client_organizations_payload(state: &ControlPlaneState, limit: usize) -> Vec<Value> {
    let organizations = state.organization_registry.lock().await;
    let missions = state.mission_board.lock().await.list(None);
    let mut entries = organizations.list_organizations();
    entries.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.organization_id.cmp(&right.organization_id))
    });
    entries
        .into_iter()
        .take(limit)
        .map(|organization| {
            let active_member_count = organizations
                .memberships(&organization.organization_id)
                .into_iter()
                .filter(|membership| membership.active)
                .count();
            let mission_count = missions
                .iter()
                .filter(|mission| {
                    mission.publisher_kind == MissionPublisherKind::Organization
                        && mission.publisher == organization.organization_id
                        && mission.status != MissionStatus::Cancelled
                })
                .count();
            json!({
                "id": organization.organization_id,
                "name": organization.name,
                "member_count": active_member_count,
                "treasury_watt": organization.treasury_watt,
                "mission_count": mission_count,
                "founded_at": timestamp_to_rfc3339(organization.created_at),
                "status": organization_client_status(organization.active, active_member_count),
            })
        })
        .collect()
}

pub(crate) async fn client_network_status(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let payload = match build_client_network_status_payload(&state).await {
        Ok(payload) => payload,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.network_status.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    Json(payload).into_response()
}

pub(crate) async fn client_peers(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ClientListQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let payload = match build_client_peers_payload(&state, limit).await {
        Ok(payload) => payload,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.peers.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": payload.len()})),
    });

    Json(Value::Array(payload)).into_response()
}

pub(crate) async fn client_self(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ClientIdentityQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let payload = match build_client_self_payload(&state, &query).await {
        Ok(payload) => payload,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.self.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: payload["id"].as_str().map(str::to_string),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    Json(payload).into_response()
}

pub(crate) async fn client_rpc_logs(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ClientRpcLogsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(20).clamp(1, 200);
    let payload = match build_client_rpc_logs_payload(&state, limit) {
        Ok(payload) => payload,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.rpc_logs.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": payload.len()})),
    });

    Json(Value::Array(payload)).into_response()
}

pub(crate) async fn client_tasks(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ClientListQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let payload = build_client_tasks_payload(&state, limit).await;

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.tasks.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": payload.len()})),
    });

    Json(Value::Array(payload)).into_response()
}

pub(crate) async fn client_organizations(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ClientListQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let payload = build_client_organizations_payload(&state, limit).await;

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.organizations.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": payload.len()})),
    });

    Json(Value::Array(payload)).into_response()
}

pub(crate) async fn client_leaderboard(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ClientLeaderboardQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let category = match LeaderboardCategory::parse(query.category.as_deref()) {
        Ok(category) => category,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response();
        }
    };
    let limit = query.limit.unwrap_or(20).clamp(1, 200);
    let payload = match build_client_leaderboard_payload(&state, category, limit).await {
        Ok(payload) => payload,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.leaderboard.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"category": category.as_str(), "count": payload.len()})),
    });

    Json(Value::Array(payload)).into_response()
}

pub(crate) async fn client_export(
    State(state): State<ControlPlaneState>,
    Query(query): Query<ClientExportQuery>,
) -> Response {
    let signed_snapshot = match build_signed_public_client_snapshot(&state, &query).await {
        Ok(snapshot) => snapshot,
        Err(error) if error.to_string().contains("unknown leaderboard category") => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": error.to_string() })),
            )
                .into_response();
        }
        Err(error) => return internal_error(&error),
    };
    let snapshot = &signed_snapshot.payload;
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.export.query".to_string(),
        status: "ok".to_string(),
        actor: Some("public".to_string()),
        subject: snapshot.operator["id"].as_str().map(str::to_string),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "peer_count": snapshot.peers.len(),
            "task_count": snapshot.tasks.len(),
            "organization_count": snapshot.organizations.len(),
            "leaderboard_count": snapshot.leaderboard.len(),
        })),
    });
    Json(signed_snapshot).into_response()
}

pub async fn build_signed_public_client_snapshot(
    state: &ControlPlaneState,
    query: &ClientExportQuery,
) -> anyhow::Result<SignedPublicClientSnapshot> {
    let public_id_query = ClientIdentityQuery {
        agent_id: query.agent_id.clone(),
        public_id: query.public_id.clone(),
    };
    let leaderboard_category = LeaderboardCategory::parse(query.leaderboard_category.as_deref())
        .map_err(anyhow::Error::msg)?;
    let snapshot =
        build_public_client_snapshot(state, query, &public_id_query, leaderboard_category).await?;
    let signature = sign_payload(&snapshot, &state.identity)?;
    Ok(SignedPublicClientSnapshot {
        payload: snapshot,
        signature,
        signer_agent_id: state.identity.agent_id.clone(),
    })
}

pub async fn push_signed_public_client_snapshot(
    client: &Client,
    gateway_url: &str,
    state: &ControlPlaneState,
    query: &ClientExportQuery,
) -> anyhow::Result<SignedPublicClientSnapshot> {
    let snapshot = build_signed_public_client_snapshot(state, query).await?;
    client
        .post(normalized_gateway_ingest_url(gateway_url))
        .json(&snapshot)
        .send()
        .await?
        .error_for_status()?;
    Ok(snapshot)
}

fn normalized_gateway_ingest_url(gateway_url: &str) -> String {
    let trimmed = gateway_url.trim_end_matches('/');
    if trimmed.ends_with("/api/ingest/snapshot") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/ingest/snapshot")
    }
}

fn controller_id_for_identity(
    identity: &PublicIdentity,
    binding: Option<&ControllerBinding>,
) -> String {
    binding
        .and_then(|binding| binding.controller_node_id.clone())
        .or_else(|| identity.legacy_agent_id.clone())
        .unwrap_or_else(|| identity.public_id.clone())
}

fn prefers_existing_identity(
    existing: &PublicIdentity,
    candidate: &PublicIdentity,
    controller_id: &str,
) -> bool {
    let existing_is_default = existing.public_id == controller_id;
    let candidate_is_default = candidate.public_id == controller_id;
    if existing_is_default != candidate_is_default {
        return !existing_is_default;
    }
    existing.public_id <= candidate.public_id
}

async fn leaderboard_bindings(state: &ControlPlaneState) -> BTreeMap<String, ControllerBinding> {
    state
        .controller_binding_registry
        .lock()
        .await
        .list()
        .into_iter()
        .map(|binding| (binding.public_id.clone(), binding))
        .collect()
}

async fn leaderboard_identities(
    state: &ControlPlaneState,
    binding_by_public_id: &BTreeMap<String, ControllerBinding>,
) -> Vec<(String, PublicIdentity)> {
    let public_identities = state.public_identity_registry.lock().await.list();
    let identities = if public_identities.is_empty() {
        vec![PublicIdentity {
            public_id: state.agent_id.clone(),
            display_name: state.agent_id.clone(),
            legacy_agent_id: Some(state.agent_id.clone()),
            active: true,
            created_at: state.started_at,
            updated_at: state.started_at,
        }]
    } else {
        public_identities
    };
    let mut identity_by_controller: BTreeMap<String, PublicIdentity> = BTreeMap::new();
    for identity in identities {
        let controller_id =
            controller_id_for_identity(&identity, binding_by_public_id.get(&identity.public_id));
        match identity_by_controller.get(&controller_id) {
            Some(existing)
                if existing.updated_at > identity.updated_at
                    || (existing.updated_at == identity.updated_at
                        && prefers_existing_identity(existing, &identity, &controller_id)) => {}
            _ => {
                identity_by_controller.insert(controller_id, identity);
            }
        }
    }
    identity_by_controller.into_iter().collect()
}

async fn leaderboard_agent_stats(
    state: &ControlPlaneState,
    identities: &[(String, PublicIdentity)],
) -> BTreeMap<String, AgentStats> {
    let mut agent_stats_by_controller = BTreeMap::new();
    for (controller_id, _) in identities {
        let agent_stats = match state.swarm_bridge.agent_view(controller_id).await {
            Ok(view) => view.stats,
            Err(_) => AgentStats::default(),
        };
        agent_stats_by_controller.insert(controller_id.clone(), agent_stats);
    }
    agent_stats_by_controller
}

async fn build_client_leaderboard_payload(
    state: &ControlPlaneState,
    category: LeaderboardCategory,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    let binding_by_public_id = leaderboard_bindings(state).await;
    let identities = leaderboard_identities(state, &binding_by_public_id).await;
    let agent_stats_by_controller = leaderboard_agent_stats(state, &identities).await;
    let missions = state.mission_board.lock().await;
    let profiles = state.citizen_registry.lock().await;
    let governance = state.governance_engine.lock().await;
    let galaxy = state.galaxy_state.lock().await;
    let settled = missions.list(Some(&MissionStatus::Settled));
    let leaderboard_view = LeaderboardComputation {
        settled: &settled,
        missions: &missions,
        profiles: &profiles,
        governance: &governance,
        galaxy: &galaxy,
        category,
    };
    let mut payload =
        leaderboard_payload(identities, &agent_stats_by_controller, &leaderboard_view);
    payload.sort_by(|left, right| {
        right["score"]
            .as_i64()
            .unwrap_or_default()
            .cmp(&left["score"].as_i64().unwrap_or_default())
            .then_with(|| {
                right["watt_balance"]
                    .as_i64()
                    .unwrap_or_default()
                    .cmp(&left["watt_balance"].as_i64().unwrap_or_default())
            })
            .then_with(|| {
                left["agent_id"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["agent_id"].as_str().unwrap_or_default())
            })
    });
    for (index, entry) in payload.iter_mut().take(limit).enumerate() {
        entry["rank"] = json!(index + 1);
    }
    payload.truncate(limit);
    Ok(payload)
}

struct LeaderboardComputation<'a> {
    settled: &'a [wattetheria_kernel::civilization::missions::CivilMission],
    missions: &'a wattetheria_kernel::civilization::missions::MissionBoard,
    profiles: &'a wattetheria_kernel::civilization::profiles::CitizenRegistry,
    governance: &'a wattetheria_kernel::governance::GovernanceEngine,
    galaxy: &'a wattetheria_kernel::civilization::galaxy::GalaxyState,
    category: LeaderboardCategory,
}

fn leaderboard_payload(
    identities: Vec<(String, PublicIdentity)>,
    agent_stats_by_controller: &BTreeMap<String, AgentStats>,
    view: &LeaderboardComputation<'_>,
) -> Vec<Value> {
    identities
        .into_iter()
        .map(|(controller_id, identity)| {
            let agent_stats = agent_stats_by_controller
                .get(&controller_id)
                .cloned()
                .unwrap_or_default();
            let scores = compute_scores(
                &controller_id,
                &agent_stats,
                view.missions,
                view.profiles,
                view.governance,
                view.galaxy,
            );
            let tasks_completed = view
                .settled
                .iter()
                .filter(|mission| mission.completed_by.as_deref() == Some(controller_id.as_str()))
                .count();
            json!({
                "agent_id": identity.public_id,
                "display_name": identity.display_name,
                "score": score_for_category(&scores, view.category),
                "watt_balance": agent_stats.watt,
                "tasks_completed": tasks_completed,
                "reputation": agent_stats.reputation,
            })
        })
        .collect()
}

async fn build_public_client_snapshot(
    state: &ControlPlaneState,
    query: &ClientExportQuery,
    identity_query: &ClientIdentityQuery,
    leaderboard_category: LeaderboardCategory,
) -> anyhow::Result<PublicClientSnapshot> {
    Ok(PublicClientSnapshot {
        generated_at: Utc::now().timestamp(),
        node_id: state.agent_id.clone(),
        public_key: state.identity.public_key.clone(),
        network_status: build_client_network_status_payload(state).await?,
        peers: build_client_peers_payload(state, query.peer_limit.unwrap_or(25).clamp(1, 200))
            .await?,
        operator: build_client_self_payload(state, identity_query).await?,
        rpc_logs: build_client_rpc_logs_payload(
            state,
            query.rpc_log_limit.unwrap_or(20).clamp(1, 200),
        )?,
        tasks: build_client_tasks_payload(state, query.task_limit.unwrap_or(50).clamp(1, 500))
            .await,
        organizations: build_client_organizations_payload(
            state,
            query.organization_limit.unwrap_or(50).clamp(1, 500),
        )
        .await,
        leaderboard: build_client_leaderboard_payload(
            state,
            leaderboard_category,
            query.leaderboard_limit.unwrap_or(20).clamp(1, 200),
        )
        .await?,
    })
}

fn timestamp_to_rfc3339(timestamp: i64) -> String {
    Utc.timestamp_opt(timestamp, 0)
        .single()
        .map_or_else(|| Utc::now().to_rfc3339(), |dt| dt.to_rfc3339())
}

fn mission_domain_label(domain: &MissionDomain) -> &'static str {
    match domain {
        MissionDomain::Wealth => "wealth",
        MissionDomain::Power => "power",
        MissionDomain::Security => "security",
        MissionDomain::Trade => "trade",
        MissionDomain::Culture => "culture",
    }
}

fn client_task_status(status: &MissionStatus) -> &'static str {
    match status {
        MissionStatus::Open => "published",
        MissionStatus::Claimed => "claimed",
        MissionStatus::Completed => "executed",
        MissionStatus::Settled | MissionStatus::Cancelled => "settled",
    }
}

fn organization_client_status(active: bool, active_member_count: usize) -> &'static str {
    if !active {
        "suspended"
    } else if active_member_count < 2 {
        "forming"
    } else {
        "active"
    }
}

fn rpc_log_level(event_type: &str) -> &'static str {
    if event_type.contains("REJECT") || event_type.contains("ERROR") || event_type.contains("FAIL")
    {
        "error"
    } else if event_type.contains("CANCEL")
        || event_type.contains("LOST")
        || event_type.contains("DENIED")
    {
        "warn"
    } else if event_type.contains("SETTLED")
        || event_type.contains("VERIFIED")
        || event_type.contains("CONNECTED")
        || event_type.contains("CREATED")
        || event_type.contains("UPDATED")
        || event_type.contains("BOOTSTRAP")
    {
        "success"
    } else {
        "info"
    }
}

fn rpc_log_message(row: &EventRecord) -> String {
    let subject = [
        "title",
        "organization_id",
        "mission_id",
        "task_id",
        "public_id",
        "proposal_id",
        "zone_id",
        "subnet_id",
    ]
    .iter()
    .find_map(|key| row.payload.get(key).and_then(Value::as_str))
    .map(str::to_string)
    .or_else(|| {
        row.payload
            .get("organization")
            .and_then(|value| value.get("organization_id"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let base = row.event_type.replace('_', " ").to_lowercase();
    match subject {
        Some(subject) => format!("{base}: {subject}"),
        None => base,
    }
}

#[derive(Debug, Clone, Copy)]
enum LeaderboardCategory {
    Wealth,
    Power,
    Security,
    Trade,
    Culture,
    Contribution,
}

impl LeaderboardCategory {
    fn parse(value: Option<&str>) -> Result<Self, &'static str> {
        match value.unwrap_or("wealth") {
            "wealth" => Ok(Self::Wealth),
            "power" => Ok(Self::Power),
            "security" => Ok(Self::Security),
            "trade" => Ok(Self::Trade),
            "culture" => Ok(Self::Culture),
            "contribution" => Ok(Self::Contribution),
            _ => Err(
                "unsupported leaderboard category; expected wealth, power, security, trade, culture, or contribution",
            ),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Wealth => "wealth",
            Self::Power => "power",
            Self::Security => "security",
            Self::Trade => "trade",
            Self::Culture => "culture",
            Self::Contribution => "contribution",
        }
    }
}

fn score_for_category(scores: &CivilizationScores, category: LeaderboardCategory) -> i64 {
    match category {
        LeaderboardCategory::Wealth => scores.wealth,
        LeaderboardCategory::Power => scores.power,
        LeaderboardCategory::Security => scores.security,
        LeaderboardCategory::Trade => scores.trade,
        LeaderboardCategory::Culture => scores.culture,
        LeaderboardCategory::Contribution => scores.total_influence,
    }
}
