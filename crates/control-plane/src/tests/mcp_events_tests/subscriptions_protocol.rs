use super::*;

#[tokio::test]
async fn mcp_events_unsubscribe_mode_is_optional_but_subscribe_mode_is_required() {
    let (_dir, router, token, _state) = subscription_app().await;
    for mode in [
        None,
        Some(json!("poll")),
        Some(Value::Null),
        Some(json!(42)),
    ] {
        let mut params = parameters();
        params["delivery"].as_object_mut().unwrap().remove("mode");
        if let Some(mode) = &mode {
            params["delivery"]["mode"] = mode.clone();
        }
        for method in ["events/subscribe", "events/unsubscribe"] {
            let (_, result) =
                modern_call(router.clone(), Some(&token), method, params.clone(), |_| {}).await;
            if method == "events/unsubscribe" && mode.is_none() {
                assert_eq!(result["result"]["resultType"], "complete");
            } else {
                assert_eq!(result["error"]["code"], -32014, "{method}: {result}");
            }
        }
    }
}

#[tokio::test]
async fn mcp_events_modern_tool_auth_and_argument_errors_are_not_internal_errors() {
    let (_dir, _router, token, mut state) = subscription_app().await;
    state.mcp_token_auth_required = true;
    let router = app(state);
    let header = |headers: &mut HeaderMap| {
        headers.insert("mcp-name", HeaderValue::from_static("get_agent_identity"));
    };
    let (status, result) = modern_call(
        router.clone(),
        Some("invalid-token"),
        "tools/call",
        json!({"name": "get_agent_identity", "arguments": {}}),
        header,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_ne!(result["error"]["code"], -32603);
    let (status, result) = modern_call(
        router,
        Some(&token),
        "tools/call",
        json!({"name": "get_agent_identity", "arguments": []}),
        header,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(result["error"]["code"], -32602);
}

#[tokio::test]
async fn mcp_events_legacy_metadata_keeps_the_legacy_tools_response() {
    let (_dir, router, token, _state) = subscription_app().await;
    let (status, result) = mcp_call(
        router,
        Some(&token),
        "tools/list",
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2025-11-25"}}),
        |_| {},
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(result["result"].get("resultType").is_none());
    assert!(
        result["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["name"] == "get_agent_identity" })
    );
}

#[tokio::test]
async fn mcp_events_modern_discovery_and_catalog_do_not_require_initialize() {
    let (_dir, router, token, _state) = subscription_app().await;
    let (status, discovery) = modern_call(
        router.clone(),
        Some(&token),
        "server/discover",
        json!({}),
        |_| {},
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(discovery["result"]["resultType"], "complete");
    assert_eq!(discovery["result"]["capabilities"]["events"], json!({}));
    assert!(
        discovery["result"]["capabilities"]["tools"]
            .get("listChanged")
            .is_none()
    );
    assert_eq!(discovery["result"]["ttlMs"], 0);
    assert_eq!(discovery["result"]["cacheScope"], "private");
    assert_eq!(
        discovery["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "wattetheria-local-control-plane"
    );
    let (_, catalog) = modern_call(
        router.clone(),
        Some(&token),
        "events/list",
        json!({}),
        |_| {},
    )
    .await;
    let events = catalog["result"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["name"], "wattetheria.agent.event");
    assert_eq!(events[0]["delivery"], json!(["webhook"]));
    assert_eq!(events[0]["inputSchema"]["additionalProperties"], false);
    assert_eq!(
        events[0]["payloadSchema"]["properties"]["type"]["type"],
        "string"
    );
    let (_, legacy) = mcp_call(
        router,
        Some(&token),
        "initialize",
        json!({"protocolVersion": "2025-11-25"}),
        |_| {},
    )
    .await;
    assert_eq!(legacy["result"]["protocolVersion"], "2025-11-25");
    assert!(legacy["result"].get("resultType").is_none());
    assert!(legacy["result"]["capabilities"].get("events").is_none());
}

#[tokio::test]
async fn mcp_events_api_runtime_does_not_advertise_or_serve_events() {
    let (_dir, router, token, _state) = events_app().await;
    let (_, discovery) = modern_call(
        router.clone(),
        Some(&token),
        "server/discover",
        json!({}),
        |_| {},
    )
    .await;
    assert!(discovery["result"]["capabilities"].get("events").is_none());
    let (status, result) =
        modern_call(router, Some(&token), "events/list", json!({}), |_| {}).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(result["error"]["code"], -32601);
}

#[tokio::test]
async fn mcp_events_modern_headers_are_checked_against_the_body() {
    let (_dir, router, token, _state) = subscription_app().await;
    for header in ["mcp-method", "mcp-protocol-version"] {
        let (status, result) = modern_call(
            router.clone(),
            Some(&token),
            "events/list",
            json!({}),
            |headers| {
                headers.remove(header);
            },
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(result["error"]["code"], -32020);
    }
    let (status, result) = modern_call(router, Some(&token), "events/list", json!({}), |headers| {
        headers.insert("mcp-method", HeaderValue::from_static("tools/call"));
    })
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(result["error"]["code"], -32020);
}

#[tokio::test]
async fn mcp_events_modern_tool_requests_keep_existing_tools() {
    let (_dir, router, token, _state) = subscription_app().await;
    let (_, catalog) = modern_call(
        router.clone(),
        Some(&token),
        "tools/list",
        json!({}),
        |_| {},
    )
    .await;
    let names: Vec<_> = catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(names.contains(&"accept_friend_request"));
    assert_eq!(catalog["result"]["ttlMs"], 0);
    let params = json!({"name": "get_agent_identity", "arguments": {}});
    let (_, missing) = modern_call(
        router.clone(),
        Some(&token),
        "tools/call",
        params.clone(),
        |_| {},
    )
    .await;
    assert_eq!(missing["error"]["code"], -32020);
    let (_, result) = modern_call(router, Some(&token), "tools/call", params, |headers| {
        headers.insert("mcp-name", HeaderValue::from_static("get_agent_identity"));
    })
    .await;
    assert_eq!(result["result"]["resultType"], "complete");
    assert!(result["result"]["content"].is_array());
}

#[tokio::test]
async fn mcp_events_subscriptions_require_owner_token_even_when_tools_allow_anonymous() {
    let (_dir, router, token, _state) = subscription_app().await;
    for method in ["events/subscribe", "events/unsubscribe"] {
        for credential in [None, Some("not-the-owner-token")] {
            let (_, result) =
                modern_call(router.clone(), credential, method, parameters(), |_| {}).await;
            assert_eq!(result["error"]["code"], -32012);
        }
    }
    let (_, result) = modern_call(
        router,
        Some(&token),
        "events/unsubscribe",
        parameters(),
        |_| {},
    )
    .await;
    assert_eq!(result["result"]["resultType"], "complete");
}

#[tokio::test]
async fn mcp_events_subscribe_rejects_invalid_inputs_without_activating_a_subscription() {
    let (_dir, router, token, state) = subscription_app().await;
    for url in [
        "http://callback.example/events",
        "https://127.0.0.1/hook",
        "https://10.1.2.3/hook",
        "https://[::1]/hook",
        "https://localhost/hook",
        "https://user:password@callback.example/hook",
    ] {
        let mut params = parameters();
        params["delivery"]["url"] = json!(url);
        let (_, result) = modern_call(
            router.clone(),
            Some(&token),
            "events/subscribe",
            params,
            |_| {},
        )
        .await;
        assert_eq!(result["error"]["code"], -32602, "{url}");
    }
    for invalid in [
        json!({"delivery": {"secret": "no-prefix"}}),
        json!({"delivery": {"secret": "whsec_AAAA"}}),
        json!({"arguments": {"type": "friend_request"}}),
        json!({"ttlMs": -1}),
        json!({"cursor": "old-history"}),
    ] {
        let mut params = parameters();
        for (key, value) in invalid.as_object().unwrap() {
            if key == "delivery" {
                params["delivery"]["secret"] = value["secret"].clone();
            } else {
                params[key] = value.clone();
            }
        }
        let (_, result) = modern_call(
            router.clone(),
            Some(&token),
            "events/subscribe",
            params,
            |_| {},
        )
        .await;
        assert_eq!(result["error"]["code"], -32602);
    }
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["activeSubscriptions"],
        0
    );
}

#[tokio::test]
async fn mcp_events_modern_metadata_version_and_method_errors_follow_protocol() {
    let (_dir, router, token, _state) = subscription_app().await;
    let headers = |headers: &mut HeaderMap| {
        headers.insert(
            "mcp-protocol-version",
            HeaderValue::from_static("2026-07-28"),
        );
        headers.insert("mcp-method", HeaderValue::from_static("server/discover"));
    };
    for params in [
        json!({}),
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}),
    ] {
        let (status, result) = mcp_call(
            router.clone(),
            Some(&token),
            "server/discover",
            params,
            headers,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(result["error"]["code"], -32602);
    }
    let (status, unsupported) = mcp_call(router.clone(), Some(&token), "server/discover", json!({"_meta": {
        "io.modelcontextprotocol/protocolVersion": "2099-01-01", "io.modelcontextprotocol/clientCapabilities": {}}}), |headers| {
        headers.insert("mcp-protocol-version", HeaderValue::from_static("2099-01-01"));
        headers.insert("mcp-method", HeaderValue::from_static("server/discover"));
    }).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(unsupported["error"]["code"], -32022);
    assert_eq!(
        unsupported["error"]["data"]["supported"],
        json!(["2026-07-28"])
    );
    let (status, unknown) =
        modern_call(router, Some(&token), "events/unknown", json!({}), |_| {}).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(unknown["error"]["code"], -32601);
}

#[tokio::test]
async fn mcp_events_modern_tools_accept_encoded_names_and_reject_unknown_tools() {
    use base64::Engine;
    let (_dir, router, token, _state) = subscription_app().await;
    let encoded = base64::engine::general_purpose::STANDARD.encode("get_agent_identity");
    let (_, result) = modern_call(
        router.clone(),
        Some(&token),
        "tools/call",
        json!({"name": "get_agent_identity", "arguments": {}}),
        |headers| {
            headers.insert(
                "mcp-name",
                HeaderValue::from_str(&format!("=?base64?{encoded}?=")).unwrap(),
            );
        },
    )
    .await;
    assert_eq!(result["result"]["resultType"], "complete");
    let (status, invalid) = modern_call(
        router,
        Some(&token),
        "tools/call",
        json!({"name": "not_a_tool", "arguments": {}}),
        |headers| {
            headers.insert("mcp-name", HeaderValue::from_static("not_a_tool"));
        },
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid["error"]["code"], -32602);
}
