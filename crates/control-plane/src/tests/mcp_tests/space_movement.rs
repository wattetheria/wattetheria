use super::*;

#[tokio::test]
async fn move_agent_mcp_schema_and_execution_support_an_exact_tile() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let listed = mcp_request(
        app.clone(),
        &token,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await;
    let tool = find_tool(listed["result"]["tools"].as_array().unwrap(), "move_agent");
    assert_schema_optional(tool, "tile");
    let tile = &tool["inputSchema"]["properties"]["tile"];
    assert_eq!(tile["type"], "array");
    assert_eq!(tile["minItems"], 2);
    assert_eq!(tile["maxItems"], 2);
    assert_eq!(tile["items"]["type"], "integer");
    assert_eq!(tile["items"]["minimum"], 0);

    let entered = mcp_request(
        app.clone(),
        &token,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"enter_space","arguments":{}}}),
    )
    .await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");
    let package = test_space_package();
    let network = test_network_id(&state);
    let current = state
        .own_space_presence
        .lock()
        .await
        .visible_get(&network, &package)
        .unwrap()
        .record
        .from_tile;
    let tile = (0..package.size[1])
        .flat_map(|y| (0..package.size[0]).map(move |x| [x, y]))
        .find(|tile| *tile != current && package.path(current, *tile).is_ok())
        .unwrap();
    let moved = mcp_request(
        app,
        &token,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"move_agent","arguments":{"tile":tile}}}),
    )
    .await;
    assert_eq!(moved["result"]["isError"], false, "{moved}");
    let record: wattetheria_gateway_contract::SpacePresenceRecord =
        serde_json::from_value(moved["result"]["structuredContent"]["presence"]["record"].clone())
            .unwrap();
    assert_eq!(record.tile_at(package.walk_speed, i64::MAX), tile);
    assert_eq!(
        record.destination_place_id,
        package.place_at_tile(tile).unwrap().place_id
    );
}
