use super::*;

#[tokio::test]
async fn agent_events_defer_inbound_dm_until_friendship_is_active() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let local_agent_did = state.agent_did.clone();
    let agent_envelope = signed_agent_event_envelope(
        &remote_identity,
        "remote-node-1",
        Some(&local_agent_did),
        "social.dm.send",
        json!({
            "source_public_id": "agent-remote.123",
            "target_public_id": "agent-local.456",
            "message_id": "dm-message-1",
            "thread_id": "dm:agent-remote.123:agent-local.456",
            "content": {"text": "hello before friendship"},
            "sent_at": 10
        }),
    );
    let event = json!({
        "event": {
            "event_id": "evt-deferred-dm-1",
            "event_type": "topic_message_requires_reply",
            "source_kind": "topic_message",
            "source_node_id": "remote-node-1",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": agent_envelope,
            "payload": {
                "feed_key": "wattswarm.dm",
                "scope_hint": "group:dm-1",
                "message_id": "dm-message-1",
                "topic_content": {
                    "kind": "direct_message",
                    "text": "hello before friendship"
                }
            },
            "requires_commit": true,
            "allowed_actions": ["reply", "ignore"],
            "created_at": 10
        }
    });

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(response["decision"], Value::Null);
    assert_eq!(
        response["detail"].as_str(),
        Some("deferred until friendship is active")
    );
    let deferred = state
        .social_store
        .get_deferred_agent_event("evt-deferred-dm-1")
        .expect("get deferred event")
        .expect("deferred event");
    assert_eq!(deferred.status, "waiting_for_friendship");
    assert_eq!(deferred.local_public_id, "agent-local.456");
    assert_eq!(deferred.remote_public_id, "agent-remote.123");

    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: "friendship-deferred-dm".to_owned(),
            local_public_id: "agent-local.456".to_owned(),
            remote_public_id: "agent-remote.123".to_owned(),
            display_name: None,
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: Some("dm:agent-remote.123:agent-local.456".to_owned()),
            created_at: 20,
            updated_at: 20,
        },
    )
    .expect("activate friendship");

    let replayed = crate::routes::agent_events::replay_deferred_dm_agent_events_for_friendship(
        &state,
        "agent-local.456",
        "agent-remote.123",
    )
    .await
    .expect("replay deferred dm events");
    assert_eq!(replayed, 1);
    let deferred = state
        .social_store
        .get_deferred_agent_event("evt-deferred-dm-1")
        .expect("get deferred event")
        .expect("deferred event");
    assert_eq!(deferred.status, "replayed");
    assert!(deferred.replayed_at.is_some());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_rejects_legacy_dm_received() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":\"reply\",\"reason\":\"respond politely\",\"payload\":{\"content\":\"hello back\"}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let local_agent_did = state.agent_did.clone();
    let remote_identity = Identity::new_random();
    let dm_envelope = signed_agent_event_envelope(
        &remote_identity,
        "social-node",
        Some(&local_agent_did),
        "social.dm",
        json!({
            "source_public_id": "peer-alpha",
            "target_public_id": "self-alpha",
            "content": "hello"
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
                runtime_adapter: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
                runtime_adapter: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let data_dir = state.data_dir.clone();
    let app = app(state);

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-1",
                        "event_type": "dm_received",
                        "source_kind": "social",
                        "source_node_id": "social-node",
                        "target_agent_id": local_agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": dm_envelope.clone(),
                        "payload": {
                            "agent_envelope": dm_envelope
                        },
                        "requires_commit": true,
                        "allowed_actions": ["reply", "ignore"],
                        "correlation_id": "thread-1",
                        "dedupe_key": "dm:thread-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(false));
    assert_eq!(response["decision"], Value::Null);
    assert_eq!(
        response["detail"].as_str(),
        Some("unsupported action reply for event_type dm_received")
    );

    let entries = crate::diagnostics::list_diagnostics(
        &data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-1".to_owned()),
            phase: Some("decision.brain_response".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    let brain_response = entries.first().expect("decision.brain_response diagnostic");
    assert!(
        brain_response.details["payload"]["response_body"]
            .as_str()
            .expect("response body")
            .contains("\"choices\"")
    );
    assert!(
        brain_response.details["payload"]["completion_content"]
            .as_str()
            .expect("completion content")
            .contains("\"action\":\"reply\"")
    );
    assert_eq!(
        brain_response.details["payload"]["parse"]["status"].as_str(),
        Some("accepted")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_auto_commit_friend_request_accepts_relationship() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":\"accept\",\"reason\":\"trusted peer\",\"payload\":{\"request_id\":\"req-auto-accept\",\"correlation_id\":\"corr-auto-accept\",\"message\":{\"mock_transport_response\":\"queued\"}}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, router, token, _policy_engine, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let base_url = format!("http://{addr}/v1");
    let local_public_id =
        bootstrap_broker_identity(router.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-remote", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Remote".to_owned(),
                Some(remote_identity.agent_did.clone()),
                true,
            )
            .expect("upsert remote identity");
    }
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "req-auto-accept".to_owned(),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            remote_node_id: Some("remote-node".to_owned()),
            direction: wattetheria_social::domain::friend_requests::FriendRequestDirection::Inbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: Some("corr-auto-accept".to_owned()),
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        },
    )
    .expect("save inbound friend request");
    let agent_envelope = signed_agent_event_envelope(
        &remote_identity,
        "remote-node",
        Some(&identity.agent_did),
        "social.relationship.request",
        json!({
            "source_public_id": remote_public_id,
            "target_public_id": "local-node-id",
            "request_id": "req-auto-accept",
            "correlation_id": "corr-auto-accept"
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
                runtime_adapter: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
                runtime_adapter: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let app = app(state.clone());

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-friend-auto-accept",
                        "event_type": "friend_request",
                        "source_kind": "peer_relationship",
                        "source_node_id": "remote-node",
                        "target_agent_id": identity.agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": agent_envelope,
                        "payload": {},
                        "requires_commit": true,
                        "allowed_actions": ["accept", "reject", "block"],
                        "correlation_id": "corr-auto-accept",
                        "dedupe_key": "friend:req-auto-accept",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(response["decision"]["action"].as_str(), Some("accept"));
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Accept
    );
    assert_eq!(commands[0].remote_node_id, "remote-node");
    assert_eq!(
        commands[0].agent_envelope.target_node_id.as_deref(),
        Some("remote-node")
    );
    assert_eq!(
        commands[0].agent_envelope.message["source_public_id"].as_str(),
        Some(local_public_id.as_str())
    );
    assert_eq!(
        commands[0].agent_envelope.message["request_id"].as_str(),
        Some("req-auto-accept")
    );
    assert_eq!(
        commands[0].agent_envelope.message["correlation_id"].as_str(),
        Some("corr-auto-accept")
    );
    drop(commands);
    let friend_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list friend requests");
    assert_eq!(friend_requests.len(), 1);
    assert_eq!(
        friend_requests[0].state,
        wattetheria_social::domain::friend_requests::FriendRequestState::DecisionPending
    );
    assert_eq!(
        friend_requests[0].decision_reason.as_deref(),
        Some(wattetheria_social::domain::friend_requests::DECISION_PENDING_ACCEPT_REASON)
    );
    let friendships = friendship_service::list_friendships(&*state.social_store, &local_public_id)
        .expect("list friendships");
    assert_eq!(friendships.as_slice(), []);

    wattetheria_social::application::orchestration_service::reconcile_relationship_views(
        &*state.social_store,
        &local_public_id,
        &[
            wattetheria_social::application::orchestration_service::RelationshipSyncView {
                counterpart:
                    wattetheria_social::application::orchestration_service::CounterpartSnapshot {
                        counterpart_public_id: remote_public_id.clone(),
                        target_agent: remote_identity.agent_did.clone(),
                        remote_node_id: "remote-node".to_owned(),
                        known_identity: None,
                        known_binding: None,
                        observed_at: 2,
                    },
                relationship_state: "accepted".to_owned(),
                last_action: Some("accept".to_owned()),
                initiated_by: "local".to_owned(),
                request_id: Some("req-auto-accept".to_owned()),
                correlation_id: Some("corr-auto-accept".to_owned()),
                requested_at: Some(1),
                responded_at: Some(2),
                updated_at: 2,
            },
        ],
    )
    .expect("reconcile accepted relationship projection");

    let friend_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list accepted friend requests");
    assert_eq!(
        friend_requests[0].state,
        wattetheria_social::domain::friend_requests::FriendRequestState::Accepted
    );
    let friendships = friendship_service::list_friendships(&*state.social_store, &local_public_id)
        .expect("list accepted friendships");
    assert_eq!(friendships.len(), 1);
    assert_eq!(friendships[0].remote_public_id, remote_public_id);
    assert_eq!(
        friendships[0].state,
        wattetheria_social::domain::friendships::FriendshipState::Active
    );
    assert_eq!(
        friendships[0].established_from_request_id.as_deref(),
        Some("req-auto-accept")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_send_null_friend_request_action_to_human_review() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":null,\"reason\":\"needs human decision\",\"payload\":{}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, router, token, _policy_engine, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let local_public_id = bootstrap_broker_identity(router, &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-remote", &remote_identity.agent_did);
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "req-human-review".to_owned(),
            local_public_id: local_public_id.clone(),
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
    .expect("save inbound friend request");
    let agent_envelope = signed_agent_event_envelope(
        &remote_identity,
        "remote-node",
        Some(&identity.agent_did),
        "social.relationship.request",
        json!({
            "source_public_id": remote_public_id,
            "request_id": "req-human-review"
        }),
    );
    let base_url = format!("http://{addr}/v1");
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
                runtime_adapter: None,
            },
        ))),
        ..state
    };
    let response = request_json(
        app(state.clone()),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-friend-human-review",
                        "event_type": "friend_request",
                        "source_kind": "peer_relationship",
                        "source_node_id": "remote-node",
                        "target_agent_id": identity.agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": agent_envelope,
                        "payload": {},
                        "requires_commit": true,
                        "allowed_actions": ["accept", "reject", "block"],
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("human_review")
    );
    assert_eq!(response["decision"]["route"].as_str(), Some("noop"));
    assert!(bridge.relationship_commands.lock().await.is_empty());
    let requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list friend requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].state,
        wattetheria_social::domain::friend_requests::FriendRequestState::Pending
    );
    let pending = authed_get_json(
        app(state.clone()),
        &token,
        &format!("/v1/client/friend-requests?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(
        pending["items"][0]["request_id"].as_str(),
        Some("req-human-review")
    );

    server.abort();
}
