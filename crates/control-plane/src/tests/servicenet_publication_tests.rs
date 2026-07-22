use super::*;
use wattetheria_kernel::servicenet::load_servicenet_publisher_state;

#[derive(Default)]
struct PublicationInterleaveControl {
    submission_count: std::sync::atomic::AtomicUsize,
    blocked_submission_started: tokio::sync::Notify,
    release_submission: tokio::sync::Notify,
    unpublish_started: tokio::sync::Notify,
}

#[derive(Clone)]
enum PublicationOutcome {
    Accepted,
    Rejected,
    ServerError,
    CommittedWithInvalidResponse,
    BlockSecondSubmission(Arc<PublicationInterleaveControl>),
}

async fn spawn_publication_servicenet(
    outcome: PublicationOutcome,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let submission_outcome = outcome.clone();
    let unpublish_outcome = outcome;
    let app = Router::new()
        .route(
            "/v1/providers/ownership-challenges",
            post(|Json(body): Json<Value>| async move {
                Json(json!({
                    "challenge_id": "00000000-0000-0000-0000-000000000123",
                    "challenge": "mock-challenge",
                    "provider_id": "provider-ui",
                    "provider_did": body["provider_did"],
                }))
            }),
        )
        .route(
            "/v1/providers/register",
            post(|Json(body): Json<Value>| async move {
                Json(json!({
                    "provider_id": body["provider_id"],
                    "provider_did": body["provider_did"],
                    "display_name": body["display_name"],
                    "status": "active",
                }))
            }),
        )
        .route(
            "/v1/agent-submissions",
            post(move || {
                let outcome = submission_outcome.clone();
                async move {
                    match outcome {
                        PublicationOutcome::Accepted => {
                            Json(json!({"status": "submitted"})).into_response()
                        }
                        PublicationOutcome::Rejected => (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error": "service_address is already registered"})),
                        )
                            .into_response(),
                        PublicationOutcome::ServerError => (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({"error": "temporary registry failure"})),
                        )
                            .into_response(),
                        PublicationOutcome::CommittedWithInvalidResponse => {
                            (StatusCode::OK, "committed").into_response()
                        }
                        PublicationOutcome::BlockSecondSubmission(control) => {
                            let submission_index = control
                                .submission_count
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            if submission_index == 1 {
                                control.blocked_submission_started.notify_one();
                                control.release_submission.notified().await;
                            }
                            Json(json!({"status": "submitted"})).into_response()
                        }
                    }
                }
            }),
        )
        .route(
            "/v1/agents/{agent_id}/unpublish",
            post(move || {
                let outcome = unpublish_outcome.clone();
                async move {
                    if let PublicationOutcome::BlockSecondSubmission(control) = outcome {
                        control.unpublish_started.notify_one();
                    }
                    Json(json!({"status": "revoked"}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, server)
}

fn rejected_customized_agent_body(agent_id: &str, adapter_url: &str) -> Value {
    json!({
        "agent_id": agent_id,
        "service_address": format!("{agent_id}@wattetheria"),
        "execution_mode": "customized_agent",
        "connection_mode": "wattetheria_direct",
        "protocol": "a2a_v1",
        "customized_agent_url": "http://127.0.0.1:8642/a2a",
        "agent_card": {
            "name": "Rejected Customized Agent",
            "description": "Exercises failed publication cleanup",
            "url": adapter_url,
            "preferredTransport": "JSONRPC",
            "protocolVersion": "1.0",
            "scope": "real_world",
            "origin": "custom_built",
            "domain": "GENERAL",
            "cost": 0,
            "currency": "USDC",
            "supportsTask": true,
            "skills": [{"name": "Test", "description": "Tests publication"}],
            "securitySchemes": {"none": {"type": "none"}},
            "security": [{"none": []}]
        }
    })
}

#[tokio::test]
async fn failed_publication_removes_the_new_service_agent_identity() {
    let (servicenet_addr, servicenet_server) =
        spawn_publication_servicenet(PublicationOutcome::Rejected).await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let identity_store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let identity_path = identity_store.identity_path("rejected-agent");

    let status = authed_post(
        app(state.clone()),
        &token,
        "/v1/wattetheria/servicenet/publish",
        rejected_customized_agent_body("rejected-agent", "https://provider.example.com/adapter"),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!identity_path.exists());
    assert!(
        load_servicenet_publisher_state(&state.data_dir)
            .unwrap()
            .registrations
            .is_empty()
    );
    servicenet_server.abort();
}

#[tokio::test]
async fn failed_update_restores_the_existing_service_agent_identity() {
    let (servicenet_addr, servicenet_server) =
        spawn_publication_servicenet(PublicationOutcome::Rejected).await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let identity_store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let original = identity_store
        .load_or_create(
            "existing-agent",
            "https://provider.example.com/original-adapter",
        )
        .unwrap();

    let status = authed_post(
        app(state),
        &token,
        "/v1/wattetheria/servicenet/publish",
        rejected_customized_agent_body(
            "existing-agent",
            "https://provider.example.com/replacement-adapter",
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(identity_store.load("existing-agent").unwrap(), original);
    servicenet_server.abort();
}

#[tokio::test]
async fn ambiguous_publication_result_keeps_identity_and_local_registration() {
    let (servicenet_addr, servicenet_server) =
        spawn_publication_servicenet(PublicationOutcome::CommittedWithInvalidResponse).await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let identity_store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let identity_path = identity_store.identity_path("ambiguous-agent");

    let status = authed_post(
        app(state.clone()),
        &token,
        "/v1/wattetheria/servicenet/publish",
        rejected_customized_agent_body("ambiguous-agent", "https://provider.example.com/adapter"),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(identity_path.exists());
    let publisher_state = load_servicenet_publisher_state(&state.data_dir).unwrap();
    assert_eq!(publisher_state.registrations.len(), 1);
    assert_eq!(publisher_state.registrations[0].agent_id, "ambiguous-agent");
    servicenet_server.abort();
}

#[tokio::test]
async fn server_error_keeps_identity_and_local_registration() {
    let (servicenet_addr, servicenet_server) =
        spawn_publication_servicenet(PublicationOutcome::ServerError).await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let identity_store = FileServiceAgentIdentityStore::new(&state.data_dir);

    let status = authed_post(
        app(state.clone()),
        &token,
        "/v1/wattetheria/servicenet/publish",
        rejected_customized_agent_body(
            "server-error-agent",
            "https://provider.example.com/adapter",
        ),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(identity_store.identity_path("server-error-agent").exists());
    assert_eq!(
        load_servicenet_publisher_state(&state.data_dir)
            .unwrap()
            .registrations
            .len(),
        1
    );
    servicenet_server.abort();
}

#[tokio::test]
async fn connection_failure_keeps_new_identity_and_staged_registration() {
    let (servicenet_addr, servicenet_server) =
        spawn_publication_servicenet(PublicationOutcome::Accepted).await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let bootstrap_status = authed_post(
        app(state.clone()),
        &token,
        "/v1/wattetheria/servicenet/publish",
        rejected_customized_agent_body(
            "existing-provider-agent",
            "https://provider.example.com/adapter",
        ),
    )
    .await;
    assert_eq!(bootstrap_status, StatusCode::OK);
    servicenet_server.abort();
    let _ = servicenet_server.await;

    let status = authed_post(
        app(state.clone()),
        &token,
        "/v1/wattetheria/servicenet/publish",
        rejected_customized_agent_body(
            "connection-error-agent",
            "https://provider.example.com/second-adapter",
        ),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let identity_store = FileServiceAgentIdentityStore::new(&state.data_dir);
    assert!(
        identity_store
            .identity_path("connection-error-agent")
            .exists()
    );
    let publisher_state = load_servicenet_publisher_state(&state.data_dir).unwrap();
    assert_eq!(publisher_state.registrations.len(), 2);
    assert!(
        publisher_state
            .registrations
            .iter()
            .any(|registration| registration.agent_id == "connection-error-agent")
    );
}

#[tokio::test]
async fn local_staging_failure_removes_identity_before_remote_submission() {
    let (servicenet_addr, servicenet_server) =
        spawn_publication_servicenet(PublicationOutcome::Accepted).await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    std::fs::write(state.data_dir.join("servicenet"), b"not a directory").unwrap();
    let identity_store = FileServiceAgentIdentityStore::new(&state.data_dir);

    let status = authed_post(
        app(state.clone()),
        &token,
        "/v1/wattetheria/servicenet/publish",
        rejected_customized_agent_body(
            "staging-error-agent",
            "https://provider.example.com/adapter",
        ),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!identity_store.identity_path("staging-error-agent").exists());
    servicenet_server.abort();
}

#[tokio::test]
async fn concurrent_agent_publications_keep_both_local_registrations() {
    let (servicenet_addr, servicenet_server) =
        spawn_publication_servicenet(PublicationOutcome::Accepted).await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let first = authed_post(
        app(state.clone()),
        &token,
        "/v1/wattetheria/servicenet/publish",
        rejected_customized_agent_body("concurrent-one", "https://one.example.com/adapter"),
    );
    let second = authed_post(
        app(state.clone()),
        &token,
        "/v1/wattetheria/servicenet/publish",
        rejected_customized_agent_body("concurrent-two", "https://two.example.com/adapter"),
    );

    let (first_status, second_status) = tokio::join!(first, second);

    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(second_status, StatusCode::OK);
    let publisher_state = load_servicenet_publisher_state(&state.data_dir).unwrap();
    assert_eq!(publisher_state.registrations.len(), 2);
    assert!(
        publisher_state
            .registrations
            .iter()
            .any(|registration| registration.agent_id == "concurrent-one")
    );
    assert!(
        publisher_state
            .registrations
            .iter()
            .any(|registration| registration.agent_id == "concurrent-two")
    );
    servicenet_server.abort();
}

#[tokio::test]
async fn unpublish_waits_for_an_inflight_publish_of_the_same_agent() {
    let control = Arc::new(PublicationInterleaveControl::default());
    let (servicenet_addr, servicenet_server) = spawn_publication_servicenet(
        PublicationOutcome::BlockSecondSubmission(Arc::clone(&control)),
    )
    .await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let initial_status = authed_post(
        app(state.clone()),
        &token,
        "/v1/wattetheria/servicenet/publish",
        rejected_customized_agent_body("serialized-agent", "https://provider.example.com/adapter"),
    )
    .await;
    assert_eq!(initial_status, StatusCode::OK);

    let update_state = state.clone();
    let update_token = token.clone();
    let update = tokio::spawn(async move {
        authed_post(
            app(update_state),
            &update_token,
            "/v1/wattetheria/servicenet/publish",
            rejected_customized_agent_body(
                "serialized-agent",
                "https://provider.example.com/updated-adapter",
            ),
        )
        .await
    });
    control.blocked_submission_started.notified().await;

    let unpublish_state = state.clone();
    let unpublish_token = token.clone();
    let unpublish = tokio::spawn(async move {
        authed_post(
            app(unpublish_state),
            &unpublish_token,
            "/v1/wattetheria/servicenet/agents/serialized-agent/unpublish",
            json!({}),
        )
        .await
    });
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            control.unpublish_started.notified(),
        )
        .await
        .is_err()
    );

    control.release_submission.notify_one();
    assert_eq!(update.await.unwrap(), StatusCode::OK);
    assert_eq!(unpublish.await.unwrap(), StatusCode::OK);
    assert!(
        load_servicenet_publisher_state(&state.data_dir)
            .unwrap()
            .registrations
            .is_empty()
    );
    servicenet_server.abort();
}
