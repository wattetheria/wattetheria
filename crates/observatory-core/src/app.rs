use crate::routes;
use crate::store::SharedStore;
use axum::Router;
use axum::routing::{get, post};
use tower_http::cors::{Any, CorsLayer};

pub fn app(store: SharedStore) -> Router {
    Router::new()
        .route("/", get(routes::index))
        .route("/healthz", get(routes::healthz))
        .route("/api/docs", get(routes::api_docs))
        .route("/api/summaries", post(routes::ingest_summary))
        .route("/api/heatmap", get(routes::heatmap))
        .route("/api/rankings", get(routes::rankings))
        .route("/api/events", get(routes::events))
        .route("/api/planets", get(routes::planets))
        .route("/api/mirror/export", get(routes::mirror_export))
        .route("/api/mirror/import", post(routes::mirror_import))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
                .allow_headers(Any),
        )
        .with_state(store)
}
