use super::*;

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn agent_social_routes_sign_and_forward_friend_and_dm_commands() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        discovered_agents: BTreeMap::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        private_hive_key_share_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Borealis".to_string(),
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
            Some("12D3KooRemotePeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }

    let relationship_response = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "action": "request",
            "message": {
                "kind": "friend_request",
                "text": "connect with me"
            },
            "extensions": {
                "source": "product"
            }
        }),
    )
    .await;
    assert_eq!(relationship_response["ok"].as_bool(), Some(true));

    let relationship_commands = bridge.relationship_commands.lock().await;
    assert_eq!(relationship_commands.len(), 1);
    let relationship_command = &relationship_commands[0];
    assert_eq!(relationship_command.remote_node_id, "12D3KooRemotePeer");
    assert_eq!(
        relationship_command.agent_envelope.capability.as_deref(),
        Some("social.friend.request")
    );
    assert_eq!(
        relationship_command
            .agent_envelope
            .source_agent_id
            .as_deref(),
        Some(identity.agent_did.as_str())
    );
    assert_eq!(
        relationship_command
            .agent_envelope
            .source_agent_card
            .as_ref()
            .and_then(|card| card.card["metadata"]["display_name"].as_str()),
        Some("Captain Aurora")
    );
    assert_eq!(
        relationship_command
            .agent_envelope
            .source_agent_card
            .as_ref()
            .and_then(|card| card.card["metadata"]["agent_id"].as_str()),
        Some(identity.agent_did.as_str())
    );
    assert_eq!(
        relationship_command
            .agent_envelope
            .source_agent_card
            .as_ref()
            .and_then(|card| card.card["metadata"]["public_id"].as_str()),
        Some(local_public_id.as_str())
    );
    assert_eq!(
        relationship_command
            .agent_envelope
            .target_agent_id
            .as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_envelope_signature_valid(
        &relationship_command.agent_envelope,
        &state.identity.public_key,
    );
    drop(relationship_commands);

    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            display_name: Some("Broker Borealis".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: None,
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed active friendship for dm policy");

    let dm_response = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/social/agent-dm/messages",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "content": {
                "type": "text",
                "text": "hello from wattetheria"
            },
            "extensions": {
                "conversation_hint": "friendship"
            }
        }),
    )
    .await;
    assert_eq!(dm_response["ok"].as_bool(), Some(true));

    let dm_commands = bridge.dm_commands.lock().await;
    assert_eq!(dm_commands.len(), 1);
    let dm_command = &dm_commands[0];
    assert_eq!(dm_command.remote_node_id, "12D3KooRemotePeer");
    assert_eq!(
        dm_command.agent_envelope.capability.as_deref(),
        Some("social.dm.send")
    );
    assert_eq!(
        dm_command
            .agent_envelope
            .source_agent_card
            .as_ref()
            .and_then(|card| card.card["metadata"]["display_name"].as_str()),
        Some("Captain Aurora")
    );
    assert_eq!(
        dm_command.agent_envelope.source_agent_id.as_deref(),
        Some(identity.agent_did.as_str())
    );
    assert_envelope_signature_valid(&dm_command.agent_envelope, &state.identity.public_key);

    let friend_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list persisted friend requests");
    assert_eq!(friend_requests.len(), 1);
    assert_eq!(friend_requests[0].remote_public_id, remote_public_id);

    let threads = thread_service::list_threads(&*state.social_store, &local_public_id)
        .expect("list persisted dm threads");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].remote_public_id, remote_public_id);

    let messages =
        message_service::list_thread_messages(&*state.social_store, &threads[0].thread_id)
            .expect("list persisted dm messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].content_json["text"].as_str(),
        Some("hello from wattetheria")
    );

    let receipts =
        receipt_service::list_message_receipts(&*state.social_store, &messages[0].message_id)
            .expect("list persisted dm receipts");
    assert_eq!(receipts.len(), 1);

    let relationship_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-friends?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(relationship_items.as_array().unwrap().len(), 1);
    assert_eq!(
        relationship_items[0]["counterpart_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );

    let thread_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-dm/threads?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(thread_items.as_array().unwrap().len(), 1);
    assert_eq!(
        thread_items[0]["counterpart_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );

    let message_items = authed_get_json(
        app,
        &token,
        &format!("/v1/wattetheria/social/agent-dm/messages?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(message_items.as_array().unwrap().len(), 1);
    assert_eq!(
        message_items[0]["content"]["text"].as_str(),
        Some("hello from wattetheria")
    );
}

#[tokio::test]
async fn agent_friend_request_by_node_omits_unknown_target_identity_fields() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_node_id = "a355df5568b7a19f0e1136beef266a81279b04a0e4c534d769614ae0a1edf665";

    let response = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "remote_node_id": remote_node_id,
            "action": "request",
            "message": {
                "kind": "friend_request",
                "mock_transport_response": "queued"
            }
        }),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let envelope = &commands[0].agent_envelope;
    assert_eq!(envelope.target_node_id.as_deref(), Some(remote_node_id));
    assert!(envelope.target_agent_id.is_none());
    assert!(envelope.message.get("target_public_id").is_none());
}

#[tokio::test]
async fn agent_friend_request_is_denied_when_counterpart_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        discovered_agents: BTreeMap::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        private_hive_key_share_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Borealis".to_string(),
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
            Some("12D3KooRemotePeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    block_service::upsert_block(
        &*state.social_store,
        &wattetheria_social::domain::blocks::SocialBlock {
            block_id: "block:alice:borealis".to_string(),
            owner_public_id: local_public_id.clone(),
            blocked_public_id: remote_public_id.clone(),
            blocked_node_id: Some("12D3KooRemotePeer".to_string()),
            reason: Some("blocked".to_string()),
            created_at: 1,
            updated_at: 1,
        },
    )
    .unwrap();

    let status = authed_post(
        app,
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "action": "request",
            "message": {
                "kind": "friend_request",
                "text": "connect with me"
            }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(bridge.relationship_commands.lock().await.is_empty());
}

#[tokio::test]
async fn agent_friends_status_uses_social_transport_binding_for_remote_node() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    let remote_node_id = "12D3KooRemotePeer".to_string();
    let mut bridge = MockSwarmBridge::default_for(identity.agent_did.clone());
    bridge.network_status = SwarmNetworkStatusView {
        running: true,
        mode: "network".to_string(),
        peer_protocol_distribution: BTreeMap::new(),
    };
    bridge.peers = vec![SwarmPeerView {
        node_id: remote_node_id.clone(),
        connected: Some(true),
        recently_seen: Some(true),
        stale: Some(false),
        last_seen_age_ms: None,
        discovery: None,
        metadata: Some(json!({"network_id": "mainnet:watt-etheria"})),
        relationship: None,
    }];
    let bridge_handle: Arc<dyn SwarmBridge> = Arc::new(bridge);
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    wattetheria_social::application::transport_binding_service::upsert_transport_binding(
        &*state.social_store,
        &wattetheria_social::domain::transport_bindings::RemoteTransportBinding {
            public_id: remote_public_id.clone(),
            agent_did: Some(remote_identity.agent_did.clone()),
            transport_kind:
                wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm,
            transport_node_id: remote_node_id.clone(),
            binding_source: "friendship".to_string(),
            binding_confidence: 90,
            binding_proof_json: None,
            binding_verified: true,
            binding_verified_at: Some(1),
            updated_at: 1,
        },
    )
    .expect("seed social transport binding");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            display_name: Some("Broker Borealis".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: Some("req-accepted-1".to_string()),
            thread_id: None,
            created_at: 1,
            updated_at: 2,
        },
    )
    .expect("seed active friendship");

    let relationship_items = authed_get_json(
        app,
        &token,
        &format!("/v1/wattetheria/social/agent-friends?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(relationship_items.as_array().unwrap().len(), 1);
    assert_eq!(
        relationship_items[0]["remote_node_id"].as_str(),
        Some(remote_node_id.as_str())
    );
    assert_eq!(relationship_items[0]["connected"].as_bool(), Some(true));
    assert_eq!(relationship_items[0]["status"].as_str(), Some("online"));
}

#[tokio::test]
async fn inbound_friend_requests_are_scoped_to_requested_public_identity() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> =
        Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);

    let primary_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let secondary_public_id = scoped_id("broker-secondary", &identity.agent_did);
    state
        .public_identity_registry
        .lock()
        .await
        .upsert(
            &secondary_public_id,
            "Broker Secondary".to_string(),
            Some(identity.agent_did.clone()),
            true,
        )
        .expect("seed secondary public identity");

    for (request_id, local_public_id, remote_public_id) in [
        (
            "request-primary",
            primary_public_id,
            scoped_id("broker-remote-primary", &Identity::new_random().agent_did),
        ),
        (
            "request-secondary",
            secondary_public_id.clone(),
            scoped_id("broker-remote-secondary", &Identity::new_random().agent_did),
        ),
    ] {
        friend_request_service::upsert_friend_request(
            &*state.social_store,
            &wattetheria_social::domain::friend_requests::FriendRequest {
                request_id: request_id.to_string(),
                local_public_id,
                remote_public_id,
                remote_node_id: None,
                direction:
                    wattetheria_social::domain::friend_requests::FriendRequestDirection::Inbound,
                state: wattetheria_social::domain::friend_requests::FriendRequestState::Pending,
                decision_reason: None,
                correlation_id: None,
                created_at: 1,
                updated_at: 1,
                expires_at: None,
            },
        )
        .expect("seed inbound friend request");
    }
    let response = authed_get_json(
        app,
        &token,
        &format!("/v1/client/friend-requests?public_id={secondary_public_id}"),
    )
    .await;

    assert_eq!(response["count"].as_u64(), Some(1));
    assert_eq!(
        response["items"][0]["request_id"].as_str(),
        Some("request-secondary")
    );
}

