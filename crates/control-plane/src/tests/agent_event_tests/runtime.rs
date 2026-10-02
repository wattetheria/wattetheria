use super::*;

fn assert_collective_skill_gate(
    data_dir: &Path,
    event_id: &str,
    expected_status: &str,
    expected_reason: &str,
) {
    let received = callback_received_diagnostic(data_dir, event_id);
    let gate = &received["brain_input"]["payload"]["collective_participation_gate"];
    assert_eq!(gate["status"].as_str(), Some(expected_status));
    assert_eq!(gate["reason"].as_str(), Some(expected_reason));
    assert!(
        gate["message"]
            .as_str()
            .is_some_and(|message| message.contains("Collective mission participation was blocked"))
    );
}

#[tokio::test]
async fn agent_events_prefer_transport_scope_hint_for_scoped_stable_runtime_sessions() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    *state.runtime_session_mode.write().await =
        wattetheria_kernel::brain::RuntimeSessionMode::StablePerScope;
    let event_id = "evt-scoped-runtime-session-1";
    let remote_identity = Identity::new_random();
    let agent_envelope = signed_agent_event_envelope(
        &remote_identity,
        "node-source",
        Some(&state.agent_did),
        "hive.message.post",
        json!({
            "content": "hello",
            "scope_hint": "group:crew-7"
        }),
    );

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": event_id,
                        "event_type": "topic_message_requires_reply",
                        "source_kind": "topic_message",
                        "source_node_id": "node-source",
                        "target_executor": "core-agent",
                        "agent_envelope": agent_envelope,
                        "payload": {
                            "network_id": "mainnet:watt-etheria",
                            "scope_hint": "group:crew-7",
                            "content": {
                                "scope_hint": "group:message-content-must-not-route-session"
                            },
                            "message_id": "message-scoped-runtime-session-1",
                            "topic_content": "hello"
                        },
                        "requires_commit": false,
                        "allowed_actions": ["reply", "ignore"],
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true), "{response}");
    let received = callback_received_diagnostic(&state.data_dir, event_id);
    let expected_session_id = format!(
        "wattetheria:identity:{}:mainnet:watt-etheria:group:crew-7",
        state.agent_did
    );
    assert_eq!(
        received["brain_input"]["runtime_session_id"].as_str(),
        Some(expected_session_id.as_str())
    );
}

#[tokio::test]
async fn agent_events_scope_collective_topic_actions_without_changing_regular_replies() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let data_dir = state.data_dir.clone();
    let local_agent_did = state.agent_did.clone();
    let remote_identity = Identity::new_random();

    for (event_id, topic_content, expected_actions) in [
        (
            "evt-collective-topic-1",
            json!({
                "type": "collective_mission",
                "phase": "joining",
                "mission_id": "mission-collective-1",
                "run_id": "run-collective-1",
                "coordinator": {
                    "agent_did": "did:key:zCoordinator",
                    "node_id": "node-coordinator"
                }
            }),
            vec!["join_collective_mission", "ignore"],
        ),
        (
            "evt-collective-topic-2",
            json!({
                "type": "collective_mission",
                "phase": "round_started",
                "mission_id": "mission-collective-1",
                "run_id": "run-collective-1",
                "coordinator": {
                    "agent_did": "did:key:zCoordinator",
                    "node_id": "node-coordinator"
                }
            }),
            vec!["submit_collective_contribution", "ignore"],
        ),
        (
            "evt-regular-topic-1",
            json!({
                "type": "chat_message",
                "text": "hello"
            }),
            vec!["reply", "ignore"],
        ),
    ] {
        let agent_envelope = signed_agent_event_envelope(
            &remote_identity,
            "node-source",
            Some(&local_agent_did),
            "hive.message.post",
            topic_content.clone(),
        );
        let response = request_json(
            app.clone(),
            Request::post("/agent-events")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "event": {
                            "event_id": event_id,
                            "event_type": "topic_message_requires_reply",
                            "source_kind": "topic_message",
                            "source_node_id": "node-source",
                            "target_agent_id": local_agent_did.clone(),
                            "target_executor": "core-agent",
                            "agent_envelope": agent_envelope,
                            "payload": {
                                "feed_key": "wattetheria.hive",
                                "scope_hint": "group:collective-test",
                                "message_id": format!("{event_id}-message"),
                                "topic_content": topic_content
                            },
                            "requires_commit": false,
                            "allowed_actions": ["reply", "ignore"],
                            "created_at": 1
                        }
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await;
        assert_eq!(response["ok"].as_bool(), Some(true), "{response}");
        assert_claim_brain_actions(&data_dir, event_id, &expected_actions);
    }
}

