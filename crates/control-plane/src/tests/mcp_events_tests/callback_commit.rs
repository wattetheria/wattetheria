use super::*;
use wattetheria_social::domain::friend_requests::{
    FriendRequest, FriendRequestDirection, FriendRequestState,
};

async fn brain_reply(
    request_id: &str,
    fail: bool,
) -> (
    String,
    tokio::task::JoinHandle<()>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let completion = json!({
        "action": "accept", "reason": "trusted peer",
        "payload": {"request_id": request_id, "correlation_id": "correlation-1"}
    })
    .to_string();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let received = calls.clone();
    let server = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            received.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let completion = completion.clone();
            async move {
                if fail {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": "brain unavailable"})),
                    )
                } else {
                    (
                        StatusCode::OK,
                        Json(json!({"choices": [{"message": {"content": completion}}]})),
                    )
                }
            }
        }),
    );
    let task = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
    (format!("http://{addr}/v1"), task, calls)
}

#[derive(Clone, Copy)]
struct FriendCommitCase {
    label: &'static str,
    seeded: bool,
}

struct FriendCommitFixture {
    dir: tempfile::TempDir,
    token: String,
    state: ControlPlaneState,
    config: std::path::PathBuf,
    key_path: std::path::PathBuf,
    owner: String,
    local_public_id: String,
    event_id: String,
    input: AgentEventEnvelope,
    brain_server: tokio::task::JoinHandle<()>,
    brain_calls: Arc<std::sync::atomic::AtomicUsize>,
}

