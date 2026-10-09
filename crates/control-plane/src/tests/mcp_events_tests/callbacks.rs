use super::*;

pub(super) fn signed_envelope(
    source: &Identity,
    target: &str,
    capability: &str,
    message: Value,
) -> SwarmAgentEnvelope {
    let protocol = "google_a2a".to_owned();
    let profile = Some("wattswarm_mesh".to_owned());
    let source_id = Some(source.agent_did.clone());
    let target_id = Some(target.to_owned());
    let node_id = Some("remote-node".to_owned());
    let capability = Some(capability.to_owned());
    let message_json = serde_json::to_string(&message).unwrap();
    let signature = sign_payload(
        &ExpectedSignedAgentEnvelopePayload {
            protocol: &protocol,
            transport_profile: profile.as_ref(),
            source_agent_id: source_id.as_ref(),
            target_agent_id: target_id.as_ref(),
            source_node_id: node_id.as_ref(),
            target_node_id: None,
            capability: capability.as_ref(),
            source_agent_card_hash: None,
            message_json: &message_json,
            extensions_json: None,
        },
        source,
    )
    .unwrap();
    SwarmAgentEnvelope {
        protocol,
        transport_profile: profile,
        source_agent_id: source_id,
        target_agent_id: target_id,
        source_node_id: node_id,
        target_node_id: None,
        capability,
        source_agent_card: None,
        message,
        extensions: None,
        signature: Some(signature),
    }
}

fn seed_subscription(dir: &tempfile::TempDir, owner: &str, token: &str, event_type: &str) {
    let store = EventStore::open(
        &dir.path().join("mcp-events.sqlite"),
        &dir.path().join("mcp-events.key"),
    )
    .unwrap();
    store
        .upsert_subscription(NewSubscription {
            id: format!("subscription-{event_type}"),
            source_kind: "agent_callback".into(),
            event_type: event_type.into(),
            target_agent_id: owner.into(),
            credential_fingerprint: hex::encode(Sha256::digest(token.as_bytes())),
            owner_id: owner.into(),
            callback_url: "https://callback.example/events".into(),
            callback_secret: "test-secret".into(),
            old_callback_secret: None,
            old_secret_expires_at: None,
            canonical_arguments: b"{}".to_vec(),
            expires_at: None,
        })
        .unwrap();
}

