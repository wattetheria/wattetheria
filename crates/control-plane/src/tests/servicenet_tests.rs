use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn servicenet_routes_list_agents_and_return_invoke_feedback() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let callback_events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let callback_app = axum::Router::new().route(
        "/agent-events",
        axum::routing::post({
            let callback_events = Arc::clone(&callback_events);
            move |Json(payload): Json<Value>| {
                let callback_events = Arc::clone(&callback_events);
                async move {
                    callback_events.lock().await.push(payload);
                    Json(json!({"ok": true, "acked_at": 1}))
                }
            }
        }),
    );
    let callback_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let callback_addr = callback_listener.local_addr().unwrap();
    let callback_server = tokio::spawn(async move {
        axum::serve(callback_listener, callback_app).await.unwrap();
    });
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        agent_executor_base_url: Some(format!("http://{callback_addr}")),
        agent_event_callback_base_url: Some(format!("http://{callback_addr}")),
        ..state
    };
    let app = app(state);

    let list_json = authed_get_json(app.clone(), &token, "/v1/wattetheria/servicenet/agents").await;
    assert_eq!(list_json["count"].as_u64(), Some(2));
    assert_eq!(
        list_json["items"][0]["agent_id"].as_str(),
        Some("agent-alpha")
    );

    let agent_json = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha",
    )
    .await;
    assert_eq!(agent_json["provider_id"].as_str(), Some("provider-one"));

    let invoke_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha/invoke",
        json!({
            "message": "hello servicenet",
            "input": {"amount": 7},
            "settlement": {
                "layer": "web3",
                "rail": "x402",
                "request": {
                    "protocol": "x402",
                    "payment_account_ref": "payment-account-123"
                }
            }
        }),
    )
    .await;
    assert_eq!(invoke_json["status"].as_str(), Some("completed"));
    assert_eq!(
        invoke_json["output"]["echo"].as_str(),
        Some("hello servicenet")
    );
    assert_eq!(invoke_json["task_id"].as_str(), Some("task-42"));
    assert_eq!(invoke_json["settlement"]["rail"].as_str(), Some("x402"));
    assert_eq!(
        invoke_json["settlement"]["request"]["payment_account_ref"].as_str(),
        Some("payment-account-123")
    );
    assert_eq!(
        invoke_json["payment_receipt"]["status"].as_str(),
        Some("submitted")
    );

    let async_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha/invoke-async",
        json!({
            "message": "hello async servicenet"
        }),
    )
    .await;
    assert_eq!(async_json["status"].as_str(), Some("running"));
    assert_eq!(
        async_json["receipt_id"].as_str(),
        Some("00000000-0000-0000-0000-000000000099")
    );

    let receipt_json = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/receipts/00000000-0000-0000-0000-000000000099",
    )
    .await;
    assert_eq!(receipt_json["receipt"]["status"].as_str(), Some("running"));

    let task_json = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha/tasks/task-42/get",
        json!({
            "history_length": 5
        }),
    )
    .await;
    assert_eq!(task_json["status"].as_str(), Some("completed"));
    assert_eq!(task_json["task_id"].as_str(), Some("task-42"));
    assert_eq!(task_json["output"]["result"].as_str(), Some("done"));
    assert_eq!(task_json["output"]["history_length"].as_u64(), Some(5));

    let callback_events = callback_events.lock().await;
    assert_eq!(callback_events.len(), 2);
    assert_eq!(
        callback_events[0]["event"]["event_type"].as_str(),
        Some("third_party_result")
    );
    assert_eq!(
        callback_events[0]["event"]["payload"]["operation"].as_str(),
        Some("invoke")
    );
    assert_eq!(
        callback_events[1]["event"]["payload"]["operation"].as_str(),
        Some("task_get")
    );
    assert_eq!(
        callback_events[1]["event"]["payload"]["task_id"].as_str(),
        Some("task-42")
    );

    callback_server.abort();
    servicenet_server.abort();
}

