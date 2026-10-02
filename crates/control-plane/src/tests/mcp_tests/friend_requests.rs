use super::*;

#[tokio::test]
async fn mcp_request_agent_friend_sends_relationship_action_to_remote_node() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "remote_node_id": "nearby-node-1",
                    "message": {
                        "kind": "friend_request",
                        "text": "hello nearby node"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, "nearby-node-1");
    assert_eq!(
        serde_json::to_value(&command.action).unwrap().as_str(),
        Some("request")
    );
    assert_eq!(
        command.agent_envelope.capability.as_deref(),
        Some("social.friend.request")
    );
    assert!(
        command
            .agent_envelope
            .message
            .get("source_public_id")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("target_public_id")
            .and_then(Value::as_str),
        Some("nearby-node-1")
    );
}

#[tokio::test]
async fn mcp_request_agent_friend_rejects_overlong_message() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "remote_node_id": "nearby-node-1",
                    "message": "x".repeat(121)
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("friend request message must be at most 120 characters")
    );
    assert_eq!(
        response["result"]["structuredContent"]["max_chars"].as_u64(),
        Some(120)
    );
    assert_eq!(
        response["result"]["structuredContent"]["actual_chars"].as_u64(),
        Some(121)
    );
    let commands = bridge.relationship_commands.lock().await;
    assert!(commands.is_empty());
}

#[tokio::test]
async fn mcp_request_agent_friend_resolves_target_agent_did_to_remote_node() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-delta", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Delta".to_string(),
                Some(remote_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &remote_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "remote-runtime".to_string(),
            Some("12D3KooTargetPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "target_agent_did": remote_identity.agent_did,
                    "remote_node_id": "stale-nearby-node",
                    "message": {
                        "kind": "friend_request",
                        "text": "hello known agent"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, "12D3KooTargetPeer");
    assert_eq!(
        command.agent_envelope.target_agent_id.as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("target_public_id")
            .and_then(Value::as_str),
        Some(remote_public_id.as_str())
    );
}

#[tokio::test]
async fn mcp_request_agent_friend_uses_discovered_node_after_local_removal() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let remote_public_id = scoped_id("broker-removed", &remote_identity.agent_did);
    let remote_node_id = "12D3KooFreshDiscoveryPeer";
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            remote_public_id.clone(),
            SwarmDiscoveredAgent {
                public_id: remote_public_id.clone(),
                remote_node_id: remote_node_id.to_string(),
                target_agent_did: remote_identity.agent_did.clone(),
                display_name: Some("Broker Removed".to_string()),
                source_agent_card: None,
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    wattetheria_social::application::remote_identity_service::upsert_remote_identity(
        &*state.social_store,
        &wattetheria_social::domain::identities::RemoteIdentityProfile {
            public_id: remote_public_id.clone(),
            agent_did: remote_identity.agent_did.clone(),
            display_name: "Broker Removed".to_string(),
            description: None,
            capabilities: Vec::new(),
            skills: Vec::new(),
            did_document_json: None,
            active: false,
            last_profile_fetched_at: Some(1),
            created_at: 1,
            updated_at: 2,
        },
    )
    .expect("seed removed remote identity");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id,
            remote_public_id: remote_public_id.clone(),
            display_name: Some("Broker Removed".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Removed,
            established_from_request_id: Some("request-removed-1".to_string()),
            thread_id: None,
            created_at: 1,
            updated_at: 2,
        },
    )
    .expect("seed removed friendship");

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "target_agent_did": remote_identity.agent_did,
                    "counterpart_public_id": remote_public_id,
                    "remote_node_id": remote_node_id
                }
            }
        }),
    )
    .await;

    assert_eq!(
        response["result"]["isError"].as_bool(),
        Some(false),
        "{response}"
    );
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].remote_node_id, remote_node_id);
    assert_eq!(
        commands[0].agent_envelope.target_agent_id.as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        commands[0].agent_envelope.message["target_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
}

#[tokio::test]
async fn mcp_request_agent_friend_does_not_fall_back_to_removed_remote_identity() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-removed-cached", &remote_identity.agent_did);
    state
        .public_identity_registry
        .lock()
        .await
        .upsert(
            &remote_public_id,
            "Broker Removed Cached".to_string(),
            Some(remote_identity.agent_did.clone()),
            true,
        )
        .expect("seed kernel identity");
    state.controller_binding_registry.lock().await.upsert(
        &remote_public_id,
        wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
        "remote-runtime".to_string(),
        Some("12D3KooCachedRemovedPeer".to_string()),
        wattetheria_kernel::civilization::identities::OwnershipScope::External,
        true,
    );
    wattetheria_social::application::remote_identity_service::upsert_remote_identity(
        &*state.social_store,
        &wattetheria_social::domain::identities::RemoteIdentityProfile {
            public_id: remote_public_id.clone(),
            agent_did: remote_identity.agent_did.clone(),
            display_name: "Broker Removed Cached".to_string(),
            description: None,
            capabilities: Vec::new(),
            skills: Vec::new(),
            did_document_json: None,
            active: false,
            last_profile_fetched_at: Some(1),
            created_at: 1,
            updated_at: 2,
        },
    )
    .expect("seed removed social identity");

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "target_agent_did": remote_identity.agent_did,
                    "counterpart_public_id": remote_public_id,
                    "remote_node_id": "12D3KooCachedRemovedPeer"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert!(bridge.relationship_commands.lock().await.is_empty());
}

