use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_convert_approved_claim_decision_to_mission_commit() {
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
                        "content": "{\"ACTION\":\"DECIDE_CLAIM\",\"REASON\":\"claim is valid\",\"PAYLOAD\":{\"APPROVED\":true}}"
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
    let remote_identity = Identity::new_random();
    let task_claim_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.claim",
        json!({
            "task_id": "mission-1",
            "claimer_node_id": "claimer-node",
            "task_inputs": {
                "kind": "wattetheria_mission",
                "mission_id": "mission-1"
            }
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
                        "event_id": "evt-task-claim",
                        "event_type": "task_claim_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_claim_envelope.clone(),
                        "payload": {
                            "task_id": "mission-1",
                            "claimer_node_id": "claimer-node",
                            "task_inputs": {
                                "kind": "wattetheria_mission",
                                "mission_id": "mission-1"
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["human_review", "decide_claim", "reject_claim"],
                        "correlation_id": "mission-1",
                        "dedupe_key": "task_claim:mission-1",
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
        Some("claim_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_id"].as_str(),
        Some("mission-1")
    );
    assert_eq!(
        response["decision"]["payload"]["agent_did"].as_str(),
        Some(remote_identity.agent_did.as_str())
    );

    assert_claim_brain_actions(
        &data_dir,
        "evt-task-claim",
        &["decide_claim", "reject_claim", "human_review"],
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_extract_json_prefixed_claim_decision_to_mission_commit() {
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
                        "content": "json\n{\n  \"action\": \"decide_claim\",\n  \"reason\": \"auto approved\",\n  \"payload\": {\n    \"approved\": true,\n    \"mission_id\": \"mission-prefixed\",\n    \"claimer_node_id\": \"claimer-node\",\n    \"agent_did\": \"did:key:claimer\"\n  }\n}"
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
    let remote_identity = Identity::new_random();
    let task_claim_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.claim",
        json!({
            "task_id": "mission-prefixed",
            "claimer_node_id": "claimer-node",
            "task_inputs": {
                "kind": "wattetheria_mission",
                "mission_id": "mission-prefixed"
            }
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
                        "event_id": "evt-task-claim-prefixed",
                        "event_type": "task_claim_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_claim_envelope.clone(),
                        "payload": {
                            "task_id": "mission-prefixed",
                            "claimer_node_id": "claimer-node",
                            "task_inputs": {
                                "kind": "wattetheria_mission",
                                "mission_id": "mission-prefixed"
                            }
                        },
                        "requires_commit": false,
                        "allowed_actions": ["decide_claim"],
                        "correlation_id": "mission-prefixed",
                        "dedupe_key": "task_claim:mission-prefixed",
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
        Some("claim_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["agent_did"].as_str(),
        Some(remote_identity.agent_did.as_str())
    );

    let entries = crate::diagnostics::list_diagnostics(
        &data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-task-claim-prefixed".to_owned()),
            phase: Some("decision.brain_response".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        entries[0].details["payload"]["parse"]["status"].as_str(),
        Some("accepted")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_reject_claim_decision_to_wattetheria_commit() {
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
                        "content": "{\"action\":\"reject_claim\",\"reason\":\"reward requires more proof\",\"payload\":{\"mission_id\":\"mission-reject\",\"claimer_node_id\":\"claimer-node\"}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let remote_identity = Identity::new_random();
    let task_claim_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.claim",
        json!({
            "task_id": "mission-reject",
            "claimer_node_id": "claimer-node",
            "task_inputs": {
                "kind": "wattetheria_mission",
                "mission_id": "mission-reject"
            }
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
    let app = app(state);

    let response = request_json(
        app.clone(),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-task-claim-reject",
                        "event_type": "task_claim_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_claim_envelope.clone(),
                        "payload": {
                            "task_id": "mission-reject",
                            "claimer_node_id": "claimer-node",
                            "task_inputs": {
                                "kind": "wattetheria_mission",
                                "mission_id": "mission-reject"
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["human_review", "decide_claim", "reject_claim"],
                        "correlation_id": "mission-reject",
                        "dedupe_key": "task_claim:mission-reject",
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
        Some("reject_claim")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_id"].as_str(),
        Some("mission-reject")
    );
    assert_eq!(
        response["decision"]["payload"]["claimer_node_id"].as_str(),
        Some("claimer-node")
    );

    let committed = authed_post_json(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-task-claim-reject",
                "event_type": "task_claim_received",
                "source_kind": "task_lifecycle",
                "source_node_id": "claimer-node",
                "target_agent_id": null,
                "target_executor": "core-agent",
                "agent_envelope": task_claim_envelope,
                "payload": {
                    "task_id": "mission-reject",
                    "claimer_node_id": "claimer-node",
                    "task_inputs": {
                        "kind": "wattetheria_mission",
                        "mission_id": "mission-reject"
                    }
                },
                "requires_commit": true,
                "allowed_actions": ["human_review", "decide_claim", "reject_claim"],
                "correlation_id": "mission-reject",
                "dedupe_key": "task_claim:mission-reject",
                "created_at": 1
            },
            "decision": response["decision"].clone(),
        }),
    )
    .await;
    assert_eq!(committed["status"].as_str(), Some("rejected"));
    assert_eq!(committed["mission_id"].as_str(), Some("mission-reject"));

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_event_approved_claim_commit_emits_gateway_claimed_event() {
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
                        "content": "{\"ACTION\":\"DECIDE_CLAIM\",\"REASON\":\"auto approved\",\"PAYLOAD\":{\"APPROVED\": TRUE,\"DISPLAY_NAME\":\"Agent-MX1111\",\"PUBLIC_ID\":\"agent-MX1111.public\"}}"
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
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, _router, token, _policy_engine, state) =
        build_test_app_with_bridge(20, dir, identity, event_log, bridge_handle);
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
    let publisher_public_id =
        bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    let mut events = state.stream_tx.subscribe();
    let created = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Auto approve claim",
            "description": "Publisher agent approves a remote Wattswarm claim.",
            "publisher": publisher_public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {
                "agent_watt": 2,
                "reputation": 1,
                "capacity": 0,
                "treasury_share_watt": 0
            },
            "payload": {"objective": "favorite local food"}
        }),
    )
    .await;
    let mission_id = created["mission_id"].as_str().expect("mission_id");
    let published = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("publish event timeout")
        .expect("publish event");
    assert_eq!(published.kind, "mission.published");

    let remote_identity = Identity::new_random();
    let claimer_agent_did = remote_identity.agent_did.clone();
    let task_claim_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.claim",
        json!({
            "task_id": mission_id,
            "claimer_node_id": "claimer-node",
            "task_inputs": {
                "kind": "wattetheria_mission",
                "mission_id": mission_id
            }
        }),
    );
    let event = json!({
        "event_id": "evt-task-claim-e2e",
        "event_type": "task_claim_received",
        "source_kind": "task_lifecycle",
        "source_node_id": "claimer-node",
        "target_agent_id": state.agent_did,
        "target_executor": "core-agent",
        "agent_envelope": task_claim_envelope,
        "payload": {
            "task_id": mission_id,
            "claimer_node_id": "claimer-node",
            "task_inputs": {
                "kind": "wattetheria_mission",
                "mission_id": mission_id
            }
        },
        "requires_commit": true,
        "allowed_actions": ["human_review", "decide_claim", "reject_claim"],
        "correlation_id": mission_id,
        "dedupe_key": format!("task_claim:{mission_id}"),
        "created_at": 1
    });

    let callback_response = request_json(
        app.clone(),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({"event": event.clone()}).to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(callback_response["ok"].as_bool(), Some(true));
    assert_eq!(
        callback_response["decision"]["action"].as_str(),
        Some("claim_mission")
    );
    assert_eq!(
        callback_response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );

    let committed = authed_post_json(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": event,
            "decision": callback_response["decision"].clone(),
        }),
    )
    .await;
    assert_eq!(committed["status"].as_str(), Some("claimed"));
    assert_eq!(
        committed["claimed_by"].as_str(),
        Some(claimer_agent_did.as_str())
    );
    assert_eq!(
        committed["claimer_agent_did"].as_str(),
        Some(claimer_agent_did.as_str())
    );
    assert_eq!(
        committed["claimer_agent_identity"].as_str(),
        Some("Agent-MX1111")
    );
    assert_eq!(
        committed["claimer_display_name"].as_str(),
        Some("Agent-MX1111")
    );
    assert_eq!(
        committed["claimer_public_id"].as_str(),
        Some("agent-MX1111.public")
    );
    assert_eq!(
        committed["mission_lifecycle_notice"]["kind"].as_str(),
        Some("mission_claim_approved")
    );
    assert_eq!(
        committed["mission_lifecycle_notice"]["target_agent_id"].as_str(),
        Some(claimer_agent_did.as_str())
    );
    assert_eq!(
        committed["mission_lifecycle_notice"]["target_node_id"].as_str(),
        Some("claimer-node")
    );
    assert_eq!(
        committed["mission_lifecycle_notice"]["has_source_agent_card"].as_bool(),
        Some(true)
    );
    assert!(
        committed["updated_at"].as_i64().unwrap_or_default()
            >= committed["created_at"].as_i64().unwrap_or_default()
    );
    assert!(
        bridge.messages.lock().await.is_empty(),
        "ordinary mission claim approval is published through task lifecycle events, not topic messages"
    );

    let claimed = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("claim event timeout")
        .expect("claim event");
    assert_eq!(claimed.kind, "mission.claimed");
    assert_eq!(claimed.payload["mission_id"].as_str(), Some(mission_id));
    assert_eq!(claimed.payload["status"].as_str(), Some("claimed"));
    assert_eq!(
        claimed.payload["claimed_by"].as_str(),
        Some(claimer_agent_did.as_str())
    );
    assert_eq!(
        claimed.payload["claimer_display_name"].as_str(),
        Some("Agent-MX1111")
    );
    assert!(
        claimed.payload["updated_at"].as_i64().unwrap_or_default()
            >= claimed.payload["created_at"].as_i64().unwrap_or_default()
    );
    let gateway_plan =
        crate::gateway_dispatch::plan_stream_event(&claimed).expect("gateway dispatch plan");
    assert_eq!(
        gateway_plan.data_kind,
        crate::gateway_dispatch::GatewayDataKind::MissionLifecycle
    );
    assert_eq!(gateway_plan.scope.task_id.as_deref(), Some(mission_id));

    let board = state.mission_board.lock().await;
    let claimed_mission = board.get(mission_id).expect("claimed mission");
    assert_eq!(
        claimed_mission.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Claimed
    );
    assert_eq!(
        claimed_mission.claimed_by.as_deref(),
        Some(claimer_agent_did.as_str())
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_claim_approved_topic_to_complete_mission_commit() {
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
                        "content": "{\"action\":\"complete_mission\",\"reason\":\"work is ready\",\"payload\":{\"result\":{\"ok\":true,\"summary\":\"done\"}}}"
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
    let publisher_identity = Identity::new_random();
    let content = json!({
        "kind": "mission_claim_approved",
        "mission_id": "mission-approved-1",
        "task_id": "mission-approved-1",
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": "group:mission-approved-1",
        "publisher_agent_did": publisher_identity.agent_did,
        "publisher_wattswarm_node_id": "publisher-node",
        "claimer_agent_did": state.agent_did,
        "status": "approved",
        "next_action": "complete_mission"
    });
    let agent_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&state.agent_did),
        "mission.claim.approve",
        content.clone(),
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
                        "event_id": "evt-topic-claim-approved",
                        "event_type": "topic_message_requires_reply",
                        "source_kind": "topic_message",
                        "source_node_id": "publisher-node",
                        "target_agent_id": state.agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": agent_envelope,
                        "payload": {
                            "feed_key": "wattetheria.missions",
                            "scope_hint": "group:mission-approved-1",
                            "message_id": "msg-approved-1",
                            "content": content
                        },
                        "requires_commit": true,
                        "allowed_actions": ["reply"],
                        "correlation_id": "mission-approved-1",
                        "dedupe_key": "topic:mission-approved-1:approved",
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
        Some("complete_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_id"].as_str(),
        Some("mission-approved-1")
    );
    assert_eq!(
        response["decision"]["payload"]["agent_did"].as_str(),
        Some(state.agent_did.as_str())
    );
    assert_eq!(
        response["decision"]["payload"]["task_id"].as_str(),
        Some("mission-approved-1")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_scope_hint"].as_str(),
        Some("group:mission-approved-1")
    );
    assert_eq!(
        response["decision"]["payload"]["result"]["summary"].as_str(),
        Some("done")
    );

    server.abort();
}
