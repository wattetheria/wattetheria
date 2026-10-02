use super::*;

fn unrelated_friend_request_relationship_view(
    identity: &Identity,
    remote_identity: &Identity,
) -> SwarmPeerRelationshipView {
    SwarmPeerRelationshipView {
        remote_node_id: "unrelated-node".to_string(),
        relationship_state: "requested".to_string(),
        last_action: "request".to_string(),
        initiated_by: "remote".to_string(),
        agent_envelope: Some(SwarmAgentEnvelope {
            protocol: "google_a2a".to_string(),
            transport_profile: None,
            source_agent_id: Some(remote_identity.agent_did.clone()),
            target_agent_id: Some(identity.agent_did.clone()),
            source_node_id: Some("unrelated-node".to_string()),
            target_node_id: Some(identity.agent_did.clone()),
            capability: Some("social.friend.request".to_string()),
            source_agent_card: None,
            message: json!({
                "kind": "friend_request",
                "request_id": "request-unrelated",
                "source_public_id": "agent-unrelated",
                "target_public_id": "local-unrelated",
                "text": "this unrelated bridge view must not be reconciled by retry maintenance"
            }),
            extensions: None,
            signature: Some("sig-unrelated".to_string()),
        }),
        requested_at: Some(1),
        responded_at: None,
        blocked_at: None,
        cleared_at: None,
        updated_at: 1,
    }
}

fn accepted_friend_request_relationship_view(
    identity: &Identity,
    remote_identity: &Identity,
    remote_node_id: &str,
    local_public_id: &str,
    remote_public_id: &str,
) -> SwarmPeerRelationshipView {
    SwarmPeerRelationshipView {
        remote_node_id: remote_node_id.to_string(),
        relationship_state: "accepted".to_string(),
        last_action: "accept".to_string(),
        initiated_by: "remote".to_string(),
        agent_envelope: Some(SwarmAgentEnvelope {
            protocol: "google_a2a".to_string(),
            transport_profile: Some("wattswarm_mesh".to_string()),
            source_agent_id: Some(remote_identity.agent_did.clone()),
            target_agent_id: Some(identity.agent_did.clone()),
            source_node_id: Some(remote_node_id.to_string()),
            target_node_id: Some(identity.agent_did.clone()),
            capability: Some("social.friend.accept".to_string()),
            source_agent_card: None,
            message: json!({
                "request_id": "remote-accepted-request",
                "correlation_id": "remote-accepted-correlation",
                "source_public_id": remote_public_id,
                "target_public_id": local_public_id,
            }),
            extensions: None,
            signature: Some("sig-remote-accepted".to_string()),
        }),
        requested_at: Some(1),
        responded_at: Some(2),
        blocked_at: None,
        cleared_at: None,
        updated_at: 2,
    }
}

#[tokio::test]
async fn reliability_maintenance_retries_due_connected_outbound_friend_request() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let mut bridge = MockSwarmBridge::default_for(identity.agent_did.clone());
    bridge.peers = vec![SwarmPeerView {
        node_id: "remote-node".to_string(),
        connected: Some(true),
        recently_seen: Some(true),
        stale: Some(false),
        last_seen_age_ms: None,
        discovery: None,
        metadata: None,
        relationship: None,
    }];
    bridge.relationship_views = Mutex::new(vec![unrelated_friend_request_relationship_view(
        &identity,
        &remote_identity,
    )]);
    let bridge = Arc::new(bridge);
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app, &token, &identity.agent_did).await;
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
            Some("remote-node".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "request-existing-pending".to_string(),
            local_public_id: local_public_id.clone(),
            remote_public_id,
            remote_node_id: Some("remote-node".to_string()),
            direction:
                wattetheria_social::domain::friend_requests::FriendRequestDirection::Outbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: Some("correlation-1".to_string()),
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        },
    )
    .unwrap();

    let processed = run_reliability_maintenance_tick_once(&state, 10)
        .await
        .expect("run reliability maintenance");

    assert_eq!(processed, 1);
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].remote_node_id, "remote-node");
    assert_eq!(
        commands[0].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Request
    );
    assert_eq!(
        commands[0].agent_envelope.target_node_id.as_deref(),
        Some("remote-node")
    );
    drop(commands);
    let task = state
        .social_store
        .get_reliability_task("friend_request", "request-existing-pending")
        .expect("get delivery task")
        .expect("delivery task");
    assert_eq!(task.attempt_count, 1);
    assert!(task.next_attempt_at > chrono::Utc::now().timestamp());
    let friend_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list friend requests after maintenance");
    assert!(
        friend_requests
            .iter()
            .all(|request| request.request_id != "request-unrelated"),
        "retry maintenance should only reconcile bridge views for due pending friend requests"
    );
}

#[tokio::test]
async fn reliability_retry_by_node_omits_unknown_target_identity_fields() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let local_public_id = bootstrap_broker_identity(app, &token, &identity.agent_did).await;
    let remote_node_id = "a355df5568b7a19f0e1136beef266a81279b04a0e4c534d769614ae0a1edf665";
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "request-node-only-retry".to_owned(),
            local_public_id,
            remote_public_id: remote_node_id.to_owned(),
            remote_node_id: Some(remote_node_id.to_owned()),
            direction:
                wattetheria_social::domain::friend_requests::FriendRequestDirection::Outbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: Some("correlation-node-only-retry".to_owned()),
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        },
    )
    .expect("seed node-only outbound request");

    let processed = run_reliability_maintenance_tick_once(&state, 10)
        .await
        .expect("run reliability maintenance");

    assert_eq!(processed, 1);
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let envelope = &commands[0].agent_envelope;
    assert_eq!(envelope.target_node_id.as_deref(), Some(remote_node_id));
    assert!(envelope.target_agent_id.is_none());
    assert!(envelope.message.get("target_public_id").is_none());
}

