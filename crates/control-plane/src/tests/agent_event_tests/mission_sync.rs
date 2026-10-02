use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_sync_mission_lifecycle_to_network_claims_before_decision() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let mut claims = NetworkMissionClaimRegistry::default();
    claims.record(
        "mission-claim-sync-1",
        "mission-claim-sync-1",
        &local_agent_did,
        "exec-claim-sync-1",
        Some("network_claim_submitted".to_string()),
        NetworkMissionClaimMetadata {
            mission_feed_key: Some("wattetheria.missions".to_string()),
            mission_scope_hint: Some("group:mission-claim-sync-1".to_string()),
            ..NetworkMissionClaimMetadata::default()
        },
    );
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS,
            &claims,
        )
        .unwrap();

    let publisher_identity = Identity::new_random();
    let approved = json!({
        "kind": "mission_claim_approved",
        "mission_id": "mission-claim-sync-1",
        "task_id": "mission-claim-sync-1",
        "claimer_agent_did": local_agent_did,
        "status": "approved"
    });
    let approved_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&local_agent_did),
        "mission.claim.approve",
        approved.clone(),
    );
    let approved_event = json!({
        "event": {
            "event_id": "evt-mission-claim-approved-sync",
            "event_type": "topic_message_requires_reply",
            "source_kind": "topic_message",
            "source_node_id": "publisher-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": approved_envelope,
            "payload": {
                "feed_key": "wattetheria.missions",
                "scope_hint": "group:mission-claim-sync-1",
                "message_id": "msg-approved-sync",
                "content": approved
            },
            "requires_commit": true,
            "allowed_actions": ["complete_mission", "ignore"],
            "created_at": 10
        }
    });

    let response = request_json(
        router.clone(),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(approved_event.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let record = registry
        .records()
        .into_iter()
        .find(|record| record.mission_id == "mission-claim-sync-1")
        .expect("claim record");
    assert_eq!(record.status.as_deref(), Some("claimed"));

    let settled = json!({
        "kind": "mission_settled",
        "mission_id": "mission-claim-sync-1",
        "task_id": "mission-claim-sync-1",
        "claimer_agent_did": local_agent_did,
        "status": "settled"
    });
    let settled_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&local_agent_did),
        "mission.settle",
        settled.clone(),
    );
    let settled_event = json!({
        "event": {
            "event_id": "evt-mission-settled-sync",
            "event_type": "topic_message_requires_reply",
            "source_kind": "topic_message",
            "source_node_id": "publisher-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": settled_envelope,
            "payload": {
                "feed_key": "wattetheria.missions",
                "scope_hint": "group:mission-claim-sync-1",
                "message_id": "msg-settled-sync",
                "content": settled
            },
            "requires_commit": false,
            "allowed_actions": ["ignore"],
            "created_at": 11
        }
    });
    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(settled_event.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let record = registry
        .records()
        .into_iter()
        .find(|record| record.mission_id == "mission-claim-sync-1")
        .expect("claim record");
    assert_eq!(record.status.as_deref(), Some("settled"));
}

