use super::*;

const AGENT_PARTICIPATION_MANIFEST_ENDPOINTS: &[&str] = &[
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
    "list_topics",
    "create_topic",
    "list_topic_messages",
    "post_topic_message",
    "subscribe_topic",
    "unsubscribe_topic",
    "list_missions",
    "publish_mission",
    "claim_mission",
    "complete_mission",
    "settle_mission",
    "list_friends",
    "upsert_friend",
    "request_agent_friend",
    "send_message",
    "fetch_messages",
    "ack_message",
    "list_servicenet_agents",
    "get_servicenet_agent",
    "invoke_servicenet_agent",
    "get_servicenet_agent_task",
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
async fn mcp_tools_list_matches_agent_participation_manifest_endpoint_surface() {
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
    let mut expected = AGENT_PARTICIPATION_MANIFEST_ENDPOINTS
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(actual, expected);
}

#[tokio::test]
async fn mcp_tools_list_surfaces_manifest_availability_metadata() {
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
    let create_topic = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("create_topic"))
        .unwrap();
    let list_topics = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("list_topics"))
        .unwrap();
    let servicenet = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("list_servicenet_agents"))
        .unwrap();

    assert_eq!(
        create_topic["_meta"]["wattetheria"]["available"].as_bool(),
        Some(false)
    );
    assert_eq!(
        list_topics["_meta"]["wattetheria"]["available"].as_bool(),
        Some(true)
    );
    assert_eq!(
        servicenet["_meta"]["wattetheria"]["available"].as_bool(),
        Some(false)
    );
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
    assert_schema_requires(
        propose_payment,
        &["counterpart_public_id", "amount", "currency", "rail"],
    );
    assert_schema_omits(propose_payment, &["public_id"]);
    assert_eq!(
        propose_payment["inputSchema"]["properties"]["layer"]["enum"][1].as_str(),
        Some("web3")
    );

    let create_topic = find_tool(tools, "create_topic");
    assert_schema_omits(create_topic, &["public_id"]);
    let post_topic_message = find_tool(tools, "post_topic_message");
    assert_schema_omits(post_topic_message, &["public_id"]);
    let subscribe_topic = find_tool(tools, "subscribe_topic");
    assert_schema_omits(subscribe_topic, &["public_id"]);
    let unsubscribe_topic = find_tool(tools, "unsubscribe_topic");
    assert_schema_requires(unsubscribe_topic, &["feed_key", "scope_hint"]);
    assert_schema_omits(unsubscribe_topic, &["public_id", "active"]);
    let upsert_friend = find_tool(tools, "upsert_friend");
    assert_schema_omits(upsert_friend, &["public_id"]);
    let request_agent_friend = find_tool(tools, "request_agent_friend");
    assert_schema_requires(request_agent_friend, &["remote_node_id"]);
    assert_schema_omits(request_agent_friend, &["public_id", "action"]);

    let settle_payment = find_tool(tools, "settle_agent_payment");
    assert_schema_requires(settle_payment, &["payment_id", "settlement_receipt"]);

    let fetch_messages = find_tool(tools, "fetch_messages");
    assert_schema_requires(fetch_messages, &["subnet_id"]);

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
}

#[tokio::test]
async fn mcp_create_topic_uses_current_local_public_identity() {
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
                "name": "create_topic",
                "arguments": {
                    "public_id": "wrong-manual-value",
                    "feed_key": "mcp-topic-feed",
                    "scope_hint": "local-test",
                    "display_name": "MCP Topic",
                    "projection_kind": "chat_room"
                }
            }
        }),
    )
    .await;

    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        content["topic"]["created_by_public_id"].as_str(),
        Some(local_public_id)
    );
}

#[tokio::test]
async fn mcp_unsubscribe_topic_uses_current_local_public_identity_and_deactivates() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge.clone());

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "unsubscribe_topic",
                "arguments": {
                    "feed_key": "codex_topic_smoke_test",
                    "scope_hint": "global",
                    "active": true
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let subscriptions = bridge.subscriptions.lock().await;
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].2, "codex_topic_smoke_test");
    assert_eq!(subscriptions[0].3, "global");
    assert!(!subscriptions[0].4);
}

#[tokio::test]
async fn mcp_list_topics_reads_configured_gateway_hives() {
    let gateway_url = spawn_gateway_topics_server(gateway_hives_fixture()).await;
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
                "name": "list_topics",
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
        Some("wattetheria-gateway.api_topics")
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
    let topics = content["topics"].as_array().unwrap();
    assert_eq!(topics.len(), 0);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_topics",
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

async fn spawn_gateway_topics_server(payload: Value) -> String {
    let gateway_app = axum::Router::new().route(
        "/api/topics",
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
    let topics = content["topics"].as_array().unwrap();
    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0]["topic_id"].as_str(), Some("hive-gateway-2"));
    assert_eq!(topics[0]["source_node_id"].as_str(), Some("node-beta"));
    assert_eq!(
        topics[0]["subscribe_route"]["feed_key"].as_str(),
        Some("wattetheria.hives")
    );
    assert_eq!(
        topics[0]["subscribe_route"]["scope_hint"].as_str(),
        Some("hive:two")
    );
    assert_eq!(
        topics[0]["subscribe_route"]["subscribe_ready"].as_bool(),
        Some(true)
    );
}

#[tokio::test]
async fn mcp_list_missions_reads_configured_gateway_tasks() {
    let gateway_app = axum::Router::new().route(
        "/api/tasks",
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
        Some("wattetheria-gateway.api_tasks")
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
async fn mcp_list_missions_marks_expired_gateway_tasks_not_claim_ready() {
    let gateway_app = axum::Router::new().route(
        "/api/tasks",
        axum::routing::get(|| async {
            axum::Json(json!([
                {
                    "id": "mission-expired",
                    "title": "Expired Gateway Mission",
                    "status": "published",
                    "source_node_id": "node-expired",
                    "task_contract": {
                        "task_id": "mission-expired",
                        "expiry_ms": 1
                    }
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
                "arguments": {"limit": 10}
            }
        }),
    )
    .await;

    let mission = &response["result"]["structuredContent"]["missions"][0];
    assert_eq!(mission["status"].as_str(), Some("expired"));
    assert_eq!(mission["expired"].as_bool(), Some(true));
    assert_eq!(mission["expiry_ms"].as_u64(), Some(1));
    assert_eq!(mission["claim_route"]["claim_ready"].as_bool(), Some(false));
    assert_eq!(
        mission["claim_route"]["claim_block_reason"].as_str(),
        Some("task_expired")
    );
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
