use super::*;
use crate::routes::agent_events::AgentEventEnvelope;
use crate::routes::board::BOARD_BATTLE_FEED_KEY;

/// A gateway that shows one match, `cm_1`, as `game`; any other id is unknown.
async fn spawn_gateway(game: Value) -> String {
    let router = axum::Router::new().route(
        "/api/cards/matches/{match_id}",
        axum::routing::get(
            move |axum::extract::Path(match_id): axum::extract::Path<String>| {
                let game = game.clone();
                async move {
                    if match_id == "cm_1" {
                        (StatusCode::OK, Json(game))
                    } else {
                        (
                            StatusCode::NOT_FOUND,
                            Json(json!({"error": "unknown", "message": "no match"})),
                        )
                    }
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    url
}

/// An invite sent by `from` that waits for `to`.
fn invite(from: &str, to: &str) -> Value {
    json!({
        "match_id": "cm_1",
        "status": "invited",
        "challenger": {"agent_did": from, "label": "Alice", "wins": 0},
        "challenged": {"agent_did": to, "label": "Bob", "wins": 0},
        "rounds": [],
        "pending_step": "challenge",
        "waiting_for": [to],
    })
}

/// A battle notice about `step` of `match_id`, posted by `source` mentioning `target`.
fn notice_event(
    source: &Identity,
    target: &str,
    match_id: &str,
    step: &str,
    event_id: &str,
) -> AgentEventEnvelope {
    let notice = json!({"kind": "card_battle", "match_id": match_id, "step": step,
        "text": "Alice invites you to a card match."});
    let envelope = signed_agent_event_envelope(
        source,
        "remote-node",
        Some(target),
        "board.message.publish",
        json!({
            "feed_key": BOARD_BATTLE_FEED_KEY, "scope_hint": "group:board-battle",
            "network_id": "local:test", "action": "message.post",
            "payload": {"category": "battle", "message": notice, "mention": target},
        }),
    );
    AgentEventEnvelope {
        event_id: event_id.to_owned(),
        event_type: "topic_message_requires_reply".to_owned(),
        source_kind: "topic_message".to_owned(),
        source_node_id: Some("remote-node".to_owned()),
        target_agent_id: None,
        target_executor: None,
        agent_envelope: Some(envelope),
        payload: json!({
            "network_id": "local:test", "feed_key": BOARD_BATTLE_FEED_KEY,
            "scope_hint": "group:board-battle", "content": notice, "topic_content": notice,
            "message_id": "remote-message",
        }),
        requires_commit: false,
        allowed_actions: vec!["reply".to_owned(), "ignore".to_owned()],
        correlation_id: None,
        dedupe_key: None,
        created_at: 1,
    }
}

async fn callback(state: &ControlPlaneState, event: &AgentEventEnvelope) -> (StatusCode, Value) {
    let (status, text) = request_text(
        app(state.clone()),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(json!({"event": event}).to_string()))
            .unwrap(),
    )
    .await;
    (status, serde_json::from_str(&text).unwrap())
}

#[tokio::test]
async fn a_battle_notice_the_gateway_bears_out_reaches_the_agent_once_with_the_match() {
    let (dir, _app, _, _, state) = build_test_app(100);
    let alice = Identity::new_random();
    let state = ControlPlaneState {
        gateway_urls: vec![spawn_gateway(invite(&alice.agent_did, &state.agent_did)).await],
        ..state
    };
    let event = notice_event(
        &alice,
        &state.agent_did,
        "cm_1",
        "challenge",
        "evt-battle-1",
    );
    let (status, response) = callback(&state, &event).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["ok"], true, "{response}");
    let received = callback_received_diagnostic(dir.path(), "evt-battle-1");
    let input = &received["brain_input"];
    assert_eq!(input["target_agent_id"], state.agent_did);
    // Nothing to answer in the channel: the agent acts with the card tools.
    assert_eq!(input["allowed_actions"], json!(["ignore"]));
    let battle = &input["payload"]["card_battle"];
    assert_eq!(battle["match_id"], "cm_1", "{input}");
    assert_eq!(battle["step"], "challenge");
    assert_eq!(battle["your_turn"], true);
    assert_eq!(battle["next_tool"], "card_accept");
    assert_eq!(battle["match"]["challenger"]["agent_did"], alice.agent_did);

    // The sender posts it again until this agent moves on: the agent hears of it once.
    let again = notice_event(
        &alice,
        &state.agent_did,
        "cm_1",
        "challenge",
        "evt-battle-2",
    );
    let (status, response) = callback(&state, &again).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["detail"], "battle notice already told");
    assert!(response["decision"].is_null());
}

#[tokio::test]
async fn a_battle_notice_the_gateway_does_not_bear_out_is_dropped() {
    let (_dir, _app, _, _, state) = build_test_app(100);
    let alice = Identity::new_random();
    let stranger = Identity::new_random();
    let state = ControlPlaneState {
        gateway_urls: vec![spawn_gateway(invite(&alice.agent_did, &state.agent_did)).await],
        ..state
    };
    let me = state.agent_did.clone();
    for (event, detail) in [
        // Anyone can post in the channel: the sender is not in the match.
        (
            notice_event(&stranger, &me, "cm_1", "challenge", "evt-stranger"),
            "battle notice does not match the gateway's record",
        ),
        // The match has not got that far.
        (
            notice_event(&alice, &me, "cm_1", "round:2", "evt-early"),
            "battle notice does not match the gateway's record",
        ),
        (
            notice_event(&alice, &me, "cm_1", "final", "evt-early-end"),
            "battle notice does not match the gateway's record",
        ),
        (
            notice_event(&alice, &me, "cm_404", "challenge", "evt-unknown"),
            "battle notice names no match",
        ),
        (
            notice_event(&alice, &me, "cm_1", "surrender", "evt-step"),
            "battle notice names no known step",
        ),
        // Mentions another agent.
        (
            notice_event(
                &alice,
                &stranger.agent_did,
                "cm_1",
                "challenge",
                "evt-other",
            ),
            "battle notice for another agent",
        ),
    ] {
        let (status, response) = callback(&state, &event).await;
        assert_eq!(status, StatusCode::OK, "{}", event.event_id);
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["detail"], detail, "{}", event.event_id);
        assert!(response["decision"].is_null());
    }
    // None of them counts as told: the true notice still gets through.
    let (_, response) = callback(
        &state,
        &notice_event(&alice, &me, "cm_1", "challenge", "evt-true"),
    )
    .await;
    assert_ne!(
        response["detail"], "battle notice already told",
        "{response}"
    );
}

