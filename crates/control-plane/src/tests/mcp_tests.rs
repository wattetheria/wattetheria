use super::*;
use wattetheria_social::domain::friend_requests::{
    FriendRequest, FriendRequestDirection, FriendRequestState,
};

const MCP_AGENT_TOOL_NAMES: &[&str] = &[
    "client_export",
    "client_task_activity",
    "list_agent_payments",
    "get_agent_payment",
    "propose_agent_payment",
    "authorize_agent_payment",
    "submit_agent_payment",
    "settle_agent_payment",
    "reject_agent_payment",
    "cancel_agent_payment",
    "list_hives",
    "create_hive",
    "list_hive_messages",
    "post_hive_message",
    "subscribe_hive",
    "unsubscribe_hive",
    "list_missions",
    "publish_mission",
    "claim_mission",
    "complete_mission",
    "settle_mission",
    "list_friends",
    "upsert_local_friend",
    "list_nearby",
    "list_friend_requests",
    "list_sent_friend_requests",
    "get_friend_request",
    "accept_friend_request",
    "reject_friend_request",
    "request_agent_friend",
    "list_agent_dm_threads",
    "list_agent_dm_messages",
    "send_agent_dm_message",
    "send_mailbox_message",
    "list_mailbox_messages",
    "ack_mailbox_message",
    "list_servicenet_agents",
    "get_servicenet_agent",
    "invoke_servicenet_agent_sync",
    "invoke_servicenet_agent_async",
    "get_servicenet_agent_task",
    "get_servicenet_receipt",
];

async fn mcp_request(app: Router, token: &str, body: Value) -> Value {
    request_json(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn mcp_tools_list_matches_expected_agent_tool_surface() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }),
    )
    .await;

    let mut actual = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected = MCP_AGENT_TOOL_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn mcp_tools_list_surfaces_tool_availability_metadata() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        agent_topic_bridge_enabled: false,
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    let create_hive = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("create_hive"))
        .unwrap();
    let list_hives = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("list_hives"))
        .unwrap();
    let servicenet = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("list_servicenet_agents"))
        .unwrap();

    assert_eq!(
        create_hive["_meta"]["wattetheria"]["available"].as_bool(),
        Some(false)
    );
    assert_eq!(
        list_hives["_meta"]["wattetheria"]["available"].as_bool(),
        Some(true)
    );
    assert_eq!(
        servicenet["_meta"]["wattetheria"]["available"].as_bool(),
        Some(false)
    );
    assert_eq!(
        servicenet["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/servicenet/agents")
    );
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
    let app = app(state);

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
    assert_eq!(content["next_offset"], Value::Null);
    assert_eq!(content["has_more"].as_bool(), Some(false));
    assert_eq!(content["known_count"].as_u64(), Some(2));
    let agents = content["items"].as_array().unwrap();
    assert_eq!(agents.len(), 1);
    let beta = &agents[0];
    assert_eq!(beta["agent_id"].as_str(), Some("agent-beta"));
    assert_eq!(beta["name"].as_str(), Some("Agent Beta"));
    assert_eq!(beta["description"].as_str(), Some("Beta test agent"));
    assert_eq!(beta["status"].as_str(), Some("online"));
    assert_eq!(beta["version"].as_str(), Some("0.2.0"));
    assert_eq!(beta["provider_id"].as_str(), Some("provider-two"));
    assert_eq!(beta["runtime"].as_str(), Some("remote_http"));
    assert_eq!(beta["protocol"].as_str(), Some("google_a2a / JSONRPC"));
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
                    "agent_id": "agent-alpha"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let agent = &response["result"]["structuredContent"];
    assert_eq!(agent["agent_id"].as_str(), Some("agent-alpha"));
    assert_eq!(agent["name"].as_str(), Some("Agent Alpha"));
    assert_eq!(agent["description"].as_str(), Some("Alpha test agent"));
    assert_eq!(agent["status"].as_str(), Some("published"));
    assert_eq!(agent["version"].as_str(), Some("0.1.0"));
    assert_eq!(agent["provider_id"].as_str(), Some("provider-one"));
    assert_eq!(agent["runtime"].as_str(), Some("remote_http"));
    assert_eq!(agent["protocol"].as_str(), Some("google_a2a / JSONRPC"));
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
#[allow(clippy::too_many_lines)]
async fn mcp_propose_agent_payment_accepts_servicenet_agent_id() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let sender_address = seed_active_payment_account(&state);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "propose_agent_payment",
                "arguments": {
                    "agent_id": "agent-alpha",
                    "amount": "0.18",
                    "currency": "USDC",
                    "rail": "x402",
                    "layer": "web3"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["ok"].as_bool(), Some(true));
    assert_eq!(
        content["payment"]["recipient_public_id"].as_str(),
        Some("agent-alpha")
    );
    assert_eq!(
        content["payment"]["recipient_address"].as_str(),
        Some("0x742d35Cc6634C0532925a3b844Bc454e4438f44e")
    );
    assert_eq!(content["payment"]["amount"].as_str(), Some("0.18"));
    assert_eq!(content["payment"]["network"].as_str(), Some("base"));
    assert_eq!(content["transport"]["mode"].as_str(), Some("servicenet"));
    assert_eq!(
        content["transport"]["agent_id"].as_str(),
        Some("agent-alpha")
    );
    let payment_id = content["payment"]["payment_id"].as_str().unwrap();

    let authorized = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "authorize_agent_payment",
                "arguments": {
                    "payment_id": payment_id
                }
            }
        }),
    )
    .await;
    assert_eq!(authorized["result"]["isError"].as_bool(), Some(false));

    let submitted = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "submit_agent_payment",
                "arguments": {
                    "payment_id": payment_id,
                    "settlement_receipt": {
                        "success": true,
                        "payer": sender_address,
                        "transaction": "0x89c91c789e57059b17285e7ba1716a1f5ff4c5dace0ea5a5135f26158d0421b9",
                        "network": "base",
                        "amount": "180000",
                        "payTo": "0x742d35Cc6634C0532925a3b844Bc454e4438f44e"
                    }
                }
            }
        }),
    )
    .await;
    assert_eq!(submitted["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        submitted["result"]["structuredContent"]["status"].as_str(),
        Some("submitted")
    );
    assert_eq!(
        submitted["result"]["structuredContent"]["amount"].as_str(),
        Some("0.18")
    );
    assert_eq!(
        submitted["result"]["structuredContent"]["settlement_receipt"]["amount"].as_str(),
        Some("0.18")
    );

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_propose_agent_payment_normalizes_stablecoin_amount_for_counterpart() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-stable", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Stable".to_string(),
                Some(remote_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &remote_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "remote-runtime".to_string(),
            Some("12D3KooStablePeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "propose_agent_payment",
                "arguments": {
                    "counterpart_public_id": remote_public_id,
                    "amount": "1",
                    "currency": "USDT",
                    "rail": "x402",
                    "layer": "web3",
                    "network": "base"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["payment"]["amount"].as_str(), Some("1"));
    let payment_commands = bridge.payment_commands.lock().await;
    assert_eq!(payment_commands.len(), 1);
    assert_eq!(
        payment_commands[0].payment["amount"].as_str(),
        Some("1000000")
    );
}

#[tokio::test]
async fn mcp_invoke_servicenet_agent_sync_attaches_agent_envelope_for_public_agent() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let agent_did = state.agent_did.clone();
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
                "name": "invoke_servicenet_agent_sync",
                "arguments": {
                    "agent_id": "agent-alpha",
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

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_invoke_servicenet_agent_async_returns_receipt_id() {
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
                "name": "invoke_servicenet_agent_async",
                "arguments": {
                    "agent_id": "agent-alpha",
                    "message": "hello servicenet"
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

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_invoke_servicenet_agent_sync_returns_authorization_url_when_oauth_is_required() {
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
                "name": "invoke_servicenet_agent_sync",
                "arguments": {
                    "agent_id": "agent-oauth",
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
        content["authorizationUrl"].as_str(),
        Some("https://auth.example.com/oauth/authorize")
    );
    assert_eq!(
        content["security"][0]["oauth2"][0].as_str(),
        Some("rides:request")
    );

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_tools_call_writes_product_diagnostics() {
    let (_dir, app, token, _policy, state) = build_test_app(100);

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "client_export",
                "arguments": {}
            }
        }),
    )
    .await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));

    let entries = crate::diagnostics::list_diagnostics(
        &state.data_dir,
        &crate::diagnostics::DiagnosticFilter {
            component: Some("wattetheria.mcp".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase == "tool.call.received"
                && entry.details["tool_name"].as_str() == Some("client_export"))
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase == "tool.call.succeeded"
                && entry.details["tool_name"].as_str() == Some("client_export"))
    );
}

