//! Lifecycle notices between agents wake the local agent; they never write a mission's
//! status, which only the gateway keeps.

use super::*;

/// A signed lifecycle notice (`mission_claim_approved` / `mission_claim_withdrawn` /
/// `mission_completed` / `mission_settled`) relayed to this node about one of its own missions, as a worker node
/// would send it, naming `claimed_as` as the claimer, which may differ from the signer.
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
        "mission_claim_withdrawn" => ("mission.claim.withdraw", "withdrawn"),
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
    state: &crate::ControlPlaneState,
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
            "description": "Relayed lifecycle notices never change a mission.",
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
    // The worker's own node asks the gateway before its claim reaches this node.
    mission_gateway::mission_gateway(state)
        .lock()
        .await
        .ask(&mission_id, &worker.agent_did);
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
async fn relayed_lifecycle_notices_never_write_a_mission_status() {
    let (_dir, router, token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let public_id = bootstrap_broker_identity(router.clone(), &token, &local_agent_did).await;
    let worker = Identity::new_random();
    let mission_id = publish_and_claim(
        &state,
        &router,
        &token,
        &public_id,
        &worker,
        "No remote writes",
    )
    .await;
    let gateway = mission_gateway::mission_gateway(&state);
    let before = gateway.lock().await.record(&mission_id).expect("mission");
    assert_eq!(before["status"].as_str(), Some("claimed"));

    // The worker, or anyone with a key, says the mission moved on: the event is accepted so
    // the agent is woken, and nothing is sent to the gateway.
    let someone = Identity::new_random();
    for (kind, claimed_as, event_id) in [
        (
            "mission_completed",
            worker.agent_did.as_str(),
            "evt-completed",
        ),
        ("mission_settled", worker.agent_did.as_str(), "evt-settled"),
        (
            "mission_claim_withdrawn",
            worker.agent_did.as_str(),
            "evt-withdrawn",
        ),
        (
            "mission_claim_approved",
            someone.agent_did.as_str(),
            "evt-claim-other",
        ),
    ] {
        let response = post_agent_event(
            router.clone(),
            relayed_lifecycle_event_claiming_as(
                &worker,
                claimed_as,
                &local_agent_did,
                &mission_id,
                kind,
                event_id,
            ),
        )
        .await;
        assert_eq!(response["ok"].as_bool(), Some(true), "{kind}: {response}");
    }

    let gateway = gateway.lock().await;
    assert_eq!(gateway.record(&mission_id).expect("mission"), before);
    for action in [
        "deliver",
        "pledge",
        "reject",
        "settle",
        "cancel",
        "reject_claim",
        "withdraw_claim",
    ] {
        assert!(gateway.commands(action).is_empty(), "{action}");
    }
}
