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
    "list_missions",
    "publish_mission",
    "claim_mission",
    "complete_mission",
    "settle_mission",
    "list_friends",
    "upsert_friend",
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
    let upsert_friend = find_tool(tools, "upsert_friend");
    assert_schema_omits(upsert_friend, &["public_id"]);

    let settle_payment = find_tool(tools, "settle_agent_payment");
    assert_schema_requires(settle_payment, &["payment_id", "settlement_receipt"]);

    let fetch_messages = find_tool(tools, "fetch_messages");
    assert_schema_requires(fetch_messages, &["subnet_id"]);
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
async fn mcp_tool_call_dispatches_to_matching_local_control_plane_endpoint() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_missions",
                "arguments": {}
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert!(response["result"]["structuredContent"].is_array());
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
