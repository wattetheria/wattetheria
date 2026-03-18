use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use wattetheria_kernel::audit::AuditEntry;

use crate::auth::{authorize, internal_error};
use crate::state::{ControlPlaneState, NetworkPeersQuery};

pub(crate) async fn network_status(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let network = match state.swarm_bridge.network_status().await {
        Ok(network) => network,
        Err(error) => return internal_error(&error),
    };
    let peers = match state.swarm_bridge.peers().await {
        Ok(peers) => peers,
        Err(error) => return internal_error(&error),
    };
    let total_nodes = peers.len() + 1;
    let active_nodes = if network.running { total_nodes } else { 0 };
    let health_percent = if total_nodes == 0 {
        0
    } else {
        ((active_nodes * 100) / total_nodes) as u64
    };
    let payload = json!({
        "running": network.running,
        "mode": network.mode,
        "total_nodes": total_nodes,
        "active_nodes": active_nodes,
        "health_percent": health_percent,
        "avg_latency_ms": 0,
        "peer_protocol_distribution": network.peer_protocol_distribution,
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "network".to_string(),
        action: "network.status.query".to_string(),
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

pub(crate) async fn network_peers(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<NetworkPeersQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let peers = match state.swarm_bridge.peers().await {
        Ok(peers) => peers,
        Err(error) => return internal_error(&error),
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let payload = peers
        .into_iter()
        .take(limit)
        .map(|peer| {
            let (lat, lng) = derived_geo(&peer.node_id);
            json!({
                "id": peer.node_id,
                "status": "online",
                "distance_km": derived_distance_km(&peer.node_id),
                "latency_ms": 0,
                "lat": lat,
                "lng": lng,
                "coordinate_source": "derived",
            })
        })
        .collect::<Vec<_>>();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "network".to_string(),
        action: "network.peers.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": payload.len()})),
    });

    Json(json!({"peers": payload})).into_response()
}

pub(crate) fn derived_geo(value: &str) -> (f64, f64) {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    let hash = hasher.finish();
    let lat_bucket = f64::from(u16::try_from(hash & 0xffff).unwrap_or(0)) / f64::from(u16::MAX);
    let lng_bucket =
        f64::from(u16::try_from((hash >> 16) & 0xffff).unwrap_or(0)) / f64::from(u16::MAX);
    let lat = -60.0 + lat_bucket * 120.0;
    let lng = -170.0 + lng_bucket * 340.0;
    (lat, lng)
}

pub(crate) fn derived_distance_km(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    25 + (hasher.finish() % 1800)
}
