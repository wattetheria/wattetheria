//! Integration test for observatory summary ingestion and aggregation endpoints.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::{Arc, RwLock};
use tower::ServiceExt;
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::summary::build_signed_summary;
use wattetheria_kernel::types::AgentStats;
use wattetheria_observatory_core::{SummaryStore, app};

#[tokio::test]
async fn observatory_accepts_signed_summary() {
    let store = Arc::new(RwLock::new(SummaryStore::default()));
    let router = app(store);

    let identity = Identity::new_random();
    let summary = build_signed_summary(
        &identity,
        Some("planet-a".to_string()),
        &AgentStats {
            power: 5,
            watt: 50,
            reputation: 2,
            capacity: 9,
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

    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let req = Request::builder()
        .uri("/api/heatmap")
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