#[tokio::test]
async fn agent_events_sync_task_claim_decision_to_network_claims_before_decision() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let mut claims = NetworkMissionClaimRegistry::default();
    claims.record(
        "mission-claim-event-sync-1",
        "remote-task-claim-event-sync-1",
        &local_agent_did,
        "exec-claim-event-sync-1",
        Some("network_claim_submitted".to_string()),
        NetworkMissionClaimMetadata {
            mission_feed_key: Some("wattetheria.missions".to_string()),
            mission_scope_hint: Some("group:mission-claim-event-sync-1".to_string()),
            ..NetworkMissionClaimMetadata::default()
        },
    );
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS,
            &claims,
        )
        .unwrap();

    let publisher_identity = Identity::new_random();
    let claim_decision = json!({
        "approved": true,
        "task_id": "remote-task-claim-event-sync-1",
        "task_inputs": {
            "mission_id": "mission-claim-event-sync-1",
            "agent_did": local_agent_did,
        }
    });
    let claim_decision_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&local_agent_did),
        "task.claim.decision",
        claim_decision.clone(),
    );
    let claim_decision_event = json!({
        "event": {
            "event_id": "evt-task-claim-decision-sync",
            "event_type": "task_claim_decision_received",
            "source_kind": "task_lifecycle",
            "source_node_id": "publisher-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": claim_decision_envelope,
            "payload": claim_decision,
            "requires_commit": false,
            "allowed_actions": ["complete_mission", "ignore"],
            "correlation_id": "remote-task-claim-event-sync-1",
            "dedupe_key": "task_claim_decision:remote-task-claim-event-sync-1",
            "created_at": 10
        }
    });

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(claim_decision_event.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let record = registry
        .records()
        .into_iter()
        .find(|record| record.mission_id == "mission-claim-event-sync-1")
        .expect("claim record");
    assert_eq!(record.status.as_deref(), Some("claimed"));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_sync_task_completion_and_settlement_to_network_claims_before_decision() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let mission_id = "mission-claim-event-sync-2";
    let task_id = "remote-task-lifecycle-event-sync-2";
    let mut claims = NetworkMissionClaimRegistry::default();
    claims.record(
        mission_id,
        task_id,
        &local_agent_did,
        "exec-claim-event-sync-2",
        Some("claimed".to_string()),
        NetworkMissionClaimMetadata {
            mission_feed_key: Some("wattetheria.missions".to_string()),
            mission_scope_hint: Some("group:mission-claim-event-sync-2".to_string()),
            ..NetworkMissionClaimMetadata::default()
        },
    );
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS,
            &claims,
        )
        .unwrap();

    let publisher_identity = Identity::new_random();
    let completion_decision = json!({
        "approved": true,
        "retry_requested": false,
        "task_id": task_id,
        "execution_id": "exec-claim-event-sync-2",
        "task_inputs": {
            "kind": "wattetheria_mission",
            "mission_id": mission_id,
            "agent_did": local_agent_did,
        }
    });
    let completion_decision_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&local_agent_did),
        "task.completion.decision",
        completion_decision.clone(),
    );
    let completion_decision_event = json!({
        "event": {
            "event_id": "evt-task-completion-decision-sync",
            "event_type": "task_completion_decision_received",
            "source_kind": "task_lifecycle",
            "source_node_id": "publisher-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": completion_decision_envelope,
            "payload": completion_decision,
            "requires_commit": false,
            "allowed_actions": ["ignore"],
            "correlation_id": task_id,
            "dedupe_key": "task_completion_decision:remote-task-lifecycle-event-sync-2",
            "created_at": 11
        }
    });

    let response = request_json(
        router.clone(),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                completion_decision_event.to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let record = registry
        .records()
        .into_iter()
        .find(|record| record.mission_id == mission_id)
        .expect("claim record");
    assert_eq!(record.status.as_deref(), Some("completed"));

    let settlement = json!({
        "task_id": task_id,
        "execution_id": "exec-claim-event-sync-2",
        "receipt": {
            "status": "settled",
            "mission_id": mission_id,
        },
        "task_inputs": {
            "kind": "wattetheria_mission",
            "mission_id": mission_id,
            "agent_did": local_agent_did,
        }
    });
    let settlement_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&local_agent_did),
        "task.settled",
        settlement.clone(),
    );
    let settlement_event = json!({
        "event": {
            "event_id": "evt-task-settled-sync",
            "event_type": "task_settled_received",
            "source_kind": "task_lifecycle",
            "source_node_id": "publisher-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": settlement_envelope,
            "payload": settlement,
            "requires_commit": false,
            "allowed_actions": ["ignore"],
            "correlation_id": task_id,
            "dedupe_key": "task_settled:remote-task-lifecycle-event-sync-2",
            "created_at": 12
        }
    });

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(settlement_event.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let record = registry
        .records()
        .into_iter()
        .find(|record| record.mission_id == mission_id)
        .expect("claim record");
    assert_eq!(record.status.as_deref(), Some("settled"));
}

#[tokio::test]
async fn agent_events_sync_mission_completed_to_publisher_board_before_decision() {
    let (_dir, router, token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let public_id = bootstrap_broker_identity(router.clone(), &token, &local_agent_did).await;
    let mission = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Publisher sync complete",
            "description": "Publisher receives completed lifecycle topic.",
            "publisher": public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {
                "agent_watt": 2,
                "reputation": 1,
                "capacity": 0,
                "treasury_share_watt": 0
            },
            "payload": {"objective": "sync"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().expect("mission_id");
    let worker_identity = Identity::new_random();
    let _claimed = authed_post_json(
        router.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({
            "mission_id": mission_id,
            "agent_did": worker_identity.agent_did,
        }),
    )
    .await;

    let completed = json!({
        "kind": "mission_completed",
        "mission_id": mission_id,
        "task_id": mission_id,
        "publisher_agent_did": local_agent_did,
        "claimer_agent_did": worker_identity.agent_did,
        "result": {"ok": true, "summary": "done"},
        "status": "completed"
    });
    let completed_envelope = signed_agent_event_envelope(
        &worker_identity,
        "worker-node",
        Some(&local_agent_did),
        "mission.complete",
        completed.clone(),
    );
    let event = json!({
        "event": {
            "event_id": "evt-mission-completed-board-sync",
            "event_type": "topic_message_requires_reply",
            "source_kind": "topic_message",
            "source_node_id": "worker-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": completed_envelope,
            "payload": {
                "feed_key": "wattetheria.missions",
                "scope_hint": format!("group:{mission_id}"),
                "message_id": "msg-completed-sync",
                "content": completed
            },
            "requires_commit": true,
            "allowed_actions": ["settle_mission", "ignore"],
            "created_at": 12
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
    let board = state.mission_board.lock().await;
    let synced = board.get(mission_id).expect("mission synced");
    assert_eq!(
        synced.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Completed
    );
    assert_eq!(
        synced.completed_by.as_deref(),
        Some(worker_identity.agent_did.as_str())
    );
    assert_eq!(
        synced.completion_result,
        Some(json!({"ok": true, "summary": "done"}))
    );
}
