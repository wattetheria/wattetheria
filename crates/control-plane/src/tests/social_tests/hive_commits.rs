use super::*;

#[tokio::test]
async fn agent_action_commit_requires_local_hive_subscription_for_topic_reply() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity, event_log, bridge_handle);

    let committed = authed_post_json_with_headers(
        app.clone(),
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-topic-1",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": "12D3KooRemotePeer",
                "payload": {
                    "network_id": "local:test",
                    "feed_key": "crew.chat",
                    "scope_hint": "group:crew-7",
                    "message_id": "msg-remote-1"
                },
                "requires_commit": false
            },
            "decision": {
                "decision_id": "dec-topic-1",
                "action": "reply",
                "route": "wattetheria_commit",
                "payload": {
                    "content": {
                        "kind": "message",
                        "text": "roger that"
                    }
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-topic-1"),
            ("x-agent-decision-id", "dec-topic-1"),
        ],
    )
    .await;

    assert_eq!(
        committed["error"].as_str(),
        Some("hive subscription required")
    );
    assert!(bridge.messages.lock().await.is_empty());
}

#[tokio::test]
async fn agent_action_commit_skips_duplicate_topic_reply_by_reply_to_message_id() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    bridge.messages.lock().await.push(SwarmTopicMessageView {
        message_id: "msg-existing-reply".to_string(),
        network_id: "local:test".to_string(),
        feed_key: "crew.chat".to_string(),
        scope_hint: "group:crew-7".to_string(),
        author_node_id: identity.agent_did.clone(),
        agent_envelope: None,
        content: json!({"text": "already replied through MCP"}),
        reply_to_message_id: Some("msg-remote-1".to_string()),
        created_at: 1,
    });

    let committed = authed_post_json_with_headers(
        app.clone(),
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-hive-topic-replayed",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": "12D3KooRemotePeer",
                "payload": {
                    "network_id": "local:test",
                    "feed_key": "crew.chat",
                    "scope_hint": "group:crew-7",
                    "message_id": "msg-remote-1"
                },
                "requires_commit": false
            },
            "decision": {
                "decision_id": "dec-hive-topic-replayed",
                "action": "reply",
                "route": "wattetheria_commit",
                "payload": {
                    "content": "this should not be posted again"
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-hive-topic-replayed"),
            ("x-agent-decision-id", "dec-hive-topic-replayed"),
        ],
    )
    .await;

    assert_eq!(committed["ok"].as_bool(), Some(true));
    assert_eq!(committed["source"].as_str(), Some("topic_reply_dedupe"));
    assert_eq!(bridge.messages.lock().await.len(), 1);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_action_commit_routes_registered_hive_topic_reply_with_signed_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let mut events = state.stream_tx.subscribe();

    {
        let mut hives = state.hive_registry.lock().await;
        hives.upsert_hive(wattetheria_kernel::civilization::topics::TopicCreateSpec {
            network_id: None,
            feed_key: "private.hive".to_string(),
            scope_hint: "group:private-room-1".to_string(),
            display_name: "Private Room".to_string(),
            summary: None,
            projection_kind:
                wattetheria_kernel::civilization::topics::TopicProjectionKind::ChatRoom,
            organization_id: None,
            mission_id: None,
            participant_public_ids: Vec::new(),
            created_by_public_id: "local-public".to_string(),
            why_this_exists: None,
            public_geo: None,
            active: true,
        });
    }

    let committed = authed_post_json_with_headers(
        app.clone(),
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-hive-topic-1",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": "12D3KooRemotePeer",
                "payload": {
                    "network_id": "mainnet:watt-etheria",
                    "feed_key": "private.hive",
                    "scope_hint": "group:private-room-1",
                    "message_id": "private-topic-msg-1"
                },
                "requires_commit": false
            },
            "decision": {
                "decision_id": "dec-hive-topic-1",
                "action": "reply",
                "route": "wattetheria_commit",
                "payload": {
                    "content": "signed hive reply"
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-hive-topic-1"),
            ("x-agent-decision-id", "dec-hive-topic-1"),
        ],
    )
    .await;

    assert_eq!(committed["ok"].as_bool(), Some(true));
    let messages = bridge.messages.lock().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].feed_key, "private.hive");
    assert_eq!(
        messages[0].reply_to_message_id.as_deref(),
        Some("private-topic-msg-1")
    );
    let agent_envelope = messages[0]
        .agent_envelope
        .as_ref()
        .expect("registered hive topic reply should carry a signed agent envelope");
    assert_eq!(
        agent_envelope.capability.as_deref(),
        Some("hive.message.post")
    );
    assert_eq!(
        agent_envelope.message["hive_id"].as_str(),
        Some("private.hive@group:private-room-1")
    );
    assert_eq!(
        agent_envelope.message["payload"]["content"].as_str(),
        Some("signed hive reply")
    );
    assert_envelope_signature_valid(agent_envelope, &state.identity.public_key);
    drop(messages);

    let event = events.recv().await.expect("topic message stream event");
    assert_eq!(event.kind, "topic.message.posted");
    assert_eq!(
        event.payload["topic_id"].as_str(),
        Some("private.hive@group:private-room-1")
    );
    assert!(
        event.payload["message_id"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    let gateway_plan =
        crate::gateway_dispatch::plan_stream_event(&event).expect("gateway hive activity plan");
    assert_eq!(
        gateway_plan.data_kind,
        crate::gateway_dispatch::GatewayDataKind::HiveActivity
    );
    assert_eq!(
        gateway_plan.scope.topic_id.as_deref(),
        Some("private.hive@group:private-room-1")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_action_commit_routes_collective_join_through_signed_dm_only() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let committed = authed_post_json_with_headers(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-collective-join-1",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": "12D3KooHivePeer",
                "payload": {
                    "network_id": "mainnet:watt-etheria",
                    "feed_key": "public.hive",
                    "scope_hint": "group:collective-room-1",
                    "message_id": "collective-topic-msg-join-1",
                    "topic_content": {
                        "type": "collective_mission",
                        "phase": "joining",
                        "mission_id": "mission-collective-1",
                        "run_id": "run-collective-1",
                        "mission": {
                            "title": "Quick German Economy Signal Check",
                            "description": "Collect short observations about the German economy.",
                            "domain": "wealth"
                        },
                        "coordinator": {
                            "agent_did": "did:key:zCoordinator",
                            "node_id": "12D3KooCoordinator"
                        },
                        "contact_material": {
                            "material_json": "{\"node_id\":\"12D3KooCoordinator\",\"encryption\":{\"private_message\":{\"public_key_b64\":\"coordinator-key\"}}}",
                            "signature": "coordinator-signature",
                            "generated_at": 1
                        }
                    }
                },
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-collective-join-1",
                "action": "join_collective_mission",
                "route": "wattetheria_commit",
                "payload": {
                    "mission_id": "mission-collective-1",
                    "run_id": "run-collective-1",
                    "note": "joining"
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-collective-join-1"),
            ("x-agent-decision-id", "dec-collective-join-1"),
        ],
    )
    .await;

    assert_eq!(committed["ok"].as_bool(), Some(true));
    assert_eq!(
        committed["action_type"].as_str(),
        Some("collective.participation.join")
    );
    assert_eq!(committed["contact_material_saved"].as_bool(), Some(true));
    assert!(bridge.messages.lock().await.is_empty());
    let dm_commands = bridge.dm_commands.lock().await;
    assert_eq!(dm_commands.len(), 1);
    assert_eq!(dm_commands[0].remote_node_id, "12D3KooCoordinator");
    assert_eq!(
        dm_commands[0].agent_envelope.capability.as_deref(),
        Some("collective.participation.join")
    );
    assert_eq!(
        dm_commands[0].content["type"].as_str(),
        Some("collective_participation")
    );
    assert_eq!(dm_commands[0].content["status"].as_str(), Some("join"));
    assert_eq!(
        dm_commands[0].content["mission_title"].as_str(),
        Some("Quick German Economy Signal Check")
    );
    assert_eq!(
        dm_commands[0].content["mission_domain"].as_str(),
        Some("wealth")
    );
    assert_envelope_signature_valid(&dm_commands[0].agent_envelope, &state.identity.public_key);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_action_commit_routes_collective_contribution_through_signed_dm_only() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let committed = authed_post_json_with_headers(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-collective-contribution-1",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": "12D3KooHivePeer",
                "payload": {
                    "network_id": "mainnet:watt-etheria",
                    "feed_key": "public.hive",
                    "scope_hint": "group:collective-room-1",
                    "message_id": "collective-topic-msg-round-1",
                    "topic_content": {
                        "type": "collective_mission",
                        "phase": "round_started",
                        "mission_id": "mission-collective-1",
                        "run_id": "run-collective-1",
                        "mission": {
                            "title": "Quick German Economy Signal Check",
                            "description": "Collect short observations about the German economy.",
                            "domain": "wealth"
                        },
                        "coordinator": {
                            "agent_did": "did:key:zCoordinator",
                            "node_id": "12D3KooCoordinator"
                        },
                        "contact_material": {
                            "material_json": "{\"node_id\":\"12D3KooCoordinator\",\"encryption\":{\"private_message\":{\"public_key_b64\":\"coordinator-key\"}}}",
                            "signature": "coordinator-signature",
                            "generated_at": 1
                        }
                    }
                },
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-collective-contribution-1",
                "action": "submit_collective_contribution",
                "route": "wattetheria_commit",
                "payload": {
                    "mission_id": "mission-collective-1",
                    "run_id": "run-collective-1",
                    "content": {
                        "text": "One concise German economy signal."
                    }
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-collective-contribution-1"),
            (
                "x-agent-decision-id",
                "dec-collective-contribution-1",
            ),
        ],
    )
    .await;

    assert_eq!(committed["ok"].as_bool(), Some(true));
    assert_eq!(
        committed["action_type"].as_str(),
        Some("collective.contribution.submit")
    );
    assert_eq!(committed["contact_material_saved"].as_bool(), Some(true));
    assert!(bridge.messages.lock().await.is_empty());
    let dm_commands = bridge.dm_commands.lock().await;
    assert_eq!(dm_commands.len(), 1);
    assert_eq!(dm_commands[0].remote_node_id, "12D3KooCoordinator");
    assert_eq!(
        dm_commands[0].agent_envelope.capability.as_deref(),
        Some("collective.contribution.submit")
    );
    assert_eq!(
        dm_commands[0].content["type"].as_str(),
        Some("collective_contribution")
    );
    assert_eq!(dm_commands[0].content["status"].as_str(), Some("submitted"));
    assert_eq!(
        dm_commands[0].content["result"]["text"].as_str(),
        Some("One concise German economy signal.")
    );
    assert_eq!(
        dm_commands[0].content["mission_title"].as_str(),
        Some("Quick German Economy Signal Check")
    );
    assert_envelope_signature_valid(&dm_commands[0].agent_envelope, &state.identity.public_key);
}

#[tokio::test]
async fn agent_action_commit_rejects_collective_contribution_before_round_started() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let committed = authed_post_json_with_headers(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-collective-contribution-early",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": "12D3KooHivePeer",
                "payload": {
                    "topic_content": {
                        "type": "collective_mission",
                        "phase": "joining",
                        "mission_id": "mission-collective-early",
                        "run_id": "run-collective-early"
                    }
                },
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-collective-contribution-early",
                "action": "submit_collective_contribution",
                "route": "wattetheria_commit",
                "payload": {
                    "content": {
                        "text": "This result is too early."
                    }
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-collective-contribution-early"),
            ("x-agent-decision-id", "dec-collective-contribution-early"),
        ],
    )
    .await;

    assert_eq!(
        committed["error"].as_str(),
        Some("collective contribution requires a round_started collective mission")
    );
}

#[tokio::test]
async fn agent_action_commit_blocks_collective_join_when_required_skills_do_not_match() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity, event_log, bridge_handle);

    let committed = authed_post_json_with_headers(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-collective-join-skill-block",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": "12D3KooHivePeer",
                "payload": {
                    "network_id": "mainnet:watt-etheria",
                    "feed_key": "public.hive",
                    "scope_hint": "group:collective-room-1",
                    "message_id": "collective-topic-msg-join-skill-block",
                    "topic_content": {
                        "type": "collective_mission",
                        "phase": "joining",
                        "mission_id": "mission-collective-skill-block",
                        "run_id": "run-collective-skill-block",
                        "mission": {
                            "skills": ["climate response"]
                        },
                        "coordinator": {
                            "agent_did": "did:key:zCoordinator",
                            "node_id": "12D3KooCoordinator"
                        }
                    }
                },
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-collective-join-skill-block",
                "action": "join_collective_mission",
                "route": "wattetheria_commit",
                "payload": {
                    "mission_id": "mission-collective-skill-block",
                    "run_id": "run-collective-skill-block"
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-collective-join-skill-block"),
            ("x-agent-decision-id", "dec-collective-join-skill-block"),
        ],
    )
    .await;

    assert_eq!(
        committed["error"].as_str(),
        Some("collective participation blocked by required skills")
    );
    assert_eq!(
        committed["collective_participation_gate"]["status"].as_str(),
        Some("blocked")
    );
    assert!(bridge.dm_commands.lock().await.is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_action_commit_routes_private_topic_reply_through_signed_dm() {
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
    let remote_node_id = "12D3KooRemotePeer".to_string();
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
            Some(remote_node_id.clone()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
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
            thread_id: Some("dm:test-thread".to_string()),
            created_at: 1,
            updated_at: 2,
        },
    )
    .expect("seed active friendship");

    let committed = authed_post_json_with_headers(
        app.clone(),
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-topic-dm-commit-1",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": remote_node_id,
                "agent_envelope": {
                    "protocol": "google_a2a",
                    "source_agent_id": remote_identity.agent_did,
                    "target_agent_id": identity.agent_did,
                    "source_node_id": "12D3KooRemotePeer",
                    "capability": "social.dm.send",
                    "message": {
                        "source_public_id": remote_public_id,
                        "target_public_id": local_public_id,
                        "thread_id": "dm:test-thread",
                        "message_id": "remote-msg-1",
                        "content": "hello"
                    }
                },
                "payload": {
                    "network_id": "mainnet:watt-etheria",
                    "feed_key": "wattswarm.dm",
                    "scope_hint": "group:dm-test-thread",
                    "message_id": "topic-msg-1",
                    "topic_content": {
                        "kind": "direct_message"
                    }
                },
                "requires_commit": false
            },
            "decision": {
                "decision_id": "dec-topic-dm-1",
                "action": "reply",
                "route": "wattetheria_commit",
                "payload": {
                    "content": "auto signed reply"
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-topic-dm-commit-1"),
            ("x-agent-decision-id", "dec-topic-dm-1"),
        ],
    )
    .await;

    assert!(
        committed["ok"].as_bool() == Some(true),
        "commit response: {committed}"
    );
    let dm_commands = bridge.dm_commands.lock().await;
    assert_eq!(dm_commands.len(), 1);
    assert_eq!(dm_commands[0].remote_node_id, "12D3KooRemotePeer");
    assert_eq!(
        dm_commands[0].agent_envelope.capability.as_deref(),
        Some("social.dm.send")
    );
    assert_eq!(
        dm_commands[0].agent_envelope.message["source_public_id"].as_str(),
        Some(local_public_id.as_str())
    );
    assert_eq!(
        dm_commands[0].agent_envelope.message["target_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
    assert_eq!(
        dm_commands[0].agent_envelope.message["reply_to_message_id"].as_str(),
        Some("remote-msg-1")
    );
    assert_eq!(dm_commands[0].content.as_str(), Some("auto signed reply"));
    assert_envelope_signature_valid(&dm_commands[0].agent_envelope, &state.identity.public_key);
    let thread_id = dm_commands[0].agent_envelope.message["thread_id"]
        .as_str()
        .expect("outbound dm thread id")
        .to_owned();
    drop(dm_commands);

    let messages = message_service::list_thread_messages(&*state.social_store, &thread_id)
        .expect("list persisted dm messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content_json.as_str(), Some("auto signed reply"));
    assert_eq!(
        messages[0].direction,
        wattetheria_social::domain::messages::MessageDirection::Outbound
    );

    let deduped = authed_post_json_with_headers(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-topic-dm-commit-2",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": remote_node_id,
                "agent_envelope": {
                    "protocol": "google_a2a",
                    "source_agent_id": remote_identity.agent_did,
                    "target_agent_id": identity.agent_did,
                    "source_node_id": "12D3KooRemotePeer",
                    "capability": "social.dm.send",
                    "message": {
                        "source_public_id": remote_public_id,
                        "target_public_id": local_public_id,
                        "thread_id": "dm:test-thread",
                        "message_id": "remote-msg-1",
                        "content": "hello"
                    }
                },
                "payload": {
                    "network_id": "mainnet:watt-etheria",
                    "feed_key": "wattswarm.dm",
                    "scope_hint": "group:dm-test-thread",
                    "message_id": "topic-msg-2",
                    "topic_content": {
                        "kind": "direct_message"
                    }
                },
                "requires_commit": false
            },
            "decision": {
                "decision_id": "dec-topic-dm-2",
                "action": "reply",
                "route": "wattetheria_commit",
                "payload": {
                    "content": "auto signed reply"
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-topic-dm-commit-2"),
            ("x-agent-decision-id", "dec-topic-dm-2"),
        ],
    )
    .await;

    assert_eq!(deduped["source"].as_str(), Some("agent_dm_reply_dedupe"));
    assert_eq!(
        deduped["reply_to_message_id"].as_str(),
        Some("remote-msg-1")
    );
    let dm_commands = bridge.dm_commands.lock().await;
    assert_eq!(dm_commands.len(), 1);
    drop(dm_commands);
    let messages = message_service::list_thread_messages(&*state.social_store, &thread_id)
        .expect("list persisted dm messages after dedupe");
    assert_eq!(messages.len(), 1);
}
