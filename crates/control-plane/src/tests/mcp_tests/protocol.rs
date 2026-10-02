use super::*;

const MCP_AGENT_TOOL_NAMES: &[&str] = &[
    "list_agent_payments",
    "get_agent_payment",
    "propose_agent_payment",
    "authorize_agent_payment",
    "submit_agent_payment",
    "settle_agent_payment",
    "reject_agent_payment",
    "cancel_agent_payment",
    "list_hives",
    "list_private_hives",
    "create_hive",
    "create_private_hive",
    "list_hive_messages",
    "post_hive_message",
    "subscribe_hive",
    "unsubscribe_hive",
    "invite_private_hive_participant",
    "list_board_channels",
    "list_board_messages",
    "publish_board_message",
    "subscribe_board_channel",
    "unsubscribe_board_channel",
    "list_missions",
    "publish_mission",
    "publish_delegated_mission",
    "publish_collective_mission",
    "start_collective_mission",
    "get_collective_mission_result",
    "claim_mission",
    "complete_mission",
    "settle_mission",
    "list_friends",
    "list_nearby",
    "search_agents",
    "get_agent_card",
    "list_friend_requests",
    "list_sent_friend_requests",
    "get_friend_request",
    "accept_friend_request",
    "reject_friend_request",
    "request_agent_friend",
    "remove_agent_friend",
    "list_agent_dm_threads",
    "list_agent_dm_messages",
    "send_agent_dm_message",
    "get_agent_identity",
    "update_agent_name",
    "list_servicenet_agents",
    "get_servicenet_agent",
    "send_service_agent_message",
    "list_published_service_agents",
    "list_service_agent_board_messages",
    "publish_service_agent_board_message",
    "get_service_agent_task",
    "list_service_agent_tasks",
    "cancel_service_agent_task",
    "subscribe_service_agent_task",
    "get_servicenet_receipt",
];

#[tokio::test]
async fn mcp_initialize_negotiates_supported_client_protocol_version() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "hermes",
                    "version": "0.16.0"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        response["result"]["serverInfo"]["name"],
        "wattetheria-local-control-plane"
    );
}

#[tokio::test]
async fn mcp_initialize_defaults_to_latest_protocol_version() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize"
        }),
    )
    .await;

    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
}

#[tokio::test]
async fn mcp_rejects_unsupported_protocol_version_header() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let (status, body) = request_text(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("mcp-protocol-version", "2099-01-01")
            .body(axum::body::Body::from(
                json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["error"]["code"], -32602);
}

#[tokio::test]
async fn mcp_tools_list_matches_expected_agent_tool_surface() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app.clone(),
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
    assert!(!actual.iter().any(|name| name == "client_export"));
    assert!(!actual.iter().any(|name| name == "client_task_activity"));
    assert!(!actual.iter().any(|name| name == "delete_servicenet_agent"));
}

#[tokio::test]
async fn every_mcp_tool_reports_clear_network_permission_block() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    state
        .local_db
        .delete_network_agent_credential("test-network", &state.agent_did)
        .unwrap();
    state
        .local_db
        .upsert_network_permission_checkpoint(
            &wattetheria_kernel::local_db::NetworkPermissionCheckpoint {
                network_id: "test-network".to_owned(),
                node_id: "test-node".to_owned(),
                agent_did: state.agent_did.clone(),
                permission_status: "pending".to_owned(),
                network_status: "stopped".to_owned(),
                revision: 2,
                last_error: Some("awaiting approval".to_owned()),
                updated_at_ms: 2,
            },
        )
        .unwrap();

    let read_only_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "list_nearby", "arguments": {}}
        }),
    )
    .await;
    let mutating_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "create_hive", "arguments": {}}
        }),
    )
    .await;

    for response in [&read_only_response, &mutating_response] {
        assert_eq!(response["result"]["isError"].as_bool(), Some(true));
        assert_eq!(
            response["result"]["structuredContent"]["error_code"],
            "network_permission_required"
        );
        let message = response["result"]["structuredContent"]["message"]
            .as_str()
            .unwrap();
        assert!(message.contains("Network permission is not active"));
        assert!(message.contains("blocked"));
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("submit_registration_or_wait_for_approval")
        );
    }
}

#[tokio::test]
async fn mcp_tools_list_surfaces_tool_availability_metadata() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        agent_topic_bridge_enabled: false,
        ..state
    };
    let app = app(state.clone());

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
    let list_board_channels = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("list_board_channels"))
        .unwrap();
    let list_board_messages = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("list_board_messages"))
        .unwrap();
    let list_service_agent_board_messages = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("list_service_agent_board_messages"))
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
        list_board_channels["_meta"]["wattetheria"]["available"].as_bool(),
        Some(true)
    );
    assert_eq!(
        list_board_messages["_meta"]["wattetheria"]["available"].as_bool(),
        Some(true)
    );
    assert_eq!(
        list_service_agent_board_messages["_meta"]["wattetheria"]["available"].as_bool(),
        Some(true)
    );
    assert_eq!(
        servicenet["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/servicenet/agents")
    );
    assert_eq!(
        list_hives["_meta"]["wattetheria"]["readOnly"].as_bool(),
        Some(true)
    );
    assert_eq!(
        create_hive["_meta"]["wattetheria"]["readOnly"].as_bool(),
        Some(false)
    );
}

#[tokio::test]
async fn mcp_allows_tools_list_without_control_plane_auth_by_default() {
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

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn mcp_allows_tools_call_without_control_plane_auth_by_default() {
    let (_dir, app, _token, _policy, _state) = build_test_app(100);

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {"name": "unknown_tool", "arguments": {}}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn mcp_requires_control_plane_auth_when_configured() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    state.mcp_token_auth_required = true;
    let app = app(state);

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

#[tokio::test]
async fn mcp_tools_call_requires_control_plane_auth_when_configured() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    state.mcp_token_auth_required = true;
    let app = app(state);

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {"name": "unknown_tool", "arguments": {}}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
