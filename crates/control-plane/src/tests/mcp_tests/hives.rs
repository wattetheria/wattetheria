use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_invite_private_hive_participant_sends_key_share_without_exposing_secret() {
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
    let remote_public_id = scoped_id("broker-private-hive", &remote_identity.agent_did);
    let hive_id = "mainnet:watt-etheria@private.hive@group:dm-private-hive-test";
    {
        let mut hives = state.hive_registry.lock().await;
        hives.upsert_hive(wattetheria_kernel::civilization::topics::TopicCreateSpec {
            network_id: Some("mainnet:watt-etheria".to_owned()),
            feed_key: "private.hive".to_owned(),
            scope_hint: "group:dm-private-hive-test".to_owned(),
            display_name: "Private Hive".to_owned(),
            summary: None,
            projection_kind:
                wattetheria_kernel::civilization::topics::TopicProjectionKind::ChatRoom,
            organization_id: None,
            mission_id: None,
            participant_public_ids: Vec::new(),
            created_by_public_id: local_public_id.clone(),
            why_this_exists: None,
            public_geo: None,
            active: true,
        });
    }
    wattetheria_social::application::transport_binding_service::upsert_transport_binding(
        &*state.social_store,
        &wattetheria_social::domain::transport_bindings::RemoteTransportBinding {
            public_id: remote_public_id.clone(),
            agent_did: Some(remote_identity.agent_did.clone()),
            transport_kind:
                wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm,
            transport_node_id: "12D3KooPrivateHivePeer".to_string(),
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
            display_name: Some("Private Hive Peer".to_string()),
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
                "name": "invite_private_hive_participant",
                "arguments": {
                    "hive_id": hive_id,
                    "counterpart_public_id": remote_public_id,
                    "display_name": "Private Hive Peer",
                    "hive_name": "Private Hive"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let result_text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool result text");
    let result_json: Value = serde_json::from_str(result_text).expect("tool result parses");
    assert_eq!(
        result_json["remote_node_id"].as_str(),
        Some("12D3KooPrivateHivePeer")
    );
    assert_eq!(result_json["feed_key"].as_str(), Some("private.hive"));
    assert_eq!(
        result_json["scope_hint"].as_str(),
        Some("group:dm-private-hive-test")
    );
    assert_eq!(
        result_json["display_name"].as_str(),
        Some("Private Hive Peer")
    );
    assert_eq!(result_json["hive_name"].as_str(), Some("Private Hive"));
    assert_eq!(
        result_json["shared_secret_b64_redacted"].as_bool(),
        Some(true)
    );
    let key_share_commands = bridge.private_hive_key_share_commands.lock().await;
    assert_eq!(key_share_commands.len(), 1);
    assert_eq!(
        key_share_commands[0].display_name.as_str(),
        "Private Hive Peer"
    );
    assert_eq!(key_share_commands[0].hive_name.as_str(), "Private Hive");
    assert_eq!(
        key_share_commands[0].invite_text.as_str(),
        "Hi Private Hive Peer, you are invited to join the private Hive \"Private Hive\". This encrypted message includes the private Hive key share so your node can unlock the Hive messages."
    );
}

#[tokio::test]
async fn mcp_create_hive_uses_current_local_public_identity() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();

    let response = mcp_request(
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
                    "feed_key": "mcp-topic-feed",
                    "scope_hint": "group:mcp-topic-feed",
                    "display_name": "MCP Hive",
                    "projection_kind": "chat_room",
                    "include_public_geo": false
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
    assert_public_geo_projection(&content["hive"]);
    let topic_id = content["hive"]["topic_id"].as_str().unwrap();
    let export_json = public_get_json(
        app,
        &format!(
            "/v1/wattetheria/client/export?public_id={local_public_id}&peer_limit=1&task_limit=1&organization_limit=1&rpc_log_limit=1&leaderboard_limit=1"
        ),
    )
    .await;
    let public_topic = export_json["payload"]["public_topics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|topic| topic["topic_id"].as_str() == Some(topic_id))
        .unwrap();
    assert_public_geo_projection(public_topic);
}

#[tokio::test]
async fn mcp_create_private_hive_defaults_to_unique_group_dm_chat_room_scope() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let first = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_private_hive",
                "arguments": {
                    "feed_key": "wattetheria.private.hives",
                    "display_name": "Private Hive",
                    "participant_public_ids": ["friend-public-1"]
                }
            }
        }),
    )
    .await;
    let second = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "create_private_hive",
                "arguments": {
                    "feed_key": "wattetheria.private.hives",
                    "display_name": "Second Private Hive"
                }
            }
        }),
    )
    .await;

    assert_eq!(first["result"]["isError"].as_bool(), Some(false));
    assert_eq!(second["result"]["isError"].as_bool(), Some(false));
    let first_hive = &first["result"]["structuredContent"]["hive"];
    let second_hive = &second["result"]["structuredContent"]["hive"];
    let first_scope = first_hive["scope_hint"].as_str().unwrap();
    let second_scope = second_hive["scope_hint"].as_str().unwrap();
    assert!(first_scope.starts_with("group:dm-"));
    assert!(second_scope.starts_with("group:dm-"));
    assert_ne!(first_scope, second_scope);
    assert_eq!(first_hive["projection_kind"].as_str(), Some("chat_room"));
    assert_eq!(
        first_hive["participant_public_ids"][0].as_str(),
        Some("friend-public-1")
    );
    assert_public_geo_omitted(first_hive);
    assert_public_geo_omitted(second_hive);
}

