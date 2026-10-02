use super::*;

#[tokio::test]
async fn public_source_agent_card_builds_signed_discovery_card() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> =
        Arc::new(MockSwarmBridge::default_for("wattswarm-node-1".to_owned()));
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);

    let _captain_public_id = bootstrap_broker_identity(app, &token, &identity.agent_did).await;
    let card = crate::social_host::public_source_agent_card(&state)
        .await
        .expect("build source agent card");
    let active_public_id = state
        .public_identity_registry
        .lock()
        .await
        .active_for_agent_did(&identity.agent_did)
        .expect("active identity")
        .public_id;

    assert_eq!(card.agent_id, identity.agent_did);
    assert_eq!(card.node_id.as_deref(), Some("wattswarm-node-1"));
    assert_eq!(
        card.card["metadata"]["display_name"].as_str(),
        card.card["name"].as_str()
    );
    assert_eq!(
        card.card["metadata"]["agent_id"].as_str(),
        Some(identity.agent_did.as_str())
    );
    assert_eq!(
        card.card["metadata"]["public_id"].as_str(),
        Some(active_public_id.as_str())
    );
    let skills = card.card["skills"]
        .as_array()
        .expect("agent card skills array");
    assert_eq!(skills.as_slice(), [] as [Value; 0]);
    state
        .social_store
        .upsert_agent_skill(&wattetheria_social::domain::agent_skills::AgentSkill {
            skill_id: "custom-research".to_string(),
            name: "Custom research".to_string(),
            description: "Can run configured research workflows.".to_string(),
            tags: vec!["research".to_string()],
            visible: true,
            source: "manual".to_string(),
            sort_order: 5,
            created_at: 10,
            updated_at: 10,
        })
        .expect("save custom advertised skill");
    let updated_card = crate::social_host::public_source_agent_card(&state)
        .await
        .expect("rebuild source agent card");
    assert_eq!(
        updated_card.card["skills"][0]["id"].as_str(),
        Some("custom-research")
    );
    let card_payload = ExpectedSignedSourceAgentCardPayload {
        agent_id: &card.agent_id,
        node_id: card.node_id.as_ref(),
        card_hash: &card.card_hash,
        issued_at: card.issued_at,
    };
    assert!(
        verify_payload(
            &card_payload,
            card.signature.as_deref().expect("missing signature"),
            &state.identity.public_key
        )
        .unwrap(),
        "exported source agent card signature must verify"
    );
}

#[tokio::test]
async fn public_source_agent_card_route_uses_current_display_name() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> =
        Arc::new(MockSwarmBridge::default_for("wattswarm-node-1".to_owned()));
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);

    let _captain_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let original = authed_get_json(app.clone(), &token, "/v1/wattetheria/source-agent-card").await;
    let public_id = original["card"]["metadata"]["public_id"]
        .as_str()
        .expect("current public id")
        .to_owned();

    authed_patch_json(
        app.clone(),
        &token,
        "/v1/civilization/public-identity",
        json!({
            "public_id": public_id,
            "display_name": "AXT2222222",
        }),
    )
    .await;
    let updated = authed_get_json(app, &token, "/v1/wattetheria/source-agent-card").await;

    assert_eq!(updated["card"]["name"].as_str(), Some("AXT2222222"));
    assert_eq!(
        updated["card"]["metadata"]["display_name"].as_str(),
        Some("AXT2222222")
    );
    assert_ne!(
        original["card_hash"].as_str(),
        updated["card_hash"].as_str(),
        "display name changes must produce a fresh signed card hash"
    );
}

#[tokio::test]
async fn agent_skills_api_updates_public_source_agent_card() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> =
        Arc::new(MockSwarmBridge::default_for("wattswarm-node-1".to_owned()));
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);

    let skills = authed_get_json(app.clone(), &token, "/v1/wattetheria/agent-skills").await;
    assert_eq!(
        skills["items"].as_array().unwrap().as_slice(),
        [] as [Value; 0]
    );

    let saved = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/agent-skills",
        json!({
            "name": "Custom research",
            "description": "Can run configured research workflows.",
            "tags": ["research"],
            "visible": true,
            "sort_order": 5
        }),
    )
    .await;
    assert_eq!(saved["item"]["skill_id"].as_str(), Some("custom-research"));

    let card = authed_get_json(app.clone(), &token, "/v1/wattetheria/source-agent-card").await;
    assert_eq!(
        card["card"]["skills"][0]["id"].as_str(),
        Some("custom-research")
    );

    let deleted = request_json(
        app.clone(),
        axum::http::Request::builder()
            .method("DELETE")
            .uri("/v1/wattetheria/agent-skills/custom-research")
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(deleted["ok"].as_bool(), Some(true));
    assert_eq!(deleted["skill_id"].as_str(), Some("custom-research"));

    let skills = authed_get_json(app.clone(), &token, "/v1/wattetheria/agent-skills").await;
    assert_eq!(
        skills["items"].as_array().unwrap().as_slice(),
        [] as [Value; 0]
    );
    let card = authed_get_json(app, &token, "/v1/wattetheria/source-agent-card").await;
    assert_eq!(
        card["card"]["skills"].as_array().unwrap().as_slice(),
        [] as [Value; 0]
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn social_host_adapters_use_active_identity_and_swarm_bridge() {
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
    let (_dir, state, _token, _policy_engine) =
        build_test_state_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = {
        let registry = state.public_identity_registry.lock().await;
        registry
            .active_for_agent_did(&identity.agent_did)
            .expect("active public identity")
            .public_id
    };
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

    let identity_provider = WattetheriaLocalIdentityProvider::new(state.clone());
    let active_identity = identity_provider
        .active_identity()
        .await
        .expect("load active identity");
    assert_eq!(active_identity.public_id, local_public_id);
    assert_eq!(active_identity.agent_did, identity.agent_did);

    let transport = WattetheriaTransportAdapter::new(state.clone());
    transport
        .send_friend_request(
            "12D3KooRemotePeer",
            &json!({
                "counterpart_public_id": remote_public_id,
                "kind": "friend_request",
                "text": "connect with me"
            }),
        )
        .await
        .expect("send friend request");
    transport
        .send_friend_decision(
            "12D3KooRemotePeer",
            &json!({
                "counterpart_public_id": remote_public_id,
                "decision": "accept",
                "request_id": "request-1"
            }),
        )
        .await
        .expect("send friend decision");
    transport
        .send_direct_message(
            "12D3KooRemotePeer",
            &json!({
                "counterpart_public_id": remote_public_id,
                "content": {
                    "type": "text",
                    "text": "hello from adapter"
                }
            }),
        )
        .await
        .expect("send direct message");

    let relationship_commands = bridge.relationship_commands.lock().await;
    assert_eq!(relationship_commands.len(), 2);
    assert_eq!(
        relationship_commands[0].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Request
    );
    assert_eq!(
        relationship_commands[1].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Accept
    );
    assert_eq!(
        relationship_commands[0]
            .agent_envelope
            .capability
            .as_deref(),
        Some("social.friend.request")
    );
    assert_eq!(
        relationship_commands[1]
            .agent_envelope
            .capability
            .as_deref(),
        Some("social.friend.accept")
    );
    drop(relationship_commands);

    let dm_commands = bridge.dm_commands.lock().await;
    assert_eq!(dm_commands.len(), 1);
    assert_eq!(
        dm_commands[0].agent_envelope.capability.as_deref(),
        Some("social.dm.send")
    );
}