#[tokio::test]
async fn mcp_array_payload_tools_return_object_structured_content() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    for (id, tool_name) in [
        (1, "list_friends"),
        (2, "list_agent_dm_threads"),
        (3, "list_agent_dm_messages"),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": tool_name,
                    "arguments": {}
                }
            }),
        )
        .await;

        assert_eq!(response["result"]["isError"].as_bool(), Some(false));
        let structured_content = &response["result"]["structuredContent"];
        assert!(structured_content.is_object(), "{tool_name}");
        assert!(structured_content["items"].is_array(), "{tool_name}");
        let text_payload: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert!(text_payload.is_object(), "{tool_name}");
        assert!(text_payload["items"].is_array(), "{tool_name}");
    }
}

#[tokio::test]
async fn mcp_request_agent_friend_sends_relationship_action_to_remote_node() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "remote_node_id": "nearby-node-1",
                    "message": {
                        "kind": "friend_request",
                        "text": "hello nearby node"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, "nearby-node-1");
    assert_eq!(
        serde_json::to_value(&command.action).unwrap().as_str(),
        Some("request")
    );
    assert_eq!(
        command.agent_envelope.capability.as_deref(),
        Some("social.friend.request")
    );
    assert!(
        command
            .agent_envelope
            .message
            .get("source_public_id")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("target_public_id")
            .and_then(Value::as_str),
        Some("nearby-node-1")
    );
}