#[tokio::test]
async fn mcp_request_agent_friend_rejects_conflicting_discovery_fields() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let other_identity = Identity::new_random();
    let remote_public_id = scoped_id("broker-verified", &remote_identity.agent_did);
    let remote_node_id = "12D3KooVerifiedPeer";
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            remote_public_id.clone(),
            SwarmDiscoveredAgent {
                public_id: remote_public_id.clone(),
                remote_node_id: remote_node_id.to_string(),
                target_agent_did: remote_identity.agent_did.clone(),
                display_name: Some("Broker Verified".to_string()),
                source_agent_card: None,
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    for (id, target_agent_did, remote_node_id) in [
        (1, other_identity.agent_did, remote_node_id),
        (2, remote_identity.agent_did, "12D3KooWrongPeer"),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": "request_agent_friend",
                    "arguments": {
                        "counterpart_public_id": remote_public_id.clone(),
                        "target_agent_did": target_agent_did,
                        "remote_node_id": remote_node_id
                    }
                }
            }),
        )
        .await;
        assert_eq!(response["result"]["isError"].as_bool(), Some(true));
        assert!(
            response["result"]["structuredContent"]["error"]
                .as_str()
                .is_some_and(|error| error.contains("discovery result does not match"))
        );
    }
    assert!(bridge.relationship_commands.lock().await.is_empty());
}

#[tokio::test]
async fn mcp_request_agent_friend_resolves_counterpart_public_id_from_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-discovery", &remote_identity.agent_did);
    let remote_node_id = "12D3KooDiscoveryPeer".to_string();
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            remote_public_id.clone(),
            SwarmDiscoveredAgent {
                public_id: remote_public_id.clone(),
                remote_node_id: remote_node_id.clone(),
                target_agent_did: remote_identity.agent_did.clone(),
                display_name: Some("Broker Discovery".to_string()),
                source_agent_card: Some(discovered_source_agent_card(
                    &remote_public_id,
                    "Broker Discovery",
                    &remote_identity.agent_did,
                    &remote_node_id,
                    "broker-discovery-card",
                )),
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Discovery".to_string(),
                Some(remote_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &remote_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "remote-runtime".to_string(),
            Some("12D3KooStaleCachedPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "counterpart_public_id": remote_public_id,
                    "message": {
                        "kind": "friend_request",
                        "text": "hello discovered public id"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, remote_node_id);
    assert_eq!(
        command.agent_envelope.target_agent_id.as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("target_public_id")
            .and_then(Value::as_str),
        Some(remote_public_id.as_str())
    );
}

async fn assert_friend_request_reaches_unregistered_discovered_agent(include_public_id: bool) {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-unregistered", &remote_identity.agent_did);
    let remote_node_id = "12D3KooUnregisteredPeer".to_string();
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            remote_public_id.clone(),
            SwarmDiscoveredAgent {
                public_id: remote_public_id.clone(),
                remote_node_id: remote_node_id.clone(),
                target_agent_did: remote_identity.agent_did.clone(),
                display_name: Some("Broker Unregistered".to_string()),
                source_agent_card: None,
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    // Arguments copied from a search_agents result for an agent that has no
    // local public identity or controller binding yet.
    let mut arguments = json!({
        "target_agent_did": remote_identity.agent_did,
        "remote_node_id": remote_node_id,
        "message": {
            "kind": "friend_request",
            "text": "hello discovered agent"
        }
    });
    if include_public_id {
        arguments["counterpart_public_id"] = json!(remote_public_id);
    }
    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": arguments
            }
        }),
    )
    .await;

    assert_eq!(
        response["result"]["isError"].as_bool(),
        Some(false),
        "{response}"
    );
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, remote_node_id);
    assert_eq!(
        command.agent_envelope.target_agent_id.as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    if include_public_id {
        assert_eq!(
            command
                .agent_envelope
                .message
                .get("target_public_id")
                .and_then(Value::as_str),
            Some(remote_public_id.as_str())
        );
    }
}

