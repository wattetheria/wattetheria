use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn swarm_relationship_views_are_scoped_to_envelope_local_public_identity() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let primary_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let secondary_public_id = scoped_id("broker-secondary", &identity.agent_did);
    state
        .public_identity_registry
        .lock()
        .await
        .upsert(
            &secondary_public_id,
            "Broker Secondary".to_owned(),
            Some(identity.agent_did.clone()),
            true,
        )
        .expect("seed secondary public identity");

    let remote_primary = Identity::new_random();
    let remote_secondary = Identity::new_random();
    let remote_primary_public_id = scoped_id("broker-remote-primary", &remote_primary.agent_did);
    let remote_secondary_public_id =
        scoped_id("broker-remote-secondary", &remote_secondary.agent_did);
    *bridge.relationship_views.lock().await = vec![
        SwarmPeerRelationshipView {
            remote_node_id: "remote-node-primary".to_owned(),
            relationship_state: "requested".to_owned(),
            last_action: "request".to_owned(),
            initiated_by: "remote".to_owned(),
            agent_envelope: Some(SwarmAgentEnvelope {
                protocol: "google_a2a".to_owned(),
                transport_profile: None,
                source_agent_id: Some(remote_primary.agent_did),
                target_agent_id: Some(identity.agent_did.clone()),
                source_node_id: Some("remote-node-primary".to_owned()),
                target_node_id: Some("local-node".to_owned()),
                capability: Some("social.friend.request".to_owned()),
                source_agent_card: None,
                message: json!({
                    "request_id": "request-primary-view",
                    "source_public_id": remote_primary_public_id,
                    "target_public_id": primary_public_id.clone()
                }),
                extensions: None,
                signature: Some("signature-primary".to_owned()),
            }),
            requested_at: Some(1),
            responded_at: None,
            blocked_at: None,
            cleared_at: None,
            updated_at: 1,
        },
        SwarmPeerRelationshipView {
            remote_node_id: "remote-node-secondary".to_owned(),
            relationship_state: "requested".to_owned(),
            last_action: "request".to_owned(),
            initiated_by: "remote".to_owned(),
            agent_envelope: Some(SwarmAgentEnvelope {
                protocol: "google_a2a".to_owned(),
                transport_profile: None,
                source_agent_id: Some(remote_secondary.agent_did),
                target_agent_id: Some(identity.agent_did),
                source_node_id: Some("remote-node-secondary".to_owned()),
                target_node_id: Some("local-node".to_owned()),
                capability: Some("social.friend.request".to_owned()),
                source_agent_card: None,
                message: json!({
                    "request_id": "request-secondary-view",
                    "source_public_id": remote_secondary_public_id,
                    "target_public_id": secondary_public_id.clone()
                }),
                extensions: None,
                signature: Some("signature-secondary".to_owned()),
            }),
            requested_at: Some(2),
            responded_at: None,
            blocked_at: None,
            cleared_at: None,
            updated_at: 2,
        },
    ];

    let primary = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/client/friend-requests?public_id={primary_public_id}"),
    )
    .await;
    let secondary = authed_get_json(
        app,
        &token,
        &format!("/v1/client/friend-requests?public_id={secondary_public_id}"),
    )
    .await;

    assert_eq!(primary["count"].as_u64(), Some(1));
    assert_eq!(
        primary["items"][0]["request_id"].as_str(),
        Some("request-primary-view")
    );
    assert_eq!(secondary["count"].as_u64(), Some(1));
    assert_eq!(
        secondary["items"][0]["request_id"].as_str(),
        Some("request-secondary-view")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_social_queries_reconcile_inbound_swarm_views_into_social_store() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    let remote_node_id = "12D3KooRemotePeer".to_string();
    let transport_thread_id = "transport-thread-42".to_string();
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
        peers: vec![SwarmPeerView {
            node_id: remote_node_id.clone(),
            connected: Some(true),
            recently_seen: Some(true),
            stale: Some(false),
            last_seen_age_ms: None,
            discovery: None,
            metadata: Some(json!({"network_id": "mainnet:watt-etheria"})),
            relationship: None,
        }],
        discovered_agents: BTreeMap::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(vec![SwarmPeerRelationshipView {
            remote_node_id: remote_node_id.clone(),
            relationship_state: "accepted".to_string(),
            last_action: "accept".to_string(),
            initiated_by: "remote".to_string(),
            agent_envelope: Some(SwarmAgentEnvelope {
                protocol: "google_a2a".to_string(),
                transport_profile: None,
                source_agent_id: Some(remote_identity.agent_did.clone()),
                target_agent_id: Some(identity.agent_did.clone()),
                source_node_id: Some(remote_node_id.clone()),
                target_node_id: None,
                capability: Some("social.friend.accept".to_string()),
                source_agent_card: Some(SwarmSourceAgentCard {
                    agent_id: remote_identity.agent_did.clone(),
                    node_id: Some(remote_node_id.clone()),
                    card_hash: "sha256:remote-card".to_string(),
                    issued_at: 1_710_000_120_000,
                    card: json!({
                        "name": "Remote Agent Alice",
                        "description": "Remote agent profile from the accepted relationship action.",
                        "metadata": {
                            "public_id": remote_public_id.clone()
                        },
                        "skills": [
                            {"id": "social-direct-message", "name": "Social direct message"},
                            {"id": "task-participation", "name": "Task participation"}
                        ]
                    }),
                    signature: Some("card-sig-1".to_string()),
                }),
                message: json!({
                    "request_id": "req-inbound-1",
                    "correlation_id": "corr-inbound-1",
                    "source_public_id": remote_identity.agent_did.clone()
                }),
                extensions: None,
                signature: Some("sig-1".to_string()),
            }),
            requested_at: Some(1_710_000_100),
            responded_at: Some(1_710_000_150),
            blocked_at: None,
            cleared_at: None,
            updated_at: 1_710_000_150,
        }]),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(vec![SwarmPeerDmThreadView {
            remote_node_id: remote_node_id.clone(),
            thread_id: transport_thread_id.clone(),
            thread_kind: "direct".to_string(),
            session_state: "ready".to_string(),
            relationship_established_at: None,
            created_at: 1_710_000_150,
            updated_at: 1_710_000_180,
            last_message_at: Some(1_710_000_180),
        }]),
        dm_messages: Mutex::new(BTreeMap::from([(
            transport_thread_id.clone(),
            vec![SwarmPeerDmMessageView {
                thread_id: transport_thread_id.clone(),
                message_id: "dm-msg-1".to_string(),
                remote_node_id: remote_node_id.clone(),
                message_kind: "message".to_string(),
                direction: "inbound".to_string(),
                delivery_state: "acknowledged".to_string(),
                a2a_protocol: "google_a2a".to_string(),
                agent_envelope: Some(SwarmAgentEnvelope {
                    protocol: "google_a2a".to_string(),
                    transport_profile: None,
                    source_agent_id: Some(remote_identity.agent_did.clone()),
                    target_agent_id: Some(identity.agent_did.clone()),
                    source_node_id: None,
                    target_node_id: None,
                    capability: Some("social.dm.send".to_string()),
                    source_agent_card: Some(SwarmSourceAgentCard {
                        agent_id: remote_identity.agent_did.clone(),
                        node_id: Some(remote_node_id.clone()),
                        card_hash: "sha256:remote-card-renamed".to_string(),
                        issued_at: 1_710_000_180_000,
                        card: json!({
                            "name": "Remote Agent Alice Renamed",
                            "metadata": {
                                "public_id": remote_public_id.clone(),
                                "display_name": "Remote Agent Alice Renamed"
                            }
                        }),
                        signature: Some("card-sig-2".to_string()),
                    }),
                    message: json!({
                        "thread_id": transport_thread_id,
                        "message_id": "dm-msg-1",
                        "source_public_id": remote_identity.agent_did.clone()
                    }),
                    extensions: None,
                    signature: Some("sig-2".to_string()),
                }),
                content: json!({"type":"text","text":"hello inbound"}),
                encrypted_body: None,
                content_encoding: None,
                created_at: 1_710_000_180,
                acknowledged_at: Some(1_710_000_181),
            }],
        )])),
        dm_commands: Mutex::new(Vec::new()),
        private_hive_key_share_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
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
            binding_verified_at: Some(1_710_000_150),
            updated_at: 1_710_000_150,
        },
    )
    .expect("seed social transport binding");
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "req-inbound-1".to_string(),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            remote_node_id: Some(remote_node_id.clone()),
            direction: wattetheria_social::domain::friend_requests::FriendRequestDirection::Inbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Accepted,
            decision_reason: Some("accepted".to_string()),
            correlation_id: Some("corr-inbound-1".to_string()),
            created_at: 1_710_000_100,
            updated_at: 1_710_000_150,
            expires_at: None,
        },
    )
    .expect("seed accepted request alongside friendship");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_node_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_node_id.clone(),
            display_name: None,
            state: wattetheria_social::domain::friendships::FriendshipState::Removed,
            established_from_request_id: Some("req-inbound-1".to_string()),
            thread_id: Some("legacy-thread".to_string()),
            created_at: 1_710_000_100,
            updated_at: 1_710_000_120,
        },
    )
    .expect("seed legacy node-id friendship");

    let client_message_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/client/friends/messages?public_id={local_public_id}"),
    )
    .await;
    let client_message_items = client_message_items
        .as_array()
        .unwrap_or_else(|| panic!("expected client DM message array, got {client_message_items}"));
    assert_eq!(client_message_items.len(), 1);
    assert_eq!(
        client_message_items[0]["content"]["text"].as_str(),
        Some("hello inbound")
    );

    let _ = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-dm/messages?public_id={local_public_id}"),
    )
    .await;

    let relationship_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-friends?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(relationship_items.as_array().unwrap().len(), 1);
    assert_eq!(
        relationship_items[0]["relationship_state"].as_str(),
        Some("active")
    );
    assert_eq!(
        relationship_items[0]["counterpart_display_name"].as_str(),
        Some("Remote Agent Alice Renamed")
    );
    assert_eq!(
        relationship_items[0]["counterpart_agent_did"].as_str(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        relationship_items[0]["counterpart_agent_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
    assert_eq!(
        relationship_items[0]["counterpart_description"].as_str(),
        Some("Remote agent profile from the accepted relationship action.")
    );
    assert_eq!(
        relationship_items[0]["counterpart_skills"][0].as_str(),
        Some("Social direct message")
    );
    assert_eq!(
        relationship_items[0]["network_id"].as_str(),
        Some("mainnet:watt-etheria")
    );
    let remote_profiles =
        wattetheria_social::application::remote_identity_service::list_remote_identities(
            &*state.social_store,
        )
        .expect("list cached remote identity profiles");
    let remote_profile = remote_profiles
        .iter()
        .find(|identity| identity.public_id == remote_public_id)
        .expect("remote identity profile cached from source agent card");
    assert_eq!(remote_profile.agent_did, remote_identity.agent_did);
    assert_eq!(remote_profile.display_name, "Remote Agent Alice Renamed");
    assert_eq!(
        remote_profile.last_profile_fetched_at,
        Some(1_710_000_120_000)
    );
    assert_eq!(remote_profile.updated_at, 1_710_000_120_000);
    assert_eq!(
        remote_profile.skills,
        vec!["Social direct message", "Task participation"]
    );

    let thread_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-dm/threads?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(thread_items.as_array().unwrap().len(), 1);
    assert_eq!(
        thread_items[0]["counterpart_display_name"].as_str(),
        Some("Remote Agent Alice Renamed")
    );

    let message_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-dm/messages?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(message_items.as_array().unwrap().len(), 1);
    assert_eq!(
        message_items[0]["content"]["text"].as_str(),
        Some("hello inbound")
    );
    assert_eq!(
        message_items[0]["counterpart_display_name"].as_str(),
        Some("Remote Agent Alice Renamed")
    );

    let friend_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list reconciled requests");
    assert_eq!(friend_requests.len(), 1);
    assert_eq!(friend_requests[0].request_id, "req-inbound-1");

    let friendships = friendship_service::list_friendships(&*state.social_store, &local_public_id)
        .expect("list reconciled friendships");
    let active_friendships = friendships
        .iter()
        .filter(|friendship| {
            friendship.state == wattetheria_social::domain::friendships::FriendshipState::Active
        })
        .collect::<Vec<_>>();
    assert_eq!(active_friendships.len(), 1);
    assert_eq!(active_friendships[0].remote_public_id, remote_public_id);
    assert!(friendships.iter().any(|friendship| {
        friendship.remote_public_id == remote_node_id
            && friendship.state == wattetheria_social::domain::friendships::FriendshipState::Removed
    }));

    let threads = thread_service::list_threads(&*state.social_store, &local_public_id)
        .expect("list reconciled threads");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].transport_thread_id, "transport-thread-42");

    let messages =
        message_service::list_thread_messages(&*state.social_store, &threads[0].thread_id)
            .expect("list reconciled messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].content_json["text"].as_str(),
        Some("hello inbound")
    );

    let receipts =
        receipt_service::list_message_receipts(&*state.social_store, &messages[0].message_id)
            .expect("list reconciled receipts");
    assert!(receipts.len() >= 2);

    bridge.relationship_views.lock().await.clear();
    bridge.dm_threads.lock().await.clear();
    bridge.dm_messages.lock().await.clear();

    let relationship_items_after_cache = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-friends?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(relationship_items_after_cache.as_array().unwrap().len(), 1);

    let thread_items_after_cache = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-dm/threads?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(thread_items_after_cache.as_array().unwrap().len(), 1);

    let message_items_after_cache = authed_get_json(
        app,
        &token,
        &format!("/v1/wattetheria/social/agent-dm/messages?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(message_items_after_cache.as_array().unwrap().len(), 1);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_social_reconcile_remote_accept_uses_target_public_id_for_outbound_request() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_node_id = "658c23767f26cf7b90971b2ac9834313515d3e289312b17e7e643569598eb95e";
    let remote_public_id = "658c23767f26cf7b90971b2ac9834313515d3e289312b17e7e643569598eb95e";
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
        dm_threads: Mutex::new(vec![SwarmPeerDmThreadView {
            remote_node_id: remote_node_id.to_string(),
            thread_id: "dm:remote-accept-thread".to_string(),
            thread_kind: "direct".to_string(),
            session_state: "ready".to_string(),
            relationship_established_at: Some(1_781_671_983),
            created_at: 1_781_671_983,
            updated_at: 1_781_671_983,
            last_message_at: None,
        }]),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        private_hive_key_share_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "request-remote-accept".to_string(),
            local_public_id: local_public_id.clone(),
            remote_public_id: local_public_id.clone(),
            remote_node_id: Some(remote_node_id.to_string()),
            direction: wattetheria_social::domain::friend_requests::FriendRequestDirection::Inbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Accepted,
            decision_reason: Some("accepted".to_string()),
            correlation_id: Some("correlation-remote-accept".to_string()),
            created_at: 1_781_671_870,
            updated_at: 1_781_671_983,
            expires_at: None,
        },
    )
    .expect("seed mis-reconciled self friend request");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{local_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: local_public_id.clone(),
            display_name: Some("Self Alias".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: Some("request-remote-accept".to_string()),
            thread_id: Some("dm:self-alias".to_string()),
            created_at: 1_781_671_870,
            updated_at: 1_781_671_983,
        },
    )
    .expect("seed mis-reconciled self friendship");
    bridge
        .relationship_views
        .lock()
        .await
        .push(SwarmPeerRelationshipView {
            remote_node_id: remote_node_id.to_string(),
            relationship_state: "accepted".to_string(),
            last_action: "accept".to_string(),
            initiated_by: "remote".to_string(),
            agent_envelope: Some(SwarmAgentEnvelope {
                protocol: "google_a2a".to_string(),
                transport_profile: Some("wattswarm_mesh".to_string()),
                source_agent_id: Some(identity.agent_did.clone()),
                target_agent_id: Some(remote_identity.agent_did.clone()),
                source_node_id: Some(identity.agent_did.clone()),
                target_node_id: Some(remote_node_id.to_string()),
                capability: Some("social.friend.request".to_string()),
                source_agent_card: None,
                message: json!({
                    "action": "request",
                    "request_id": "request-remote-accept",
                    "correlation_id": "correlation-remote-accept",
                    "source_public_id": local_public_id,
                    "target_public_id": remote_public_id,
                    "payload": "hello"
                }),
                extensions: None,
                signature: Some("sig-remote-accept".to_string()),
            }),
            requested_at: Some(1_781_671_870),
            responded_at: Some(1_781_671_983),
            blocked_at: None,
            cleared_at: None,
            updated_at: 1_781_671_983,
        });

    let relationship_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-friends?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(relationship_items.as_array().unwrap().len(), 1);
    assert_eq!(
        relationship_items[0]["counterpart_public_id"].as_str(),
        Some(remote_public_id)
    );
    assert_eq!(
        relationship_items[0]["relationship_state"].as_str(),
        Some("active")
    );

    let friendships = friendship_service::list_friendships(&*state.social_store, &local_public_id)
        .expect("list reconciled friendships");
    let active_friendships = friendships
        .iter()
        .filter(|friendship| {
            friendship.state == wattetheria_social::domain::friendships::FriendshipState::Active
        })
        .collect::<Vec<_>>();
    assert_eq!(active_friendships.len(), 1);
    assert_eq!(active_friendships[0].remote_public_id, remote_public_id);
    assert_ne!(active_friendships[0].remote_public_id, local_public_id);
    assert!(friendships.iter().any(|friendship| {
        friendship.remote_public_id == local_public_id
            && friendship.state == wattetheria_social::domain::friendships::FriendshipState::Removed
    }));

    let friend_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list reconciled friend requests");
    assert_eq!(friend_requests.len(), 1);
    assert_eq!(friend_requests[0].remote_public_id, remote_public_id);
    assert_eq!(
        friend_requests[0].state,
        wattetheria_social::domain::friend_requests::FriendRequestState::Accepted
    );

    let thread_items = authed_get_json(
        app,
        &token,
        &format!("/v1/wattetheria/social/agent-dm/threads?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(thread_items.as_array().unwrap().len(), 1);
    assert_eq!(
        thread_items[0]["counterpart_public_id"].as_str(),
        Some(remote_public_id)
    );
}

fn inbound_friend_request_relationship_view(
    identity: &Identity,
    remote_identity: &Identity,
    remote_node_id: &str,
    local_public_id: &str,
    remote_public_id: &str,
) -> SwarmPeerRelationshipView {
    SwarmPeerRelationshipView {
        remote_node_id: remote_node_id.to_string(),
        relationship_state: "requested".to_string(),
        last_action: "request".to_string(),
        initiated_by: "remote".to_string(),
        agent_envelope: Some(SwarmAgentEnvelope {
            protocol: "google_a2a".to_string(),
            transport_profile: None,
            source_agent_id: Some(remote_identity.agent_did.clone()),
            target_agent_id: Some(identity.agent_did.clone()),
            source_node_id: Some(remote_node_id.to_string()),
            target_node_id: Some("local-node".to_string()),
            capability: Some("social.friend.request".to_string()),
            source_agent_card: None,
            message: json!({
                "request_id": "request-inbound",
                "source_public_id": remote_public_id,
                "target_public_id": local_public_id,
            }),
            extensions: None,
            signature: Some("sig-inbound".to_string()),
        }),
        requested_at: Some(1),
        responded_at: None,
        blocked_at: None,
        cleared_at: None,
        updated_at: 1,
    }
}

#[tokio::test]
async fn accepted_inbound_request_stays_friends_when_swarm_view_loses_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_identity = Identity::new_random();
    let remote_public_id = scoped_id("broker-remote", &remote_identity.agent_did);
    let remote_node_id = "remote-node-sandbox";

    *bridge.relationship_views.lock().await = vec![inbound_friend_request_relationship_view(
        &identity,
        &remote_identity,
        remote_node_id,
        &local_public_id,
        &remote_public_id,
    )];
    let requests = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/client/friend-requests?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(requests["count"].as_u64(), Some(1));

    let accepted = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/social/friend-requests/request-inbound/accept",
        json!({"public_id": local_public_id}),
    )
    .await;
    assert_ne!(accepted["ok"].as_bool(), Some(false), "{accepted}");

    // After a local accept wattswarm keeps one accepted view for the node,
    // with no agent envelope left to name the counterpart.
    *bridge.relationship_views.lock().await = vec![SwarmPeerRelationshipView {
        remote_node_id: remote_node_id.to_string(),
        relationship_state: "accepted".to_string(),
        last_action: "accept".to_string(),
        initiated_by: "local".to_string(),
        agent_envelope: None,
        requested_at: Some(1),
        responded_at: Some(2),
        blocked_at: None,
        cleared_at: None,
        updated_at: 2,
    }];
    let _ = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/client/friend-requests?public_id={local_public_id}"),
    )
    .await;

    let friendships = friendship_service::list_friendships(&*state.social_store, &local_public_id)
        .expect("friendships");
    let friendship = friendships
        .iter()
        .find(|friendship| friendship.remote_public_id == remote_public_id)
        .expect("public id friendship");
    assert_eq!(
        friendship.state,
        wattetheria_social::domain::friendships::FriendshipState::Active
    );
    assert!(
        friendships
            .iter()
            .all(|friendship| friendship.remote_public_id != remote_node_id),
        "no node-id alias friendship: {friendships:?}"
    );
    let request =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("requests")
            .into_iter()
            .find(|request| request.request_id == "request-inbound")
            .expect("inbound request");
    assert_eq!(
        request.direction,
        wattetheria_social::domain::friend_requests::FriendRequestDirection::Inbound
    );

    let dm_response = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/social/agent-dm/messages",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "content": {"type": "text", "text": "hello after accept"},
        }),
    )
    .await;
    assert_eq!(dm_response["ok"].as_bool(), Some(true), "{dm_response}");
    assert_eq!(bridge.dm_commands.lock().await.len(), 1);
}

