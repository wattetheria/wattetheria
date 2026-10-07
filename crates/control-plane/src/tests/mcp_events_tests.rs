use super::*;
use crate::mcp_events::McpEvents;
use crate::mcp_events::catalog::ProjectOutcome;
use crate::mcp_events::store::{EventStore, NewSubscription};
use crate::routes::agent_events::AgentEventEnvelope;
use axum::http::{HeaderMap, HeaderValue, Request};
use sha2::Sha256;

async fn events_app() -> (tempfile::TempDir, Router, String, ControlPlaneState) {
    let (dir, _router, token, _policy, mut state) = build_test_app(100);
    let events = McpEvents::open(
        dir.path().join("mcp-events.sqlite"),
        dir.path().join("mcp-events.key"),
        state.agent_did.clone(),
        &token,
    )
    .await
    .unwrap();
    state.mcp_events = Some(Arc::new(events));
    (dir, app(state.clone()), token, state)
}

async fn mcp_call(
    router: Router,
    token: Option<&str>,
    method: &str,
    params: Value,
    change_headers: impl FnOnce(&mut HeaderMap),
) -> (StatusCode, Value) {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "mcp-protocol-version",
        HeaderValue::from_static("2025-11-25"),
    );
    headers.insert("host", HeaderValue::from_static("127.0.0.1:7777"));
    if let Some(token) = token {
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
    }
    change_headers(&mut headers);
    let request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .body(axum::body::Body::from(
            json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}).to_string(),
        ))
        .unwrap();
    let (mut parts, body) = request.into_parts();
    parts.headers = headers;
    let response = router
        .oneshot(Request::from_parts(parts, body))
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

fn event(owner: &str) -> AgentEventEnvelope {
    AgentEventEnvelope {
        event_id: "source-event-1".into(),
        event_type: "third_party_result".into(),
        source_kind: "task_lifecycle".into(),
        source_node_id: None,
        target_agent_id: Some(owner.into()),
        target_executor: None,
        agent_envelope: None,
        payload: json!({"agent_id": "agent-1", "task_id": "task-1"}),
        requires_commit: false,
        allowed_actions: Vec::new(),
        correlation_id: None,
        dedupe_key: None,
        created_at: 1_760_000_000_000,
    }
}

mod callback_commit;
mod callbacks;
mod subscription_receiver;
mod subscriptions;

#[tokio::test]
async fn redelivery_reenters_brain_after_transient_failure() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let (_dir, _router, _token, mut state) = events_app().await;
    let healthy = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let server_healthy = healthy.clone();
    let server_calls = calls.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let healthy = server_healthy.clone();
            let calls = server_calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                if healthy.load(Ordering::SeqCst) {
                    (
                        StatusCode::OK,
                        Json(json!({"choices": [{"message": {"content":
                            r#"{"action":"human_review","reason":"retry succeeded","payload":{}}"#
                        }}]})),
                    )
                } else {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": "temporary"})),
                    )
                }
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    state.brain_engine = Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
        &BrainProviderConfig::OpenaiCompatible {
            base_url: format!("http://{addr}/v1"),
            model: "test-brain".into(),
            api_key_env: None,
            runtime_adapter: None,
        },
    )));
    let mut input = event(&state.agent_did);
    input.allowed_actions = vec!["human_review".into()];
    let request = || {
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(json!({"event": input}).to_string()))
            .unwrap()
    };
    let failed = request_json(app(state.clone()), request()).await;
    assert_eq!(failed["ok"], false);
    let failed_calls = calls.load(Ordering::SeqCst);
    assert!(failed_calls > 0);
    healthy.store(true, Ordering::SeqCst);
    let retried = request_json(app(state.clone()), request()).await;
    assert_eq!(retried["ok"], true);
    assert_eq!(retried["decision"]["action"], "human_review");
    assert!(calls.load(Ordering::SeqCst) > failed_calls);
    let repeated = request_json(app(state.clone()), request()).await;
    assert_ne!(
        repeated["decision"]["decision_id"],
        retried["decision"]["decision_id"]
    );
    server.abort();
}

async fn seed_lifecycle_subscription(config: std::path::PathBuf, key_path: std::path::PathBuf) {
    let owner = "events-owner";
    let token = "events-token";
    tokio::task::spawn_blocking(move || {
        let store = EventStore::open(&config, &key_path).unwrap();
        store
            .upsert_subscription(NewSubscription {
                id: "subscription-1".into(),
                source_kind: "agent_callback".into(),
                event_type: "third_party_result".into(),
                target_agent_id: owner.into(),
                credential_fingerprint: hex::encode(Sha256::digest(token.as_bytes())),
                owner_id: owner.into(),
                callback_url: "https://callback.example/events".into(),
                callback_secret: "test-secret".into(),
                old_callback_secret: None,
                old_secret_expires_at: None,
                canonical_arguments: b"{}".to_vec(),
                expires_at: None,
            })
            .unwrap();
    })
    .await
    .unwrap();
}

async fn receipt_lifecycle(config: std::path::PathBuf, key_path: std::path::PathBuf) {
    let owner = "events-owner";
    let token = "events-token";
    seed_lifecycle_subscription(config.clone(), key_path.clone()).await;
    let service = McpEvents::open(config.clone(), key_path.clone(), owner.into(), token)
        .await
        .unwrap();
    let input = event(owner);
    let outcome = ProjectOutcome {
        decision_status: "no_action",
        commit_status: "not_requested",
        chosen_action: None,
        route: None,
        requires_action: false,
    };
    service.publish(&input, &outcome).await.unwrap();
    service.publish(&input, &outcome).await.unwrap();
    assert_eq!(service.diagnostics().await.unwrap()["pendingOutbox"], 1);
    tokio::task::spawn_blocking(move || drop(service))
        .await
        .unwrap();

    let reopened = McpEvents::open(config.clone(), key_path.clone(), owner.into(), token)
        .await
        .unwrap();
    reopened.publish(&input, &outcome).await.unwrap();
    assert_eq!(reopened.diagnostics().await.unwrap()["pendingOutbox"], 1);
    tokio::task::spawn_blocking(move || drop(reopened))
        .await
        .unwrap();
    let body: Value = tokio::task::spawn_blocking(move || {
        let store = EventStore::open(&config, &key_path).unwrap();
        let pending = store
            .claim_outbox(
                owner,
                &hex::encode(Sha256::digest(token.as_bytes())),
                Utc::now().timestamp(),
                30,
                None,
            )
            .unwrap()
            .unwrap();
        serde_json::from_slice(&pending.payload).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(body["name"], "wattetheria.agent.third_party_result");
    assert_eq!(body["data"]["decision_status"], "no_action");
    assert_eq!(body["data"]["task_id"], "task-1");
    let source_timestamp =
        chrono::DateTime::<Utc>::from_timestamp_millis(i64::try_from(input.created_at).unwrap())
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    assert_eq!(body["timestamp"], source_timestamp);
}

#[tokio::test]
async fn sqlite_callback_duplicate_no_action_outbox_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    receipt_lifecycle(
        dir.path().join("mcp-events.sqlite"),
        dir.path().join("mcp-events.key"),
    )
    .await;
}