async fn seed_commit_subscription(
    config: std::path::PathBuf,
    key_path: std::path::PathBuf,
    owner: String,
    token: String,
    id: String,
    event_type: &'static str,
) {
    tokio::task::spawn_blocking(move || {
        EventStore::open(&config, &key_path)
            .unwrap()
            .upsert_subscription(NewSubscription {
                id,
                source_kind: "agent_callback".into(),
                event_type: event_type.into(),
                target_agent_id: owner.clone(),
                credential_fingerprint: hex::encode(Sha256::digest(token.as_bytes())),
                owner_id: owner,
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

fn friend_callback_event(
    owner: &str,
    remote: &Identity,
    remote_public_id: &str,
    request_id: &str,
    case: FriendCommitCase,
) -> AgentEventEnvelope {
    let mut input = event(owner);
    input.event_id = format!("commit-{}", case.label);
    input.event_type = "friend_request".into();
    input.source_kind = "peer_relationship".into();
    input.source_node_id = Some("remote-node".into());
    let signed_message = if case.label == "commit_failed" {
        json!({"request_id": request_id, "correlation_id": "correlation-1"})
    } else {
        json!({
            "source_public_id": remote_public_id,
            "target_public_id": "local-node-id",
            "request_id": request_id,
            "correlation_id": "correlation-1"
        })
    };
    input.agent_envelope = Some(callbacks::signed_envelope(
        remote,
        owner,
        "social.relationship.request",
        signed_message,
    ));
    input.payload = json!({});
    input.requires_commit = true;
    input.allowed_actions = vec!["accept".into(), "reject".into()];
    input
}

async fn commit_cases() {
    for case in [
        FriendCommitCase {
            label: "accepted",
            seeded: true,
        },
        FriendCommitCase {
            label: "brain_failed",
            seeded: true,
        },
        FriendCommitCase {
            label: "commit_failed",
            seeded: false,
        },
    ] {
        let fixture = setup_friend_commit_case(case).await;
        assert_friend_commit_case(fixture, case).await;
    }
}

async fn setup_friend_commit_case(case: FriendCommitCase) -> FriendCommitFixture {
    let label = case.label;
    let (dir, _router, token, _policy, mut state) = build_test_app(100);
    let owner = state.agent_did.clone();
    let config = dir.path().join("mcp-events.sqlite");
    let key_path = dir.path().join("mcp-events.key");
    let local_public_id = bootstrap_broker_identity(app(state.clone()), &token, &owner).await;
    let remote = Identity::new_random();
    let remote_public_id = scoped_id("remote", &remote.agent_did);
    state
        .public_identity_registry
        .lock()
        .await
        .upsert(
            &remote_public_id,
            "Remote".into(),
            Some(remote.agent_did.clone()),
            true,
        )
        .unwrap();
    let request_id = format!("request-{label}");
    if case.seeded {
        friend_request_service::upsert_friend_request(
            &*state.social_store,
            &FriendRequest {
                request_id: request_id.clone(),
                local_public_id: local_public_id.clone(),
                remote_public_id: remote_public_id.clone(),
                remote_node_id: Some("remote-node".into()),
                direction: FriendRequestDirection::Inbound,
                state: FriendRequestState::Pending,
                decision_reason: None,
                correlation_id: Some("correlation-1".into()),
                created_at: 1,
                updated_at: 1,
                expires_at: None,
            },
        )
        .unwrap();
    }
    let (base_url, brain_server, brain_calls) =
        brain_reply(&request_id, label == "brain_failed").await;
    state.brain_engine = Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
        &BrainProviderConfig::OpenaiCompatible {
            base_url,
            model: "test-brain".into(),
            api_key_env: None,
            runtime_adapter: None,
        },
    )));
    let input = friend_callback_event(&owner, &remote, &remote_public_id, &request_id, case);
    let event_id = input.event_id.clone();

    seed_commit_subscription(
        config.clone(),
        key_path.clone(),
        owner.clone(),
        token.clone(),
        format!("subscription-{label}"),
        "friend_request",
    )
    .await;
    state.mcp_events = Some(Arc::new(
        McpEvents::open(config.clone(), key_path.clone(), owner.clone(), &token)
            .await
            .unwrap(),
    ));
    FriendCommitFixture {
        dir,
        token,
        state,
        config,
        key_path,
        owner,
        local_public_id,
        event_id,
        input,
        brain_server,
        brain_calls,
    }
}

async fn assert_friend_commit_case(fixture: FriendCommitFixture, case: FriendCommitCase) {
    let FriendCommitFixture {
        dir,
        token,
        state,
        config,
        key_path,
        owner,
        local_public_id,
        event_id,
        input,
        brain_server,
        brain_calls,
    } = fixture;
    let label = case.label;
    let callback_request = json!({"event": input});
    let callback = || {
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(callback_request.to_string()))
            .unwrap()
    };
    let response = request_json(app(state.clone()), callback()).await;
    assert_eq!(
        response["decision"]["action"],
        if label == "brain_failed" {
            Value::Null
        } else {
            json!("accept")
        },
        "{label}: {response}"
    );
    let action_record = state
        .local_db
        .load_agent_action_commit_for_event_action(&event_id, "social.agent_relationship_action")
        .unwrap();
    if label == "accepted" {
        assert_eq!(
            action_record.as_ref().map(|record| record.status.as_str()),
            Some("accepted")
        );
        let requests =
            friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
                .unwrap();
        assert_eq!(requests[0].state, FriendRequestState::Accepted);
    } else {
        assert!(
            action_record.is_none_or(|record| record.status != "accepted"),
            "{label}"
        );
    }

    let inspect_config = config.clone();
    let inspect_key = key_path.clone();
    let fingerprint = hex::encode(Sha256::digest(token.as_bytes()));
    tokio::task::spawn_blocking(move || {
        let store = EventStore::open(&inspect_config, &inspect_key).unwrap();
        assert!(
            store
                .claim_outbox(&owner, &fingerprint, Utc::now().timestamp(), 30, None)
                .unwrap()
                .is_none()
        );
    })
    .await
    .unwrap();
    assert!(brain_calls.load(std::sync::atomic::Ordering::SeqCst) > 0);

    let before = commit_received_count(&dir, &event_id);
    let duplicate = request_json(app(state.clone()), callback()).await;
    if label == "brain_failed" {
        assert_eq!(duplicate["ok"], false);
    } else {
        assert_ne!(
            duplicate["decision"]["decision_id"],
            response["decision"]["decision_id"]
        );
        assert_eq!(commit_received_count(&dir, &event_id), before + 1);
    }
    brain_server.abort();
    tokio::task::spawn_blocking(move || drop(state))
        .await
        .unwrap();
}

async fn review_brain() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            Json(json!({"choices": [{"message": {"content":
                r#"{"action":"human_review","reason":"needs review","payload":{}}"#
            }}]}))
        }),
    );
    let task = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
    (format!("http://{addr}/v1"), task)
}

