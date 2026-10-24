use super::*;
use crate::mcp_events::AgentEventMode;
use crate::routes::agent_events::AgentEventEnvelope;

fn hive_event(
    source: &Identity,
    target: Option<&str>,
    scope_hint: &str,
    content: &Value,
) -> AgentEventEnvelope {
    let envelope = signed_agent_event_envelope(
        source,
        "remote-node",
        target,
        "hive.message.post",
        json!({
            "feed_key": "hive.chat", "scope_hint": scope_hint,
            "network_id": "local:test", "action": "message.post",
            "payload": {"content": content, "mention": target},
        }),
    );
    AgentEventEnvelope {
        event_id: "evt-public-hive".to_owned(),
        event_type: "topic_message_requires_reply".to_owned(),
        source_kind: "topic_message".to_owned(),
        source_node_id: Some("remote-node".to_owned()),
        target_agent_id: None,
        target_executor: None,
        agent_envelope: Some(envelope),
        payload: json!({
            "network_id": "local:test", "feed_key": "hive.chat", "scope_hint": scope_hint,
            "content": content, "topic_content": content, "message_id": "remote-message",
        }),
        requires_commit: false,
        allowed_actions: vec!["reply".to_owned(), "ignore".to_owned()],
        correlation_id: None,
        dedupe_key: None,
        created_at: 1,
    }
}