#[tokio::test]
async fn mcp_request_agent_friend_resolves_target_agent_did_to_remote_node() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-delta", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Delta".to_string(),
                Some(remote_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &remote_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "remote-runtime".to_string(),
            Some("12D3KooTargetPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "target_agent_did": remote_identity.agent_did,
                    "remote_node_id": "stale-nearby-node",
                    "message": {
                        "kind": "friend_request",
                        "text": "hello known agent"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, "12D3KooTargetPeer");
    assert_eq!(
        command.agent_envelope.target_agent_id.as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("target_public_id")
            .and_then(Value::as_str),
        Some(remote_public_id.as_str())
    );
}

#[tokio::test]
async fn mcp_send_agent_dm_message_sends_signed_direct_message_to_friend() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let local_public_id = context
        .public_memory_owner
        .public
        .unwrap_or(context.public_memory_owner.controller);
    let remote_public_id = scoped_id("broker-dm", &remote_identity.agent_did);
    wattetheria_social::application::remote_identity_service::upsert_remote_identity(
        &*state.social_store,
        &wattetheria_social::domain::identities::RemoteIdentityProfile {
            public_id: remote_public_id.clone(),
            agent_did: remote_identity.agent_did.clone(),
            display_name: "Broker DM".to_string(),
            description: None,
            capabilities: Vec::new(),
            skills: Vec::new(),
            did_document_json: None,
            active: true,
            last_profile_fetched_at: Some(1),
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed remote identity");
    wattetheria_social::application::transport_binding_service::upsert_transport_binding(
        &*state.social_store,
        &wattetheria_social::domain::transport_bindings::RemoteTransportBinding {
            public_id: remote_public_id.clone(),
            agent_did: Some(remote_identity.agent_did.clone()),
            transport_kind:
                wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm,
            transport_node_id: "12D3KooDmPeer".to_string(),
            binding_source: "friendship".to_string(),
            binding_confidence: 90,
            binding_proof_json: None,
            binding_verified: true,
            binding_verified_at: Some(1),
            updated_at: 1,
        },
    )
    .expect("seed remote transport binding");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: None,
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed active friendship");

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_agent_dm_message",
                "arguments": {
                    "counterpart_public_id": remote_public_id,
                    "content": {
                        "type": "text",
                        "text": "hello over private group dm"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.dm_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, "12D3KooDmPeer");
    assert_eq!(
        command.agent_envelope.capability.as_deref(),
        Some("social.dm.send")
    );
    assert_eq!(
        command.content["text"].as_str(),
        Some("hello over private group dm")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_accept_and_reject_friend_requests_send_relationship_actions() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let accept_identity = Identity::new_random();
    let reject_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let local_public_id = context
        .public_memory_owner
        .public
        .unwrap_or(context.public_memory_owner.controller);
    let accept_public_id = scoped_id("broker-accept", &accept_identity.agent_did);
    let reject_public_id = scoped_id("broker-reject", &reject_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &accept_public_id,
                "Broker Accept".to_string(),
                Some(accept_identity.agent_did.clone()),
                true,
            )
            .unwrap();
        identities
            .upsert(
                &reject_public_id,
                "Broker Reject".to_string(),
                Some(reject_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &accept_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "accept-runtime".to_string(),
            Some("12D3KooAcceptPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
        bindings.upsert(
            &reject_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "reject-runtime".to_string(),
            Some("12D3KooRejectPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    for (request_id, remote_public_id, remote_node_id, correlation_id) in [
        (
            "req-accept-1",
            accept_public_id.as_str(),
            "12D3KooAcceptPeer",
            "corr-accept-1",
        ),
        (
            "req-reject-1",
            reject_public_id.as_str(),
            "12D3KooRejectPeer",
            "corr-reject-1",
        ),
    ] {
        friend_request_service::upsert_friend_request(
            &*state.social_store,
            &FriendRequest {
                request_id: request_id.to_string(),
                local_public_id: local_public_id.clone(),
                remote_public_id: remote_public_id.to_string(),
                remote_node_id: Some(remote_node_id.to_string()),
                direction: FriendRequestDirection::Inbound,
                state: FriendRequestState::Pending,
                decision_reason: None,
                correlation_id: Some(correlation_id.to_string()),
                created_at: 1,
                updated_at: 1,
                expires_at: None,
            },
        )
        .expect("save inbound friend request");
    }

    let accept_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "accept_friend_request",
                "arguments": {"request_id": "req-accept-1"}
            }
        }),
    )
    .await;
    let reject_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "reject_friend_request",
                "arguments": {"request_id": "req-reject-1"}
            }
        }),
    )
    .await;

    assert_eq!(accept_response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(reject_response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Accept
    );
    assert_eq!(commands[0].remote_node_id, "12D3KooAcceptPeer");
    assert_eq!(
        commands[0]
            .agent_envelope
            .message
            .get("request_id")
            .and_then(Value::as_str),
        Some("req-accept-1")
    );
    assert_eq!(
        commands[0]
            .agent_envelope
            .message
            .get("correlation_id")
            .and_then(Value::as_str),
        Some("corr-accept-1")
    );
    assert_eq!(
        commands[1].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Reject
    );
    assert_eq!(commands[1].remote_node_id, "12D3KooRejectPeer");
    assert_eq!(
        commands[1]
            .agent_envelope
            .message
            .get("request_id")
            .and_then(Value::as_str),
        Some("req-reject-1")
    );
}

#[tokio::test]
async fn mcp_list_nearby_returns_compact_peer_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        peers: vec![SwarmPeerView {
            node_id: "peer-nearby-1".to_owned(),
            connected: Some(true),
            discovery: Some(json!({
                "source_kind": "bootstrap"
            })),
            metadata: Some(json!({
                "endpoint_id": "iroh-endpoint-nearby",
                "network_id": "mainnet:watt-galaxy",
                "protocol_version": "wattswarm/1.0.0",
                "handshake_status": "identified",
                "observed_addr": "198.51.100.2:4001",
                "listen_addrs": ["203.0.113.10:4001"]
            })),
            relationship: None,
        }],
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge;
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge_handle);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_nearby",
                "arguments": {}
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["ok"].as_bool(), Some(true));
    assert_eq!(content["count"].as_u64(), Some(1));
    let item = &content["items"][0];
    assert_eq!(item["remote_node_id"].as_str(), Some("peer-nearby-1"));
    assert_eq!(item["status"].as_str(), Some("online"));
    assert_eq!(item["connected"].as_bool(), Some(true));
    assert_eq!(item["endpoint"].as_str(), Some("iroh-endpoint-nearby"));
    assert_eq!(item["discovery"]["source_kind"].as_str(), Some("bootstrap"));
    assert_eq!(
        item["metadata"]["observed_addr"].as_str(),
        Some("198.51.100.2:4001")
    );
    assert_eq!(
        item["metadata"]["listen_addrs"][0].as_str(),
        Some("203.0.113.10:4001")
    );
    assert!(item.get("node_id").is_none());
    assert!(item.get("source_kind").is_none());
    assert!(item.get("request_agent_friend_arguments").is_none());
    assert!(item.get("target_agent_did").is_none());
    assert!(item.get("counterpart_public_id").is_none());
    assert!(item.get("relationship_state").is_none());
    assert!(item.get("relationship").is_none());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_friend_request_tools_split_list_and_detail_views() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-inbound", &remote_identity.agent_did);
    let remote_node_id = "12D3KooInboundPeer".to_string();
    let bridge = Arc::new(MockSwarmBridge {
        peers: vec![SwarmPeerView {
            node_id: remote_node_id.clone(),
            connected: Some(true),
            discovery: Some(json!({"source_kind": "bootstrap"})),
            metadata: Some(json!({
                "endpoint_id": "iroh-endpoint-inbound",
                "network_id": "mainnet:watt-etheria",
                "protocol_version": "wattswarm/1.0.0",
                "handshake_status": "identified",
                "observed_addr": "198.51.100.2:4001",
                "listen_addrs": ["203.0.113.10:4001"]
            })),
            relationship: None,
        }],
        relationship_views: Mutex::new(vec![
            SwarmPeerRelationshipView {
                remote_node_id: remote_node_id.clone(),
                relationship_state: "requested".to_string(),
                last_action: "request".to_string(),
                initiated_by: "remote".to_string(),
                agent_envelope: Some(SwarmAgentEnvelope {
                    protocol: "google_a2a".to_string(),
                    transport_profile: None,
                    source_agent_id: Some(remote_identity.agent_did.clone()),
                    target_agent_id: Some(identity.agent_did.clone()),
                    source_node_id: Some(remote_node_id.clone()),
                    target_node_id: None,
                    capability: Some("peer.relationship.request".to_string()),
                    source_agent_card: None,
                    message: json!({
                        "kind": "friend_request",
                        "text": "hello, I am Alice from node X",
                        "request_id": "req-inbound-1",
                        "correlation_id": "corr-inbound-1",
                        "sent_at": 1_710_000_100
                    }),
                    extensions: None,
                    signature: Some("sig-inbound".to_string()),
                }),
                requested_at: Some(1_710_000_100),
                responded_at: None,
                blocked_at: None,
                cleared_at: None,
                updated_at: 1_710_000_105,
            },
            SwarmPeerRelationshipView {
                remote_node_id: "12D3KooOutboundPeer".to_string(),
                relationship_state: "requested".to_string(),
                last_action: "request".to_string(),
                initiated_by: "local".to_string(),
                agent_envelope: Some(SwarmAgentEnvelope {
                    protocol: "google_a2a".to_string(),
                    transport_profile: None,
                    source_agent_id: Some(identity.agent_did.clone()),
                    target_agent_id: Some(remote_identity.agent_did.clone()),
                    source_node_id: None,
                    target_node_id: Some("12D3KooOutboundPeer".to_string()),
                    capability: Some("peer.relationship.request".to_string()),
                    source_agent_card: None,
                    message: json!({
                        "kind": "friend_request",
                        "text": "outbound hello",
                        "request_id": "req-outbound-1",
                        "correlation_id": "corr-outbound-1",
                        "sent_at": 1_710_000_090
                    }),
                    extensions: None,
                    signature: Some("sig-outbound".to_string()),
                }),
                requested_at: Some(1_710_000_090),
                responded_at: None,
                blocked_at: None,
                cleared_at: None,
                updated_at: 1_710_000_095,
            },
        ]),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge;
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Agent Alice".to_string(),
                Some(remote_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &remote_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "remote-runtime".to_string(),
            Some(remote_node_id.clone()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }

    let list_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_friend_requests",
                "arguments": {}
            }
        }),
    )
    .await;
    let list_content = &list_response["result"]["structuredContent"];
    assert_eq!(list_content["ok"].as_bool(), Some(true));
    assert_eq!(list_content["count"].as_u64(), Some(1));
    assert_eq!(
        list_content["items"][0]["request_id"].as_str(),
        Some("req-inbound-1")
    );
    assert_eq!(
        list_content["items"][0]["from"].as_str(),
        Some("Agent Alice")
    );
    assert_eq!(
        list_content["items"][0]["preview"].as_str(),
        Some("hello, I am Alice from node X")
    );
    assert!(list_content["items"][0].get("network").is_none());
    assert!(list_content["items"][0].get("remote_node_id").is_none());

    let sent_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_sent_friend_requests",
                "arguments": {}
            }
        }),
    )
    .await;
    let sent_content = &sent_response["result"]["structuredContent"];
    assert_eq!(sent_content["count"].as_u64(), Some(1));
    assert_eq!(
        sent_content["items"][0]["request_id"].as_str(),
        Some("req-outbound-1")
    );
    assert_eq!(sent_content["items"][0]["state"].as_str(), Some("pending"));

    let get_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "get_friend_request",
                "arguments": {
                    "request_id": "req-inbound-1"
                }
            }
        }),
    )
    .await;
    let detail = &get_response["result"]["structuredContent"];
    assert_eq!(detail["ok"].as_bool(), Some(true));
    assert_eq!(
        detail["agent"]["display_name"].as_str(),
        Some("Agent Alice")
    );
    assert_eq!(
        detail["agent"]["agent_did"].as_str(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(detail["message"]["kind"].as_str(), Some("friend_request"));
    assert_eq!(
        detail["message"]["text"].as_str(),
        Some("hello, I am Alice from node X")
    );
    assert_eq!(
        detail["network"]["remote_node_id"].as_str(),
        Some(remote_node_id.as_str())
    );
    assert_eq!(detail["network"]["status"].as_str(), Some("online"));
    assert_eq!(
        detail["network"]["metadata"]["observed_addr"].as_str(),
        Some("198.51.100.2:4001")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_tools_list_surfaces_precise_input_schemas_for_agent_tools() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();

    let publish_mission = find_tool(tools, "publish_mission");
    assert_schema_requires(
        publish_mission,
        &["title", "description", "domain", "reward", "payload"],
    );
    assert_eq!(
        publish_mission["inputSchema"]["properties"]["title"]["type"].as_str(),
        Some("string")
    );
    assert_schema_omits(publish_mission, &["publisher", "publisher_kind"]);
    assert_eq!(
        publish_mission["inputSchema"]["properties"]
            .get("body")
            .and_then(Value::as_object),
        None
    );

    let propose_payment = find_tool(tools, "propose_agent_payment");
    assert_schema_requires(propose_payment, &["amount", "currency", "rail"]);
    assert_schema_omits(propose_payment, &["public_id"]);
    assert_eq!(
        propose_payment["inputSchema"]["properties"]["agent_id"]["type"].as_str(),
        Some("string")
    );
    assert_eq!(
        propose_payment["inputSchema"]["properties"]["layer"]["enum"][1].as_str(),
        Some("web3")
    );

    let create_hive = find_tool(tools, "create_hive");
    assert_schema_omits(create_hive, &["public_id", "initial_message"]);
    assert_eq!(
        create_hive["inputSchema"]["properties"]["scope_hint"]["description"].as_str(),
        Some(
            "Wattswarm scope hint. Valid values are `global`, `region:<id>`, `node:<id>`, `local:<id>`, or `group:<id>`. For Hives, use `group:<hive-or-topic-id>`; do not use `topic:<id>`."
        )
    );
    let post_hive_message = find_tool(tools, "post_hive_message");
    assert_schema_omits(post_hive_message, &["public_id"]);
    let subscribe_hive = find_tool(tools, "subscribe_hive");
    assert_schema_omits(subscribe_hive, &["public_id"]);
    let unsubscribe_hive = find_tool(tools, "unsubscribe_hive");
    assert_schema_requires(unsubscribe_hive, &["hive_id"]);
    assert_schema_omits(unsubscribe_hive, &["public_id", "active"]);
    let upsert_local_friend = find_tool(tools, "upsert_local_friend");
    assert_schema_omits(upsert_local_friend, &["public_id"]);
    assert_eq!(
        find_tool(tools, "list_friends")["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-friends")
    );
    assert_eq!(
        upsert_local_friend["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friends")
    );
    let list_nearby = find_tool(tools, "list_nearby");
    assert_eq!(
        list_nearby["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/nearby")
    );
    assert!(
        list_nearby["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    let list_friend_requests = find_tool(tools, "list_friend_requests");
    assert_eq!(
        list_friend_requests["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests")
    );
    assert!(
        list_friend_requests["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("limit")
    );
    assert_schema_omits(list_friend_requests, &["direction", "state"]);
    let list_sent_friend_requests = find_tool(tools, "list_sent_friend_requests");
    assert_eq!(
        list_sent_friend_requests["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/sent-friend-requests")
    );
    let get_friend_request = find_tool(tools, "get_friend_request");
    assert_schema_requires(get_friend_request, &["request_id"]);
    assert_eq!(
        get_friend_request["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests/{request_id}")
    );
    let accept_friend_request = find_tool(tools, "accept_friend_request");
    assert_schema_requires(accept_friend_request, &["request_id"]);
    assert_eq!(
        accept_friend_request["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests/{request_id}/accept")
    );
    let reject_friend_request = find_tool(tools, "reject_friend_request");
    assert_schema_requires(reject_friend_request, &["request_id"]);
    assert_eq!(
        reject_friend_request["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests/{request_id}/reject")
    );
    let request_agent_friend = find_tool(tools, "request_agent_friend");
    assert!(
        request_agent_friend["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("target_agent_did")
    );
    assert!(
        !request_agent_friend["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field.as_str() == Some("remote_node_id"))
    );
    assert_schema_omits(request_agent_friend, &["public_id", "action"]);
    let list_agent_dm_threads = find_tool(tools, "list_agent_dm_threads");
    assert_eq!(
        list_agent_dm_threads["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-dm/threads")
    );
    let list_agent_dm_messages = find_tool(tools, "list_agent_dm_messages");
    assert_eq!(
        list_agent_dm_messages["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-dm/messages")
    );
    let send_agent_dm_message = find_tool(tools, "send_agent_dm_message");
    assert_schema_requires(send_agent_dm_message, &["counterpart_public_id", "content"]);
    assert_schema_omits(send_agent_dm_message, &["public_id"]);
    assert_eq!(
        send_agent_dm_message["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-dm/messages")
    );

    let settle_payment = find_tool(tools, "settle_agent_payment");
    assert_schema_requires(settle_payment, &["payment_id", "settlement_receipt"]);

    let submit_payment = find_tool(tools, "submit_agent_payment");
    assert_schema_requires(submit_payment, &["payment_id"]);
    assert!(
        submit_payment["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("settlement_receipt")
    );
    assert!(
        !submit_payment["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field.as_str() == Some("settlement_receipt"))
    );

    let get_servicenet_receipt = find_tool(tools, "get_servicenet_receipt");
    assert_schema_requires(get_servicenet_receipt, &["receipt_id"]);

    let list_mailbox_messages = find_tool(tools, "list_mailbox_messages");
    assert_schema_requires(list_mailbox_messages, &["subnet_id"]);

    let list_missions = find_tool(tools, "list_missions");
    assert_eq!(
        list_missions["description"].as_str(),
        Some("Browse the bounded Wattetheria network mission market from the configured gateway.")
    );
    assert_eq!(
        list_missions["inputSchema"]["properties"]["limit"]["type"].as_str(),
        Some("integer")
    );
    assert_eq!(
        list_missions["inputSchema"]["properties"]["offset"]["type"].as_str(),
        Some("integer")
    );

    let claim_mission = find_tool(tools, "claim_mission");
    assert_schema_requires(claim_mission, &["mission_id", "agent_did"]);
    assert_eq!(
        claim_mission["inputSchema"]["properties"]["claim_route"]["description"].as_str(),
        Some("Claim route object returned by list_missions.")
    );
    assert_eq!(
        claim_mission["inputSchema"]["properties"]["mission_scope_hint"]["type"].as_str(),
        Some("string")
    );
    let complete_mission = find_tool(tools, "complete_mission");
    assert_schema_requires(complete_mission, &["mission_id", "agent_did"]);
    assert_eq!(
        complete_mission["inputSchema"]["properties"]["result"]["description"].as_str(),
        Some("Mission completion result to submit as the Wattswarm candidate output.")
    );
    assert_eq!(
        complete_mission["inputSchema"]["properties"]["claim_route"]["description"].as_str(),
        Some("Claim route object returned by list_missions for network missions.")
    );
    let settle_mission = find_tool(tools, "settle_mission");
    assert_schema_requires(settle_mission, &["mission_id"]);
    assert_eq!(
        settle_mission["inputSchema"]["properties"]["candidate_id"]["description"].as_str(),
        Some("Wattswarm candidate ID to accept before settling.")
    );
}

#[tokio::test]
async fn mcp_publish_mission_uses_current_local_public_identity() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "publish_mission",
                "arguments": {
                    "title": "MCP local publisher",
                    "description": "Publisher should be injected by the local MCP server.",
                    "publisher": "wrong-manual-value",
                    "publisher_kind": "system",
                    "domain": "trade",
                    "reward": {
                        "agent_watt": 10,
                        "reputation": 0,
                        "capacity": 0,
                        "treasury_share_watt": 0
                    },
                    "payload": {"objective": "identity-default"}
                }
            }
        }),
    )
    .await;

    let mission = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(mission["publisher"].as_str(), Some(local_public_id));
    assert_eq!(mission["publisher_kind"].as_str(), Some("player"));
    let mission_id = mission["mission_id"].as_str().expect("mission id");
    assert_eq!(mission["task_id"].as_str(), Some(mission_id));
    assert_eq!(mission["task_type"].as_str(), Some("wattetheria.mission"));
    assert_eq!(
        mission["mission_scope_hint"].as_str(),
        Some(format!("group:{mission_id}").as_str())
    );
    assert_eq!(
        mission["swarm_scope"],
        json!({"kind": "group", "id": mission_id})
    );
    assert_eq!(
        mission["task_contract"]["task_id"].as_str(),
        Some(mission_id)
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["swarm_scope"],
        json!({"kind": "group", "id": mission_id})
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["mission_scope_hint"].as_str(),
        mission["mission_scope_hint"].as_str()
    );
}

#[tokio::test]
async fn mcp_create_hive_uses_current_local_public_identity() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_hive",
                "arguments": {
                    "public_id": "wrong-manual-value",
                    "feed_key": "mcp-topic-feed",
                    "scope_hint": "group:mcp-topic-feed",
                    "display_name": "MCP Hive",
                    "projection_kind": "chat_room"
                }
            }
        }),
    )
    .await;

    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        content["hive"]["created_by_public_id"].as_str(),
        Some(local_public_id)
    );
}