#[tokio::test]
async fn agent_events_allow_collective_join_when_visible_skills_match() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let data_dir = state.data_dir.clone();
    let local_agent_did = state.agent_did.clone();
    state
        .social_store
        .upsert_agent_skill(&wattetheria_social::domain::agent_skills::AgentSkill {
            skill_id: "climate-response".to_owned(),
            name: "Climate response planning".to_owned(),
            description: "Plans heat and climate response workflows.".to_owned(),
            tags: vec!["resilience".to_owned()],
            visible: true,
            source: "manual".to_owned(),
            sort_order: 1,
            created_at: 1,
            updated_at: 1,
        })
        .expect("save visible skill");
    let remote_identity = Identity::new_random();
    let topic_content = json!({
        "type": "collective_mission",
        "phase": "joining",
        "mission_id": "mission-collective-skill-allow",
        "run_id": "run-collective-skill-allow",
        "mission": {
            "skills": ["climate response"]
        },
        "coordinator": {
            "agent_did": "did:key:zCoordinator",
            "node_id": "node-coordinator"
        }
    });
    let agent_envelope = signed_agent_event_envelope(
        &remote_identity,
        "node-source",
        Some(&local_agent_did),
        "hive.message.post",
        topic_content.clone(),
    );

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-collective-skill-allow",
                        "event_type": "topic_message_requires_reply",
                        "source_kind": "topic_message",
                        "source_node_id": "node-source",
                        "target_agent_id": local_agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": agent_envelope,
                        "payload": {
                            "feed_key": "wattetheria.hive",
                            "scope_hint": "group:collective-test",
                            "message_id": "evt-collective-skill-allow-message",
                            "topic_content": topic_content
                        },
                        "requires_commit": false,
                        "allowed_actions": ["reply", "ignore"],
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true), "{response}");
    assert_claim_brain_actions(
        &data_dir,
        "evt-collective-skill-allow",
        &["join_collective_mission", "ignore"],
    );
}

#[tokio::test]
async fn agent_events_block_collective_join_when_required_skills_do_not_match() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let data_dir = state.data_dir.clone();
    let local_agent_did = state.agent_did.clone();
    let remote_identity = Identity::new_random();
    let topic_content = json!({
        "type": "collective_mission",
        "phase": "joining",
        "mission_id": "mission-collective-skill-block",
        "run_id": "run-collective-skill-block",
        "mission": {
            "skills": ["climate response"]
        },
        "coordinator": {
            "agent_did": "did:key:zCoordinator",
            "node_id": "node-coordinator"
        }
    });
    let agent_envelope = signed_agent_event_envelope(
        &remote_identity,
        "node-source",
        Some(&local_agent_did),
        "hive.message.post",
        topic_content.clone(),
    );

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-collective-skill-block",
                        "event_type": "topic_message_requires_reply",
                        "source_kind": "topic_message",
                        "source_node_id": "node-source",
                        "target_agent_id": local_agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": agent_envelope,
                        "payload": {
                            "feed_key": "wattetheria.hive",
                            "scope_hint": "group:collective-test",
                            "message_id": "evt-collective-skill-block-message",
                            "topic_content": topic_content
                        },
                        "requires_commit": false,
                        "allowed_actions": ["reply", "ignore"],
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true), "{response}");
    assert_claim_brain_actions(&data_dir, "evt-collective-skill-block", &["ignore"]);
    assert_collective_skill_gate(
        &data_dir,
        "evt-collective-skill-block",
        "blocked",
        "required_skills_not_met",
    );
}
