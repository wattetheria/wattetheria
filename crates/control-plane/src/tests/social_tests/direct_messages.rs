use super::*;

#[tokio::test]
async fn agent_dm_is_denied_when_counterpart_is_blocked() {
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
    wattetheria_social::application::transport_binding_service::upsert_transport_binding(
        &*state.social_store,
        &wattetheria_social::domain::transport_bindings::RemoteTransportBinding {
            public_id: remote_public_id.clone(),
            agent_did: Some(remote_identity.agent_did.clone()),
            transport_kind:
                wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm,
            transport_node_id: "12D3KooRemotePeer".to_string(),
            binding_source: "friendship".to_string(),
            binding_confidence: 90,
            binding_proof_json: None,
            binding_verified: true,
            binding_verified_at: Some(1),
            updated_at: 1,
        },
    )
    .unwrap();
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
        "/v1/wattetheria/social/agent-dm/messages",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "content": {
                "type": "text",
                "text": "hello from wattetheria"
            }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(bridge.dm_commands.lock().await.is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn dm_queries_keep_history_and_other_friends_when_a_friend_is_removed_or_blocked() {
    use wattetheria_social::domain::friendships::{Friendship, FriendshipState};

    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge.clone());
    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let mut friendships = Vec::new();
    for remote in ["removed-peer", "blocked-peer", "active-peer"] {
        let thread_id = format!("dm:{remote}");
        let friendship = Friendship {
            friendship_id: format!("friend:{remote}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote.to_string(),
            display_name: None,
            state: FriendshipState::Active,
            established_from_request_id: None,
            thread_id: Some(thread_id.clone()),
            created_at: 10,
            updated_at: 10,
        };
        friendship_service::upsert_friendship(&*state.social_store, &friendship).unwrap();
        friendships.push(friendship);
        bridge.dm_threads.lock().await.push(SwarmPeerDmThreadView {
            remote_node_id: remote.to_string(),
            thread_id: thread_id.clone(),
            thread_kind: "direct".to_string(),
            session_state: "ready".to_string(),
            relationship_established_at: Some(10),
            created_at: 10,
            updated_at: 20,
            last_message_at: Some(20),
        });
        bridge.dm_messages.lock().await.insert(
            thread_id.clone(),
            vec![SwarmPeerDmMessageView {
                thread_id,
                message_id: format!("old:{remote}"),
                remote_node_id: remote.to_string(),
                message_kind: "message".to_string(),
                direction: "inbound".to_string(),
                delivery_state: "delivered".to_string(),
                a2a_protocol: "google_a2a".to_string(),
                agent_envelope: None,
                content: json!({"text": "saved history"}),
                encrypted_body: None,
                content_encoding: None,
                created_at: 20,
                acknowledged_at: None,
            }],
        );
    }
    let url = format!("/v1/client/friends/messages?public_id={local_public_id}");
    let initial = authed_get_json(app.clone(), &token, &url).await;
    assert_eq!(initial.as_array().expect("initial messages").len(), 3);

    for (friendship, next_state) in friendships.iter_mut().zip([
        FriendshipState::Removed,
        FriendshipState::Blocked,
        FriendshipState::Active,
    ]) {
        friendship.state = next_state;
        friendship.updated_at = 30;
        friendship_service::upsert_friendship(&*state.social_store, friendship).unwrap();
    }
    for messages in bridge.dm_messages.lock().await.values_mut() {
        let mut new_message = messages[0].clone();
        new_message.message_id = format!("new:{}", new_message.remote_node_id);
        new_message.created_at = 40;
        messages.push(new_message);
        let mut synthetic = messages[0].clone();
        synthetic.message_id = format!("relationship-established:{}", synthetic.thread_id);
        synthetic.message_kind = "relationship_established".to_string();
        synthetic.content = json!({"synthetic": true});
        messages.push(synthetic);
    }

    // Repeated reads must keep saved history and never import inactive friends' new messages.
    for _ in 0..2 {
        let payload = authed_get_json(app.clone(), &token, &url).await;
        let items = payload
            .as_array()
            .expect("all conversations remain readable");
        assert_eq!(items.len(), 5);
        assert!(
            items
                .iter()
                .any(|item| item["message_id"] == "new:active-peer")
        );
        for remote in ["removed-peer", "blocked-peer"] {
            assert!(
                items
                    .iter()
                    .any(|item| item["message_id"] == format!("old:{remote}"))
            );
            for filter in [
                format!("thread_id=dm:{remote}"),
                format!("counterpart_public_id={remote}"),
            ] {
                let history =
                    authed_get_json(app.clone(), &token, &format!("{url}&{filter}")).await;
                assert_eq!(history.as_array().expect("saved conversation").len(), 1);
                assert_eq!(history[0]["message_id"], format!("old:{remote}"));
            }
            let stored = message_service::list_thread_messages(
                &*state.social_store,
                &format!("dm:{remote}"),
            )
            .unwrap();
            assert_eq!(stored.len(), 1);
        }
    }
    let stored_friendships =
        friendship_service::list_friendships(&*state.social_store, &local_public_id).unwrap();
    assert_eq!(stored_friendships.len(), friendships.len());
    for friendship in friendships {
        assert!(stored_friendships.contains(&friendship));
    }
}