#[tokio::test]
async fn mcp_create_hive_rejects_invalid_scope_hint_with_actionable_error() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_hive",
                "arguments": {
                    "feed_key": "wattetheria.hives",
                    "scope_hint": "topic:bad-hive",
                    "display_name": "Bad Hive Scope",
                    "projection_kind": "chat_room"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["_meta"]["httpStatus"].as_u64(),
        Some(400)
    );
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["field"].as_str(), Some("scope_hint"));
    assert_eq!(content["received"].as_str(), Some("topic:bad-hive"));
    assert_eq!(
        content["error"].as_str(),
        Some(
            "invalid scope_hint: expected global, region:<id>, node:<id>, local:<id>, or group:<id>; for Hives use group:<id>"
        )
    );
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("group:<id>")
    );
}

#[tokio::test]
async fn mcp_unsubscribe_hive_uses_current_local_public_identity_and_deactivates() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge.clone());

    let create_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_hive",
                "arguments": {
                    "public_id": "wrong-manual-value",
                    "feed_key": "codex_topic_smoke_test",
                    "scope_hint": "group:codex-topic-smoke-test",
                    "display_name": "Codex Hive",
                    "projection_kind": "chat_room"
                }
            }
        }),
    )
    .await;
    let hive_id = create_response["result"]["structuredContent"]["hive"]["topic_id"]
        .as_str()
        .unwrap();

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "unsubscribe_hive",
                "arguments": {
                    "hive_id": hive_id,
                    "active": true
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let subscriptions = bridge.subscriptions.lock().await;
    assert_eq!(subscriptions.len(), 2);
    assert_eq!(subscriptions[1].2, "codex_topic_smoke_test");
    assert_eq!(subscriptions[1].3, "group:codex-topic-smoke-test");
    assert!(!subscriptions[1].4);
}

