use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::auth::authorize;
use crate::state::{ControlPlaneState, GalaxyMapQuery};
use wattetheria_kernel::audit::AuditEntry;

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
