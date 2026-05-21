use super::*;

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
