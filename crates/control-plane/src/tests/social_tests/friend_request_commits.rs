use super::*;

#[tokio::test]
async fn agent_action_commit_rejects_social_block_without_side_effects() {
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

    let committed = authed_post(
        app.clone(),
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-friend-1",
                "event_type": "friend_request",
                "source_kind": "peer_relationship",
                "source_node_id": "12D3KooRemotePeer",
                "target_agent_id": identity.agent_did,
                "payload": {
                    "agent_envelope": {
                        "message": {
                            "source_public_id": remote_public_id,
                            "target_public_id": local_public_id,
                        }
                    }
                },
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-friend-1",
                "action": "block",
                "route": "wattetheria_commit",
                "payload": {
                    "message": {"kind": "friend_request", "text": "blocked"}
                }
            }
        }),
    )
    .await;

    assert_eq!(committed, StatusCode::BAD_REQUEST);
    let blocks = block_service::list_blocks(&*state.social_store, &local_public_id)
        .expect("list social blocks");
    assert_eq!(blocks.as_slice(), []);

    let relationship_commands = bridge.relationship_commands.lock().await;
    assert!(relationship_commands.is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_action_commit_resolves_friend_request_before_legacy_node_target() {
    let dir = tempfile::tempdir().unwrap();
    let primary_identity = Identity::new_random();
    let secondary_identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(
        primary_identity.agent_did.clone(),
    ));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, primary_identity.clone(), event_log, bridge_handle);

    let primary_public_id =
        bootstrap_broker_identity(app.clone(), &token, &primary_identity.agent_did).await;
    let secondary_public_id = scoped_id("broker-secondary", &secondary_identity.agent_did);
    let remote_public_id = scoped_id("broker-remote", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &secondary_public_id,
                "Broker Secondary".to_owned(),
                Some(secondary_identity.agent_did.clone()),
                true,
            )
            .expect("seed secondary local identity");
        identities
            .upsert(
                &remote_public_id,
                "Broker Remote".to_owned(),
                Some(remote_identity.agent_did),
                true,
            )
            .expect("seed remote identity");
    }
    state.controller_binding_registry.lock().await.upsert(
        &remote_public_id,
        wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
        "remote-runtime".to_owned(),
        Some("remote-node".to_owned()),
        wattetheria_kernel::civilization::identities::OwnershipScope::External,
        true,
    );
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "request-node-target".to_owned(),
            local_public_id: secondary_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            remote_node_id: Some("remote-node".to_owned()),
            direction: wattetheria_social::domain::friend_requests::FriendRequestDirection::Inbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: Some("correlation-node-target".to_owned()),
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        },
    )
    .expect("seed inbound request");

    let legacy_node_target = "a355df5568b7a19f0e1136beef266a81279b04a0e4c534d769614ae0a1edf665";
    let committed = authed_post_json_with_headers(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-friend-node-target",
                "event_type": "friend_request",
                "source_kind": "peer_relationship",
                "source_node_id": "remote-node",
                "target_agent_id": legacy_node_target,
                "payload": {
                    "agent_envelope": {
                        "message": {
                            "source_public_id": remote_public_id,
                            "target_public_id": legacy_node_target,
                            "request_id": "request-node-target"
                        }
                    }
                },
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-friend-node-target",
                "action": "accept",
                "route": "wattetheria_commit",
                "payload": {
                    "request_id": "request-node-target",
                    "message": {"mock_transport_response": "queued"}
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-friend-node-target"),
            ("x-agent-decision-id", "dec-friend-node-target"),
        ],
    )
    .await;

    assert_eq!(committed["ok"].as_bool(), Some(true));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].agent_envelope.message["source_public_id"].as_str(),
        Some(secondary_public_id.as_str())
    );
    drop(commands);
    assert_eq!(
        friend_request_service::list_friend_requests(&*state.social_store, &primary_public_id)
            .expect("list primary requests")
            .as_slice(),
        []
    );
    let secondary_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &secondary_public_id)
            .expect("list secondary requests");
    assert_eq!(secondary_requests.len(), 1);
    assert_eq!(
        secondary_requests[0].state,
        wattetheria_social::domain::friend_requests::FriendRequestState::DecisionPending
    );
}

#[tokio::test]
async fn agent_action_commit_rejects_real_target_did_request_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let primary_identity = Identity::new_random();
    let secondary_identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(
        primary_identity.agent_did.clone(),
    ));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, primary_identity.clone(), event_log, bridge_handle);

    let _primary_public_id =
        bootstrap_broker_identity(app.clone(), &token, &primary_identity.agent_did).await;
    let secondary_public_id = scoped_id("broker-secondary", &secondary_identity.agent_did);
    let remote_public_id = scoped_id("broker-remote", &remote_identity.agent_did);
    state
        .public_identity_registry
        .lock()
        .await
        .upsert(
            &secondary_public_id,
            "Broker Secondary".to_owned(),
            Some(secondary_identity.agent_did),
            true,
        )
        .expect("seed secondary local identity");
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "request-target-conflict".to_owned(),
            local_public_id: secondary_public_id,
            remote_public_id: remote_public_id.clone(),
            remote_node_id: Some("remote-node".to_owned()),
            direction: wattetheria_social::domain::friend_requests::FriendRequestDirection::Inbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: None,
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        },
    )
    .expect("seed inbound request");

    let committed = authed_post_json_with_headers(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-friend-target-conflict",
                "event_type": "friend_request",
                "source_kind": "peer_relationship",
                "source_node_id": "remote-node",
                "target_agent_id": primary_identity.agent_did,
                "payload": {
                    "agent_envelope": {
                        "message": {
                            "source_public_id": remote_public_id,
                            "request_id": "request-target-conflict"
                        }
                    }
                },
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-friend-target-conflict",
                "action": "accept",
                "route": "wattetheria_commit",
                "payload": {"request_id": "request-target-conflict"}
            }
        }),
        &[
            ("x-agent-event-id", "evt-friend-target-conflict"),
            ("x-agent-decision-id", "dec-friend-target-conflict"),
        ],
    )
    .await;

    assert_eq!(
        committed["error"].as_str(),
        Some("friend_request target_agent_id conflicts with request_id")
    );
    assert!(bridge.relationship_commands.lock().await.is_empty());
}