async fn assert_signed_friend_requires_review(
    state: &ControlPlaneState,
    remote: &Identity,
    owner: &str,
) {
    let mut friend = event(owner);
    friend.event_id = "signed-friend-event".into();
    friend.event_type = "friend_request".into();
    friend.source_kind = "peer_relationship".into();
    friend.source_node_id = Some("remote-node".into());
    friend.agent_envelope = Some(signed_envelope(
        remote,
        owner,
        "social.relationship.request",
        json!({"request_id": "request-1", "source_public_id": "remote.123"}),
    ));
    friend.payload = json!({"request_id": "unsigned-must-not-project"});
    friend.allowed_actions = vec!["accept".into(), "reject".into()];
    let response = request_json(
        app(state.clone()),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(json!({"event": friend}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(response["ok"], true);
    assert!(response["decision"].is_null());
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["pendingOutbox"],
        1
    );
}

async fn assert_private_dm_defers_and_replays(
    state: &ControlPlaneState,
    remote: &Identity,
    owner: &str,
) {
    let mut dm = event(owner);
    dm.event_id = "signed-private-dm-event".into();
    dm.event_type = "topic_message_requires_reply".into();
    dm.source_kind = "topic_message".into();
    dm.source_node_id = Some("remote-node".into());
    dm.agent_envelope = Some(signed_envelope(
        remote,
        owner,
        "social.dm.send",
        json!({
            "source_public_id": "remote.123", "target_public_id": "local.456",
            "message_id": "dm-1", "thread_id": "dm:remote.123:local.456",
            "content": {"text": "private message"}, "sent_at": 10
        }),
    ));
    dm.payload = json!({
        "feed_key": "wattswarm.dm", "scope_hint": "group:dm-1", "message_id": "dm-1",
        "topic_content": {"kind": "direct_message", "text": "private message"}
    });
    dm.allowed_actions = vec!["reply".into(), "ignore".into()];
    let response = request_json(
        app(state.clone()),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(json!({"event": dm}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(response["detail"], "deferred until friendship is active");
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["pendingOutbox"],
        1
    );
    let deferred = state
        .social_store
        .get_deferred_agent_event("signed-private-dm-event")
        .unwrap()
        .unwrap();
    assert_eq!(deferred.status, "waiting_for_friendship");

    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: "friendship-dm".into(),
            local_public_id: "local.456".into(),
            remote_public_id: "remote.123".into(),
            display_name: None,
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: Some("dm:remote.123:local.456".into()),
            created_at: 20,
            updated_at: 20,
        },
    )
    .unwrap();
    assert_failed_external_dm_replay_stays_pending(state).await;
    let replayed = crate::routes::agent_events::replay_deferred_dm_agent_events_for_friendship(
        state,
        "local.456",
        "remote.123",
    )
    .await
    .unwrap();
    assert_eq!(replayed, 1);
    assert_eq!(
        state
            .social_store
            .get_deferred_agent_event("signed-private-dm-event")
            .unwrap()
            .unwrap()
            .status,
        "replayed"
    );
    assert_eq!(
        crate::routes::agent_events::replay_deferred_dm_agent_events_for_friendship(
            state,
            "local.456",
            "remote.123"
        )
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["pendingOutbox"],
        2
    );
}

#[tokio::test]
async fn signed_friend_and_private_dm_follow_business_gates_with_events_store() {
    let (dir, _router, token, mut state) = events_app().await;
    state.agent_event_mode = crate::mcp_events::AgentEventMode::McpEvents;
    let remote = Identity::new_random();
    let owner = state.agent_did.clone();
    seed_subscription(&dir, &owner, &token, "friend_request");
    seed_subscription(&dir, &owner, &token, "topic_message_requires_reply");
    state.mcp_events = Some(Arc::new(
        McpEvents::open(
            dir.path().join("mcp-events.sqlite"),
            dir.path().join("mcp-events.key"),
            owner.clone(),
            &token,
        )
        .await
        .unwrap(),
    ));
    assert_signed_friend_requires_review(&state, &remote, &owner).await;
    assert_private_dm_defers_and_replays(&state, &remote, &owner).await;
}

#[tokio::test]
async fn mcp_events_mode_pushes_every_event_to_the_env_webhook_without_any_subscription() {
    let (_dir, _router, _token, mut state) = events_app().await;
    state.agent_event_mode = crate::mcp_events::AgentEventMode::McpEvents;
    let remote = Identity::new_random();
    let owner = state.agent_did.clone();

    let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let wake_server = tokio::spawn(async move {
        let router = Router::new().route(
            "/wake",
            axum::routing::post(move |headers: HeaderMap, body: axum::body::Bytes| {
                let sender = sender.clone();
                async move {
                    let _ = sender.send((headers, body));
                    axum::http::StatusCode::OK
                }
            }),
        );
        axum::serve(listener, router).await.unwrap();
    });
    let events = state.mcp_events.clone().unwrap();
    events
        .apply_env_webhook(
            crate::mcp_events::EnvWebhookConfig::from_values(
                Some(&format!("http://{address}/wake")),
                Some("Authorization: Bearer wake-key"),
                None,
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let mut friend = event(&owner);
    friend.event_id = "friend-1".into();
    friend.event_type = "friend_request".into();
    friend.source_kind = "peer_relationship".into();
    friend.source_node_id = Some("remote-node".into());
    friend.agent_envelope = Some(signed_envelope(
        &remote,
        &owner,
        "social.relationship.request",
        json!({"request_id": "request-1", "source_public_id": "remote.123"}),
    ));
    friend.allowed_actions = vec!["accept".into(), "reject".into()];
    let mut result = event(&owner);
    result.event_id = "result-1".into();
    for event in [friend, result] {
        let response = request_json(
            app(state.clone()),
            Request::post("/agent-events")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(json!({"event": event}).to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(response["ok"], true);
        assert_eq!(response["detail"], "accepted for MCP Events");
        assert!(response["decision"].is_null());
    }
    for _ in 0..3 {
        events.delivery_tick(None).await.unwrap();
    }

    let mut names = Vec::new();
    while let Ok((headers, body)) = received.try_recv() {
        assert_eq!(headers["authorization"], "Bearer wake-key");
        assert!(headers.contains_key("webhook-signature"));
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["data"]["requires_action"], true);
        names.push(body["name"].as_str().unwrap().to_owned());
    }
    names.sort();
    assert_eq!(
        names,
        [
            "wattetheria.agent.friend_request",
            "wattetheria.agent.third_party_result"
        ]
    );
    wake_server.abort();
}

async fn assert_failed_external_dm_replay_stays_pending(state: &ControlPlaneState) {
    let mut unavailable = state.clone();
    unavailable.mcp_events = None;
    assert_eq!(
        crate::routes::agent_events::replay_deferred_dm_agent_events_for_friendship(
            &unavailable,
            "local.456",
            "remote.123"
        )
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        state
            .social_store
            .get_deferred_agent_event("signed-private-dm-event")
            .unwrap()
            .unwrap()
            .status,
        "waiting_for_friendship"
    );
}
