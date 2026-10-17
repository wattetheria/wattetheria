use super::{
    AgentActionExecutionArgs, append_agent_action_execution, bad_request, internal_error,
    payload_string, topic_reply_already_posted,
};
use crate::routes::board::{BoardMessageBody, board_category_for_route, publish_board_message};
use crate::state::{AgentActionCommitBody, AgentActionCommitEvent, ControlPlaneState};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

pub(super) fn event_category(event: &AgentActionCommitEvent) -> Option<&'static str> {
    board_category_for_route(
        event.payload.get("feed_key")?.as_str()?,
        event.payload.get("scope_hint")?.as_str()?,
    )
}

pub(super) async fn commit_board_reply(
    state: ControlPlaneState,
    headers: HeaderMap,
    body: AgentActionCommitBody,
    content: Value,
) -> Response {
    let category = event_category(&body.event).expect("Board category checked");
    let feed_key = body.event.payload["feed_key"]
        .as_str()
        .expect("Board feed checked");
    let scope_hint = body.event.payload["scope_hint"]
        .as_str()
        .expect("Board scope checked");
    let network_id = match state.swarm_bridge.current_network_id().await {
        Ok(value) => value,
        Err(error) => return internal_error(&error),
    };
    if payload_string(&body.event.payload, "/network_id").is_some_and(|id| id != network_id) {
        return bad_request("Board reply network does not match current network");
    }
    let reply_to_message_id = payload_string(&body.decision.payload, "/reply_to_message_id")
        .or_else(|| payload_string(&body.event.payload, "/message_id"));
    let request_json = json!({
        "event_id": body.event.event_id,
        "decision_id": body.decision.decision_id,
        "action": body.decision.action,
        "feed_key": feed_key,
        "scope_hint": scope_hint,
        "reply_to_message_id": reply_to_message_id,
    });
    let already_posted = match topic_reply_already_posted(
        &state,
        Some(&network_id),
        feed_key,
        scope_hint,
        reply_to_message_id.as_deref(),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return internal_error(&error),
    };
    let response = if already_posted {
        Json(json!({
            "ok": true,
            "source": "topic_reply_dedupe",
            "action_type": "board.message.reply",
            "reply_to_message_id": reply_to_message_id,
        }))
        .into_response()
    } else {
        publish_board_message(
            State(state.clone()),
            headers.clone(),
            Json(BoardMessageBody {
                public_id: payload_string(&body.decision.payload, "/public_id"),
                category: category.to_owned(),
                message: content,
                reply_to_message_id,
                mention: payload_string(&body.decision.payload, "/mention"),
            }),
        )
        .await
    };
    if response.status().is_success() {
        let response_json = json!({"ok": true, "action_type": "board.message.reply"});
        if let Err(error) = append_agent_action_execution(
            &state,
            &headers,
            AgentActionExecutionArgs {
                action_type: "board.message.reply",
                domain: "board",
                target_id: Some(feed_key.to_owned()),
                actor_public_id: None,
                actor_agent_did: Some(state.agent_did.clone()),
                request_json: &request_json,
                response_json: &response_json,
            },
        ) {
            return internal_error(&error);
        }
    }
    response
}
