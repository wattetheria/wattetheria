use crate::models::IngestResponse;
use crate::store::SharedStore;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use serde::Deserialize;
use wattetheria_kernel::types::SignedSummary;

#[derive(Debug, Deserialize)]
pub struct RankingQuery {
    pub metric: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct EventQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct PlanetQuery {
    pub limit: Option<usize>,
}

pub async fn index() -> impl IntoResponse {
    Html(include_str!("index.html"))
}

pub async fn healthz(State(store): State<SharedStore>) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    Json(serde_json::json!({
        "ok": true,
        "entries": guard.len(),
        "config": guard.config(),
    }))
}

pub async fn api_docs() -> impl IntoResponse {
    Json(serde_json::json!({
        "service": "wattetheria-observatory",
        "authoritative": false,
        "routes": [
            {"method": "POST", "path": "/api/summaries", "description": "ingest signed summary"},
            {"method": "GET", "path": "/api/heatmap", "description": "subnet activity aggregates"},
            {"method": "GET", "path": "/api/rankings", "description": "watt/power/contribution rankings"},
            {"method": "GET", "path": "/api/events", "description": "recent summary event stream"},
            {"method": "GET", "path": "/api/planets", "description": "planet health indicators"},
            {"method": "GET", "path": "/api/mirror/export", "description": "export snapshot for mirror sync"},
            {"method": "POST", "path": "/api/mirror/import", "description": "import mirrored snapshot"}
        ]
    }))
}

pub async fn ingest_summary(
    State(store): State<SharedStore>,
    Json(summary): Json<SignedSummary>,
) -> impl IntoResponse {
    let mut guard = store.write().expect("store write lock");
    match guard.ingest(summary) {
        Ok(result) => accepted(result),
        Err(error) if error.to_string().contains("rate_limit_exceeded_for_agent") => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

pub async fn heatmap(State(store): State<SharedStore>) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    Json(guard.heatmap())
}

pub async fn rankings(
    State(store): State<SharedStore>,
    Query(query): Query<RankingQuery>,
) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    let metric = query.metric.as_deref().unwrap_or("watt");
    let limit = query.limit.unwrap_or(20).max(1);
    Json(guard.rankings(metric, limit))
}

pub async fn events(
    State(store): State<SharedStore>,
    Query(query): Query<EventQuery>,
) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    Json(guard.event_stream(query.limit.unwrap_or(50).max(1)))
}

pub async fn planets(
    State(store): State<SharedStore>,
    Query(query): Query<PlanetQuery>,
) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    Json(guard.planet_health(query.limit.unwrap_or(20).max(1)))
}

pub async fn mirror_export(State(store): State<SharedStore>) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    Json(guard.export_snapshot())
}

pub async fn mirror_import(
    State(store): State<SharedStore>,
    Json(summaries): Json<Vec<SignedSummary>>,
) -> impl IntoResponse {
    let mut guard = store.write().expect("store write lock");
    match guard.ingest_batch(summaries) {
        Ok(result) => accepted(result),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

fn accepted(result: IngestResponse) -> axum::response::Response {
    (
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(result).expect("serialize ingest result")),
    )
        .into_response()
}