#[tokio::test]
async fn mcp_request_agent_friend_falls_back_to_remote_node_for_unknown_did() {
    assert_friend_request_reaches_unregistered_discovered_agent(false).await;
}

#[tokio::test]
async fn mcp_request_agent_friend_accepts_full_discovery_result_for_unknown_did() {
    assert_friend_request_reaches_unregistered_discovered_agent(true).await;
}

#[tokio::test]
async fn mcp_request_agent_friend_resolves_display_name_from_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-display", &remote_identity.agent_did);
    let remote_node_id = "12D3KooDisplayPeer".to_string();
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            remote_public_id.clone(),
            SwarmDiscoveredAgent {
                public_id: remote_public_id.clone(),
                remote_node_id: remote_node_id.clone(),
                target_agent_did: remote_identity.agent_did.clone(),
                display_name: Some("Broker Display".to_string()),
                source_agent_card: Some(discovered_source_agent_card(
                    &remote_public_id,
                    "Broker Display",
                    &remote_identity.agent_did,
                    &remote_node_id,
                    "broker-display-card",
                )),
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "display_name": "@Broker Display",
                    "message": {
                        "kind": "friend_request",
                        "text": "hello discovered display name"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, remote_node_id);
    assert_eq!(
        command.agent_envelope.target_agent_id.as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("target_public_id")
            .and_then(Value::as_str),
        Some(remote_public_id.as_str())
    );
}

#[tokio::test]
async fn mcp_request_agent_friend_rejects_display_name_before_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity_a = Identity::new_random();
    let remote_identity_b = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id_a = scoped_id("broker-display-a", &remote_identity_a.agent_did);
    let remote_public_id_b = scoped_id("broker-display-b", &remote_identity_b.agent_did);
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [
            (
                remote_public_id_a.clone(),
                SwarmDiscoveredAgent {
                    public_id: remote_public_id_a.clone(),
                    remote_node_id: "12D3KooDisplayPeerA".to_string(),
                    target_agent_did: remote_identity_a.agent_did.clone(),
                    display_name: Some("Broker Display".to_string()),
                    source_agent_card: Some(discovered_source_agent_card(
                        &remote_public_id_a,
                        "Broker Display",
                        &remote_identity_a.agent_did,
                        "12D3KooDisplayPeerA",
                        "broker-display-a-card",
                    )),
                },
            ),
            (
                remote_public_id_b.clone(),
                SwarmDiscoveredAgent {
                    public_id: remote_public_id_b.clone(),
                    remote_node_id: "12D3KooDisplayPeerB".to_string(),
                    target_agent_did: remote_identity_b.agent_did.clone(),
                    display_name: Some("Broker Display".to_string()),
                    source_agent_card: Some(discovered_source_agent_card(
                        &remote_public_id_b,
                        "Broker Display",
                        &remote_identity_b.agent_did,
                        "12D3KooDisplayPeerB",
                        "broker-display-b-card",
                    )),
                },
            ),
        ]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "display_name": "Broker Display",
                    "message": "hello ambiguous display name"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert!(
        response["result"]["structuredContent"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("multiple discovery records matched display_name"))
    );
    assert!(bridge.relationship_commands.lock().await.is_empty());
}