#[tokio::test]
async fn mcp_list_hives_reads_configured_gateway_hives() {
    let gateway_url = spawn_gateway_hives_server(gateway_hives_fixture()).await;
    let (dir, app, token, _policy, _state) = build_test_app(100);
    std::fs::write(
        dir.path().join("config.json"),
        json!({"gateway_urls": [gateway_url]}).to_string(),
    )
    .unwrap();

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_hives",
                "arguments": {
                    "limit": 1,
                    "offset": 1,
                    "projection_kind": "working_group"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["source"].as_str(),
        Some("wattetheria-gateway.v1_hives")
    );
    assert_eq!(content["scope"].as_str(), Some("network"));
    assert_eq!(
        content["pagination"].as_str(),
        Some("gateway_limit_client_offset")
    );
    assert_eq!(content["limit"].as_u64(), Some(1));
    assert_eq!(content["offset"].as_u64(), Some(1));
    assert_eq!(content["known_count"].as_u64(), Some(1));
    assert_eq!(content["has_more"].as_bool(), Some(false));
    let hives = content["hives"].as_array().unwrap();
    assert_eq!(hives.len(), 0);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_hives",
                "arguments": {
                    "limit": 2,
                    "projection_kind": "working_group"
                }
            }
        }),
    )
    .await;
    assert_gateway_hive_topic(&response);
}

