use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_translates_topic_dm_reply_to_wattetheria_commit() {
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
                        "content": "{\"action\":\"reply\",\"reason\":\"respond to dm\",\"payload\":{\"content\":\"signed reply\"}}"
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
    let dm_message = json!({
        "source_public_id": "peer-alpha",
        "target_public_id": "self-alpha",
        "content": "hello",
        "thread_id": "dm:self-alpha:peer-alpha"
    });
    let dm_envelope = signed_agent_event_envelope(
        &remote_identity,
        "social-node",
        Some(&local_agent_did),
        "social.dm.send",
        dm_message.clone(),
    );
    let expected_source_agent_id = dm_envelope.source_agent_id.clone();
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: "friendship-topic-dm-reply".to_owned(),
            local_public_id: "self-alpha".to_owned(),
            remote_public_id: "peer-alpha".to_owned(),
            display_name: None,
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: Some("dm:self-alpha:peer-alpha".to_owned()),
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed active friendship");
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
                        "event_id": "evt-topic-dm-1",
                        "event_type": "topic_message_requires_reply",
                        "source_kind": "topic_message",
                        "source_node_id": "social-node",
                        "target_agent_id": local_agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": dm_envelope.clone(),
                        "payload": {
                            "network_id": "mainnet:watt-etheria",
                            "feed_key": "wattswarm.dm",
                            "scope_hint": "group:dm-self-peer",
                            "message_id": "topic-msg-1",
                            "content": "hello",
                            "topic_content": {
                                "kind": "direct_message",
                                "agent_envelope": dm_envelope,
                                "content": "hello"
                            }
                        },
                        "requires_commit": false,
                        "allowed_actions": ["reply", "ignore"],
                        "correlation_id": "wattswarm.dm",
                        "dedupe_key": "topic_message:topic-msg-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(response["decision"]["action"].as_str(), Some("reply"));
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["content"].as_str(),
        Some("signed reply")
    );
    let entries = crate::diagnostics::list_diagnostics(
        &data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-topic-dm-1".to_owned()),
            phase: Some("callback.received".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    let received = entries.first().expect("callback.received diagnostic");
    let brain_input = &received.details["payload"]["brain_input"];
    assert_eq!(
        brain_input["agent_envelope"]["source_agent_id"].as_str(),
        expected_source_agent_id.as_deref()
    );
    assert!(brain_input["payload"]["agent_envelope"].is_null());
    assert!(brain_input["payload"]["topic_content"]["agent_envelope"].is_null());
    assert_eq!(
        brain_input["payload"]["topic_content"]["content"].as_str(),
        Some("hello")
    );
    assert!(brain_input["payload"]["content"].is_null());

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_reports_openai_compatible_missing_content_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {}
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
            "task_id": "task-1",
            "event_kind": "task_claimed"
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
                        "event_id": "evt-missing-content",
                        "event_type": "task_claim_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_claim_envelope.clone(),
                        "payload": {
                            "task_id": "task-1",
                            "event_kind": "task_claimed"
                        },
                        "requires_commit": false,
                        "allowed_actions": ["human_review", "decide_claim", "reject_claim"],
                        "correlation_id": "task-1",
                        "dedupe_key": "task_claim:task-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(false));
    let detail = response["detail"].as_str().expect("response detail");
    assert!(detail.contains("openai-compatible response missing content"));
    assert!(detail.contains("response_body="));
    assert!(detail.contains("\"choices\""));

    let entries = crate::diagnostics::list_diagnostics(
        &data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-missing-content".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    let failed = entries
        .iter()
        .find(|entry| entry.phase == "decision.failed")
        .expect("decision.failed diagnostic");
    assert_eq!(
        failed.details["payload"]["callback_response"]["ok"].as_bool(),
        Some(false)
    );
    assert!(
        failed.details["payload"]["error"]
            .as_str()
            .expect("decision error")
            .contains("response_body=")
    );

    server.abort();
}
