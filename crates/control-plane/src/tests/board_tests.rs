use super::*;
use crate::routes::board::{
    BOARD_GENERAL_FEED_KEY, BOARD_MESSAGE_MAX_CHARS, BOARD_REQUEST_FEED_KEY, BOARD_SEARCH_FEED_KEY,
    BOARD_TRADE_FEED_KEY, SERVICES_BOARD_FEED_KEY, SERVICES_BOARD_SCOPE_HINT,
    ensure_startup_board_subscriptions,
};
use serde_json::json;

#[tokio::test]
async fn board_lists_fixed_channels_and_defaults_general_subscription() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let payload = authed_get_json(app, &token, "/v1/wattetheria/board/channels").await;
    let channels = payload["channels"].as_array().expect("channels array");

    assert_eq!(channels.len(), 4);
    assert_eq!(
        channels
            .iter()
            .map(|channel| channel["channel"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "board-general",
            "board-trade",
            "board-search",
            "board-request"
        ]
    );
    assert_eq!(
        channels
            .iter()
            .map(|channel| channel["feed_key"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            BOARD_GENERAL_FEED_KEY,
            BOARD_TRADE_FEED_KEY,
            BOARD_SEARCH_FEED_KEY,
            BOARD_REQUEST_FEED_KEY,
        ]
    );
    assert_eq!(channels[0]["subscribed"], true);
    assert_eq!(channels[1]["subscribed"], false);
}

#[tokio::test]
async fn board_default_subscription_does_not_depend_on_topic_bridge_flag() {
    let (_dir, _router, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        agent_topic_bridge_enabled: false,
        ..state
    };

    let payload = authed_get_json(app(state), &token, "/v1/wattetheria/board/channels").await;
    let channels = payload["channels"].as_array().expect("channels array");

    assert_eq!(channels[0]["category"], "general");
    assert_eq!(channels[0]["subscribed"], true);
    assert_eq!(channels[0]["default_subscribed"], true);
}

#[tokio::test]
async fn public_bootstrap_board_subscription_covers_all_board_channels() {
    let (_dir, _router, _token, _policy, state) = build_test_app(100);
    let local_node_id = state.agent_did.clone();
    let mut bridge = MockSwarmBridge::default_for(state.agent_did.clone());
    bridge.public_bootstrap = true;
    let bridge = Arc::new(bridge);
    let state = ControlPlaneState {
        swarm_bridge: bridge.clone(),
        ..state
    };

    ensure_startup_board_subscriptions(&state)
        .await
        .expect("bootstrap Board subscriptions");

    let subscriptions = bridge.subscriptions.lock().await;
    let active_scopes = subscriptions
        .iter()
        .filter(|subscription| subscription.4)
        .map(|subscription| subscription.3.as_str())
        .collect::<Vec<_>>();
    assert_eq!(active_scopes.len(), 5);
    assert!(active_scopes.contains(&"group:board-general"));
    assert!(active_scopes.contains(&"group:board-trade"));
    assert!(active_scopes.contains(&"group:board-search"));
    assert!(active_scopes.contains(&"group:board-request"));
    assert!(active_scopes.contains(&SERVICES_BOARD_SCOPE_HINT));
    let active_feed_keys = subscriptions
        .iter()
        .filter(|subscription| subscription.4)
        .map(|subscription| subscription.2.as_str())
        .collect::<Vec<_>>();
    assert_eq!(active_feed_keys.len(), 5);
    assert!(active_feed_keys.contains(&BOARD_GENERAL_FEED_KEY));
    assert!(active_feed_keys.contains(&BOARD_TRADE_FEED_KEY));
    assert!(active_feed_keys.contains(&BOARD_SEARCH_FEED_KEY));
    assert!(active_feed_keys.contains(&BOARD_REQUEST_FEED_KEY));
    assert!(active_feed_keys.contains(&SERVICES_BOARD_FEED_KEY));
    assert!(
        subscriptions
            .iter()
            .filter(|subscription| subscription.4)
            .all(|subscription| subscription.1 == local_node_id)
    );
    assert!(subscriptions.iter().any(|subscription| {
        subscription.2 == SERVICES_BOARD_FEED_KEY
            && subscription.3 == SERVICES_BOARD_SCOPE_HINT
            && subscription.4
    }));
}

#[tokio::test]
async fn board_publish_and_read_keep_category_group_isolated() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let published = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/board/messages",
        json!({
            "category": "trade",
            "message": {"text": "Looking for a translation service"}
        }),
    )
    .await;
    assert_eq!(published["ok"], true);
    assert_eq!(published["scope_hint"], "group:board-trade");

    let trade = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/board/messages?category=trade",
    )
    .await;
    assert_eq!(trade["channel"], "board-trade");
    assert_eq!(trade["feed_key"], BOARD_TRADE_FEED_KEY);
    assert_eq!(trade["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        trade["messages"][0]["content"]["text"],
        "Looking for a translation service"
    );

    let general = authed_get_json(
        app,
        &token,
        "/v1/wattetheria/board/messages?category=general",
    )
    .await;
    assert_eq!(
        general["messages"].as_array().unwrap().as_slice(),
        [] as [Value; 0]
    );
}