#[tokio::test]
async fn envelope_less_view_for_multi_agent_node_does_not_retire_friendship() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_node_id = "remote-node-multi-agent";
    for public_id in ["agent-friend", "agent-neighbour"] {
        wattetheria_social::application::transport_binding_service::upsert_transport_binding(
            &*state.social_store,
            &wattetheria_social::domain::transport_bindings::RemoteTransportBinding {
                public_id: public_id.to_string(),
                agent_did: None,
                transport_kind:
                    wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm,
                transport_node_id: remote_node_id.to_string(),
                binding_source: "test".to_string(),
                binding_confidence: 50,
                binding_proof_json: None,
                binding_verified: false,
                binding_verified_at: None,
                updated_at: 1,
            },
        )
        .expect("seed transport binding");
    }
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:agent-friend"),
            local_public_id: local_public_id.clone(),
            remote_public_id: "agent-friend".to_string(),
            display_name: None,
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: Some("request-friend".to_string()),
            thread_id: None,
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed friendship");
    *bridge.relationship_views.lock().await = vec![SwarmPeerRelationshipView {
        remote_node_id: remote_node_id.to_string(),
        relationship_state: "accepted".to_string(),
        last_action: "accept".to_string(),
        initiated_by: "local".to_string(),
        agent_envelope: None,
        requested_at: Some(1),
        responded_at: Some(2),
        blocked_at: None,
        cleared_at: None,
        updated_at: 2,
    }];

    let _ = authed_get_json(
        app,
        &token,
        &format!("/v1/client/friend-requests?public_id={local_public_id}"),
    )
    .await;

    let friendships = friendship_service::list_friendships(&*state.social_store, &local_public_id)
        .expect("friendships");
    assert_eq!(friendships.len(), 1, "{friendships:?}");
    assert_eq!(friendships[0].remote_public_id, "agent-friend");
    assert_eq!(
        friendships[0].state,
        wattetheria_social::domain::friendships::FriendshipState::Active
    );
}
