use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use chrono::{TimeZone, Utc};
use reqwest::Client;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::missions::{
    CivilMission, MissionPublisherKind, MissionStatus,
};
use wattetheria_kernel::storage::event_log::EventRecord;
use wattetheria_kernel::swarm_bridge::{
    SwarmDiagnosticsQuery, SwarmDiagnosticsSnapshot, SwarmPeerView,
};

use crate::auth::{authorize, internal_error};
use crate::ledger_client::watt_balance;
use crate::routes::board::build_public_board_messages_snapshot_payload;
use crate::routes::civilization::build_agent_relationship_payload;
use crate::routes::client_swarm::{
    build_hives_payload, build_public_topic_messages_snapshot_payload,
};
use crate::routes::gateway_missions::board_view;
use crate::routes::identity::resolve_identity_context;
use crate::routes::reward_view::{
    active_wallet_payment_account_payload, wallet_payment_accounts_payload,
    wallet_payment_binding_payload,
};
use crate::state::{
    ClientExportQuery, ClientIdentityQuery, ClientListQuery, ClientRpcLogsQuery, ControlPlaneState,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicClientSnapshot {
    pub generated_at: i64,
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_id: Option<String>,
    pub public_key: String,
    pub network_status: Value,
    pub nodes: Vec<Value>,
    pub peers: Vec<Value>,
    pub operator: Value,
    pub rpc_logs: Vec<Value>,
    #[serde(default)]
    pub public_blocks: Vec<Value>,
    #[serde(default)]
    pub public_topics: Vec<Value>,
    #[serde(default)]
    pub public_topic_messages: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_board_messages: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub space_presences: Vec<wattetheria_gateway_contract::SignedSpacePresence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub world_events: Vec<wattetheria_kernel::swarm_bridge::EventAnnouncementPayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_presence: Option<wattetheria_gateway_contract::space::NodePresence>,
    pub organizations: Vec<Value>,
    /// Always empty: the gateway ranks Agents from its own records. The field stays because
    /// gateways parse it as required and verify the signature over the re-serialized payload.
    pub leaderboard: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPublicClientSnapshot {
    pub payload: PublicClientSnapshot,
    pub signature: String,
    pub signer_agent_did: String,
}

async fn build_client_network_status_payload(state: &ControlPlaneState) -> anyhow::Result<Value> {
    let network_id = state
        .swarm_bridge
        .current_network_id()
        .await
        .ok()
        .filter(|network| !network.trim().is_empty());
    let network = state.swarm_bridge.network_status().await.ok();
    let peers = state.swarm_bridge.peers().await.unwrap_or_default();
    let swarm_running = network.as_ref().is_some_and(|n| n.running);
    let active_peers = if swarm_running { peers.len() } else { 0 };
    let active_nodes = 1 + active_peers;
    let total_nodes = 1 + peers.len();
    let health_percent = ((active_nodes * 100) / total_nodes.max(1)) as u64;
    Ok(json!({
        "network_id": network_id,
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
    Ok(swarm_peers_with_live_connection_status(state)
        .await?
        .into_iter()
        .take(limit)
        .map(build_client_peer_payload)
        .collect())
}

async fn swarm_peers_with_live_connection_status(
    state: &ControlPlaneState,
) -> anyhow::Result<Vec<SwarmPeerView>> {
    let mut peers = state.swarm_bridge.peers().await?;
    let overrides = live_peer_connection_status(state).await;
    apply_live_peer_connection_status(&mut peers, &overrides);
    Ok(peers)
}

async fn live_peer_connection_status(
    state: &ControlPlaneState,
) -> BTreeMap<String, PeerConnectionHealth> {
    let query = SwarmDiagnosticsQuery {
        limit: Some(1),
        ..SwarmDiagnosticsQuery::default()
    };
    state
        .swarm_bridge
        .diagnostics(query)
        .await
        .map(|snapshot| peer_connection_status_from_diagnostics(&snapshot))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PeerConnectionHealth {
    connected: bool,
    recently_seen: bool,
    stale: bool,
    last_seen_age_ms: Option<u64>,
}

fn peer_connection_status_from_diagnostics(
    diagnostics: &SwarmDiagnosticsSnapshot,
) -> BTreeMap<String, PeerConnectionHealth> {
    diagnostics
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.get("peer_health"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let peer_id = entry
                .get("network_peer_id")
                .or_else(|| entry.get("node_id"))
                .or_else(|| entry.get("remote_node_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            let connected = entry.get("connected").and_then(Value::as_bool)?;
            let recently_seen = entry
                .get("recently_seen")
                .and_then(Value::as_bool)
                .unwrap_or(connected);
            let stale = entry.get("stale").and_then(Value::as_bool).unwrap_or(false);
            let last_seen_age_ms = entry.get("last_seen_age_ms").and_then(Value::as_u64);
            Some((
                peer_id.to_owned(),
                PeerConnectionHealth {
                    connected,
                    recently_seen,
                    stale,
                    last_seen_age_ms,
                },
            ))
        })
        .collect()
}

fn apply_live_peer_connection_status(
    peers: &mut [SwarmPeerView],
    connection_status: &BTreeMap<String, PeerConnectionHealth>,
) {
    for peer in peers {
        if let Some(health) = connection_status.get(&peer.node_id).copied() {
            peer.connected = Some(health.connected);
            peer.recently_seen = Some(health.recently_seen);
            peer.stale = Some(health.stale);
            peer.last_seen_age_ms = health.last_seen_age_ms;
        }
    }
}

fn build_client_peer_payload(peer: SwarmPeerView) -> Value {
    let node_id = peer.node_id;
    let connected = peer.connected.unwrap_or(true);
    let recently_seen = peer.recently_seen.unwrap_or(connected);
    let stale = peer.stale.unwrap_or(false);
    let source_kind = peer
        .discovery
        .as_ref()
        .and_then(|value| value.get("source_kind"))
        .and_then(Value::as_str);
    let relationship_state = peer
        .relationship
        .as_ref()
        .and_then(|value| value.get("relationship_state"))
        .and_then(Value::as_str);
    let endpoint = peer
        .metadata
        .as_ref()
        .and_then(iroh_endpoint_id)
        .or_else(|| peer.discovery.as_ref().and_then(iroh_endpoint_id));
    let status = if connected {
        "online"
    } else if relationship_state == Some("blocked") {
        "blocked"
    } else if stale {
        "stale"
    } else if peer.discovery.is_some() {
        "discovered"
    } else {
        "offline"
    };

    let mut object = json!({
        "id": node_id.clone(),
        "node_id": node_id,
        "latency_ms": 0,
        "status": status,
        "connected": connected,
        "recently_seen": recently_seen,
        "stale": stale,
        "source_kind": source_kind,
        "relationship_state": relationship_state,
        "endpoint": endpoint,
        "discovery": peer.discovery,
        "metadata": peer.metadata,
        "relationship": peer.relationship,
    });
    if let Value::Object(object) = &mut object
        && let Some(last_seen_age_ms) = peer.last_seen_age_ms
    {
        object.insert("last_seen_age_ms".to_owned(), json!(last_seen_age_ms));
    }
    object
}

fn insert_if_some(object: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    let Some(value) = value else {
        return;
    };
    match &value {
        Value::Null => {}
        Value::String(value) if value.trim().is_empty() => {}
        Value::Array(value) if value.is_empty() => {}
        Value::Object(value) if value.is_empty() => {}
        _ => {
            object.insert(key.to_string(), value);
        }
    }
}

fn compact_nearby_peer_payload(peer: SwarmPeerView) -> Value {
    let source_agent_card = peer_source_agent_card(peer.metadata.as_ref(), peer.discovery.as_ref());
    let agent_card = source_agent_card
        .as_ref()
        .and_then(|card| card.get("card"))
        .cloned();
    let node_id = peer.node_id;
    let connected = peer.connected.unwrap_or(true);
    let recently_seen = peer.recently_seen.unwrap_or(connected);
    let stale = peer.stale.unwrap_or(false);
    let endpoint = peer
        .metadata
        .as_ref()
        .and_then(iroh_endpoint_id)
        .or_else(|| peer.discovery.as_ref().and_then(iroh_endpoint_id));
    let status = if connected {
        "online"
    } else if stale {
        "stale"
    } else if peer.discovery.is_some() {
        "discovered"
    } else {
        "offline"
    };

    let mut object = Map::new();
    object.insert("remote_node_id".to_string(), Value::String(node_id));
    object.insert("status".to_string(), Value::String(status.to_string()));
    object.insert("connected".to_string(), Value::Bool(connected));
    object.insert("recently_seen".to_string(), Value::Bool(recently_seen));
    object.insert("stale".to_string(), Value::Bool(stale));
    if let Some(last_seen_age_ms) = peer.last_seen_age_ms {
        object.insert("last_seen_age_ms".to_string(), json!(last_seen_age_ms));
    }
    insert_if_some(
        &mut object,
        "endpoint",
        endpoint.map(|value| Value::String(value.to_string())),
    );
    insert_if_some(&mut object, "discovery", peer.discovery);
    insert_if_some(&mut object, "metadata", peer.metadata);
    insert_if_some(&mut object, "source_agent_card", source_agent_card);
    insert_if_some(&mut object, "agent_card", agent_card);
    Value::Object(object)
}

fn peer_source_agent_card(metadata: Option<&Value>, discovery: Option<&Value>) -> Option<Value> {
    metadata
        .and_then(|value| value.pointer("/contact_material/source_agent_card"))
        .or_else(|| metadata.and_then(|value| value.get("source_agent_card")))
        .or_else(|| discovery.and_then(|value| value.get("source_agent_card")))
        .cloned()
}

fn iroh_endpoint_id(value: &Value) -> Option<&str> {
    value
        .get("endpoint_id")
        .and_then(Value::as_str)
        .or_else(|| value.get("local_iroh_endpoint_id").and_then(Value::as_str))
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|metadata| metadata.get("endpoint_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .get("extra")
                .and_then(|extra| extra.get("endpoint_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .get("transports")
                .and_then(Value::as_array)
                .and_then(|transports| transports.iter().find_map(iroh_endpoint_id))
        })
}

async fn build_client_self_payload(
    state: &ControlPlaneState,
    query: &ClientIdentityQuery,
) -> anyhow::Result<Value> {
    let context = resolve_identity_context(
        state,
        query.public_id.as_deref(),
        query.agent_did.as_deref(),
    )
    .await;
    let controller_id = context.public_memory_owner.controller.clone();
    let public_id = context.public_identity.as_ref().map_or_else(
        || controller_id.clone(),
        |identity| identity.public_id.clone(),
    );
    let display_name = context.public_identity.as_ref().map_or_else(
        || public_id.clone(),
        |identity| identity.display_name.clone(),
    );
    let wallet_agent_did = context
        .public_memory_owner
        .agent_did
        .clone()
        .or_else(|| {
            context
                .public_identity
                .as_ref()
                .and_then(|identity| identity.agent_did.clone())
        })
        .unwrap_or_else(|| state.agent_did.clone());
    let watt = watt_balance(state, &wallet_agent_did).await;
    let payment_account = active_wallet_payment_account_payload(state);
    let payment_accounts = wallet_payment_accounts_payload(state);
    let payment_account_binding = wallet_payment_binding_payload(state);
    let geo = &state.geo_location;
    Ok(json!({
        "id": public_id,
        "display_name": display_name,
        "agent_did": wallet_agent_did.clone(),
        "watt_balance": watt.balance,
        "watt_balance_available": watt.available,
        "watt_ledger_account": wallet_agent_did.clone(),
        "wallet_bound_agent_did": wallet_agent_did,
        "active_payment_account": payment_account,
        "payment_accounts": payment_accounts,
        "payment_account_binding": payment_account_binding,
        "status": "online",
        "lat": geo.lat,
        "lng": geo.lng,
        "coordinate_source": geo.source,
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

/// The node's organizations. `missions`, the gateway's view, adds each one's mission count;
/// the snapshot leaves it out, since the gateway counts the missions it keeps.
async fn build_client_organizations_payload(
    state: &ControlPlaneState,
    limit: usize,
    missions: Option<&[CivilMission]>,
) -> Vec<Value> {
    let organizations = state.organization_registry.lock().await;
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
            let mut entry = json!({
                "id": organization.organization_id,
                "name": organization.name,
                "member_count": active_member_count,
                "treasury_watt": organization.treasury_watt,
                "founded_at": timestamp_to_rfc3339(organization.created_at),
                "status": organization_client_status(organization.active, active_member_count),
            });
            if let Some(missions) = missions {
                entry["mission_count"] = json!(
                    missions
                        .iter()
                        .filter(|mission| {
                            mission.publisher_kind == MissionPublisherKind::Organization
                                && mission.publisher == organization.organization_id
                                && mission.status != MissionStatus::Cancelled
                        })
                        .count()
                );
            }
            entry
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

pub(crate) async fn list_nearby(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ClientListQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let items = match swarm_peers_with_live_connection_status(&state).await {
        Ok(peers) => peers
            .into_iter()
            .take(limit)
            .map(compact_nearby_peer_payload)
            .collect::<Vec<_>>(),
        Err(error) => return internal_error(&error),
    };
    let count = items.len();
    let payload = json!({
        "ok": true,
        "generated_at": Utc::now().timestamp(),
        "count": count,
        "items": items,
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.nearby.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": count})),
    });

    Json(payload).into_response()
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
    let missions = match board_view(&state).await {
        Ok(missions) => missions.list(None),
        Err(error) => return error.into_response(),
    };
    let payload = build_client_organizations_payload(&state, limit, Some(&missions)).await;

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

pub(crate) async fn client_export(
    State(state): State<ControlPlaneState>,
    Query(query): Query<ClientExportQuery>,
) -> Response {
    let signed_snapshot = match build_signed_public_client_snapshot(&state, &query).await {
        Ok(snapshot) => snapshot,
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
            "organization_count": snapshot.organizations.len(),
            "public_blocks_count": snapshot.public_blocks.len(),
        })),
    });
    Json(signed_snapshot).into_response()
}

pub async fn build_signed_public_client_snapshot(
    state: &ControlPlaneState,
    query: &ClientExportQuery,
) -> anyhow::Result<SignedPublicClientSnapshot> {
    let public_id_query = ClientIdentityQuery {
        agent_did: query.agent_did.clone(),
        public_id: query.public_id.clone(),
    };
    let snapshot = build_public_client_snapshot(state, query, &public_id_query).await?;
    let signature = state.sign_payload(&snapshot)?;
    Ok(SignedPublicClientSnapshot {
        payload: snapshot,
        signature,
        signer_agent_did: state.identity.agent_did.clone(),
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

fn source_identity_from_operator(operator: &Value) -> (Option<String>, Option<String>) {
    let public_id = operator
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let agent_identity = operator
        .get("display_name")
        .and_then(Value::as_str)
        .map(str::to_string);
    (public_id, agent_identity)
}

fn attach_source_identity(
    mut value: Value,
    source_public_id: Option<&str>,
    source_agent_identity: Option<&str>,
) -> Value {
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    insert_source_identity_fields(object, source_public_id, source_agent_identity);
    value
}

fn insert_source_identity_fields(
    object: &mut Map<String, Value>,
    source_public_id: Option<&str>,
    source_agent_identity: Option<&str>,
) {
    if let Some(source_public_id) = source_public_id {
        object
            .entry("source_public_id".to_string())
            .or_insert_with(|| Value::String(source_public_id.to_string()));
    }
    if let Some(source_agent_identity) = source_agent_identity {
        object
            .entry("source_agent_identity".to_string())
            .or_insert_with(|| Value::String(source_agent_identity.to_string()));
        object
            .entry("source_display_name".to_string())
            .or_insert_with(|| Value::String(source_agent_identity.to_string()));
    }
}

fn attach_source_identity_to_values(
    values: Vec<Value>,
    source_public_id: Option<&str>,
    source_agent_identity: Option<&str>,
) -> Vec<Value> {
    values
        .into_iter()
        .map(|value| attach_source_identity(value, source_public_id, source_agent_identity))
        .collect()
}

async fn build_public_blocks_payload(
    state: &ControlPlaneState,
    identity_query: &ClientIdentityQuery,
) -> anyhow::Result<Vec<Value>> {
    let relationship_entries =
        build_agent_relationship_payload(state, identity_query.public_id.as_deref(), None).await?;
    Ok(relationship_entries
        .into_iter()
        .filter(|entry| entry["relationship_state"].as_str() == Some("blocked"))
        .collect())
}

fn client_export_node_limit(query: &ClientExportQuery) -> usize {
    query
        .node_limit
        .or(query.peer_limit)
        .unwrap_or(25)
        .clamp(1, 200)
}

async fn build_public_board_snapshot_payload(
    state: &ControlPlaneState,
    query: &ClientExportQuery,
    source_public_id: Option<&str>,
    source_agent_identity: Option<&str>,
) -> Vec<Value> {
    attach_source_identity_to_values(
        build_public_board_messages_snapshot_payload(
            state,
            query.rpc_log_limit.unwrap_or(100).clamp(1, 200),
        )
        .await
        .unwrap_or_default(),
        source_public_id,
        source_agent_identity,
    )
}

#[allow(clippy::too_many_lines)]
async fn build_public_client_snapshot(
    state: &ControlPlaneState,
    query: &ClientExportQuery,
    identity_query: &ClientIdentityQuery,
) -> anyhow::Result<PublicClientSnapshot> {
    let operator = build_client_self_payload(state, identity_query).await?;
    let (source_public_id, source_agent_identity) = source_identity_from_operator(&operator);
    let public_blocks = build_public_blocks_payload(state, identity_query).await?;
    let nodes = build_client_peers_payload(state, client_export_node_limit(query)).await?;
    let source_public_id_ref = source_public_id.as_deref();
    let source_agent_identity_ref = source_agent_identity.as_deref();
    let space = crate::routes::space::snapshot::export(state).await?;
    Ok(PublicClientSnapshot {
        generated_at: Utc::now().timestamp(),
        node_id: state.agent_did.clone(),
        agent_identity: source_agent_identity.clone(),
        public_id: source_public_id.clone(),
        public_key: state.identity.public_key.clone(),
        network_status: build_client_network_status_payload(state).await?,
        nodes: attach_source_identity_to_values(
            nodes.clone(),
            source_public_id_ref,
            source_agent_identity_ref,
        ),
        peers: attach_source_identity_to_values(
            nodes,
            source_public_id_ref,
            source_agent_identity_ref,
        ),
        operator,
        rpc_logs: attach_source_identity_to_values(
            build_client_rpc_logs_payload(state, query.rpc_log_limit.unwrap_or(20).clamp(1, 200))?,
            source_public_id_ref,
            source_agent_identity_ref,
        ),
        public_blocks: attach_source_identity_to_values(
            public_blocks,
            source_public_id_ref,
            source_agent_identity_ref,
        ),
        public_topics: attach_source_identity_to_values(
            build_hives_payload(state, query.task_limit.unwrap_or(50).clamp(1, 200))
                .await
                .unwrap_or_default(),
            source_public_id_ref,
            source_agent_identity_ref,
        ),
        public_topic_messages: attach_source_identity_to_values(
            build_public_topic_messages_snapshot_payload(
                state,
                query.rpc_log_limit.unwrap_or(100).clamp(1, 200),
            )
            .await
            .unwrap_or_default(),
            source_public_id_ref,
            source_agent_identity_ref,
        ),
        public_board_messages: build_public_board_snapshot_payload(
            state,
            query,
            source_public_id_ref,
            source_agent_identity_ref,
        )
        .await,
        space_presences: space.space_presences,
        world_events: space.world_events,
        node_presence: space.node_presence,
        organizations: attach_source_identity_to_values(
            build_client_organizations_payload(
                state,
                query.organization_limit.unwrap_or(50).clamp(1, 500),
                None,
            )
            .await,
            source_public_id_ref,
            source_agent_identity_ref,
        ),
        leaderboard: Vec::new(),
    })
}

fn timestamp_to_rfc3339(timestamp: i64) -> String {
    Utc.timestamp_opt(timestamp, 0)
        .single()
        .map_or_else(|| Utc::now().to_rfc3339(), |dt| dt.to_rfc3339())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_peer_health_promotes_discovered_peer_to_connected() {
        let diagnostics = SwarmDiagnosticsSnapshot {
            ok: true,
            generated_at: "1970-01-01T00:00:00Z".to_owned(),
            network_service_started: true,
            snapshot: Some(json!({
                "peer_health": [
                    {
                        "network_peer_id": "peer-a",
                        "connected": true,
                        "recently_seen": true,
                        "stale": false,
                        "last_seen_age_ms": 12_000
                    }
                ]
            })),
            diagnostics: Vec::new(),
        };
        let connection_status = peer_connection_status_from_diagnostics(&diagnostics);
        let mut peers = vec![SwarmPeerView {
            node_id: "peer-a".to_owned(),
            connected: Some(false),
            recently_seen: Some(false),
            stale: Some(true),
            last_seen_age_ms: None,
            discovery: Some(json!({
                "source_kind": "contact_material_probe"
            })),
            metadata: None,
            relationship: None,
        }];

        apply_live_peer_connection_status(&mut peers, &connection_status);
        let full_payload = build_client_peer_payload(peers[0].clone());
        let compact_payload = compact_nearby_peer_payload(peers.remove(0));

        assert_eq!(full_payload["connected"].as_bool(), Some(true));
        assert_eq!(full_payload["recently_seen"].as_bool(), Some(true));
        assert_eq!(full_payload["stale"].as_bool(), Some(false));
        assert_eq!(full_payload["last_seen_age_ms"].as_u64(), Some(12_000));
        assert_eq!(full_payload["status"].as_str(), Some("online"));
        assert_eq!(compact_payload["connected"].as_bool(), Some(true));
        assert_eq!(compact_payload["recently_seen"].as_bool(), Some(true));
        assert_eq!(compact_payload["stale"].as_bool(), Some(false));
        assert_eq!(compact_payload["last_seen_age_ms"].as_u64(), Some(12_000));
        assert_eq!(compact_payload["status"].as_str(), Some("online"));
    }

    #[test]
    fn live_peer_health_reports_stale_connected_peer() {
        let diagnostics = SwarmDiagnosticsSnapshot {
            ok: true,
            generated_at: "1970-01-01T00:00:00Z".to_owned(),
            network_service_started: true,
            snapshot: Some(json!({
                "peer_health": [
                    {
                        "network_peer_id": "peer-a",
                        "connected": false,
                        "recently_seen": false,
                        "stale": true,
                        "last_seen_age_ms": 181_000
                    }
                ]
            })),
            diagnostics: Vec::new(),
        };
        let connection_status = peer_connection_status_from_diagnostics(&diagnostics);
        let mut peers = vec![SwarmPeerView {
            node_id: "peer-a".to_owned(),
            connected: Some(true),
            recently_seen: Some(true),
            stale: Some(false),
            last_seen_age_ms: None,
            discovery: Some(json!({
                "source_kind": "bootstrap"
            })),
            metadata: None,
            relationship: None,
        }];

        apply_live_peer_connection_status(&mut peers, &connection_status);
        let payload = build_client_peer_payload(peers.remove(0));

        assert_eq!(payload["connected"].as_bool(), Some(false));
        assert_eq!(payload["recently_seen"].as_bool(), Some(false));
        assert_eq!(payload["stale"].as_bool(), Some(true));
        assert_eq!(payload["last_seen_age_ms"].as_u64(), Some(181_000));
        assert_eq!(payload["status"].as_str(), Some("stale"));
    }

    #[test]
    fn compact_nearby_payload_ignores_wattswarm_relationship_state() {
        let peer = SwarmPeerView {
            node_id: "peer-a".to_owned(),
            connected: Some(false),
            recently_seen: Some(false),
            stale: Some(true),
            last_seen_age_ms: Some(181_000),
            discovery: Some(json!({
                "source_kind": "bootstrap"
            })),
            metadata: None,
            relationship: Some(json!({
                "relationship_state": "blocked"
            })),
        };

        let full_payload = build_client_peer_payload(peer.clone());
        let compact_payload = compact_nearby_peer_payload(peer);

        assert_eq!(full_payload["status"].as_str(), Some("blocked"));
        assert_eq!(full_payload["relationship_state"].as_str(), Some("blocked"));
        assert_eq!(
            full_payload["relationship"]["relationship_state"].as_str(),
            Some("blocked")
        );
        assert_eq!(compact_payload["status"].as_str(), Some("stale"));
        assert!(compact_payload.get("relationship").is_none());
    }
}