#[tokio::test]
async fn queued_relationship_reject_waits_for_swarm_projection() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-queued-reject", &remote_identity.agent_did);
    state
        .public_identity_registry
        .lock()
        .await
        .upsert(
            &remote_public_id,
            "Queued Reject Remote".to_owned(),
            Some(remote_identity.agent_did.clone()),
            true,
        )
        .expect("seed remote identity");
    state.controller_binding_registry.lock().await.upsert(
        &remote_public_id,
        wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
        "queued-reject-runtime".to_owned(),
        Some("remote-node".to_owned()),
        wattetheria_kernel::civilization::identities::OwnershipScope::External,
        true,
    );
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "request-queued-reject".to_owned(),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            remote_node_id: Some("remote-node".to_owned()),
            direction: wattetheria_social::domain::friend_requests::FriendRequestDirection::Inbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: Some("correlation-queued-reject".to_owned()),
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        },
    )
    .expect("seed pending request");

    let response = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "remote_node_id": "remote-node",
            "action": "reject",
            "message": {
                "request_id": "request-queued-reject",
                "correlation_id": "correlation-queued-reject",
                "mock_transport_response": "queued"
            }
        }),
    )
    .await;

    assert_eq!(response["queued"].as_bool(), Some(true));
    let requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list friend requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].state,
        wattetheria_social::domain::friend_requests::FriendRequestState::DecisionPending
    );
    assert_eq!(
        requests[0].decision_reason.as_deref(),
        Some(wattetheria_social::domain::friend_requests::DECISION_PENDING_REJECT_REASON)
    );
    let friendships = friendship_service::list_friendships(&*state.social_store, &local_public_id)
        .expect("list friendships");
    assert_eq!(friendships.as_slice(), []);

    let repeated = authed_post(
        app,
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "remote_node_id": "remote-node",
            "action": "reject",
            "message": {
                "request_id": "request-queued-reject",
                "correlation_id": "correlation-queued-reject",
                "mock_transport_response": "queued"
            }
        }),
    )
    .await;
    assert_eq!(repeated, StatusCode::CONFLICT);
    assert_eq!(bridge.relationship_commands.lock().await.len(), 1);
}