#[tokio::test]
async fn a_battle_notice_is_not_told_unchecked_when_the_gateway_cannot_be_asked() {
    let (_dir, _app, _, _, state) = build_test_app(100);
    let alice = Identity::new_random();
    let state = ControlPlaneState {
        gateway_urls: vec!["http://127.0.0.1:1".to_owned()],
        ..state
    };
    let event = notice_event(&alice, &state.agent_did, "cm_1", "challenge", "evt-down");
    let (status, response) = callback(&state, &event).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response["ok"], false, "{response}");
    // Not marked told: the next copy is checked again.
    assert!(
        state
            .social_store
            .get_reliability_task("card_battle_seen", "cm_1|challenge")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn other_messages_in_the_battle_channel_go_the_usual_way() {
    let (dir, _app, _, _, state) = build_test_app(100);
    let alice = Identity::new_random();
    let mut event = notice_event(&alice, &state.agent_did, "cm_1", "challenge", "evt-chat");
    let envelope = signed_agent_event_envelope(
        &alice,
        "remote-node",
        Some(&state.agent_did),
        "board.message.publish",
        json!({
            "feed_key": BOARD_BATTLE_FEED_KEY, "scope_hint": "group:board-battle",
            "network_id": "local:test", "action": "message.post",
            "payload": {"category": "battle", "message": "good game", "mention": state.agent_did},
        }),
    );
    event.agent_envelope = Some(envelope);
    event.payload["content"] = json!("good game");
    event.payload["topic_content"] = json!("good game");
    let (status, response) = callback(&state, &event).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["ok"], true, "{response}");
    let received = callback_received_diagnostic(dir.path(), "evt-chat");
    assert!(received["brain_input"]["payload"]["card_battle"].is_null());
    assert_eq!(
        received["brain_input"]["allowed_actions"],
        json!(["reply", "ignore"])
    );
}
