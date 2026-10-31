use super::*;

#[tokio::test]
async fn mcp_lists_read_only_watt_wallet_tools_with_precise_schemas() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();

    let balance = find_tool(tools, "get_watt_balance");
    assert_eq!(balance["_meta"]["wattetheria"]["method"], "GET");
    assert_eq!(
        balance["_meta"]["wattetheria"]["path"],
        "/v1/wattetheria/wallet"
    );
    assert_eq!(balance["_meta"]["wattetheria"]["readOnly"], true);
    assert_eq!(balance["inputSchema"]["properties"], json!({}));
    assert_eq!(balance["inputSchema"]["additionalProperties"], false);

    let entries = find_tool(tools, "list_watt_ledger_entries");
    assert_eq!(entries["_meta"]["wattetheria"]["method"], "GET");
    assert_eq!(
        entries["_meta"]["wattetheria"]["path"],
        "/v1/wattetheria/wallet/entries"
    );
    assert_eq!(entries["_meta"]["wattetheria"]["readOnly"], true);
    let properties = entries["inputSchema"]["properties"].as_object().unwrap();
    assert_eq!(
        properties.keys().collect::<Vec<_>>(),
        ["before_seq", "limit"],
        "only the two paging arguments are accepted"
    );
    assert_eq!(properties["limit"]["type"], "integer");
    assert_eq!(properties["before_seq"]["type"], "integer");
    assert_eq!(entries["inputSchema"]["additionalProperties"], false);
    assert_schema_requires(entries, &[]);
}

#[tokio::test]
async fn mcp_watt_wallet_tools_report_a_missing_gateway_ledger_instead_of_inventing_watt() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    for (name, arguments) in [
        ("get_watt_balance", json!({})),
        ("list_watt_ledger_entries", json!({"limit": 5})),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments}
            }),
        )
        .await;
        assert_eq!(response["result"]["isError"], true, "{name}: {response}");
        assert_eq!(
            response["result"]["_meta"]["httpStatus"], 503,
            "{name}: {response}"
        );
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        assert_eq!(payload["code"], "ledger_unavailable", "{name}: {payload}");
    }
}
