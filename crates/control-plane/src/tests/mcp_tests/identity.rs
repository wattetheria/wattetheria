use super::*;

#[tokio::test]
async fn mcp_lists_get_agent_identity_tool() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tool = find_tool(
        response["result"]["tools"].as_array().unwrap(),
        "get_agent_identity",
    );

    assert_eq!(tool["_meta"]["wattetheria"]["method"], "GET");
    assert_eq!(
        tool["_meta"]["wattetheria"]["path"],
        "/v1/civilization/public-identity"
    );
    assert_eq!(tool["_meta"]["wattetheria"]["available"], true);
    assert_eq!(tool["_meta"]["wattetheria"]["readOnly"], true);
    assert_eq!(tool["inputSchema"]["properties"], json!({}));
    assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    assert_schema_requires(tool, &[]);
}

#[tokio::test]
async fn mcp_get_agent_identity_reads_current_local_identity_without_mutating_it() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let public_id = context.public_identity.unwrap().public_id;
    authed_patch_json(
        app.clone(),
        &token,
        "/v1/civilization/public-identity",
        json!({"public_id": public_id, "display_name": "Current Local Agent"}),
    )
    .await;
    let identities_before = state.public_identity_registry.lock().await.list();
    let expected = authed_get_json(app.clone(), &token, "/v1/civilization/public-identity").await;
    assert_eq!(expected["public_identity"]["public_id"], public_id);
    assert_eq!(
        expected["public_identity"]["display_name"],
        "Current Local Agent"
    );
    assert_eq!(expected["public_identity"]["agent_did"], state.agent_did);

    for params in [
        json!({"name": "get_agent_identity", "arguments": {}}),
        json!({"name": "get_agent_identity"}),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": params
            }),
        )
        .await;

        assert_eq!(response["result"]["isError"], false);
        assert_eq!(response["result"]["structuredContent"], expected);
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), expected);
        assert!(!response.to_string().contains("private_key"));
        assert!(!response.to_string().contains(&token));
    }

    assert_eq!(
        state.public_identity_registry.lock().await.list(),
        identities_before
    );
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    assert_eq!(
        crate::routes::identity::identity_context_response(&context),
        expected
    );
}

#[tokio::test]
async fn mcp_get_agent_identity_rejects_selectors_without_recording_success() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let other_agent = Identity::new_random();
    let other_public_id = scoped_id("other-agent", &other_agent.agent_did);
    state
        .public_identity_registry
        .lock()
        .await
        .upsert(
            &other_public_id,
            "Other Agent".to_string(),
            Some(other_agent.agent_did.clone()),
            true,
        )
        .unwrap();
    let events_before: Value = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::CONTRIBUTION_EVENT_LOG)
        .unwrap();

    for arguments in [
        json!({"public_id": other_public_id}),
        json!({"agent_did": other_agent.agent_did}),
        json!({"query": {"public_id": other_public_id}}),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "get_agent_identity", "arguments": arguments}
            }),
        )
        .await;

        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["structuredContent"]["error"],
            "get_agent_identity does not accept arguments"
        );
        assert!(
            response["result"]["structuredContent"]
                .get("public_identity")
                .is_none()
        );
    }

    let events_after: Value = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::CONTRIBUTION_EVENT_LOG)
        .unwrap();
    assert_eq!(events_after, events_before);
}

#[tokio::test]
async fn mcp_get_agent_identity_requires_network_permission() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    state
        .local_db
        .delete_network_agent_credential("test-network", &state.agent_did)
        .unwrap();
    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "get_agent_identity", "arguments": {}}
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["error_code"],
        "network_permission_required"
    );
    assert!(
        response["result"]["structuredContent"]
            .get("public_identity")
            .is_none()
    );
}

#[tokio::test]
async fn mcp_get_agent_identity_requires_authentication() {
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
                        "params": {"name": "get_agent_identity", "arguments": {}}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_lists_update_agent_name_tool() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tool = find_tool(
        response["result"]["tools"].as_array().unwrap(),
        "update_agent_name",
    );

    assert_eq!(tool["_meta"]["wattetheria"]["method"], "PATCH");
    assert_eq!(
        tool["_meta"]["wattetheria"]["path"],
        "/v1/civilization/public-identity"
    );
    assert_eq!(tool["_meta"]["wattetheria"]["available"], true);
    assert_eq!(tool["_meta"]["wattetheria"]["readOnly"], false);
    assert_schema_requires(tool, &["display_name"]);
    assert_schema_optional(tool, "public_id");
}

#[tokio::test]
async fn mcp_updates_local_display_name_without_changing_controller_binding() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let public_id = context.public_identity.unwrap().public_id;
    let binding_before = state
        .controller_binding_registry
        .lock()
        .await
        .get(&public_id)
        .unwrap();

    for (arguments, display_name) in [
        (json!({"display_name": "New Local Name"}), "New Local Name"),
        (
            json!({"public_id": "not-the-local-identity", "display_name": "Local Name Two"}),
            "Local Name Two",
        ),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "update_agent_name",
                    "arguments": arguments
                }
            }),
        )
        .await;

        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["public_identity"]["public_id"],
            public_id
        );
        assert_eq!(
            response["result"]["structuredContent"]["public_identity"]["display_name"],
            display_name
        );
        assert_eq!(
            response["result"]["structuredContent"]["controller_binding"],
            serde_json::to_value(&binding_before).unwrap()
        );
    }

    assert_eq!(
        state
            .controller_binding_registry
            .lock()
            .await
            .get(&public_id),
        Some(binding_before)
    );
    assert_eq!(
        state
            .public_identity_registry
            .lock()
            .await
            .get(&public_id)
            .unwrap()
            .display_name,
        "Local Name Two"
    );
}

#[tokio::test]
async fn mcp_rejects_empty_and_invalid_public_display_names() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let identity_before = context.public_identity.unwrap();

    for display_name in [
        String::new(),
        " ".to_string(),
        "x".repeat(41),
        "bad\nname".to_string(),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "update_agent_name",
                    "arguments": {"display_name": display_name}
                }
            }),
        )
        .await;

        assert_eq!(response["result"]["isError"], true);
        assert_eq!(response["result"]["_meta"]["httpStatus"], 400);
    }

    assert_eq!(
        state
            .public_identity_registry
            .lock()
            .await
            .get(&identity_before.public_id),
        Some(identity_before)
    );
}