#[tokio::test]
async fn mcp_create_private_hive_rejects_non_private_scope_hint() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_private_hive",
                "arguments": {
                    "feed_key": "wattetheria.private.hives",
                    "scope_hint": "group:public-room",
                    "display_name": "Not Private"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("create_private_hive scope_hint must use group:dm-<id>")
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
async fn mcp_post_hive_message_requires_local_subscription() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge.clone());

    let blocked = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "post_hive_message",
                "arguments": {
                    "hive_id": "mainnet:test@crew.chat@group:crew-7",
                    "network_id": "mainnet:test",
                    "feed_key": "crew.chat",
                    "scope_hint": "group:crew-7",
                    "content": {"text": "blocked"}
                }
            }
        }),
    )
    .await;

    assert_eq!(blocked["result"]["isError"].as_bool(), Some(true));
    assert_eq!(blocked["result"]["_meta"]["httpStatus"].as_u64(), Some(403));
    assert_eq!(
        blocked["result"]["structuredContent"]["error"].as_str(),
        Some("hive subscription required")
    );
    assert!(bridge.messages.lock().await.is_empty());

    let create_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "create_hive",
                "arguments": {
                    "feed_key": "crew.chat",
                    "scope_hint": "group:crew-7",
                    "display_name": "Crew Seven",
                    "projection_kind": "chat_room",
                    "network_id": "mainnet:test"
                }
            }
        }),
    )
    .await;
    assert_eq!(create_response["result"]["isError"].as_bool(), Some(false));

    let posted = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "post_hive_message",
                "arguments": {
                    "hive_id": "mainnet:test@crew.chat@group:crew-7",
                    "content": {"text": "中文 English العربية 日本語 🙂 allowed"}
                }
            }
        }),
    )
    .await;

    assert_eq!(posted["result"]["isError"].as_bool(), Some(false));
    let oversized = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "post_hive_message",
                "arguments": {
                    "hive_id": "mainnet:test@crew.chat@group:crew-7",
                    "content": {"text": "x".repeat(4097)}
                }
            }
        }),
    )
    .await;
    assert_eq!(oversized["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        oversized["result"]["_meta"]["httpStatus"].as_u64(),
        Some(400)
    );
    let messages = bridge.messages.lock().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].content["text"].as_str(),
        Some("中文 English العربية 日本語 🙂 allowed")
    );
}

#[tokio::test]
async fn mcp_unsubscribe_hive_uses_current_local_public_identity_and_removes_local_subscription() {
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
        app.clone(),
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

    let hives = authed_get_json(app, &token, "/v1/wattetheria/hives?include_inactive=true").await;
    assert!(
        hives["hives"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["topic_id"].as_str() != Some(hive_id))
    );
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
        Some("wattetheria-gateway.api_hives")
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

#[tokio::test]
async fn mcp_list_private_hives_reads_local_private_hives_only() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    {
        let mut hives = state.hive_registry.lock().await;
        hives.upsert_hive(test_hive_spec(
            "private.hive",
            "group:dm-private-active",
            "Private Active",
            true,
        ));
        hives.upsert_hive(test_hive_spec(
            "wattetheria.hives",
            "group:public-topic",
            "Public Hive",
            true,
        ));
        hives.upsert_hive(test_hive_spec(
            "private.hive",
            "group:dm-private-inactive",
            "Private Inactive",
            false,
        ));
    }

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_private_hives",
                "arguments": {
                    "network_id": "mainnet:watt-etheria"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["source"].as_str(),
        Some("wattetheria.local_hive_registry")
    );
    assert_eq!(content["scope"].as_str(), Some("local_private"));
    assert_eq!(content["known_count"].as_u64(), Some(1));
    let hives = content["hives"].as_array().unwrap();
    assert_eq!(hives.len(), 1);
    assert_eq!(hives[0]["display_name"].as_str(), Some("Private Active"));
    assert_eq!(hives[0]["feed_key"].as_str(), Some("private.hive"));
    assert_eq!(
        hives[0]["scope_hint"].as_str(),
        Some("group:dm-private-active")
    );
    assert!(
        !serde_json::to_string(content)
            .unwrap()
            .contains("shared_secret_b64")
    );

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_private_hives",
                "arguments": {
                    "include_inactive": true
                }
            }
        }),
    )
    .await;
    assert_eq!(
        response["result"]["structuredContent"]["known_count"].as_u64(),
        Some(2)
    );
}

