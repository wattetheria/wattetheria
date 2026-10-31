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
                "agent_watt": 0,
                "reputation": 1,
                "capacity": 0
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

#[tokio::test]
async fn agent_events_cancel_an_expired_mission_instead_of_completing_it() {
    let (_dir, router, token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let public_id = bootstrap_broker_identity(router.clone(), &token, &local_agent_did).await;
    let mission = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Expires before completion",
            "description": "A late completion must not pay out.",
            "publisher": public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {"agent_watt": 0, "reputation": 1, "capacity": 0},
            "payload": {"objective": "sync"},
            "expires_at": chrono::Utc::now().timestamp() + 3600
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().expect("mission_id");
    let worker_identity = Identity::new_random();
    authed_post_json(
        router.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({"mission_id": mission_id, "agent_did": worker_identity.agent_did}),
    )
    .await;
    {
        let mut board = state.mission_board.lock().await;
        board
            .set_expiry(mission_id, Some(chrono::Utc::now().timestamp() - 1))
            .expect("expire");
        board
            .mark_bounty_escrowed(mission_id, "escrow-late")
            .expect("escrow");
    }
    let mut events = state.stream_tx.subscribe();

    let event = late_mission_completed_event(&worker_identity, &local_agent_did, mission_id);
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
    let synced = board.get(mission_id).expect("mission tracked");
    assert_eq!(
        synced.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Cancelled
    );
    assert!(synced.completed_by.is_none());
    assert_eq!(
        synced.escrow_id.as_deref(),
        Some("escrow-late"),
        "escrow stays as a pending refund for the sweep"
    );
    drop(board);
    let mut announced = Vec::new();
    while let Ok(event) = events.try_recv() {
        if event.kind == "mission.cancelled" {
            announced.push(event.payload);
        }
    }
    assert_eq!(announced.len(), 1, "{announced:?}");
    assert_eq!(announced[0]["cancel_reason"], json!("expired"));
}

fn late_mission_completed_event(
    worker_identity: &Identity,
    local_agent_did: &str,
    mission_id: &str,
) -> serde_json::Value {
    relayed_lifecycle_event(
        worker_identity,
        local_agent_did,
        mission_id,
        "mission_completed",
        "evt-mission-completed-after-expiry",
    )
}

/// A signed lifecycle notice (`mission_claim_approved` / `mission_completed` / `mission_settled`)
/// relayed to this node about one of its own missions, as a worker node would send it.
fn relayed_lifecycle_event(
    sender: &Identity,
    local_agent_did: &str,
    mission_id: &str,
    kind: &str,
    event_id: &str,
) -> serde_json::Value {
    relayed_lifecycle_event_claiming_as(
        sender,
        &sender.agent_did,
        local_agent_did,
        mission_id,
        kind,
        event_id,
    )
}

/// Like [`relayed_lifecycle_event`] but the content names `claimed_as` as the claimer, which may
/// differ from the signer.
fn relayed_lifecycle_event_claiming_as(
    sender: &Identity,
    claimed_as: &str,
    local_agent_did: &str,
    mission_id: &str,
    kind: &str,
    event_id: &str,
) -> serde_json::Value {
    let (capability, status) = match kind {
        "mission_claim_approved" => ("mission.claim", "claimed"),
        "mission_completed" => ("mission.complete", "completed"),
        _ => ("mission.settle", "settled"),
    };
    let content = json!({
        "kind": kind,
        "mission_id": mission_id,
        "task_id": mission_id,
        "publisher_agent_did": local_agent_did,
        "claimer_agent_did": claimed_as,
        "result": {"ok": true},
        "status": status
    });
    let envelope = signed_agent_event_envelope(
        sender,
        "worker-node",
        Some(local_agent_did),
        capability,
        content.clone(),
    );
    json!({
        "event": {
            "event_id": event_id,
            "event_type": "topic_message_requires_reply",
            "source_kind": "topic_message",
            "source_node_id": "worker-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": envelope,
            "payload": {
                "feed_key": "wattetheria.missions",
                "scope_hint": format!("group:{mission_id}"),
                "message_id": format!("msg-{event_id}"),
                "content": content
            },
            "requires_commit": true,
            "allowed_actions": ["settle_mission", "ignore"],
            "created_at": 12
        }
    })
}

async fn publish_and_claim(
    router: &axum::Router,
    token: &str,
    public_id: &str,
    worker: &Identity,
    title: &str,
) -> String {
    let mission = authed_post_json(
        router.clone(),
        token,
        "/v1/wattetheria/missions",
        json!({
            "title": title,
            "description": "Relayed lifecycle notices are checked against the board.",
            "publisher": public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {"agent_watt": 0, "reputation": 1, "capacity": 0},
            "payload": {"objective": "sync"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"]
        .as_str()
        .expect("mission_id")
        .to_owned();
    authed_post_json(
        router.clone(),
        token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({"mission_id": mission_id, "agent_did": worker.agent_did}),
    )
    .await;
    mission_id
}

async fn post_agent_event(router: axum::Router, event: serde_json::Value) -> serde_json::Value {
    request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await
}

#[tokio::test]
async fn relayed_settlement_never_settles_a_locally_published_mission() {
    let (_dir, router, token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let public_id = bootstrap_broker_identity(router.clone(), &token, &local_agent_did).await;
    let worker = Identity::new_random();
    let mission_id =
        publish_and_claim(&router, &token, &public_id, &worker, "No remote settle").await;
    authed_post_json(
        router.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({"mission_id": mission_id, "agent_did": worker.agent_did}),
    )
    .await;
    {
        let mut board = state.mission_board.lock().await;
        board
            .mark_bounty_escrowed(&mission_id, "escrow-held")
            .expect("escrow");
    }

    // The worker (or anyone with a key) says "settled": the board does not settle, the escrow
    // stays, and the event itself is still accepted so the rest of the pipeline continues.
    let response = post_agent_event(
        router.clone(),
        relayed_lifecycle_event(
            &worker,
            &local_agent_did,
            &mission_id,
            "mission_settled",
            "evt-forged-settle",
        ),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    {
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).expect("mission");
        assert_eq!(
            stored.status,
            wattetheria_kernel::civilization::missions::MissionStatus::Completed
        );
        assert!(stored.settled_at.is_none());
        assert_eq!(stored.escrow_id.as_deref(), Some("escrow-held"));
    }

    // After the local settle route runs, the same notice is an idempotent echo.
    {
        let mut board = state.mission_board.lock().await;
        board
            .clear_bounty_escrow(&mission_id)
            .expect("no ledger in this test");
    }
    authed_post_json(
        router.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/settle"),
        json!({"mission_id": mission_id, "agent_did": worker.agent_did}),
    )
    .await;
    let response = post_agent_event(
        router.clone(),
        relayed_lifecycle_event(
            &worker,
            &local_agent_did,
            &mission_id,
            "mission_settled",
            "evt-echo-settle",
        ),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let board = state.mission_board.lock().await;
    let stored = board.get(&mission_id).expect("mission");
    assert_eq!(
        stored.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Settled
    );
    assert!(stored.settled_at.is_some());
}

#[tokio::test]
async fn relayed_completion_or_claim_by_another_agent_is_rejected() {
    let (_dir, router, token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let public_id = bootstrap_broker_identity(router.clone(), &token, &local_agent_did).await;
    let worker = Identity::new_random();
    let intruder = Identity::new_random();
    let mission_id = publish_and_claim(&router, &token, &public_id, &worker, "Guarded payee").await;

    for (kind, event_id) in [
        ("mission_completed", "evt-forged-complete"),
        ("mission_claim_approved", "evt-forged-claim"),
    ] {
        let response = post_agent_event(
            router.clone(),
            relayed_lifecycle_event(&intruder, &local_agent_did, &mission_id, kind, event_id),
        )
        .await;
        assert_eq!(response["ok"].as_bool(), Some(true), "{kind}");
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).expect("mission");
        assert_eq!(
            stored.status,
            wattetheria_kernel::civilization::missions::MissionStatus::Claimed,
            "{kind}"
        );
        assert_eq!(
            stored.claimed_by.as_deref(),
            Some(worker.agent_did.as_str())
        );
        assert!(stored.completed_by.is_none(), "{kind}");
    }

    // Signing as the intruder while naming the real claimer in the content is rejected too.
    let response = post_agent_event(
        router.clone(),
        relayed_lifecycle_event_claiming_as(
            &intruder,
            &worker.agent_did,
            &local_agent_did,
            &mission_id,
            "mission_completed",
            "evt-impersonated-complete",
        ),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    {
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).expect("mission");
        assert_eq!(
            stored.status,
            wattetheria_kernel::civilization::missions::MissionStatus::Claimed
        );
        assert!(stored.completed_by.is_none());
    }

    // The real claimer's completion is applied and names it as the payee.
    let response = post_agent_event(
        router.clone(),
        relayed_lifecycle_event(
            &worker,
            &local_agent_did,
            &mission_id,
            "mission_completed",
            "evt-real-complete",
        ),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let board = state.mission_board.lock().await;
    let stored = board.get(&mission_id).expect("mission");
    assert_eq!(
        stored.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Completed
    );
    assert_eq!(
        stored.completed_by.as_deref(),
        Some(worker.agent_did.as_str())
    );
}

#[tokio::test]
async fn publisher_signed_lifecycle_echo_does_not_touch_its_own_board() {
    let (_dir, router, token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let public_id = bootstrap_broker_identity(router.clone(), &token, &local_agent_did).await;
    let worker = Identity::new_random();
    let mission = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Echo of own decision",
            "description": "The publisher's own notices come back from the swarm.",
            "publisher": public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {"agent_watt": 0, "reputation": 1, "capacity": 0},
            "payload": {"objective": "sync"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"]
        .as_str()
        .expect("mission_id")
        .to_owned();

    // A claim approval the publisher itself signed, naming the worker, loops back while the
    // board is still Open: it must not claim the mission for anyone.
    let response = post_agent_event(
        router.clone(),
        node_signed_lifecycle_event(
            &state,
            &worker.agent_did,
            &mission_id,
            "mission_claim_approved",
        ),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let board = state.mission_board.lock().await;
    let stored = board.get(&mission_id).expect("mission");
    assert_eq!(
        stored.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Open
    );
    assert!(stored.claimed_by.is_none());
}

/// A lifecycle notice signed by this node's own identity (as the swarm would echo the node's
/// claim approval or settlement back to it), naming `claimed_as` as the claimer.
fn node_signed_lifecycle_event(
    state: &crate::ControlPlaneState,
    claimed_as: &str,
    mission_id: &str,
    kind: &str,
) -> serde_json::Value {
    let protocol = "google_a2a".to_owned();
    let transport_profile = Some("wattswarm_mesh".to_owned());
    let source_agent_id = Some(state.agent_did.clone());
    let source_node_id = Some("publisher-node".to_owned());
    let target_agent_id = Some(state.agent_did.clone());
    let capability = Some("mission.claim".to_owned());
    let content = json!({
        "kind": kind,
        "mission_id": mission_id,
        "task_id": mission_id,
        "publisher_agent_did": state.agent_did,
        "claimer_agent_did": claimed_as,
        "status": "claimed"
    });
    let message_json = serde_json::to_string(&content).expect("message serializes");
    let signed_payload = ExpectedSignedAgentEnvelopePayload {
        protocol: &protocol,
        transport_profile: transport_profile.as_ref(),
        source_agent_id: source_agent_id.as_ref(),
        target_agent_id: target_agent_id.as_ref(),
        source_node_id: source_node_id.as_ref(),
        target_node_id: None,
        capability: capability.as_ref(),
        source_agent_card_hash: None,
        message_json: &message_json,
        extensions_json: None,
    };
    let signature = state.sign_payload(&signed_payload).expect("node signs");
    let envelope = SwarmAgentEnvelope {
        protocol,
        transport_profile,
        source_agent_id,
        target_agent_id,
        source_node_id,
        target_node_id: None,
        capability,
        source_agent_card: None,
        message: content.clone(),
        extensions: None,
        signature: Some(signature),
    };
    json!({
        "event": {
            "event_id": "evt-own-echo",
            "event_type": "topic_message_requires_reply",
            "source_kind": "topic_message",
            "source_node_id": "publisher-node",
            "target_agent_id": state.agent_did,
            "target_executor": "core-agent",
            "agent_envelope": envelope,
            "payload": {
                "feed_key": "wattetheria.missions",
                "scope_hint": format!("group:{mission_id}"),
                "message_id": "msg-own-echo",
                "content": content
            },
            "requires_commit": true,
            "allowed_actions": ["ignore"],
            "created_at": 12
        }
    })
}