fn console_agent_publish_body(
    agent_id: Option<&str>,
    provider_id: Option<&str>,
    version: &str,
    risk_level: &str,
    description: &str,
    supports_task: bool,
) -> Value {
    let mut body = json!({
        "version": version,
        "risk_level": risk_level,
        "agent_card": {
            "name": "Console Agent",
            "description": description,
            "url": "https://console-agent.example.com/a2a",
            "preferredTransport": "JSONRPC",
            "protocolVersion": "1.0",
            "scope": "real_world",
            "origin": "custom_built",
            "domain": "GENERAL",
            "cost": if supports_task { 20 } else { 18 },
            "currency": "USDC",
            "supportsTask": supports_task,
            "skills": [{"name": "Lookup", "description": "Looks up records"}],
            "securitySchemes": {"none": {"type": "none"}},
            "security": [{"none": []}]
        }
    });
    if let Some(agent_id) = agent_id {
        body["agent_id"] = json!(agent_id);
    }
    if let Some(provider_id) = provider_id {
        body["provider_id"] = json!(provider_id);
    }
    body
}

fn assert_servicenet_template(template: &Value) {
    assert_eq!(
        template["defaults"]["preferredTransport"].as_str(),
        Some("JSONRPC")
    );
    assert!(
        template["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["name"].as_str() == Some("skills"))
    );
}

fn assert_published_console_agent(published_json: &Value, target_agent_id: &str, owner_did: &str) {
    assert_eq!(published_json["count"].as_u64(), Some(1));
    assert_eq!(published_json["provider_did"].as_str(), Some(owner_did));
    assert_eq!(
        published_json["items"][0]["agent_id"].as_str(),
        Some(target_agent_id)
    );
    assert_eq!(
        published_json["items"][0]["agent_card"]["name"].as_str(),
        Some("Console Agent")
    );
}

async fn spawn_list_counting_servicenet() -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
    Arc<AtomicUsize>,
) {
    let list_calls = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route(
        "/v1/agents",
        axum::routing::get({
            let list_calls = Arc::clone(&list_calls);
            move || {
                let list_calls = Arc::clone(&list_calls);
                async move {
                    list_calls.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"items": [], "count": 0, "limit": 100, "offset": 0}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, server, list_calls)
}

#[tokio::test]
async fn servicenet_published_agents_uses_local_publisher_state_only() {
    let (servicenet_addr, servicenet_server, list_calls) = spawn_list_counting_servicenet().await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let publisher_dir = state.data_dir.join("servicenet");
    fs::create_dir_all(&publisher_dir).unwrap();
    fs::write(
        publisher_dir.join("publisher-state.json"),
        serde_json::to_vec(&json!({
            "registrations": [{
                "provider_id": "provider-local",
                "provider_did": state.agent_did,
                "agent_id": "agent-local-only",
                "card_hash": "sha256:local",
                "version": "0.1.0",
                "updated_at": "2026-06-04T00:00:00Z",
                "agent_card": {
                    "name": "Local Only Agent",
                    "description": "Only this node published it",
                    "url": "https://local-only.example.com/a2a"
                },
                "deployment": {},
                "review": {"risk_level": "low"}
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let published = authed_get_json(
        app(state),
        &token,
        "/v1/wattetheria/servicenet/published-agents",
    )
    .await;
    assert_eq!(published["count"].as_u64(), Some(1));
    assert_eq!(
        published["items"][0]["agent_id"].as_str(),
        Some("agent-local-only")
    );
    assert_eq!(list_calls.load(Ordering::SeqCst), 0);
    servicenet_server.abort();
}

#[tokio::test]
async fn servicenet_template_and_publish_routes_support_console_flow() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state.clone());

    let template = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/agent-card-template",
    )
    .await;
    assert_servicenet_template(&template);

    let publish_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/publish",
        console_agent_publish_body(
            None,
            None,
            "0.1.0",
            "low",
            "Published from the console",
            false,
        ),
    )
    .await;
    assert_eq!(publish_json["status"].as_str(), Some("ok"));
    assert_eq!(publish_json["provider_id"].as_str(), Some("provider-ui"));
    assert_eq!(
        publish_json["provider_did"].as_str(),
        Some(state.agent_did.as_str())
    );
    let agent_id = publish_json["agent_id"].as_str().unwrap();
    assert!(agent_id.starts_with("console-agent-"));
    assert_eq!(
        publish_json["submission"]["attestations"]["provider_attester_did"].as_str(),
        Some(state.agent_did.as_str())
    );

    let published_json = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/published-agents",
    )
    .await;
    assert_published_console_agent(&published_json, agent_id, &state.agent_did);

    let update_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/publish",
        console_agent_publish_body(
            Some(agent_id),
            Some("provider-ui"),
            "0.1.1",
            "medium",
            "Updated from the console",
            true,
        ),
    )
    .await;
    assert_eq!(update_json["status"].as_str(), Some("ok"));
    assert_eq!(update_json["agent_id"].as_str(), Some(agent_id));
    assert_eq!(update_json["provider_id"].as_str(), Some("provider-ui"));
    assert_eq!(update_json["submission"]["version"].as_str(), Some("0.1.1"));

    let forbidden_update = authed_post(
        app,
        &token,
        "/v1/wattetheria/servicenet/publish",
        console_agent_publish_body(
            Some(agent_id),
            Some("provider-other"),
            "0.1.2",
            "medium",
            "Rejected update",
            true,
        ),
    )
    .await;
    assert_eq!(forbidden_update, StatusCode::FORBIDDEN);

    servicenet_server.abort();
}

#[tokio::test]
async fn servicenet_callback_decision_routes_into_mission_commit() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let mission_id = {
        let mut board = state.mission_board.lock().await;
        board
            .publish(
                "Inspect Result",
                "mission seeded for servicenet callback",
                &state.agent_did,
                wattetheria_kernel::civilization::missions::MissionPublisherKind::System,
                wattetheria_kernel::civilization::missions::MissionDomain::Trade,
                None,
                None,
                None,
                None,
                wattetheria_kernel::civilization::missions::MissionReward {
                    agent_watt: 0,
                    reputation: 0,
                    capacity: 0,
                    treasury_share_watt: 0,
                },
                Value::Null,
            )
            .mission_id
    };

    let callback_app = axum::Router::new().route(
        "/agent-events",
        axum::routing::post({
            let mission_id = mission_id.clone();
            move |Json(_payload): Json<Value>| {
                let mission_id = mission_id.clone();
                async move {
                    Json(json!({
                        "ok": true,
                        "acked_at": 1,
                        "decision": {
                            "decision_id": "dec-servicenet-1",
                            "action": "claim_mission",
                            "route": "wattetheria_commit",
                            "payload": {
                                "mission_id": mission_id,
                            }
                        }
                    }))
                }
            }
        }),
    );
    let callback_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let callback_addr = callback_listener.local_addr().unwrap();
    let callback_server = tokio::spawn(async move {
        axum::serve(callback_listener, callback_app).await.unwrap();
    });

    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        agent_executor_base_url: Some(format!("http://{callback_addr}")),
        agent_event_callback_base_url: Some(format!("http://{callback_addr}")),
        ..state
    };
    let app = app(state.clone());

    let invoke_json = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha/invoke",
        json!({
            "message": "claim the mission",
            "input": {"mode": "analysis"}
        }),
    )
    .await;
    assert_eq!(invoke_json["status"].as_str(), Some("completed"));

    let board = state.mission_board.lock().await;
    let claimed = board
        .list(None)
        .into_iter()
        .find(|mission| mission.mission_id == mission_id)
        .expect("mission present");
    assert_eq!(
        claimed.claimed_by.as_deref(),
        Some(state.agent_did.as_str())
    );
    assert_eq!(
        claimed.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Claimed
    );

    callback_server.abort();
    servicenet_server.abort();
}