struct TaskReviewFixture {
    dir: tempfile::TempDir,
    token: String,
    state: ControlPlaneState,
    config: std::path::PathBuf,
    key_path: std::path::PathBuf,
    owner: String,
    event_id: &'static str,
    input: AgentEventEnvelope,
    brain_server: Option<tokio::task::JoinHandle<()>>,
}

async fn task_human_review_cases() {
    let fixture = setup_task_review_case().await;
    assert_task_review_case(fixture).await;
}

async fn setup_task_review_case() -> TaskReviewFixture {
    let (dir, _router, token, _policy, mut state) = build_test_app(100);
    let owner = state.agent_did.clone();
    let config = dir.path().join("mcp-events.sqlite");
    let key_path = dir.path().join("mcp-events.key");
    let remote = Identity::new_random();
    let event_id = "task-review-normal";
    let payload = json!({
        "task_id": "mission-review-1", "claimer_node_id": "remote-node",
        "task_inputs": {"kind": "wattetheria_mission", "mission_id": "mission-review-1"}
    });
    let mut input = event(&owner);
    input.event_id = event_id.into();
    input.event_type = "task_claim_received".into();
    input.source_kind = "task_lifecycle".into();
    input.source_node_id = Some("remote-node".into());
    input.agent_envelope = Some(callbacks::signed_envelope(
        &remote,
        &owner,
        "task.claim",
        payload.clone(),
    ));
    input.payload = payload;
    input.requires_commit = true;
    input.allowed_actions = vec![
        "human_review".into(),
        "decide_claim".into(),
        "reject_claim".into(),
    ];

    seed_commit_subscription(
        config.clone(),
        key_path.clone(),
        owner.clone(),
        token.clone(),
        "subscription-task-review".into(),
        "task_claim_received",
    )
    .await;

    state.mcp_events = Some(Arc::new(
        McpEvents::open(config.clone(), key_path.clone(), owner.clone(), &token)
            .await
            .unwrap(),
    ));
    let (base_url, task) = review_brain().await;
    state.brain_engine = Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
        &BrainProviderConfig::OpenaiCompatible {
            base_url,
            model: "test-brain".into(),
            api_key_env: None,
            runtime_adapter: None,
        },
    )));
    let brain_server = Some(task);
    TaskReviewFixture {
        dir,
        token,
        state,
        config,
        key_path,
        owner,
        event_id,
        input,
        brain_server,
    }
}

async fn assert_task_review_case(fixture: TaskReviewFixture) {
    let TaskReviewFixture {
        dir,
        token,
        state,
        config,
        key_path,
        owner,
        event_id,
        input,
        brain_server,
    } = fixture;
    let callback_request = json!({"event": input});
    let request = || {
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(callback_request.to_string()))
            .unwrap()
    };
    let response = app(state.clone()).oneshot(request()).await.unwrap();
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["decision"]["action"], "human_review");
    assert_eq!(body["decision"]["route"], "wattetheria_commit");

    let inspect_config = config.clone();
    let inspect_key = key_path.clone();
    let inspect_owner = owner.clone();
    let fingerprint = hex::encode(Sha256::digest(token.as_bytes()));
    tokio::task::spawn_blocking(move || {
        let store = EventStore::open(&inspect_config, &inspect_key).unwrap();
        assert!(
            store
                .claim_outbox(
                    &inspect_owner,
                    &fingerprint,
                    Utc::now().timestamp(),
                    30,
                    None
                )
                .unwrap()
                .is_none()
        );
    })
    .await
    .unwrap();
    assert_eq!(commit_received_count(&dir, event_id), 1);
    let duplicate = request_json(app(state.clone()), request()).await;
    assert_ne!(
        duplicate["decision"]["decision_id"],
        body["decision"]["decision_id"]
    );
    assert_eq!(commit_received_count(&dir, event_id), 2);
    if let Some(task) = brain_server {
        task.abort();
    }
    tokio::task::spawn_blocking(move || drop(state))
        .await
        .unwrap();
}