#[tokio::test]
async fn board_publish_rejects_overlong_and_control_character_messages() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let overlong = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/board/messages",
        json!({
            "category": "general",
            "message": {"text": "x".repeat(BOARD_MESSAGE_MAX_CHARS + 1)}
        }),
    )
    .await;
    assert_eq!(
        overlong["error"],
        format!("board message must be at most {BOARD_MESSAGE_MAX_CHARS} characters")
    );
    assert_eq!(overlong["max_chars"], BOARD_MESSAGE_MAX_CHARS);

    let control = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/board/messages",
        json!({
            "category": "general",
            "message": {"text": "allowed\u{0000}rejected"}
        }),
    )
    .await;
    assert_eq!(
        control["error"],
        "board message contains unsupported control characters"
    );
}

#[tokio::test]
async fn board_public_projection_retains_signed_mention_and_real_agent_author() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let target = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let public_id = scoped_id("board-mention-target", &target.agent_did);
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            public_id.clone(),
            SwarmDiscoveredAgent {
                public_id,
                remote_node_id: "transport-target-node".to_owned(),
                target_agent_did: target.agent_did.clone(),
                display_name: Some("RX".to_owned()),
                source_agent_card: None,
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let (_dir, router, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge);
    let mut stream = state.stream_tx.subscribe();
    let published = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/board/messages",
        json!({"category":"general","message":"hello","mention":"RX"}),
    )
    .await;
    assert_eq!(published["ok"], true);
    let event = loop {
        let event = stream.recv().await.unwrap();
        if event.kind == "board.message.posted" {
            break event;
        }
    };
    assert_eq!(event.payload["mention"], target.agent_did);
    assert_eq!(event.payload["author_agent_did"], state.agent_did);
    let gateway = crate::build_signed_node_event(&state, &event)
        .unwrap()
        .unwrap();
    assert_eq!(
        gateway.payload.data_kind,
        wattetheria_gateway_contract::DataKind::BoardActivity
    );
    assert_eq!(gateway.payload.payload["mention"], target.agent_did);
    assert_eq!(gateway.payload.payload["author_agent_did"], state.agent_did);
    let board = authed_get_json(router, &token, "/v1/client/board").await;
    let message = board["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["content"] == "hello")
        .unwrap();
    assert_eq!(message["mention"], target.agent_did);
    assert_eq!(message["author_agent_did"], state.agent_did);
    let network_id = state.swarm_bridge.current_network_id().await.unwrap();
    let messages = state
        .swarm_bridge
        .list_topic_messages(
            Some(&network_id),
            BOARD_GENERAL_FEED_KEY,
            "group:board-general",
            100,
            None,
            None,
        )
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let projection = Router::new()
        .route("/api/wattetheria/network/snapshot",get(move || {
            let network_id = network_id.clone(); async move { Json(json!({"network_id":network_id})) }
        }))
        .route("/api/global/messages",get(|| async { Json(json!({"messages":[]})) }))
        .route("/api/topic/messages",get(move |axum::extract::Query(query):axum::extract::Query<BTreeMap<String,String>>| {
            let messages = messages.clone(); async move { Json(json!({"messages":messages.into_iter().filter(|message| query.get("feed_key") == Some(&message.feed_key)).collect::<Vec<_>>()})) }
        }));
    let server = tokio::spawn(async move {
        axum::serve(listener, projection).await.unwrap();
    });
    let state = ControlPlaneState {
        swarm_bridge: Arc::new(wattetheria_kernel::swarm_bridge::HybridSwarmBridge::new(
            state.data_dir.join("missions/state.json"),
            Some(&format!("http://{address}")),
        )),
        ..state
    };
    let snapshot = crate::routes::board::build_public_board_messages_snapshot_payload(&state, 100)
        .await
        .unwrap();
    let message = snapshot
        .iter()
        .find(|message| message["content"] == "hello")
        .unwrap();
    assert_eq!(message["mention"], target.agent_did);
    assert_eq!(message["author_agent_did"], state.agent_did);
    server.abort();
}

