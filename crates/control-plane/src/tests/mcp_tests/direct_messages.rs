use super::*;

fn seed_dm_history(
    state: &ControlPlaneState,
    local_public_id: &str,
    remote_public_id: &str,
    display_name: &str,
    times: std::ops::Range<i64>,
    friendship_state: wattetheria_social::domain::friendships::FriendshipState,
) -> String {
    use wattetheria_social::domain::messages::{
        DeliveryState, DirectMessage, MessageDirection, MessageKind, ReadState,
    };
    use wattetheria_social::domain::threads::{DirectThread, ThreadState};

    let thread_id = format!("dm:{remote_public_id}");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friend:{remote_public_id}"),
            local_public_id: local_public_id.to_string(),
            remote_public_id: remote_public_id.to_string(),
            display_name: Some(display_name.to_string()),
            state: friendship_state,
            established_from_request_id: None,
            thread_id: Some(thread_id.clone()),
            created_at: times.start,
            updated_at: times.end,
        },
    )
    .unwrap();
    wattetheria_social::application::thread_service::upsert_thread(
        &*state.social_store,
        &DirectThread {
            thread_id: thread_id.clone(),
            local_public_id: local_public_id.to_string(),
            remote_public_id: remote_public_id.to_string(),
            transport_thread_id: thread_id.clone(),
            state: if friendship_state
                == wattetheria_social::domain::friendships::FriendshipState::Active
            {
                ThreadState::Ready
            } else {
                ThreadState::Closed
            },
            last_message_at: Some(times.end - 1),
            created_at: times.start,
            updated_at: times.end,
        },
    )
    .unwrap();
    for time in times {
        wattetheria_social::application::message_service::upsert_message(
            &*state.social_store,
            &DirectMessage {
                thread_id: thread_id.clone(),
                message_id: format!("{thread_id}:{time}"),
                transport_message_id: None,
                local_public_id: local_public_id.to_string(),
                remote_public_id: remote_public_id.to_string(),
                direction: MessageDirection::Inbound,
                message_kind: MessageKind::Message,
                content_json: json!({"text": format!("{display_name} history {time}")}),
                encrypted_body: None,
                content_encoding: None,
                agent_envelope_json: None,
                agent_signature: None,
                delivery_state: DeliveryState::Delivered,
                read_state: ReadState::Unread,
                created_at: time,
                updated_at: time,
            },
        )
        .unwrap();
    }
    thread_id
}

#[tokio::test]
async fn mcp_dm_history_prefers_display_name_before_message_limit() {
    use wattetheria_social::domain::friendships::FriendshipState;

    let (_dir, app, token, _, state) = build_test_app(100);
    bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    let local_public_id = crate::routes::identity::resolve_identity_context(&state, None, None)
        .await
        .public_identity
        .unwrap()
        .public_id;
    let target = scoped_id("history-target", &Identity::new_random().agent_did);
    let other = scoped_id("history-other", &Identity::new_random().agent_did);
    let target_thread = seed_dm_history(
        &state,
        &local_public_id,
        &target,
        "Quiet Friend",
        1..2,
        FriendshipState::Removed,
    );
    let other_thread = seed_dm_history(
        &state,
        &local_public_id,
        &other,
        "Busy Friend",
        10..211,
        FriendshipState::Active,
    );

    for arguments in [
        json!({"display_name": "Quiet Friend"}),
        json!({"display_name": "  Quiet Friend  ", "counterpart_public_id": other, "thread_id": other_thread}),
        json!({"display_name": "Quiet Friend", "counterpart_public_id": "did:key:not-a-public-id"}),
        json!({"counterpart_public_id": target}),
        json!({"thread_id": target_thread}),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "list_agent_dm_messages", "arguments": arguments}
            }),
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        let items = response["result"]["structuredContent"]["items"]
            .as_array()
            .unwrap();
        assert_eq!(items.len(), 1, "arguments={arguments}; response={response}");
        assert_eq!(items[0]["counterpart_public_id"], target);
        assert_eq!(items[0]["thread_id"], target_thread);
        assert_eq!(items[0]["message_id"], format!("{target_thread}:1"));
        assert_eq!(items[0]["content"]["text"], "Quiet Friend history 1");
    }
    for arguments in [json!({}), json!({"display_name": "Busy Friend"})] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "list_agent_dm_messages", "arguments": arguments}
            }),
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        let items = response["result"]["structuredContent"]["items"]
            .as_array()
            .unwrap();
        assert_eq!(items.len(), 200);
        assert!(
            items
                .iter()
                .all(|item| item["counterpart_public_id"] == other)
        );
        assert_eq!(items[0]["created_at"], 210);
        assert_eq!(items[199]["created_at"], 11);
    }
    for (thread, expected_count) in [(&target_thread, 1), (&other_thread, 201)] {
        let saved = wattetheria_social::application::message_service::list_thread_messages(
            &*state.social_store,
            thread,
        )
        .unwrap();
        assert_eq!(saved.len(), expected_count);
    }
}