fn commit_received_count(dir: &tempfile::TempDir, event_id: &str) -> usize {
    crate::diagnostics::list_diagnostics(
        dir.path(),
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some(event_id.into()),
            ..Default::default()
        },
    )
    .unwrap()
    .iter()
    .filter(|entry| entry.phase == "agent_action.commit.received")
    .count()
}

#[tokio::test]
async fn sqlite_callback_projection_preserves_commit_and_redelivery_semantics() {
    commit_cases().await;
    task_human_review_cases().await;
}

#[tokio::test]
async fn callback_store_failure_does_not_change_brain_or_commit() {
    for subscribed in [false, true] {
        let case = FriendCommitCase {
            label: "accepted",
            seeded: true,
        };
        let mut fixture = setup_friend_commit_case(case).await;
        if !subscribed {
            let config = fixture.config.clone();
            tokio::task::spawn_blocking(move || {
                rusqlite::Connection::open(config)
                    .unwrap()
                    .execute_batch("DELETE FROM mcp_event_subscriptions")
                    .unwrap();
            })
            .await
            .unwrap();
            fixture.state.mcp_events = Some(Arc::new(
                McpEvents::open(
                    fixture.config.clone(),
                    fixture.key_path.clone(),
                    fixture.owner.clone(),
                    &fixture.token,
                )
                .await
                .unwrap(),
            ));
        }
        let config = fixture.config.clone();
        tokio::task::spawn_blocking(move || {
            rusqlite::Connection::open(config)
                .unwrap()
                .execute_batch("DROP TABLE mcp_event_outbox; DROP TABLE mcp_event_subscriptions")
                .unwrap();
        })
        .await
        .unwrap();
        let response = app(fixture.state.clone())
            .oneshot(
                Request::post("/agent-events")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"event": fixture.input}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["decision"]["action"], "accept");
        assert_eq!(body["ok"], true);
        let requests = friend_request_service::list_friend_requests(
            &*fixture.state.social_store,
            &fixture.local_public_id,
        )
        .unwrap();
        assert_eq!(requests[0].state, FriendRequestState::Accepted);
        fixture.brain_server.abort();
        tokio::task::spawn_blocking(move || drop(fixture))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn mcp_events_mode_publishes_without_brain_or_automatic_commit() {
    let case = FriendCommitCase {
        label: "accepted",
        seeded: true,
    };
    let mut fixture = setup_friend_commit_case(case).await;
    fixture.state.agent_event_mode = crate::mcp_events::AgentEventMode::McpEvents;
    let request = || {
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({"event": fixture.input}).to_string(),
            ))
            .unwrap()
    };
    for _ in 0..2 {
        let response = app(fixture.state.clone()).oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["ok"], true);
        assert!(body["decision"].is_null());
    }
    assert_eq!(
        fixture
            .brain_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(commit_received_count(&fixture.dir, &fixture.event_id), 0);
    let requests = friend_request_service::list_friend_requests(
        &*fixture.state.social_store,
        &fixture.local_public_id,
    )
    .unwrap();
    assert_eq!(requests[0].state, FriendRequestState::Pending);
    let config = fixture.config.clone();
    let key_path = fixture.key_path.clone();
    let owner = fixture.owner.clone();
    let fingerprint = hex::encode(Sha256::digest(fixture.token.as_bytes()));
    let projected: Value = tokio::task::spawn_blocking(move || {
        let store = EventStore::open(&config, &key_path).unwrap();
        let pending = store
            .claim_outbox(&owner, &fingerprint, Utc::now().timestamp(), 30, None)
            .unwrap()
            .unwrap();
        assert!(
            store
                .claim_outbox(&owner, &fingerprint, Utc::now().timestamp(), 30, None)
                .unwrap()
                .is_none()
        );
        serde_json::from_slice(&pending.payload).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(projected["data"]["decision_status"], "pending_external");
    assert_eq!(projected["data"]["commit_status"], "not_requested");
    assert_eq!(projected["data"]["requires_action"], true);
    assert!(projected["data"].get("chosen_action").is_none());
    fixture.brain_server.abort();
    tokio::task::spawn_blocking(move || drop(fixture))
        .await
        .unwrap();
}

#[tokio::test]
async fn mcp_events_mode_returns_retryable_error_without_brain_fallback() {
    for unavailable in [true, false] {
        let case = FriendCommitCase {
            label: "accepted",
            seeded: true,
        };
        let mut fixture = setup_friend_commit_case(case).await;
        fixture.state.agent_event_mode = crate::mcp_events::AgentEventMode::McpEvents;
        if unavailable {
            fixture.state.mcp_events = None;
        } else {
            let config = fixture.config.clone();
            tokio::task::spawn_blocking(move || {
                rusqlite::Connection::open(config)
                    .unwrap()
                    .execute_batch("DROP TABLE mcp_event_outbox")
                    .unwrap();
            })
            .await
            .unwrap();
        }
        let response = app(fixture.state.clone())
            .oneshot(
                Request::post("/agent-events")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"event": fixture.input}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["ok"], false);
        assert!(body["decision"].is_null());
        assert_eq!(
            fixture
                .brain_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(commit_received_count(&fixture.dir, &fixture.event_id), 0);
        fixture.brain_server.abort();
        tokio::task::spawn_blocking(move || drop(fixture))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn external_agent_uses_existing_mcp_action_after_verified_event() {
    let mut fixture = setup_friend_commit_case(FriendCommitCase {
        label: "accepted",
        seeded: true,
    })
    .await;
    fixture.state.agent_event_mode = crate::mcp_events::AgentEventMode::McpEvents;
    let context =
        crate::routes::identity::resolve_identity_context(&fixture.state, None, None).await;
    let local_public_id = context
        .public_memory_owner
        .public
        .unwrap_or(context.public_memory_owner.controller);
    let mut request = friend_request_service::list_friend_requests(
        &*fixture.state.social_store,
        &fixture.local_public_id,
    )
    .unwrap()
    .remove(0);
    request.local_public_id.clone_from(&local_public_id);
    friend_request_service::upsert_friend_request(&*fixture.state.social_store, &request).unwrap();
    fixture.local_public_id = local_public_id;
    let mut forged = serde_json::to_value(&fixture.input).unwrap();
    forged["agent_envelope"]["signature"] = json!("invalid-signature");
    let bad = app(fixture.state.clone())
        .oneshot(
            Request::post("/agent-events")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(json!({"event": forged}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        fixture
            .state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["pendingOutbox"],
        0
    );
    let accepted = app(fixture.state.clone())
        .oneshot(
            Request::post("/agent-events")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"event": fixture.input}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
    let config = fixture.config.clone();
    let key_path = fixture.key_path.clone();
    let owner = fixture.owner.clone();
    let fingerprint = hex::encode(Sha256::digest(fixture.token.as_bytes()));
    let projected: Value = tokio::task::spawn_blocking(move || {
        let store = EventStore::open(&config, &key_path).unwrap();
        let pending = store
            .claim_outbox(&owner, &fingerprint, Utc::now().timestamp(), 30, None)
            .unwrap()
            .unwrap();
        serde_json::from_slice(&pending.payload).unwrap()
    })
    .await
    .unwrap();
    let (status, action) = mcp_call(
        app(fixture.state.clone()),
        Some(&fixture.token),
        "tools/call",
        json!({"name": "accept_friend_request", "arguments": {"request_id": projected["data"]["request_id"]}}),
        |_| {},
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{action}");
    assert_eq!(action["result"]["isError"], false, "{action}");
    let requests = friend_request_service::list_friend_requests(
        &*fixture.state.social_store,
        &fixture.local_public_id,
    )
    .unwrap();
    assert_ne!(requests[0].state, FriendRequestState::Pending);
    assert_eq!(
        fixture
            .brain_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    fixture.brain_server.abort();
    tokio::task::spawn_blocking(move || drop(fixture))
        .await
        .unwrap();
}
