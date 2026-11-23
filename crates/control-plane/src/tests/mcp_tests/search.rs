use super::*;

/// One MCP request to an address that asks for `tools` (`None`: the plain `/mcp`).
async fn mcp_at(app: Router, token: &str, tools: Option<&str>, body: Value) -> (StatusCode, Value) {
    let uri = tools.map_or_else(|| "/mcp".to_owned(), |tools| format!("/mcp?tools={tools}"));
    let (status, text) = request_text(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    (status, serde_json::from_str(&text).unwrap())
}

async fn list(app: Router, token: &str, tools: Option<&str>) -> Vec<Value> {
    let (status, response) = mcp_at(
        app,
        token,
        tools,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    response["result"]["tools"].as_array().unwrap().clone()
}

async fn call(
    app: Router,
    token: &str,
    tools: Option<&str>,
    name: &str,
    arguments: Value,
) -> Value {
    let (status, response) = mcp_at(
        app,
        token,
        tools,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": name, "arguments": arguments}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    response["result"].clone()
}

fn names(tools: &[Value]) -> Vec<&str> {
    tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn the_address_picks_the_whole_list_or_the_short_one() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let full = list(app.clone(), &token, None).await;
    assert_eq!(list(app.clone(), &token, Some("full")).await, full);
    assert!(names(&full).contains(&"hunt_fight"));
    assert!(!names(&full).contains(&"find_tools"));
    assert!(!names(&full).contains(&"use_tool"));

    let short = list(app.clone(), &token, Some("search")).await;
    assert_eq!(
        names(&short),
        [
            "enter_space",
            "leave_space",
            "list_places",
            "get_place",
            "move_agent",
            "list_games",
            "game_info",
            "list_hive_messages",
            "post_hive_message",
            "get_agent_identity",
            "get_watt_balance",
            "find_tools",
            "use_tool",
        ]
    );
    // A listed tool is the very tool of the whole list.
    for tool in short.iter().take(short.len() - 2) {
        assert_eq!(tool, find_tool(&full, tool["name"].as_str().unwrap()));
    }
    // The point of it: the short list is a small part of the whole one.
    let size = |tools: &[Value]| serde_json::to_string(tools).unwrap().len();
    assert!(
        size(&short) * 6 < size(&full),
        "short {} of {}",
        size(&short),
        size(&full)
    );

    let (status, refused) = mcp_at(
        app,
        &token,
        Some("some"),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("full or search")
    );
}

#[tokio::test]
async fn find_tools_gives_the_groups_then_the_definitions() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let full = list(app.clone(), &token, None).await;
    let search = Some("search");

    // No arguments: every group with its tool names, and every tool of the whole list is there.
    let index = call(app.clone(), &token, search, "find_tools", json!({})).await;
    assert_eq!(index["isError"], false, "{index}");
    let groups = index["structuredContent"]["groups"].as_array().unwrap();
    let mut indexed: Vec<&str> = groups
        .iter()
        .flat_map(|group| group["tools"].as_array().unwrap())
        .map(|name| name.as_str().unwrap())
        .collect();
    let mut all = names(&full);
    indexed.sort_unstable();
    all.sort_unstable();
    assert_eq!(indexed, all);
    let hunt = groups
        .iter()
        .find(|group| group["group"] == "hunt")
        .unwrap();
    assert!(
        hunt["tools"]
            .as_array()
            .unwrap()
            .contains(&json!("smithy_forge"))
    );

    // A group: its tools as the whole list defines them.
    let farm = call(
        app.clone(),
        &token,
        search,
        "find_tools",
        json!({"group": "farm"}),
    )
    .await;
    let farm = farm["structuredContent"]["tools"].as_array().unwrap();
    assert_eq!(farm.len(), 8, "{farm:?}");
    for tool in farm {
        let listed = find_tool(&full, tool["name"].as_str().unwrap());
        assert_eq!(tool["description"], listed["description"]);
        assert_eq!(tool["inputSchema"], listed["inputSchema"]);
        assert_eq!(tool["group"], "farm");
    }

    // A query: the best match first, no more than asked for.
    let found = call(
        app.clone(),
        &token,
        search,
        "find_tools",
        json!({"query": "steal a crop", "limit": 3}),
    )
    .await;
    let tools = found["structuredContent"]["tools"].as_array().unwrap();
    assert_eq!(tools[0]["name"], "farm_steal", "{found}");
    assert!(tools.len() <= 3);
    let forge = call(
        app.clone(),
        &token,
        search,
        "find_tools",
        json!({"query": "forge"}),
    )
    .await;
    assert_eq!(
        forge["structuredContent"]["tools"][0]["name"],
        "smithy_forge"
    );
    let none = call(
        app.clone(),
        &token,
        search,
        "find_tools",
        json!({"query": "zzzz"}),
    )
    .await;
    assert_eq!(none["isError"], false);
    assert_eq!(none["structuredContent"]["tools"], json!([]));
    assert!(none["structuredContent"]["groups"].is_array());

    for bad in [
        json!({"group": "cooking"}),
        json!({"query": "farm", "limit": 0}),
        json!({"query": 7}),
    ] {
        let refused = call(app.clone(), &token, search, "find_tools", bad.clone()).await;
        assert_eq!(refused["isError"], true, "{bad}");
    }
}

#[tokio::test]
async fn use_tool_runs_the_named_tool_as_a_direct_call_would() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let search = Some("search");
    let direct = call(app.clone(), &token, None, "get_agent_identity", json!({})).await;
    assert_eq!(direct["isError"], false, "{direct}");
    for arguments in [
        json!({"name": "get_agent_identity", "arguments": {}}),
        json!({"name": "get_agent_identity"}),
    ] {
        let through = call(app.clone(), &token, search, "use_tool", arguments).await;
        assert_eq!(through, direct);
    }
    // A tool left out of the short list can still be called by its own name.
    let unlisted = call(app.clone(), &token, search, "list_friends", json!({})).await;
    assert_eq!(
        unlisted,
        call(app.clone(), &token, None, "list_friends", json!({})).await
    );

    for bad in [
        json!({}),
        json!({"name": "no_such_tool"}),
        json!({"name": "find_tools"}),
        json!({"name": "use_tool"}),
        json!({"name": "client_export"}),
        json!({"name": "get_agent_identity", "arguments": []}),
    ] {
        let refused = call(app.clone(), &token, search, "use_tool", bad.clone()).await;
        assert_eq!(refused["isError"], true, "{bad}");
    }

    // Without search mode the two tools do not exist.
    for name in ["find_tools", "use_tool"] {
        let unknown = call(app.clone(), &token, None, name, json!({})).await;
        assert_eq!(unknown["isError"], true, "{name}");
        assert!(unknown.to_string().contains("unknown tool"), "{unknown}");
    }
}