#[tokio::test]
async fn reliability_maintenance_retries_queued_friend_request_decision() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let local_public_id = bootstrap_broker_identity(app, &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-decision-retry", &remote_identity.agent_did);
    state
        .public_identity_registry
        .lock()
        .await
        .upsert(
            &remote_public_id,
            "Decision Retry Remote".to_owned(),
            Some(remote_identity.agent_did),
            true,
        )
        .expect("seed remote identity");
    state.controller_binding_registry.lock().await.upsert(
        &remote_public_id,
        wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
        "decision-retry-runtime".to_owned(),
        Some("remote-node".to_owned()),
        wattetheria_kernel::civilization::identities::OwnershipScope::External,
        true,
    );
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "request-decision-retry".to_owned(),
            local_public_id: local_public_id.clone(),
            remote_public_id,
            remote_node_id: Some("remote-node".to_owned()),
            direction: wattetheria_social::domain::friend_requests::FriendRequestDirection::Inbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::DecisionPending,
            decision_reason: Some(
                wattetheria_social::domain::friend_requests::DECISION_PENDING_ACCEPT_REASON
                    .to_owned(),
            ),
            correlation_id: Some("correlation-decision-retry".to_owned()),
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        },
    )
    .expect("seed queued decision");

    let processed = run_reliability_maintenance_tick_once(&state, 10)
        .await
        .expect("run reliability maintenance");

    assert_eq!(processed, 1);
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Accept
    );
    assert_eq!(
        commands[0].agent_envelope.capability.as_deref(),
        Some("social.friend.accept")
    );
    drop(commands);
    let task = state
        .social_store
        .get_reliability_task("friend_request", "request-decision-retry")
        .expect("get decision retry task")
        .expect("decision retry task");
    assert_eq!(task.attempt_count, 1);
}

#[tokio::test]
async fn reliability_maintenance_settles_reverse_accepted_outbound_request_without_retrying() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_node_id = "remote-node";
    let mut bridge = MockSwarmBridge::default_for(identity.agent_did.clone());
    bridge.peers = vec![SwarmPeerView {
        node_id: remote_node_id.to_string(),
        connected: Some(true),
        recently_seen: Some(true),
        stale: Some(false),
        last_seen_age_ms: None,
        discovery: None,
        metadata: None,
        relationship: None,
    }];
    let bridge = Arc::new(bridge);
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app, &token, &identity.agent_did).await;
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
            Some(remote_node_id.to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    bridge
        .relationship_views
        .lock()
        .await
        .push(accepted_friend_request_relationship_view(
            &identity,
            &remote_identity,
            remote_node_id,
            &local_public_id,
            &remote_public_id,
        ));
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "stale-outbound-pending".to_string(),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_node_id.to_string(),
            remote_node_id: Some(remote_node_id.to_string()),
            direction:
                wattetheria_social::domain::friend_requests::FriendRequestDirection::Outbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: Some("stale-correlation".to_string()),
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        },
    )
    .unwrap();
    state
        .social_store
        .record_reliability_attempt("friend_request", "stale-outbound-pending", 2, 3, None)
        .expect("seed retry task");

    let processed = run_reliability_maintenance_tick_once(&state, 10)
        .await
        .expect("run reliability maintenance");

    assert_eq!(processed, 0);
    assert!(bridge.relationship_commands.lock().await.is_empty());
    let requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list friend requests after maintenance");
    let settled = requests
        .iter()
        .find(|request| request.request_id == "stale-outbound-pending")
        .expect("settled stale outbound request");
    assert_eq!(
        settled.state,
        wattetheria_social::domain::friend_requests::FriendRequestState::Accepted
    );
    assert!(
        state
            .social_store
            .get_reliability_task("friend_request", "stale-outbound-pending")
            .expect("get retry task")
            .is_none()
    );
}

#[tokio::test]
async fn reliability_maintenance_retries_recently_seen_but_unconnected_outbound_friend_request() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let mut bridge = MockSwarmBridge::default_for(identity.agent_did.clone());
    // Recently seen but not a strict live peer: reachable for retry after loosening.
    bridge.peers = vec![SwarmPeerView {
        node_id: "remote-node".to_string(),
        connected: Some(false),
        recently_seen: Some(true),
        stale: Some(false),
        last_seen_age_ms: None,
        discovery: None,
        metadata: None,
        relationship: None,
    }];
    bridge.relationship_views = Mutex::new(vec![unrelated_friend_request_relationship_view(
        &identity,
        &remote_identity,
    )]);
    let bridge = Arc::new(bridge);
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app, &token, &identity.agent_did).await;
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
            Some("remote-node".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "request-recently-seen-pending".to_string(),
            local_public_id: local_public_id.clone(),
            remote_public_id,
            remote_node_id: Some("remote-node".to_string()),
            direction:
                wattetheria_social::domain::friend_requests::FriendRequestDirection::Outbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: Some("correlation-1".to_string()),
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        },
    )
    .unwrap();

    let processed = run_reliability_maintenance_tick_once(&state, 10)
        .await
        .expect("run reliability maintenance");

    assert_eq!(processed, 1);
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].remote_node_id, "remote-node");
    drop(commands);
    let task = state
        .social_store
        .get_reliability_task("friend_request", "request-recently-seen-pending")
        .expect("get delivery task")
        .expect("delivery task");
    assert_eq!(task.attempt_count, 1);
}