#[tokio::test]
async fn queued_outbound_friend_request_is_persisted_for_reliability() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-queued-request", &remote_identity.agent_did);
    state
        .public_identity_registry
        .lock()
        .await
        .upsert(
            &remote_public_id,
            "Queued Request Remote".to_owned(),
            Some(remote_identity.agent_did),
            true,
        )
        .expect("seed remote identity");
    state.controller_binding_registry.lock().await.upsert(
        &remote_public_id,
        wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
        "queued-request-runtime".to_owned(),
        Some("remote-node".to_owned()),
        wattetheria_kernel::civilization::identities::OwnershipScope::External,
        true,
    );

    let response = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "remote_node_id": "remote-node",
            "action": "request",
            "message": {
                "kind": "friend_request",
                "mock_transport_response": "queued"
            }
        }),
    )
    .await;

    assert_eq!(response["queued"].as_bool(), Some(true));
    let requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list friend requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].direction,
        wattetheria_social::domain::friend_requests::FriendRequestDirection::Outbound
    );
    assert_eq!(
        requests[0].state,
        wattetheria_social::domain::friend_requests::FriendRequestState::Pending
    );
    assert_eq!(
        requests[0].request_id,
        bridge.relationship_commands.lock().await[0]
            .agent_envelope
            .message["request_id"]
            .as_str()
            .expect("queued command request id")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_friend_request_creates_new_id_but_denies_active_friendship() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Borealis".to_string(),
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
            Some("12D3KooRemotePeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "request-existing-pending".to_string(),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            remote_node_id: Some("12D3KooRemotePeer".to_string()),
            direction:
                wattetheria_social::domain::friend_requests::FriendRequestDirection::Outbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: None,
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        },
    )
    .unwrap();

    let next_request_status = authed_post(
        app.clone(),
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "action": "request",
            "message": {
                "kind": "friend_request",
                "request_id": "request-existing-pending",
                "text": "retry connect with me"
            }
        }),
    )
    .await;

    assert_eq!(next_request_status, StatusCode::ACCEPTED);
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let next_request_id = commands[0].agent_envelope.message["request_id"]
        .as_str()
        .expect("new request id")
        .to_owned();
    assert_ne!(next_request_id, "request-existing-pending");
    drop(commands);
    let sent_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list sent requests");
    assert_eq!(sent_requests.len(), 2);
    assert!(sent_requests.iter().any(|request| {
        request.request_id == "request-existing-pending"
            && request.state
                == wattetheria_social::domain::friend_requests::FriendRequestState::Cancelled
            && request.decision_reason.as_deref() == Some("superseded_by_new_request")
    }));
    assert!(sent_requests.iter().any(|request| {
        request.request_id == next_request_id
            && request.state
                == wattetheria_social::domain::friend_requests::FriendRequestState::Pending
    }));

    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            display_name: Some("Broker Borealis".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: Some("request-existing-pending".to_string()),
            thread_id: Some("dm:alice:borealis".to_string()),
            created_at: 2,
            updated_at: 2,
        },
    )
    .unwrap();

    let active_friend_status = authed_post(
        app,
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "action": "request",
            "message": {
                "kind": "friend_request",
                "text": "should not send to an active friend"
            }
        }),
    )
    .await;

    assert_eq!(active_friend_status, StatusCode::FORBIDDEN);
    assert_eq!(bridge.relationship_commands.lock().await.len(), 1);
}