async fn spawn_gateway_hives_server(payload: Value) -> String {
    let gateway_app = axum::Router::new().route(
        "/v1/wattetheria/hives",
        axum::routing::get(move || async move { axum::Json(payload) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, gateway_app).await.unwrap();
    });
    gateway_url
}

fn gateway_hives_fixture() -> Value {
    json!([
        {
            "topic_id": "hive-gateway-1",
            "display_name": "Gateway Hive One",
            "projection_kind": "guild",
            "status": "active",
            "feed_key": "wattetheria.hives",
            "scope_hint": "hive:one",
            "source_node_id": "node-alpha"
        },
        {
            "topic_id": "hive-gateway-2",
            "display_name": "Gateway Hive Two",
            "projection_kind": "working_group",
            "status": "active",
            "feed_key": "wattetheria.hives",
            "scope_hint": "hive:two",
            "source_node_id": "node-beta",
            "organization_id": "org-filter"
        },
        {
            "topic_id": "hive-inactive",
            "display_name": "Inactive Gateway Hive",
            "projection_kind": "guild",
            "status": "inactive",
            "feed_key": "wattetheria.hives",
            "scope_hint": "hive:inactive"
        }
    ])
}

fn assert_gateway_hive_topic(response: &Value) {
    let content = &response["result"]["structuredContent"];
    let hives = content["hives"].as_array().unwrap();
    assert_eq!(hives.len(), 1);
    assert_eq!(hives[0]["topic_id"].as_str(), Some("hive-gateway-2"));
    assert_eq!(hives[0]["hive_id"].as_str(), Some("hive-gateway-2"));
    assert_eq!(hives[0]["source_node_id"].as_str(), Some("node-beta"));
    assert_eq!(
        hives[0]["subscribe_route"]["feed_key"].as_str(),
        Some("wattetheria.hives")
    );
    assert_eq!(
        hives[0]["subscribe_route"]["scope_hint"].as_str(),
        Some("hive:two")
    );
    assert_eq!(
        hives[0]["subscribe_route"]["subscribe_ready"].as_bool(),
        Some(true)
    );
}

