use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_allows_task_result_to_settle_mission_via_commit_plane() {
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
                        "content": "{\"action\":\"settle_mission\",\"reason\":\"publisher accepted result\",\"payload\":{\"mission_id\":\"mission-1\",\"agent_did\":\"agent-worker\"}}"
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
    let task_result_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.result",
        json!({
            "task_id": "mission-1",
            "mission_id": "mission-1",
            "candidate_output": {
                "mission_id": "mission-1",
                "agent_did": "agent-worker"
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
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-task-result",
                        "event_type": "task_result_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_result_envelope,
                        "payload": {
                            "task_id": "mission-1",
                            "mission_id": "mission-1",
                            "candidate_output": {
                                "mission_id": "mission-1",
                                "agent_did": "agent-worker"
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["human_review", "settle_mission"],
                        "correlation_id": "mission-1",
                        "dedupe_key": "task_result:mission-1:cand-1",
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
        Some("settle_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn accepted_task_result_settles_using_approved_claimer_identity() {
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
                        "content": "{\"action\":\"accept_result\",\"reason\":\"publisher accepted result\",\"payload\":{}}"
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
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Accepted result identity mismatch",
            "description": "Accepted result must use the already approved claimer identity.",
            "publisher": publisher_public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {
                "agent_watt": 2,
                "reputation": 1,
                "capacity": 0,
                "treasury_share_watt": 0
            },
            "payload": {"objective": "verify accepted result identity"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().expect("mission_id");
    let claimer_identity = Identity::new_random();
    let claimer_agent_did = claimer_identity.agent_did.clone();
    let claimed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({
            "mission_id": mission_id,
            "agent_did": claimer_agent_did,
            "task_id": mission_id,
            "claim_route": {
                "agent_event_payload": {
                    "claimer_node_id": "claimer-node"
                }
            }
        }),
    )
    .await;
    assert_eq!(claimed["status"].as_str(), Some("claimed"));
    assert_eq!(
        claimed["claimed_by"].as_str(),
        Some(claimer_agent_did.as_str())
    );

    let mismatched_result_agent = "wrong-result-agent";
    let task_result_payload = json!({
        "task_id": mission_id,
        "mission_id": mission_id,
        "candidate_id": "cand-accepted",
        "candidate_output": {
            "kind": "wattetheria_mission_result",
            "mission_id": mission_id,
            "agent_did": mismatched_result_agent,
            "result": {"ok": true}
        }
    });
    let task_result_envelope = signed_agent_event_envelope(
        &claimer_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.result",
        task_result_payload.clone(),
    );
    let event = json!({
        "event_id": "evt-task-result-accepted-claimed-by",
        "event_type": "task_result_received",
        "source_kind": "task_lifecycle",
        "source_node_id": "claimer-node",
        "target_agent_id": state.agent_did,
        "target_executor": "core-agent",
        "agent_envelope": task_result_envelope,
        "payload": task_result_payload,
        "requires_commit": true,
        "allowed_actions": ["human_review", "accept_result", "reject_result", "request_retry"],
        "correlation_id": mission_id,
        "dedupe_key": format!("task_result:{mission_id}:cand-accepted"),
        "created_at": 1
    });

    let response = request_json(
        app.clone(),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({"event": event.clone()}).to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("settle_mission")
    );
    assert_eq!(
        response["decision"]["payload"]["agent_did"].as_str(),
        Some(mismatched_result_agent)
    );

    let committed = authed_post_json(
        app.clone(),
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": event,
            "decision": response["decision"].clone(),
        }),
    )
    .await;
    assert_eq!(committed["status"].as_str(), Some("settled"));
    assert_eq!(
        committed["claimed_by"].as_str(),
        Some(claimer_agent_did.as_str())
    );
    assert_eq!(
        committed["completed_by"].as_str(),
        Some(claimer_agent_did.as_str())
    );

    let board = state.mission_board.lock().await;
    let settled_mission = board.get(mission_id).expect("settled mission");
    assert_eq!(
        settled_mission.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Settled
    );
    assert_eq!(
        settled_mission.claimed_by.as_deref(),
        Some(claimer_agent_did.as_str())
    );
    assert_eq!(
        settled_mission.completed_by.as_deref(),
        Some(claimer_agent_did.as_str())
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_action_commit_settles_mission_completed_topic_without_candidate_finalize() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy_engine, state) =
        build_test_app_with_bridge(20, dir, identity, event_log, bridge_handle);
    let publisher_public_id =
        bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    let worker_identity = Identity::new_random();
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Ordinary mission completed topic",
            "description": "Publisher settles an ordinary mission lifecycle topic.",
            "publisher": publisher_public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {
                "agent_watt": 2,
                "reputation": 1,
                "capacity": 0,
                "treasury_share_watt": 0
            },
            "payload": {"objective": "ordinary mission"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().expect("mission_id");
    let _claimed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({
            "mission_id": mission_id,
            "agent_did": worker_identity.agent_did,
        }),
    )
    .await;
    let _completed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({
            "mission_id": mission_id,
            "agent_did": worker_identity.agent_did,
            "result": {"ok": true, "summary": "done"}
        }),
    )
    .await;
    bridge.messages.lock().await.clear();

    let content = json!({
        "kind": "mission_completed",
        "mission_id": mission_id,
        "task_id": mission_id,
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": format!("group:{mission_id}"),
        "publisher_agent_did": state.agent_did,
        "claimer_agent_did": worker_identity.agent_did,
        "agent_did": worker_identity.agent_did,
        "result": {"ok": true, "summary": "done"},
        "status": "completed",
        "next_action": "settle_mission"
    });
    let agent_envelope = signed_agent_event_envelope(
        &worker_identity,
        "worker-node",
        Some(&state.agent_did),
        "mission.complete",
        content.clone(),
    );
    let committed = authed_post_json(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-topic-mission-completed-commit",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": "worker-node",
                "target_agent_id": state.agent_did,
                "agent_envelope": agent_envelope,
                "payload": {
                    "feed_key": "wattetheria.missions",
                    "scope_hint": format!("group:{mission_id}"),
                    "message_id": "msg-completed-commit",
                    "content": content
                },
                "allowed_actions": ["settle_mission", "ignore"],
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-topic-mission-completed-settle",
                "action": "settle_mission",
                "route": "wattetheria_commit",
                "payload": {}
            }
        }),
    )
    .await;

    assert_eq!(committed["status"].as_str(), Some("settled"));
    assert_eq!(
        committed["completed_by"].as_str(),
        Some(worker_identity.agent_did.as_str())
    );
    assert!(committed.get("swarm_finalize").is_none());
    assert!(committed.get("candidate_id").is_none());
    assert_eq!(
        committed["mission_lifecycle_notice"]["kind"].as_str(),
        Some("mission_settled")
    );

    assert!(
        bridge.messages.lock().await.is_empty(),
        "ordinary mission completion and settlement use task lifecycle events, not topic messages"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_mission_completed_topic_to_settle_mission_commit() {
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
                        "content": "{\"action\":\"settle_mission\",\"reason\":\"ordinary mission result accepted\",\"payload\":{}}"
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
    let claimer_identity = Identity::new_random();
    let content = json!({
        "kind": "mission_completed",
        "mission_id": "mission-completed-1",
        "task_id": "mission-completed-1",
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": "group:mission-completed-1",
        "publisher_agent_did": state.agent_did,
        "publisher_wattswarm_node_id": "publisher-node",
        "claimer_agent_did": claimer_identity.agent_did,
        "agent_did": claimer_identity.agent_did,
        "result": {"ok": true, "summary": "done"},
        "status": "completed",
        "next_action": "settle_mission"
    });
    let agent_envelope = signed_agent_event_envelope(
        &claimer_identity,
        "claimer-node",
        Some(&state.agent_did),
        "mission.complete",
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
                        "event_id": "evt-topic-mission-completed",
                        "event_type": "topic_message_requires_reply",
                        "source_kind": "topic_message",
                        "source_node_id": "claimer-node",
                        "target_agent_id": state.agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": agent_envelope,
                        "payload": {
                            "feed_key": "wattetheria.missions",
                            "scope_hint": "group:mission-completed-1",
                            "message_id": "msg-completed-1",
                            "content": content
                        },
                        "requires_commit": true,
                        "allowed_actions": ["reply"],
                        "correlation_id": "mission-completed-1",
                        "dedupe_key": "topic:mission-completed-1:completed",
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
        Some("settle_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_id"].as_str(),
        Some("mission-completed-1")
    );
    assert_eq!(
        response["decision"]["payload"]["agent_did"].as_str(),
        Some(claimer_identity.agent_did.as_str())
    );
    assert_eq!(
        response["decision"]["payload"]["task_id"].as_str(),
        Some("mission-completed-1")
    );
    assert!(
        response["decision"]["payload"]
            .get("candidate_id")
            .is_none()
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_convert_accept_result_to_settle_mission_commit() {
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
                        "content": "{\"action\":\"accept_result\",\"reason\":\"result is acceptable\",\"payload\":{}}"
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
    let task_result_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.result",
        json!({
            "task_id": "mission-1",
            "candidate_output": {
                "kind": "wattetheria_mission_result",
                "mission_id": "mission-1",
                "agent_did": "agent-worker"
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
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-task-result-accept",
                        "event_type": "task_result_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_result_envelope,
                        "payload": {
                            "task_id": "mission-1",
                            "candidate_output": {
                                "kind": "wattetheria_mission_result",
                                "mission_id": "mission-1",
                                "agent_did": "agent-worker"
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["human_review", "accept_result"],
                        "correlation_id": "mission-1",
                        "dedupe_key": "task_result:mission-1:cand-1",
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
        Some("settle_mission")
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
        Some("agent-worker")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_reject_result_decision_to_wattetheria_commit() {
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
                        "content": "{\"action\":\"reject_result\",\"reason\":\"result needs proof\",\"payload\":{\"candidate_id\":\"cand-reject\"}}"
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
    let task_result_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.result",
        json!({
            "task_id": "mission-result-reject",
            "candidate_id": "cand-reject",
            "candidate_output": {
                "kind": "wattetheria_mission_result",
                "mission_id": "mission-result-reject",
                "agent_did": "agent-worker"
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
    let event = json!({
        "event_id": "evt-task-result-reject",
        "event_type": "task_result_received",
        "source_kind": "task_lifecycle",
        "source_node_id": "claimer-node",
        "target_agent_id": null,
        "target_executor": "core-agent",
        "agent_envelope": task_result_envelope,
        "payload": {
            "task_id": "mission-result-reject",
            "candidate_id": "cand-reject",
            "candidate_output": {
                "kind": "wattetheria_mission_result",
                "mission_id": "mission-result-reject",
                "agent_did": "agent-worker"
            }
        },
        "requires_commit": true,
        "allowed_actions": ["human_review", "accept_result", "reject_result", "request_retry"],
        "correlation_id": "mission-result-reject",
        "dedupe_key": "task_result:mission-result-reject:cand-reject",
        "created_at": 1
    });

    let response = request_json(
        app.clone(),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({"event": event.clone()}).to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("reject_result")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );

    let committed = authed_post_json(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": event,
            "decision": response["decision"].clone(),
        }),
    )
    .await;
    assert_eq!(committed["status"].as_str(), Some("rejected"));
    assert_eq!(
        committed["mission_id"].as_str(),
        Some("mission-result-reject")
    );
    assert_eq!(committed["candidate_id"].as_str(), Some("cand-reject"));

    server.abort();
}
