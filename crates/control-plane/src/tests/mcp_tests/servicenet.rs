use super::*;

fn alpha_servicenet_settlement() -> Value {
    json!({
        "layer": "web3",
        "rail": "x402",
        "request": {
            "settlement_receipt": alpha_x402_settlement_receipt()
        }
    })
}

#[tokio::test]
async fn mcp_list_servicenet_agents_reads_configured_servicenet() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state.clone());

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_servicenet_agents",
                "arguments": {
                    "limit": 1,
                    "offset": 1
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["count"].as_u64(), Some(1));
    assert_eq!(content["limit"].as_u64(), Some(1));
    assert_eq!(content["offset"].as_u64(), Some(1));
    assert_eq!(content["next_offset"].as_u64(), Some(2));
    assert_eq!(content["has_more"].as_bool(), Some(true));
    assert_eq!(content["known_count"].as_u64(), Some(4));
    let agents = content["items"].as_array().unwrap();
    assert_eq!(agents.len(), 1);
    let beta = &agents[0];
    assert_eq!(beta["service_address"].as_str(), Some("beta@wattetheria"));
    assert_eq!(beta["name"].as_str(), Some("Agent Beta"));
    assert_eq!(beta["description"].as_str(), Some("Beta test agent"));
    assert_eq!(beta["status"].as_str(), Some("online"));
    assert_eq!(beta["version"].as_str(), Some("0.2.0"));
    assert_eq!(beta["provider_id"].as_str(), Some("provider-two"));
    assert_eq!(beta["runtime"].as_str(), Some("wattetheria_adapter"));
    assert_eq!(beta["protocol"].as_str(), Some("a2a_v1 / JSONRPC"));
    assert!(beta.get("url").is_none());
    assert_eq!(beta["risk_level"].as_str(), Some("medium"));
    assert_eq!(beta["reputation_score"].as_f64(), Some(500.0));
    assert_eq!(beta["cost"].as_u64(), Some(7));
    assert_eq!(beta["currency"].as_str(), Some("USDT"));
    assert!(beta.get("skills").is_none());

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_get_servicenet_agent_returns_enriched_summary() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_servicenet_agent",
                "arguments": {
                    "service_address": "alpha@wattetheria"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let agent = &response["result"]["structuredContent"];
    assert_eq!(agent["service_address"].as_str(), Some("alpha@wattetheria"));
    assert_eq!(agent["name"].as_str(), Some("Agent Alpha"));
    assert_eq!(agent["description"].as_str(), Some("Alpha test agent"));
    assert_eq!(agent["status"].as_str(), Some("published"));
    assert_eq!(agent["version"].as_str(), Some("0.1.0"));
    assert_eq!(agent["provider_id"].as_str(), Some("provider-one"));
    assert_eq!(agent["runtime"].as_str(), Some("wattetheria_adapter"));
    assert_eq!(agent["protocol"].as_str(), Some("a2a_v1 / JSONRPC"));
    assert!(agent.get("url").is_none());
    assert_eq!(agent["risk_level"].as_str(), Some("low"));
    assert_eq!(agent["reputation_score"].as_f64(), Some(750.0));
    assert_eq!(agent["cost"].as_u64(), Some(18));
    assert_eq!(agent["currency"].as_str(), Some("USDC"));
    assert_eq!(agent["supportsTask"].as_bool(), Some(true));
    assert_eq!(
        agent["payment"]["params"]["accepts"][0]["payTo"].as_str(),
        Some("0x742d35Cc6634C0532925a3b844Bc454e4438f44e")
    );
    assert_eq!(
        agent["skills"],
        json!([
            {
                "name": "Get weather",
                "description": "Returns current weather"
            }
        ])
    );

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_get_servicenet_agent_exposes_url_only_for_direct_agent() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_servicenet_agent",
                "arguments": {
                    "service_address": "beta@wattetheria"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let agent = &response["result"]["structuredContent"];
    assert_eq!(agent["service_address"].as_str(), Some("beta@wattetheria"));
    assert_eq!(agent["url"].as_str(), Some("https://example.net/adapter"));

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_send_service_agent_message_rejects_paid_agent_without_settlement_receipt() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_service_agent_message",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "message": "hello without payment proof"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert!(
        response["result"]["structuredContent"]["error"]
            .as_str()
            .unwrap()
            .contains("requires x402 settlement_receipt")
    );

    servicenet_server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_send_service_agent_message_reuses_runtime_sync_chain() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let callback_events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let callback_app = Router::new().route(
        "/agent-events",
        post({
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
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let agent_did = state.agent_did.clone();
    let expected_public_id = state
        .public_identity_registry
        .lock()
        .await
        .active_for_agent_did(&agent_did)
        .expect("default public identity should exist")
        .public_id;
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        agent_event_callback_base_url: Some(format!("http://{callback_addr}")),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_service_agent_message",
                "arguments": {
                    "service_address": "runtime@wattetheria",
                    "message": "hello servicenet"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["status"].as_str(), Some("completed"));
    assert_eq!(
        content["output"]["agent_envelope_source"].as_str(),
        Some(agent_did.as_str())
    );
    assert_eq!(
        content["output"]["caller_public_id"].as_str(),
        Some(expected_public_id.as_str())
    );
    let callback_events = callback_events.lock().await;
    assert_eq!(callback_events.len(), 1);
    assert_eq!(
        callback_events[0]["event"]["payload"]["response"]["agent_id"].as_str(),
        Some("agent-runtime")
    );
    assert!(
        callback_events[0]["event"]["payload"]["response"]
            .get("service_address")
            .is_none()
    );
    assert_eq!(
        callback_events[0]["event"]["payload"]["operation"].as_str(),
        Some("invoke")
    );
    assert_eq!(
        callback_events[0]["event"]["agent_envelope"]["source_agent_id"].as_str(),
        Some(agent_did.as_str())
    );
    assert_eq!(
        callback_events[0]["event"]["agent_envelope"]["target_agent_id"].as_str(),
        Some("agent-runtime")
    );
    let envelope_extensions = &callback_events[0]["event"]["agent_envelope"]["extensions"];
    assert!(
        envelope_extensions["nonce"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        envelope_extensions["request_digest"]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:"))
    );
    let issued_at_ms = envelope_extensions["issued_at_ms"]
        .as_u64()
        .expect("issued_at_ms should be present");
    let expires_at_ms = envelope_extensions["expires_at_ms"]
        .as_u64()
        .expect("expires_at_ms should be present");
    assert!(expires_at_ms > issued_at_ms);

    callback_server.abort();
    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_rejects_removed_servicenet_invoke_tools() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let app = app(state);

    for tool_name in [
        "invoke_servicenet_agent_sync",
        "invoke_servicenet_agent_async",
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": tool_name,
                "method": "tools/call",
                "params": {"name": tool_name, "arguments": {}}
            }),
        )
        .await;
        assert_eq!(response["result"]["isError"].as_bool(), Some(true));
        assert_eq!(
            response["result"]["structuredContent"]["error"].as_str(),
            Some(format!("unknown tool: {tool_name}").as_str())
        );
    }
}

#[tokio::test]
async fn mcp_send_service_agent_message_normalizes_query_to_message() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_service_agent_message",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "query": "Recommend dishes",
                    "settlement": alpha_servicenet_settlement()
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        response["result"]["structuredContent"]["output"]["echo"].as_str(),
        Some("Recommend dishes"),
        "response: {response}"
    );

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_send_service_agent_message_resolves_service_address() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_service_agent_message",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "message": "hello by service address",
                    "settlement": alpha_servicenet_settlement()
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["service_address"].as_str(),
        Some("alpha@wattetheria")
    );
    assert_eq!(
        content["output"]["echo"].as_str(),
        Some("hello by service address")
    );

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_send_service_agent_message_selects_async_a2a_semantics() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_service_agent_message",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "message": "start a background task",
                    "return_immediately": true,
                    "settlement": alpha_servicenet_settlement()
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["service_address"], "alpha@wattetheria");
    assert_eq!(content["output"]["echo"], "start a background task");
    assert_eq!(content["output"]["return_immediately"], true);

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_send_service_agent_message_reuses_runtime_async_chain() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state.clone());

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_service_agent_message",
                "arguments": {
                    "service_address": "runtime@wattetheria",
                    "message": "hello servicenet",
                    "return_immediately": true
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["status"].as_str(), Some("running"));
    assert_eq!(
        content["receipt_id"].as_str(),
        Some("00000000-0000-0000-0000-000000000099")
    );
    let pending: crate::routes::servicenet::async_jobs::ServiceNetAsyncInvocationStore = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::SERVICENET_ASYNC_INVOCATIONS)
        .expect("load async invocation store")
        .expect("async invocation store exists");
    assert!(
        pending
            .invocations
            .contains_key("00000000-0000-0000-0000-000000000099")
    );

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_get_servicenet_receipt_returns_receipt_status() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_servicenet_receipt",
                "arguments": {
                    "receipt_id": "00000000-0000-0000-0000-000000000099"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["receipt"]["status"].as_str(), Some("running"));
    assert_eq!(
        content["receipt"]["receipt_id"].as_str(),
        Some("00000000-0000-0000-0000-000000000099")
    );
    assert_eq!(
        content["receipt"]["service_address"].as_str(),
        Some("alpha@wattetheria")
    );
    assert!(content["receipt"].get("agent_id").is_none());

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_delete_servicenet_agent_resolves_service_address() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    crate::routes::servicenet::publish::save_publisher_state(
        &state.data_dir,
        &crate::routes::servicenet::publish::ServiceNetPublisherState {
            registrations: vec![
                crate::routes::servicenet::publish::ServiceNetPublisherRegistration {
                    provider_id: "provider-one".to_string(),
                    provider_did: state.servicenet_provider.did.clone(),
                    agent_id: "agent-alpha".to_string(),
                    service_did: "did:key:z6Mkg5K92URgXhcuTfqt9jntq75JgPKgaQj36ougEQ3PrDXM"
                        .to_string(),
                    service_address: Some("alpha@wattetheria".to_string()),
                    card_hash: "sha256:agent-alpha".to_string(),
                    version: "0.1.0".to_string(),
                    updated_at: "2026-06-04T00:00:00Z".to_string(),
                    execution: wattetheria_kernel::servicenet::ServiceAgentExecution::default(),
                    agent_card: json!({}),
                    deployment: json!({}),
                    review: json!({}),
                },
            ],
        },
    )
    .expect("save publisher state");
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "delete_servicenet_agent",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "reason": "retired"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["status"].as_str(), Some("ok"));
    assert_eq!(
        content["service_address"].as_str(),
        Some("alpha@wattetheria")
    );
    assert!(content.get("agent_id").is_none());
    assert_eq!(content["unpublished"]["status"].as_str(), Some("revoked"));
    assert_eq!(
        content["unpublished"]["service_address"].as_str(),
        Some("alpha@wattetheria")
    );
    assert!(content["unpublished"].get("agent_id").is_none());

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_get_service_agent_task_resolves_service_address() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_service_agent_task",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "task_id": "task-42",
                    "history_length": 3
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["status"].as_str(), Some("completed"));
    assert_eq!(content["task_id"].as_str(), Some("task-42"));
    assert_eq!(content["output"]["history_length"].as_u64(), Some(3));
    assert_eq!(
        content["service_address"].as_str(),
        Some("alpha@wattetheria")
    );
    assert!(content.get("agent_id").is_none());

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_send_service_agent_message_returns_authorization_url_when_oauth_is_required() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_service_agent_message",
                "arguments": {
                    "service_address": "oauth@wattetheria",
                    "message": "request ride"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["status"].as_str(), Some("auth_required"));
    assert_eq!(
        content["service_address"].as_str(),
        Some("oauth@wattetheria")
    );
    assert_eq!(
        content["authorizationUrl"].as_str(),
        Some("https://auth.example.com/oauth/authorize")
    );
    assert_eq!(
        content["security"][0]["oauth2"][0].as_str(),
        Some("rides:request")
    );

    servicenet_server.abort();
}