#[tokio::test]
async fn mcp_list_missions_reads_configured_gateway_tasks() {
    let gateway_app = axum::Router::new().route(
        "/v1/wattetheria/missions",
        axum::routing::get(|| async {
            axum::Json(json!([
                {
                    "id": "mission-gateway-1",
                    "title": "Gateway Mission One",
                    "status": "published",
                    "source_node_id": "node-alpha",
                    "mission_scope_hint": "group:mission-gateway-1",
                    "task_contract": {
                        "task_id": "mission-gateway-1",
                        "inputs": {
                            "swarm_scope": {"kind": "group", "id": "mission-gateway-1"}
                        }
                    }
                },
                {
                    "task_id": "not-a-mission",
                    "task_type": "topic_consensus",
                    "terminal_state": "open"
                },
                {
                    "id": "mission-gateway-2",
                    "title": "Gateway Mission Two",
                    "status": "published",
                    "source_node_id": "node-beta",
                    "mission_scope_hint": "group:mission-gateway-2",
                    "task_contract": {
                        "task_id": "mission-gateway-2",
                        "inputs": {
                            "swarm_scope": {"kind": "group", "id": "mission-gateway-2"}
                        }
                    }
                },
                {
                    "id": "mission-gateway-settled",
                    "title": "Settled Gateway Mission",
                    "status": "settled",
                    "source_node_id": "node-gamma"
                }
            ]))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, gateway_app).await.unwrap();
    });

    let (dir, app, token, _policy, _state) = build_test_app(100);
    std::fs::write(
        dir.path().join("config.json"),
        json!({"gateway_urls": [gateway_url]}).to_string(),
    )
    .unwrap();

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_missions",
                "arguments": {
                    "limit": 1,
                    "offset": 1,
                    "status": "open"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["source"].as_str(),
        Some("wattetheria-gateway.v1_missions")
    );
    assert_eq!(content["scope"].as_str(), Some("network"));
    assert_eq!(
        content["pagination"].as_str(),
        Some("gateway_limit_client_offset")
    );
    assert_eq!(content["limit"].as_u64(), Some(1));
    assert_eq!(content["offset"].as_u64(), Some(1));
    assert_eq!(content["known_count"].as_u64(), Some(2));
    assert_eq!(content["has_more"].as_bool(), Some(false));
    let missions = content["missions"].as_array().unwrap();
    assert_eq!(missions.len(), 1);
    assert_eq!(
        missions[0]["mission_id"].as_str(),
        Some("mission-gateway-2")
    );
    assert_eq!(missions[0]["task_id"].as_str(), Some("mission-gateway-2"));
    assert_eq!(missions[0]["source_node_id"].as_str(), Some("node-beta"));
    assert_eq!(missions[0]["status"].as_str(), Some("published"));
    assert_gateway_claim_route(&missions[0], "mission-gateway-2", "node-beta");
}

