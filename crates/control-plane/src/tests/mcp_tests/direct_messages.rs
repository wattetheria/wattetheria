use super::*;

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