#[tokio::test]
async fn mcp_dm_history_rejects_did_as_public_id_with_display_name_hint() {
    let (_dir, app, token, _, state) = build_test_app(100);
    bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "list_agent_dm_messages",
                "arguments": {"counterpart_public_id": "did:key:not-a-public-id"}
            }
        }),
    )
    .await;
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(response["result"]["_meta"]["httpStatus"], 400);
    assert_eq!(
        response["result"]["structuredContent"]["error"],
        "counterpart_public_id must be a public ID, not a DID; prefer display_name or provide an agent-... public ID"
    );
}

#[tokio::test]
async fn mcp_dm_history_uses_current_name_instead_of_stale_friendship_name() {
    use wattetheria_social::domain::friendships::FriendshipState;

    let (_dir, app, token, _, state) = build_test_app(100);
    bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    let local_public_id = crate::routes::identity::resolve_identity_context(&state, None, None)
        .await
        .public_identity
        .unwrap()
        .public_id;
    let renamed = scoped_id("history-renamed", &Identity::new_random().agent_did);
    let current = scoped_id("history-current", &Identity::new_random().agent_did);
    seed_dm_history(
        &state,
        &local_public_id,
        &renamed,
        "Original Name",
        10..11,
        FriendshipState::Removed,
    );
    seed_dm_history(
        &state,
        &local_public_id,
        &current,
        "Original Name",
        1..2,
        FriendshipState::Active,
    );
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(&renamed, "Renamed Friend".to_string(), None, true)
            .unwrap();
        identities
            .upsert(&current, "Original Name".to_string(), None, true)
            .unwrap();
    }
    for (display_name, public_id, created_at) in [
        ("Original Name", &current, 1),
        ("Renamed Friend", &renamed, 10),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {
                    "name": "list_agent_dm_messages",
                    "arguments": {"display_name": display_name}
                }
            }),
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        let items = response["result"]["structuredContent"]["items"]
            .as_array()
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["counterpart_public_id"], *public_id);
        assert_eq!(items[0]["created_at"], created_at);
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_send_agent_dm_message_sends_signed_direct_message_to_friend() {
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
    let remote_public_id = scoped_id("broker-dm", &remote_identity.agent_did);
    wattetheria_social::application::remote_identity_service::upsert_remote_identity(
        &*state.social_store,
        &wattetheria_social::domain::identities::RemoteIdentityProfile {
            public_id: remote_public_id.clone(),
            agent_did: remote_identity.agent_did.clone(),
            display_name: "Broker DM".to_string(),
            description: None,
            capabilities: Vec::new(),
            skills: Vec::new(),
            did_document_json: None,
            active: true,
            last_profile_fetched_at: Some(1),
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed remote identity");
    wattetheria_social::application::transport_binding_service::upsert_transport_binding(
        &*state.social_store,
        &wattetheria_social::domain::transport_bindings::RemoteTransportBinding {
            public_id: remote_public_id.clone(),
            agent_did: Some(remote_identity.agent_did.clone()),
            transport_kind:
                wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm,
            transport_node_id: "12D3KooDmPeer".to_string(),
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
            display_name: Some("Broker DM".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: None,
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed active friendship");
    wattetheria_social::application::thread_service::upsert_thread(
        &*state.social_store,
        &wattetheria_social::domain::threads::DirectThread {
            thread_id: "dm:existing-ms-thread".to_string(),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            transport_thread_id: "dm:existing-ms-thread".to_string(),
            state: wattetheria_social::domain::threads::ThreadState::Ready,
            last_message_at: Some(1_780_801_347_838),
            created_at: 1_780_801_347_838,
            updated_at: 1_780_801_347_838,
        },
    )
    .expect("seed existing millisecond dm thread");

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_agent_dm_message",
                "arguments": {
                    "display_name": "Broker DM",
                    "content": {
                        "type": "text",
                        "text": "中文 English العربية 日本語 🙂 hello over private group dm"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let oversized_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "send_agent_dm_message",
                "arguments": {
                    "display_name": "Broker DM",
                    "content": {"text": "x".repeat(4097)}
                }
            }
        }),
    )
    .await;
    assert_eq!(
        oversized_response["result"]["isError"].as_bool(),
        Some(true)
    );
    assert_eq!(
        oversized_response["result"]["_meta"]["httpStatus"].as_u64(),
        Some(400)
    );
    let commands = bridge.dm_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, "12D3KooDmPeer");
    assert_eq!(
        command.agent_envelope.capability.as_deref(),
        Some("social.dm.send")
    );
    assert_eq!(
        command.content["text"].as_str(),
        Some("中文 English العربية 日本語 🙂 hello over private group dm")
    );
    let thread = wattetheria_social::application::thread_service::find_thread(
        &*state.social_store,
        &local_public_id,
        &remote_public_id,
    )
    .expect("find dm thread")
    .expect("dm thread exists");
    assert!(thread.updated_at >= thread.created_at);
    assert!(thread.updated_at >= 1_000_000_000_000);

    let friends_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_friends",
                "arguments": {
                    "display_name": "Broker DM"
                }
            }
        }),
    )
    .await;
    let friends = &friends_response["result"]["structuredContent"];
    assert_eq!(friends["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        friends["items"][0]["counterpart_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );

    let threads_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "list_agent_dm_threads",
                "arguments": {
                    "display_name": "Broker DM"
                }
            }
        }),
    )
    .await;
    let threads = &threads_response["result"]["structuredContent"];
    assert_eq!(threads["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        threads["items"][0]["counterpart_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );

    let messages_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "list_agent_dm_messages",
                "arguments": {
                    "display_name": "Broker DM"
                }
            }
        }),
    )
    .await;
    let messages = &messages_response["result"]["structuredContent"];
    assert_eq!(messages["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        messages["items"][0]["counterpart_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
    assert_eq!(
        messages["items"][0]["content"]["text"].as_str(),
        Some("中文 English العربية 日本語 🙂 hello over private group dm")
    );
}

#[tokio::test]
async fn mcp_send_agent_dm_message_rejects_missing_or_ambiguous_display_name() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
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

    let missing_target_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_agent_dm_message",
                "arguments": {
                    "content": {
                        "type": "text",
                        "text": "hello without target"
                    }
                }
            }
        }),
    )
    .await;
    assert_eq!(
        missing_target_response["result"]["structuredContent"]["error"].as_str(),
        Some("display_name or counterpart_public_id is required")
    );

    for remote_public_id in ["broker-duplicate-a", "broker-duplicate-b"] {
        friendship_service::upsert_friendship(
            &*state.social_store,
            &wattetheria_social::domain::friendships::Friendship {
                friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
                local_public_id: local_public_id.clone(),
                remote_public_id: remote_public_id.to_string(),
                display_name: Some("Duplicate Broker".to_string()),
                state: wattetheria_social::domain::friendships::FriendshipState::Active,
                established_from_request_id: None,
                thread_id: None,
                created_at: 1,
                updated_at: 1,
            },
        )
        .expect("seed duplicate active friendship");
    }

    let duplicate_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "send_agent_dm_message",
                "arguments": {
                    "display_name": "Duplicate Broker",
                    "content": {
                        "type": "text",
                        "text": "hello duplicate"
                    }
                }
            }
        }),
    )
    .await;
    assert_eq!(
        duplicate_response["result"]["structuredContent"]["error"].as_str(),
        Some("multiple active friends matched display_name; provide counterpart_public_id")
    );
    let commands = bridge.dm_commands.lock().await;
    assert!(commands.is_empty());
}