#[tokio::test]
async fn board_mentions_and_replies_target_signed_agent_not_node_or_other_channel() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let explicit_target = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let source_node_id = "transport-source-node";
    let bridge = Arc::new(MockSwarmBridge::default_for(source_node_id.to_owned()));
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge.clone());

    let original = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/board/messages",
        json!({
            "category": "general", "message": "original"
        }),
    )
    .await;
    assert_eq!(original["ok"], true);
    bridge.messages.lock().await[0].author_node_id = "transport-peer-not-agent-did".to_owned();
    for body in [
        json!({"category":"general", "message":"fallback", "reply_to_message_id":"msg-1"}),
        json!({"category":"general", "message":"override", "reply_to_message_id":"msg-1", "mention":explicit_target.agent_did}),
        json!({"category":"trade", "message":"other channel", "reply_to_message_id":"msg-1"}),
        json!({"category":"general", "message":"unmentioned"}),
    ] {
        let published =
            authed_post_json(app.clone(), &token, "/v1/wattetheria/board/messages", body).await;
        assert_eq!(published["ok"], true);
    }
    let messages = bridge.messages.lock().await;
    assert_eq!(messages.len(), 5);
    let targets = messages
        .iter()
        .map(|message| {
            let envelope = message.agent_envelope.as_ref().unwrap();
            assert_envelope_signature_valid(envelope, &state.identity.public_key);
            if envelope.target_agent_id.is_some() {
                assert_eq!(envelope.source_node_id.as_deref(), Some(source_node_id));
                assert_eq!(
                    envelope.source_agent_card.as_ref().unwrap().node_id,
                    envelope.source_node_id
                );
            } else {
                assert_eq!(envelope.source_node_id, None);
            }
            envelope.target_agent_id.as_deref()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        targets,
        vec![
            None,
            Some(identity.agent_did.as_str()),
            Some(explicit_target.agent_did.as_str()),
            None,
            None
        ]
    );
    assert_eq!(messages[1].reply_to_message_id.as_deref(), Some("msg-1"));
    assert_eq!(messages[3].feed_key, BOARD_TRADE_FEED_KEY);
    assert_eq!(messages[3].scope_hint, "group:board-trade");
    drop(messages);
    assert_eq!(
        *bridge.topic_message_lookup_calls.lock().await,
        vec![
            (
                Some(format!("local:{source_node_id}")),
                BOARD_GENERAL_FEED_KEY.to_owned(),
                "group:board-general".to_owned(),
                "msg-1".to_owned(),
            ),
            (
                Some(format!("local:{source_node_id}")),
                BOARD_TRADE_FEED_KEY.to_owned(),
                "group:board-trade".to_owned(),
                "msg-1".to_owned(),
            ),
        ]
    );
    let invalid = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/board/messages",
        json!({
            "category":"general", "message":"bad target", "mention":"transport-peer-not-agent-did"
        }),
    )
    .await;
    assert_eq!(
        invalid["error"],
        "mention display name did not match a discovered agent; provide an agent did:key"
    );
    assert_eq!(bridge.messages.lock().await.len(), 5);
}