fn test_hive_spec(
    feed_key: &str,
    scope_hint: &str,
    display_name: &str,
    active: bool,
) -> wattetheria_kernel::civilization::topics::TopicCreateSpec {
    wattetheria_kernel::civilization::topics::TopicCreateSpec {
        network_id: Some("mainnet:watt-etheria".to_owned()),
        feed_key: feed_key.to_owned(),
        scope_hint: scope_hint.to_owned(),
        display_name: display_name.to_owned(),
        summary: None,
        projection_kind: wattetheria_kernel::civilization::topics::TopicProjectionKind::ChatRoom,
        organization_id: None,
        mission_id: None,
        participant_public_ids: Vec::new(),
        created_by_public_id: "local-public".to_owned(),
        why_this_exists: None,
        public_geo: None,
        active,
    }
}

#[tokio::test]
async fn mcp_subscribe_hive_uses_gateway_subscribe_route_when_hive_is_not_local() {
    let gateway_url = spawn_gateway_hives_server(gateway_hives_fixture()).await;
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge.clone());
    std::fs::write(
        dir.path().join("config.json"),
        json!({"gateway_urls": [gateway_url]}).to_string(),
    )
    .unwrap();

    let list_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
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
    let hive = &list_response["result"]["structuredContent"]["hives"][0];
    let route = &hive["subscribe_route"];

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "subscribe_hive",
                "arguments": {
                    "hive_id": hive["hive_id"],
                    "network_id": route["network_id"],
                    "feed_key": route["feed_key"],
                    "scope_hint": route["scope_hint"],
                    "display_name": hive["display_name"],
                    "summary": hive["summary"],
                    "projection_kind": hive["projection_kind"],
                    "organization_id": hive["organization_id"]
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let subscriptions = bridge.subscriptions.lock().await;
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].2, "wattetheria.hives");
    assert_eq!(subscriptions[0].3, "group:hive-two");
    assert!(subscriptions[0].4);

    let client_hives = authed_get_json(app, &token, "/v1/client/hives?limit=10").await;
    let hives = client_hives.as_array().unwrap();
    let subscribed = hives
        .iter()
        .find(|item| {
            item["feed_key"].as_str() == Some("wattetheria.hives")
                && item["scope_hint"].as_str() == Some("group:hive-two")
        })
        .expect("subscribed gateway Hive is persisted locally");
    assert_eq!(
        subscribed["display_name"].as_str(),
        Some("Gateway Hive Two")
    );
    assert_eq!(
        subscribed["summary"].as_str(),
        Some("Gateway Hive Two summary")
    );
    assert_eq!(
        subscribed["projection_kind"].as_str(),
        Some("working_group")
    );
}

#[tokio::test]
async fn mcp_list_hives_reads_startup_resolved_gateway_urls() {
    // Native deployments hand the gateway to the kernel only through
    // `--gateway-config-path` / `--gateway-url`, with no `config.json` entry and no
    // WATTETHERIA_GATEWAY_* env vars; queries must still find the gateway.
    let gateway_url = spawn_gateway_hives_server(gateway_hives_fixture()).await;
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> =
        Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (_dir, state, token, _policy) =
        build_test_state_with_bridge(100, dir, identity, event_log, bridge);
    let state = ControlPlaneState {
        gateway_urls: vec![gateway_url],
        ..state
    };
    assert!(!state.data_dir.join("config.json").exists());

    let response = mcp_request(
        app(state),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "list_hives", "arguments": {}}
        }),
    )
    .await;

    assert_eq!(
        response["result"]["isError"].as_bool(),
        Some(false),
        "{response}"
    );
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["source"].as_str(),
        Some("wattetheria-gateway.api_hives")
    );
    assert_eq!(content["known_count"].as_u64(), Some(2));
}

async fn spawn_gateway_hives_server(payload: Value) -> String {
    let gateway_app = axum::Router::new().route(
        "/api/hives",
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
            "scope_hint": "group:hive-one",
            "source_node_id": "node-alpha"
        },
        {
            "topic_id": "hive-gateway-2",
            "display_name": "Gateway Hive Two",
            "summary": "Gateway Hive Two summary",
            "projection_kind": "working_group",
            "status": "active",
            "feed_key": "wattetheria.hives",
            "scope_hint": "group:hive-two",
            "source_node_id": "node-beta",
            "organization_id": "org-filter"
        },
        {
            "topic_id": "hive-inactive",
            "display_name": "Inactive Gateway Hive",
            "projection_kind": "guild",
            "status": "inactive",
            "feed_key": "wattetheria.hives",
            "scope_hint": "group:hive-inactive"
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
        Some("group:hive-two")
    );
    assert_eq!(
        hives[0]["subscribe_route"]["subscribe_ready"].as_bool(),
        Some(true)
    );
}

fn assert_public_geo_omitted(value: &Value) {
    assert!(value.get("lat").is_none());
    assert!(value.get("lng").is_none());
    assert!(value.get("coordinate_source").is_none());
}
