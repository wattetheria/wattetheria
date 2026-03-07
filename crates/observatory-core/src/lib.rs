//! Non-authoritative observatory API for signed summaries, rankings, and mirror replication.

pub mod app;
pub mod models;
pub mod routes;
pub mod store;

pub use app::app;
pub use models::{
    EventStreamEntry, HeatPoint, IngestResponse, PlanetHealthEntry, RankingEntry, StoreConfig,
};
pub use store::{SharedStore, SummaryStore};

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::json;
    use std::sync::{Arc, RwLock};
    use tower::ServiceExt;
    use wattetheria_kernel::identity::Identity;
    use wattetheria_kernel::summary::build_signed_summary;
    use wattetheria_kernel::types::AgentStats;

    #[tokio::test]
    async fn ingest_and_query() {
        let store = Arc::new(RwLock::new(SummaryStore::default()));
        let app = app(store.clone());

        let identity = Identity::new_random();
        let summary = build_signed_summary(
            &identity,
            Some("planet-a".to_string()),
            &AgentStats {
                power: 5,
                watt: 120,
                reputation: 4,
                capacity: 40,
            },
            &[],
        )
        .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/api/summaries")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&summary).unwrap()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);

        let req = Request::builder()
            .uri("/api/heatmap")
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/rankings?metric=wealth")
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/events?limit=10")
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/planets?limit=10")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rejects_invalid_signature() {
        let store = Arc::new(RwLock::new(SummaryStore::default()));
        let app = app(store);
        let identity = Identity::new_random();
        let mut summary =
            build_signed_summary(&identity, None, &AgentStats::default(), &[]).unwrap();
        summary.watt = 999;

        let req = Request::builder()
            .method("POST")
            .uri("/api/summaries")
            .header("content-type", "application/json")
            .body(Body::from(json!(summary).to_string()))
            .unwrap();

        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rate_limit_returns_429() {
        let store = Arc::new(RwLock::new(SummaryStore::with_config(StoreConfig {
            max_entries: 100,
            max_entry_age_sec: 3600,
            max_ingest_per_agent_per_minute: 1,
        })));
        let app = app(store);

        let identity = Identity::new_random();
        let summary = build_signed_summary(&identity, None, &AgentStats::default(), &[]).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/api/summaries")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&summary).unwrap()))
            .unwrap();
        let first = app.clone().oneshot(req).await.unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);

        let req = Request::builder()
            .method("POST")
            .uri("/api/summaries")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&summary).unwrap()))
            .unwrap();
        let second = app.oneshot(req).await.unwrap();
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn mirror_roundtrip_works() {
        let store = Arc::new(RwLock::new(SummaryStore::default()));
        let app = app(store);
        let identity = Identity::new_random();
        let summary = build_signed_summary(
            &identity,
            Some("planet-x".to_string()),
            &AgentStats {
                power: 8,
                watt: 88,
                reputation: 1,
                capacity: 3,
            },
            &[],
        )
        .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/api/mirror/import")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&vec![summary]).unwrap()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);

        let req = Request::builder()
            .uri("/api/mirror/export")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let body = res.into_body().collect().await.unwrap().to_bytes();
        let exported: Vec<wattetheria_kernel::types::SignedSummary> =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(exported.len(), 1);
    }

    #[tokio::test]
    async fn rankings_support_civilization_metrics() {
        let store = Arc::new(RwLock::new(SummaryStore::default()));
        let app = app(store);

        for (power, watt, reputation, capacity) in [(4, 60, 2, 4), (8, 40, 9, 2)] {
            let identity = Identity::new_random();
            let summary = build_signed_summary(
                &identity,
                Some("planet-a".to_string()),
                &AgentStats {
                    power,
                    watt,
                    reputation,
                    capacity,
                },
                &[],
            )
            .unwrap();

            let req = Request::builder()
                .method("POST")
                .uri("/api/summaries")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&summary).unwrap()))
                .unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::ACCEPTED);
        }

        for metric in ["wealth", "power", "security", "trade", "culture"] {
            let req = Request::builder()
                .uri(format!("/api/rankings?metric={metric}&limit=5"))
                .body(Body::empty())
                .unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let body = res.into_body().collect().await.unwrap().to_bytes();
            let rows: Vec<RankingEntry> = serde_json::from_slice(&body).unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].metric, metric);
        }
    }
}