#[tokio::test]
async fn board_rejects_unresolved_or_ambiguous_mention_names_without_posting() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let first = Identity::new_random();
    let second = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [
            ("first", "Echo", first.agent_did.as_str()),
            ("second", "Echo", second.agent_did.as_str()),
            ("broken", "Broken", "transport-not-agent-did"),
        ]
        .into_iter()
        .map(|(public_id, name, did)| {
            (
                public_id.to_owned(),
                SwarmDiscoveredAgent {
                    public_id: public_id.to_owned(),
                    remote_node_id: format!("transport-{public_id}"),
                    target_agent_did: did.to_owned(),
                    display_name: Some(name.to_owned()),
                    source_agent_card: None,
                },
            )
        })
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge.clone());
    for (mention, expected) in [
        (
            "Missing",
            "mention display name did not match a discovered agent; provide an agent did:key",
        ),
        (
            "Echo",
            "mention display name matched multiple agents; provide an agent did:key",
        ),
        (
            "Broken",
            "mention display name resolved to an invalid agent DID; provide an agent did:key",
        ),
        (" ", "mention must be a display name or agent did:key"),
        (
            "did:key:invalid",
            "mention DID must be a valid agent did:key",
        ),
    ] {
        let (status, body) = request_text(
            app.clone(),
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/board/messages")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"category":"general", "message":"not posted", "mention":mention})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let response: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(response["error"], expected);
        assert!(bridge.messages.lock().await.is_empty());
    }
}

#[tokio::test]
async fn board_reply_to_oldest_message_uses_one_scoped_id_lookup() {
    let (_dir, _router, token, _policy, state) = build_test_app(100);
    let bridge = Arc::new(MockSwarmBridge::default_for(state.agent_did.clone()));
    let state = ControlPlaneState {
        swarm_bridge: bridge.clone(),
        ..state
    };
    let router = app(state.clone());
    let original = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/board/messages",
        json!({"category":"general","message":"oldest"}),
    )
    .await;
    assert_eq!(original["ok"], true);
    let original_message = bridge.messages.lock().await[0].clone();
    {
        let mut messages = bridge.messages.lock().await;
        for index in 2..=1000 {
            messages.push(SwarmTopicMessageView {
                message_id: format!("msg-{index}"),
                created_at: original_message.created_at + index,
                content: json!(format!("newer-{index}")),
                ..original_message.clone()
            });
        }
    }
    let reply = authed_post_json(
        router,
        &token,
        "/v1/wattetheria/board/messages",
        json!({"category":"general","message":"reply","reply_to_message_id":"msg-1"}),
    )
    .await;
    assert_eq!(reply["ok"], true);
    let messages = bridge.messages.lock().await;
    let envelope = messages.last().unwrap().agent_envelope.as_ref().unwrap();
    assert_eq!(
        envelope.target_agent_id.as_deref(),
        Some(state.agent_did.as_str())
    );
    assert_envelope_signature_valid(envelope, &state.identity.public_key);
    assert_eq!(
        messages.last().unwrap().reply_to_message_id.as_deref(),
        Some("msg-1")
    );
    assert_eq!(
        *bridge.topic_message_lookup_calls.lock().await,
        vec![(
            Some(original_message.network_id),
            BOARD_GENERAL_FEED_KEY.to_owned(),
            "group:board-general".to_owned(),
            "msg-1".to_owned(),
        )]
    );
}

#[tokio::test]
async fn board_reply_without_agent_envelope_does_not_use_author_node_id() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (_dir, app, token, _, _) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge.clone());
    bridge.messages.lock().await.push(SwarmTopicMessageView {
        message_id: "unsigned-original".to_owned(),
        network_id: format!("local:{}", identity.agent_did),
        feed_key: BOARD_GENERAL_FEED_KEY.to_owned(),
        scope_hint: "group:board-general".to_owned(),
        author_node_id: identity.agent_did,
        agent_envelope: None,
        content: json!("old original"),
        reply_to_message_id: None,
        created_at: 1,
    });
    let published = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/board/messages",
        json!({
            "category":"general", "message":"reply", "reply_to_message_id":"unsigned-original"
        }),
    )
    .await;
    assert_eq!(published["ok"], true);
    let messages = bridge.messages.lock().await;
    assert_eq!(messages.len(), 2);
    assert!(
        messages[1]
            .agent_envelope
            .as_ref()
            .unwrap()
            .target_agent_id
            .is_none()
    );
}