#[tokio::test]
async fn mcp_claim_mission_reports_duplicate_network_claim() {
    let (dir, app, token, _policy, state) = build_test_app(100);
    let mission_id = "mission-mcp-duplicate-claim";
    let agent_did = state.agent_did.clone();
    seed_mcp_gateway_remote_mission(dir.path(), &state, mission_id).await;

    let first = mcp_claim_mission(app.clone(), &token, mission_id, &agent_did).await;
    assert_eq!(first["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        first["result"]["structuredContent"]["status"].as_str(),
        Some("network_claim_submitted")
    );

    let second = mcp_claim_mission(app, &token, mission_id, &agent_did).await;
    assert_eq!(second["result"]["isError"].as_bool(), Some(true));
    let content = &second["result"]["structuredContent"];
    assert_eq!(content["code"].as_str(), Some("mission_already_claimed"));
    assert_eq!(content["claim_status"].as_str(), Some("already_claimed"));
    assert_eq!(content["mission_id"].as_str(), Some(mission_id));
    assert_eq!(content["task_id"].as_str(), Some(mission_id));
    assert_eq!(content["agent_did"].as_str(), Some(agent_did.as_str()));
    assert_eq!(second["result"]["_meta"]["httpStatus"].as_u64(), Some(409));
}

#[tokio::test]
async fn mcp_claim_mission_reports_gateway_claimed_status() {
    let (dir, app, token, _policy, state) = build_test_app(100);
    let mission_id = "mission-mcp-gateway-claimed";
    let agent_did = state.agent_did.clone();
    seed_mcp_gateway_remote_mission_with_status(dir.path(), &state, mission_id, "claimed").await;

    let response = mcp_claim_mission(app, &token, mission_id, &agent_did).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["code"].as_str(), Some("mission_already_claimed"));
    assert_eq!(content["claim_status"].as_str(), Some("already_claimed"));
    assert_eq!(content["mission_id"].as_str(), Some(mission_id));
    assert_eq!(
        response["result"]["_meta"]["httpStatus"].as_u64(),
        Some(409)
    );
}

async fn mcp_claim_mission(app: Router, token: &str, mission_id: &str, agent_did: &str) -> Value {
    mcp_request(
        app,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "claim_mission",
                "arguments": {
                    "mission_id": mission_id,
                    "agent_did": agent_did
                }
            }
        }),
    )
    .await
}

async fn seed_mcp_gateway_remote_mission(
    data_dir: &std::path::Path,
    state: &ControlPlaneState,
    mission_id: &str,
) {
    seed_mcp_gateway_remote_mission_with_status(data_dir, state, mission_id, "published").await;
}

async fn seed_mcp_gateway_remote_mission_with_status(
    data_dir: &std::path::Path,
    state: &ControlPlaneState,
    mission_id: &str,
    status: &str,
) {
    let mut contract = state
        .swarm_bridge
        .sample_task_contract(mission_id)
        .await
        .unwrap();
    contract.task_type = "wattetheria.mission".to_string();
    contract.inputs = json!({
        "kind": "wattetheria_mission",
        "mission_id": mission_id,
        "publisher": "publisher-public",
        "publisher_agent_did": "did:agent:publisher",
        "publisher_wattswarm_node_id": "publisher-node",
        "swarm_scope": {"kind": "group", "id": mission_id},
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": format!("group:{mission_id}"),
        "reward": {"agent_watt": 10},
        "payload": {"work": "deliver"}
    });
    let gateway_task = json!({
        "id": mission_id,
        "task_id": mission_id,
        "task_type": "wattetheria.mission",
        "title": "Remote mission",
        "status": status,
        "source_node_id": "publisher-node",
        "publisher_wattswarm_node_id": "publisher-node",
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": format!("group:{mission_id}"),
        "task_contract": contract,
    });
    let gateway_app = Router::new().route(
        "/v1/wattetheria/missions",
        get(move || {
            let gateway_task = gateway_task.clone();
            async move { Json(json!([gateway_task])) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, gateway_app).await.unwrap();
    });
    std::fs::write(
        data_dir.join("config.json"),
        json!({"gateway_urls": [gateway_url]}).to_string(),
    )
    .unwrap();
}

fn assert_gateway_claim_route(mission: &Value, mission_id: &str, node_id: &str) {
    let scope_hint = format!("group:{mission_id}");
    assert_eq!(
        mission["publisher_wattswarm_node_id"].as_str(),
        Some(node_id)
    );
    assert_eq!(
        mission["mission_feed_key"].as_str(),
        Some("wattetheria.missions")
    );
    assert_eq!(
        mission["mission_scope_hint"].as_str(),
        Some(scope_hint.as_str())
    );
    assert_eq!(
        mission["swarm_scope"],
        json!({"kind": "group", "id": mission_id})
    );
    assert_eq!(mission["claim_route"]["task_id"].as_str(), Some(mission_id));
    assert_eq!(
        mission["claim_route"]["mission_id"].as_str(),
        Some(mission_id)
    );
    assert_eq!(
        mission["claim_route"]["publisher_wattswarm_node_id"].as_str(),
        Some(node_id)
    );
    assert_eq!(
        mission["claim_route"]["mission_scope_hint"].as_str(),
        Some(scope_hint.as_str())
    );
    assert_eq!(
        mission["claim_route"]["task_contract_available"].as_bool(),
        Some(true)
    );
    assert_eq!(mission["claim_route"]["claim_ready"].as_bool(), Some(true));
}

#[tokio::test]
async fn mcp_requires_control_plane_auth() {
    let (_dir, app, _token, _policy, _state) = build_test_app(100);

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

fn find_tool<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some(name))
        .unwrap()
}

fn assert_schema_requires(tool: &Value, expected: &[&str]) {
    let required = tool["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(Value::as_str)
        .collect::<Vec<_>>();
    for field in expected {
        assert!(
            required.contains(&Some(*field)),
            "expected {} schema to require {field}, got {required:?}",
            tool["name"].as_str().unwrap()
        );
    }
}

fn assert_schema_omits(tool: &Value, omitted: &[&str]) {
    let properties = tool["inputSchema"]["properties"].as_object().unwrap();
    for field in omitted {
        assert!(
            !properties.contains_key(*field),
            "expected {} schema to hide local identity field {field}",
            tool["name"].as_str().unwrap()
        );
    }
}