async fn callback(state: &ControlPlaneState, event: &AgentEventEnvelope) -> Value {
    request_json(
        app(state.clone()),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(json!({"event": event}).to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn public_hive_unmentioned_and_other_target_messages_do_not_enter_brain_or_mcp() {
    let (dir, _app, _, _, mut state) = build_test_app(100);
    let remote = Identity::new_random();
    for mode in [AgentEventMode::ApiRuntime, AgentEventMode::McpEvents] {
        state.agent_event_mode = mode;
        // An unavailable event service must not affect receive-only messages.
        state.mcp_events = None;
        for target in [None, Some(remote.agent_did.as_str())] {
            let mut event = hive_event(&remote, target, "group:public-chat", &json!("hello"));
            // Unsigned outer targeting cannot turn a broadcast into a local mention.
            if target.is_none() {
                event.target_agent_id = Some(state.agent_did.clone());
            }
            let response = callback(&state, &event).await;
            assert_eq!(response["ok"], true, "{response}");
            assert!(response["decision"].is_null());
            assert_eq!(
                response["detail"],
                "public Hive message received without local mention"
            );
        }
    }
    let entries = crate::diagnostics::list_diagnostics(
        dir.path(),
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-public-hive".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        !entries
            .iter()
            .any(|entry| entry.phase == "callback.received")
    );
}

#[tokio::test]
async fn public_hive_signed_mention_enters_brain_with_reply_and_ignore() {
    let (dir, _app, _, _, state) = build_test_app(100);
    let remote = Identity::new_random();
    let event = hive_event(
        &remote,
        Some(&state.agent_did),
        "group:public-chat",
        &json!("hello"),
    );
    let response = callback(&state, &event).await;
    assert_eq!(response["ok"], true, "{response}");
    let received = callback_received_diagnostic(dir.path(), &event.event_id);
    assert_eq!(received["brain_input"]["target_agent_id"], state.agent_did);
    assert_eq!(
        received["brain_input"]["allowed_actions"],
        json!(["reply", "ignore"])
    );
    let mut tampered = event;
    tampered.agent_envelope.as_mut().unwrap().target_agent_id = None;
    let response = callback(&state, &tampered).await;
    assert_eq!(
        response["error"],
        "agent_envelope signature verification failed"
    );
}

#[tokio::test]
async fn public_hive_filter_preserves_private_hive_and_mission_decisions() {
    let (dir, _app, _, _, state) = build_test_app(100);
    let remote = Identity::new_random();
    for (index, scope_hint, content, expected_actions) in [
        (
            0,
            "group:dm-private-hive",
            json!("private message"),
            json!(["reply", "ignore"]),
        ),
        (
            1,
            "group:public-chat",
            json!({"type":"collective_mission", "phase":"joining", "mission_id":"mission-1"}),
            json!(["join_collective_mission", "ignore"]),
        ),
    ] {
        let mut event = hive_event(&remote, None, scope_hint, &content);
        event.event_id = format!("evt-hive-preserved-{index}");
        let response = callback(&state, &event).await;
        assert_eq!(response["ok"], true, "{response}");
        let received = callback_received_diagnostic(dir.path(), &event.event_id);
        assert_eq!(received["brain_input"]["allowed_actions"], expected_actions);
    }
}

#[tokio::test]
async fn public_hive_mention_is_forwarded_to_mcp_events_but_broadcast_is_not() {
    let (dir, _app, token, _, mut state) = build_test_app(100);
    state.agent_event_mode = AgentEventMode::McpEvents;
    let database = dir.path().join("events.sqlite");
    let key = dir.path().join("events.key");
    let store = crate::mcp_events::store::EventStore::open(&database, &key).unwrap();
    store
        .upsert_subscription(crate::mcp_events::store::NewSubscription {
            id: "public-hive-subscription".to_owned(),
            source_kind: "agent_callback".to_owned(),
            event_type: "topic_message_requires_reply".to_owned(),
            target_agent_id: state.agent_did.clone(),
            credential_fingerprint: hex::encode(sha2::Sha256::digest(token.as_bytes())),
            owner_id: state.agent_did.clone(),
            callback_url: "https://callback.example/events".to_owned(),
            callback_secret: "test-secret".to_owned(),
            old_callback_secret: None,
            old_secret_expires_at: None,
            canonical_arguments: b"{}".to_vec(),
            expires_at: None,
        })
        .unwrap();
    drop(store);
    let events = Arc::new(
        crate::mcp_events::McpEvents::open(database, key, state.agent_did.clone(), &token)
            .await
            .unwrap(),
    );
    state.mcp_events = Some(events.clone());
    let remote = Identity::new_random();
    let broadcast = hive_event(&remote, None, "group:public-chat", &json!("broadcast"));
    assert_eq!(callback(&state, &broadcast).await["ok"], true);
    assert_eq!(events.diagnostics().await.unwrap()["pendingOutbox"], 0);
    let mut mentioned = hive_event(
        &remote,
        Some(&state.agent_did),
        "group:public-chat",
        &json!("directed"),
    );
    mentioned.event_id = "evt-mentioned-hive".to_owned();
    assert_eq!(callback(&state, &mentioned).await["ok"], true);
    assert_eq!(events.diagnostics().await.unwrap()["pendingOutbox"], 1);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn public_hive_post_mentions_use_name_or_did_and_keep_history_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let target = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            "hive-target".to_owned(),
            SwarmDiscoveredAgent {
                public_id: "hive-target".to_owned(),
                remote_node_id: "target-node".to_owned(),
                target_agent_did: target.agent_did.clone(),
                display_name: Some("RX".to_owned()),
                source_agent_card: None,
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge);
    let created = authed_post_json(app.clone(), &token, "/v1/wattetheria/hives", json!({
        "feed_key":"hive.chat", "scope_hint":"group:public-chat", "display_name":"Public Chat", "projection_kind":"chat_room"
    })).await;
    let hive_id = created["hive"]["topic_id"].as_str().unwrap();
    let path = format!("/v1/wattetheria/hives/{hive_id}/messages");
    let mut stream = state.stream_tx.subscribe();
    for mention in ["RX", "@RX", target.agent_did.as_str()] {
        let response = authed_post_json(app.clone(), &token, "/mcp", json!({
            "jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{
                "name":"post_hive_message", "arguments":{"hive_id":hive_id, "content":"hello", "mention":mention}
            }
        })).await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        let event = loop {
            let event = stream.try_recv().unwrap();
            if event.kind == "topic.message.posted" {
                break event;
            }
        };
        assert_eq!(event.payload["mention"], target.agent_did);
        assert_eq!(event.payload["author_agent_did"], state.agent_did);
        let expected_name = if mention.starts_with("did:") {
            Value::Null
        } else {
            json!("RX")
        };
        assert_eq!(event.payload["mention_display_name"], expected_name);
    }
    let messages = authed_get_json(app.clone(), &token, &path).await;
    assert_eq!(messages["messages"].as_array().unwrap().len(), 3);
    for message in messages["messages"].as_array().unwrap() {
        assert_eq!(message["mention"], target.agent_did);
        assert_eq!(message["author_agent_did"], state.agent_did);
    }
    assert_eq!(messages["messages"][0]["mention_display_name"], "RX");
    assert_eq!(messages["messages"][1]["mention_display_name"], "RX");
    assert!(
        messages["messages"][2]
            .get("mention_display_name")
            .is_none()
    );
    let signed_messages = state
        .swarm_bridge
        .list_topic_messages(
            Some(&test_network_id(&state)),
            "hive.chat",
            "group:public-chat",
            50,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(signed_messages.len(), 3);
    for message in signed_messages {
        let envelope = message.agent_envelope.as_ref().unwrap();
        assert_envelope_signature_valid(envelope, &state.identity.public_key);
        assert_eq!(
            envelope.target_agent_id.as_deref(),
            Some(target.agent_did.as_str())
        );
        assert_eq!(envelope.message["payload"]["mention"], target.agent_did);
    }
    let client_messages = authed_get_json(
        app.clone(),
        &token,
        "/v1/client/hives/messages?feed_key=hive.chat&scope_hint=group:public-chat",
    )
    .await;
    assert_eq!(client_messages["messages"].as_array().unwrap().len(), 3);
    for message in client_messages["messages"].as_array().unwrap() {
        assert_eq!(message["mention"], target.agent_did);
        assert_eq!(message["author_agent_did"], state.agent_did);
    }
    let rejected = authed_post_json(
        app.clone(),
        &token,
        &path,
        json!({"content":"not posted", "mention":"unknown"}),
    )
    .await;
    assert_eq!(
        rejected["error"],
        "mention display name did not match a discovered agent; provide an agent did:key"
    );
    for mention in ["@", " @@ ", " "] {
        let rejected = authed_post_json(
            app.clone(),
            &token,
            &path,
            json!({"content":"not posted", "mention":mention}),
        )
        .await;
        assert_eq!(
            rejected["error"],
            "mention must be a display name or agent did:key"
        );
    }
    let messages = authed_get_json(app.clone(), &token, &path).await;
    assert_eq!(messages["messages"].as_array().unwrap().len(), 3);
    let posted = authed_post_json(
        app.clone(),
        &token,
        &path,
        json!({"content":"optional reply", "reply_to_message_id":"msg-1"}),
    )
    .await;
    assert_eq!(posted["ok"], true);
    let messages = authed_get_json(app, &token, &path).await;
    assert_eq!(messages["messages"].as_array().unwrap().len(), 4);
    let reply = messages["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["content"] == "optional reply")
        .unwrap();
    assert!(reply["mention"].is_null());
}

#[tokio::test]
async fn public_hive_mention_does_not_change_private_hive_posts_or_history() {
    let (_dir, app, token, _, state) = build_test_app(100);
    let created = authed_post_json(app.clone(), &token, "/v1/wattetheria/hives", json!({
        "feed_key":"private.hive", "scope_hint":"group:dm-private-hive", "display_name":"Private Chat", "projection_kind":"chat_room"
    })).await;
    let hive_id = created["hive"]["topic_id"].as_str().unwrap();
    let path = format!("/v1/wattetheria/hives/{hive_id}/messages");
    let mut stream = state.stream_tx.subscribe();
    let response = authed_post_json(
        app.clone(),
        &token,
        &path,
        json!({
            "content":"private message", "mention":"unknown-name-must-not-be-resolved"
        }),
    )
    .await;
    assert_eq!(response["ok"], true, "{response}");
    let posted = loop {
        let event = stream.try_recv().unwrap();
        if event.kind == "topic.message.posted" {
            break event;
        }
    };
    assert!(posted.payload.get("mention").is_none());
    assert!(posted.payload.get("author_agent_did").is_none());
    let stored = state
        .swarm_bridge
        .list_topic_messages(
            Some(&test_network_id(&state)),
            "private.hive",
            "group:dm-private-hive",
            50,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    let envelope = stored[0].agent_envelope.as_ref().unwrap();
    assert_envelope_signature_valid(envelope, &state.identity.public_key);
    assert!(envelope.target_agent_id.is_none());
    assert!(envelope.message["payload"].get("mention").is_none());
    for uri in [
        path.as_str(),
        "/v1/client/hives/messages?feed_key=private.hive&scope_hint=group:dm-private-hive",
    ] {
        let history = authed_get_json(app.clone(), &token, uri).await;
        let rows = history["messages"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{history}");
        assert_eq!(rows[0]["content"], "private message");
        assert!(rows[0].get("mention").is_none());
        assert!(rows[0].get("author_agent_did").is_none());
    }
}