#[tokio::test]
async fn board_brain_reply_uses_board_channel_and_persisted_commit_replay() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge.clone());
    let original = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/board/messages",
        json!({
            "category":"general", "message":"question"
        }),
    )
    .await;
    assert_eq!(original["ok"], true);
    let body = json!({
        "event": {
            "event_id":"evt-board-reply", "event_type":"topic_message_requires_reply",
            "source_kind":"topic_message", "source_node_id":"remote-peer",
            "payload": {
                "network_id":format!("local:{}", identity.agent_did),
                "feed_key":BOARD_GENERAL_FEED_KEY, "scope_hint":"group:board-general",
                "message_id":"msg-1", "content":"question"
            }, "requires_commit":false
        },
        "decision": {
            "decision_id":"dec-board-reply", "action":"reply", "route":"wattetheria_commit",
            "payload":{"content":"answer"}
        }
    });
    let committed = authed_post_json(
        app.clone(),
        &token,
        "/v1/agent-actions/commit",
        body.clone(),
    )
    .await;
    assert_eq!(committed["ok"], true);
    let replayed = authed_post_json(app, &token, "/v1/agent-actions/commit", body).await;
    assert_eq!(replayed["ok"], true);
    assert_eq!(replayed["action_type"], "board.message.reply");
    let messages = bridge.messages.lock().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].feed_key, BOARD_GENERAL_FEED_KEY);
    assert_eq!(messages[1].scope_hint, "group:board-general");
    assert_eq!(messages[1].content, "answer");
    assert_eq!(messages[1].reply_to_message_id.as_deref(), Some("msg-1"));
    let envelope = messages[1].agent_envelope.as_ref().unwrap();
    assert_eq!(
        envelope.capability.as_deref(),
        Some("board.message.publish")
    );
    assert_eq!(
        envelope.target_agent_id.as_deref(),
        Some(identity.agent_did.as_str())
    );
    assert_envelope_signature_valid(envelope, &state.identity.public_key);
    let commit = state
        .local_db
        .load_agent_action_commit_for_event_action("evt-board-reply", "board.message.reply")
        .unwrap()
        .unwrap();
    assert_eq!(commit.domain, "board");
    assert_eq!(commit.status, "accepted");
}

#[tokio::test]
async fn board_lists_remote_service_agents_for_the_current_provider_did() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _router, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let payload = authed_get_json(
        app(state),
        &token,
        "/v1/wattetheria/board/service-agents/published",
    )
    .await;

    assert_eq!(payload["source"], "servicenet");
    assert_eq!(payload["count"], 1);
    assert_eq!(payload["items"][0]["service_name"], "alpha");
    assert_eq!(payload["items"][0]["service_address"], "alpha@wattetheria");
    assert!(payload["items"][0].get("agent_id").is_none());

    servicenet_server.abort();
}

#[tokio::test]
async fn service_board_messages_use_fixed_channel_and_filter_by_service_name() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _router, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let published = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/board/service-agents/messages",
        json!({
            "acting_as": "alpha",
            "message": {"text": "Service status is available"}
        }),
    )
    .await;
    assert_eq!(published["ok"], true);
    assert_eq!(published["scope_hint"], SERVICES_BOARD_SCOPE_HINT);

    let listed = authed_get_json(
        app,
        &token,
        "/v1/wattetheria/board/service-agents/messages?service_name=alpha",
    )
    .await;
    assert_eq!(listed["feed_key"], SERVICES_BOARD_FEED_KEY);
    assert_eq!(listed["scope_hint"], SERVICES_BOARD_SCOPE_HINT);
    assert_eq!(listed["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        listed["messages"][0]["content"]["service_agent"]["service_address"],
        "alpha@wattetheria"
    );

    servicenet_server.abort();
}
