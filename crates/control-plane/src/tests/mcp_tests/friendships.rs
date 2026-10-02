use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_remove_agent_friend_updates_local_relationships_without_network_command() {
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
    let remote_public_id = scoped_id("broker-remove", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Remove".to_string(),
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
            Some("12D3KooRemovePeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            display_name: Some("Broker Remove".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: Some("request-remove-1".to_string()),
            thread_id: Some(format!("dm:{local_public_id}:{remote_public_id}")),
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed active friendship");
    bridge
        .relationship_views
        .lock()
        .await
        .push(SwarmPeerRelationshipView {
            remote_node_id: "12D3KooRemovePeer".to_owned(),
            relationship_state: "accepted".to_owned(),
            last_action: "accept".to_owned(),
            initiated_by: "local".to_owned(),
            agent_envelope: None,
            requested_at: Some(1),
            responded_at: Some(1),
            blocked_at: None,
            cleared_at: None,
            updated_at: 1,
        });

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "remove_agent_friend",
                "arguments": {
                    "display_name": "Broker Remove",
                    "message": {
                        "kind": "friend_remove",
                        "text": "remove friend"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let retry = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "remove_agent_friend",
                "arguments": {"counterpart_public_id": remote_public_id}
            }
        }),
    )
    .await;
    assert_eq!(retry["result"]["isError"].as_bool(), Some(false));
    assert!(bridge.relationship_commands.lock().await.is_empty());
    let views = bridge.relationship_views.lock().await;
    assert_eq!(views[0].relationship_state, "none");
    assert_eq!(views[0].last_action, "remove");
    drop(views);

    let friendships = friendship_service::list_friendships(&*state.social_store, &local_public_id)
        .expect("list friendships after remove");
    assert_eq!(friendships.len(), 1);
    assert_eq!(
        friendships[0].friendship_id,
        format!("friendship:{local_public_id}:{remote_public_id}")
    );
    assert_eq!(friendships[0].remote_public_id, remote_public_id);
    assert_eq!(
        friendships[0].state,
        wattetheria_social::domain::friendships::FriendshipState::Removed
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_accept_and_reject_friend_requests_send_relationship_actions() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let accept_identity = Identity::new_random();
    let reject_identity = Identity::new_random();
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
    let accept_public_id = scoped_id("broker-accept", &accept_identity.agent_did);
    let reject_public_id = scoped_id("broker-reject", &reject_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &accept_public_id,
                "Broker Accept".to_string(),
                Some(accept_identity.agent_did.clone()),
                true,
            )
            .unwrap();
        identities
            .upsert(
                &reject_public_id,
                "Broker Reject".to_string(),
                Some(reject_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &accept_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "accept-runtime".to_string(),
            Some("12D3KooAcceptPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
        bindings.upsert(
            &reject_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "reject-runtime".to_string(),
            Some("12D3KooRejectPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    for (request_id, remote_public_id, remote_node_id, correlation_id) in [
        (
            "req-accept-1",
            accept_public_id.as_str(),
            "12D3KooAcceptPeer",
            "corr-accept-1",
        ),
        (
            "req-reject-1",
            reject_public_id.as_str(),
            "12D3KooRejectPeer",
            "corr-reject-1",
        ),
    ] {
        friend_request_service::upsert_friend_request(
            &*state.social_store,
            &FriendRequest {
                request_id: request_id.to_string(),
                local_public_id: local_public_id.clone(),
                remote_public_id: remote_public_id.to_string(),
                remote_node_id: Some(remote_node_id.to_string()),
                direction: FriendRequestDirection::Inbound,
                state: FriendRequestState::Pending,
                decision_reason: None,
                correlation_id: Some(correlation_id.to_string()),
                created_at: 1,
                updated_at: 1,
                expires_at: None,
            },
        )
        .expect("save inbound friend request");
    }

    let accept_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "accept_friend_request",
                "arguments": {"display_name": "Broker Accept"}
            }
        }),
    )
    .await;
    let reject_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "reject_friend_request",
                "arguments": {"display_name": "Broker Reject"}
            }
        }),
    )
    .await;

    assert_eq!(accept_response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(reject_response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Accept
    );
    assert_eq!(commands[0].remote_node_id, "12D3KooAcceptPeer");
    assert_eq!(
        commands[0]
            .agent_envelope
            .message
            .get("request_id")
            .and_then(Value::as_str),
        Some("req-accept-1")
    );
    assert_eq!(
        commands[0]
            .agent_envelope
            .message
            .get("correlation_id")
            .and_then(Value::as_str),
        Some("corr-accept-1")
    );
    assert_eq!(
        commands[1].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Reject
    );
    assert_eq!(commands[1].remote_node_id, "12D3KooRejectPeer");
    assert_eq!(
        commands[1]
            .agent_envelope
            .message
            .get("request_id")
            .and_then(Value::as_str),
        Some("req-reject-1")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_friend_request_tools_split_list_and_detail_views() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-inbound", &remote_identity.agent_did);
    let remote_node_id = "12D3KooInboundPeer".to_string();
    let bridge = Arc::new(MockSwarmBridge {
        peers: vec![SwarmPeerView {
            node_id: remote_node_id.clone(),
            connected: Some(true),
            recently_seen: Some(true),
            stale: Some(false),
            last_seen_age_ms: None,
            discovery: Some(json!({"source_kind": "bootstrap"})),
            metadata: Some(json!({
                "endpoint_id": "iroh-endpoint-inbound",
                "network_id": "mainnet:watt-etheria",
                "protocol_version": "wattswarm/1.0.0",
                "handshake_status": "identified",
                "observed_addr": "198.51.100.2:4001",
                "listen_addrs": ["203.0.113.10:4001"]
            })),
            relationship: None,
        }],
        relationship_views: Mutex::new(vec![
            SwarmPeerRelationshipView {
                remote_node_id: remote_node_id.clone(),
                relationship_state: "requested".to_string(),
                last_action: "request".to_string(),
                initiated_by: "remote".to_string(),
                agent_envelope: Some(SwarmAgentEnvelope {
                    protocol: "google_a2a".to_string(),
                    transport_profile: None,
                    source_agent_id: Some(remote_identity.agent_did.clone()),
                    target_agent_id: Some(identity.agent_did.clone()),
                    source_node_id: Some(remote_node_id.clone()),
                    target_node_id: None,
                    capability: Some("peer.relationship.request".to_string()),
                    source_agent_card: Some(SwarmSourceAgentCard {
                        agent_id: remote_identity.agent_did.clone(),
                        node_id: Some(remote_node_id.clone()),
                        card_hash: "sha256:alice-display-card".to_string(),
                        issued_at: 1_710_000_100,
                        card: json!({
                            "name": "Agent Alice Display",
                            "metadata": {
                                "display_name": "Agent Alice Display"
                            },
                            "skills": [
                                {
                                    "id": "social-direct-message",
                                    "name": "Social direct message",
                                    "description": "Can send and receive signed peer relationship and direct message events."
                                }
                            ]
                        }),
                        signature: Some("sig-alice-display-card".to_string()),
                    }),
                    message: json!({
                        "kind": "friend_request",
                        "text": "hello, I am Alice from node X",
                        "request_id": "req-inbound-1",
                        "correlation_id": "corr-inbound-1",
                        "sent_at": 1_710_000_100
                    }),
                    extensions: None,
                    signature: Some("sig-inbound".to_string()),
                }),
                requested_at: Some(1_710_000_100),
                responded_at: None,
                blocked_at: None,
                cleared_at: None,
                updated_at: 1_710_000_105,
            },
            SwarmPeerRelationshipView {
                remote_node_id: "12D3KooOutboundPeer".to_string(),
                relationship_state: "requested".to_string(),
                last_action: "request".to_string(),
                initiated_by: "local".to_string(),
                agent_envelope: Some(SwarmAgentEnvelope {
                    protocol: "google_a2a".to_string(),
                    transport_profile: None,
                    source_agent_id: Some(identity.agent_did.clone()),
                    target_agent_id: Some(remote_identity.agent_did.clone()),
                    source_node_id: None,
                    target_node_id: Some("12D3KooOutboundPeer".to_string()),
                    capability: Some("peer.relationship.request".to_string()),
                    source_agent_card: None,
                    message: json!({
                        "kind": "friend_request",
                        "text": "outbound hello",
                        "request_id": "req-outbound-1",
                        "correlation_id": "corr-outbound-1",
                        "sent_at": 1_710_000_090
                    }),
                    extensions: None,
                    signature: Some("sig-outbound".to_string()),
                }),
                requested_at: Some(1_710_000_090),
                responded_at: None,
                blocked_at: None,
                cleared_at: None,
                updated_at: 1_710_000_095,
            },
        ]),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge;
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Agent Alice".to_string(),
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
            Some(remote_node_id.clone()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }

    let list_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_friend_requests",
                "arguments": {}
            }
        }),
    )
    .await;
    let list_content = &list_response["result"]["structuredContent"];
    assert_eq!(list_content["ok"].as_bool(), Some(true));
    assert_eq!(list_content["count"].as_u64(), Some(1));
    assert_eq!(
        list_content["items"][0]["request_id"].as_str(),
        Some("req-inbound-1")
    );
    assert_eq!(
        list_content["items"][0]["from"].as_str(),
        Some("Agent Alice Display")
    );
    assert_eq!(
        list_content["items"][0]["preview"].as_str(),
        Some("hello, I am Alice from node X")
    );
    assert_eq!(
        list_content["items"][0]["counterpart_skills"][0].as_str(),
        Some("Social direct message")
    );
    assert_eq!(
        list_content["items"][0]["direction"].as_str(),
        Some("inbound")
    );
    assert_eq!(list_content["items"][0]["state"].as_str(), Some("pending"));
    assert_eq!(
        list_content["items"][0]["remote_node_id"].as_str(),
        Some(remote_node_id.as_str())
    );
    assert_eq!(
        list_content["items"][0]["counterpart_agent_did"].as_str(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        list_content["items"][0]["agent"]["display_name"].as_str(),
        Some("Agent Alice Display")
    );
    assert_eq!(
        list_content["items"][0]["agent"]["agent_card"]["name"].as_str(),
        Some("Agent Alice Display")
    );
    assert_eq!(
        list_content["items"][0]["agent_card"]["metadata"]["display_name"].as_str(),
        Some("Agent Alice Display")
    );
    assert_eq!(
        list_content["items"][0]["source_agent_card"]["card_hash"].as_str(),
        Some("sha256:alice-display-card")
    );
    assert!(list_content["items"][0].get("agent_envelope").is_none());
    assert_eq!(
        list_content["items"][0]["message"]["text"].as_str(),
        Some("hello, I am Alice from node X")
    );
    assert_eq!(
        list_content["items"][0]["message"]["request_id"].as_str(),
        Some("req-inbound-1")
    );
    assert_eq!(
        list_content["items"][0]["network"]["remote_node_id"].as_str(),
        Some(remote_node_id.as_str())
    );
    assert_eq!(
        list_content["items"][0]["network"]["metadata"]["network_id"].as_str(),
        Some("mainnet:watt-etheria")
    );

    let sent_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_sent_friend_requests",
                "arguments": {}
            }
        }),
    )
    .await;
    let sent_content = &sent_response["result"]["structuredContent"];
    assert_eq!(sent_content["count"].as_u64(), Some(1));
    assert_eq!(
        sent_content["items"][0]["request_id"].as_str(),
        Some("req-outbound-1")
    );
    assert_eq!(sent_content["items"][0]["state"].as_str(), Some("pending"));

    let get_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "get_friend_request",
                "arguments": {
                    "display_name": "Agent Alice Display"
                }
            }
        }),
    )
    .await;
    let detail = &get_response["result"]["structuredContent"];
    assert_eq!(detail["ok"].as_bool(), Some(true));
    assert_eq!(
        detail["agent"]["display_name"].as_str(),
        Some("Agent Alice Display")
    );
    assert_eq!(
        detail["agent"]["skills"][0].as_str(),
        Some("Social direct message")
    );
    assert_eq!(
        detail["agent"]["counterpart_skills"][0].as_str(),
        Some("Social direct message")
    );
    assert_eq!(
        detail["agent"]["agent_did"].as_str(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(detail["message"]["kind"].as_str(), Some("friend_request"));
    assert_eq!(
        detail["message"]["text"].as_str(),
        Some("hello, I am Alice from node X")
    );
    assert_eq!(
        detail["network"]["remote_node_id"].as_str(),
        Some(remote_node_id.as_str())
    );
    assert_eq!(detail["network"]["status"].as_str(), Some("online"));
    assert_eq!(
        detail["network"]["metadata"]["observed_addr"].as_str(),
        Some("198.51.100.2:4001")
    );
}
