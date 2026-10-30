use super::*;
use wattetheria_gateway_contract::SignedSpacePresence;

async fn call_mcp_tool(app: Router, token: &str, name: &str, arguments: Value) -> Value {
    mcp_request(
        app,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }),
    )
    .await
}

fn assert_three_leave_success_diagnostics(state: &ControlPlaneState) {
    let diagnostics = crate::diagnostics::list_diagnostics(
        &state.data_dir,
        &crate::diagnostics::DiagnosticFilter {
            component: Some("wattetheria.mcp".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        diagnostics
            .iter()
            .filter(|entry| entry.phase == "tool.call.succeeded"
                && entry.details["tool_name"] == "leave_space")
            .count(),
        3
    );
}

#[tokio::test]
async fn mcp_leave_space_reports_departure_only_once() {
    let (_dir, app, token, _policy, state) = build_test_app(100);

    let first_leave = call_mcp_tool(app.clone(), &token, "leave_space", json!({})).await;
    assert_eq!(first_leave["result"]["isError"], false);
    assert_eq!(
        first_leave["result"]["structuredContent"]["presence"],
        Value::Null
    );

    let entered = call_mcp_tool(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false);
    assert_eq!(
        entered["result"]["structuredContent"]["presence"]["record"]["entered"], true,
        "{entered}"
    );

    let actual_leave = call_mcp_tool(app.clone(), &token, "leave_space", json!({})).await;
    assert_eq!(actual_leave["result"]["isError"], false);
    let left_presence: SignedSpacePresence =
        serde_json::from_value(actual_leave["result"]["structuredContent"]["presence"].clone())
            .unwrap();
    assert!(!left_presence.record.entered);

    let repeated_leave = call_mcp_tool(app, &token, "leave_space", json!({})).await;
    assert_eq!(repeated_leave["result"]["isError"], false);
    assert_eq!(
        repeated_leave["result"]["structuredContent"]["presence"],
        Value::Null
    );
    assert_eq!(state.event_log.verify_chain().unwrap(), (true, None));

    assert_three_leave_success_diagnostics(&state);
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
