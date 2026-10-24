use axum::Json;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};
use wattetheria_kernel::civilization::topics::HiveProfile;
use wattetheria_kernel::swarm_bridge::{SwarmAgentEnvelope, SwarmTopicMessageView};

use crate::auth::internal_error;
use crate::routes::agent_events::{
    AgentEventCallbackResponse, AgentEventEnvelope, is_collective_mission_topic_event,
    is_mission_event,
};
use crate::routes::board::board_target::{BoardTargetError, resolve_board_target};
use crate::routes::identity::IdentityContextView;
use crate::state::{ControlPlaneState, HiveMessageBody};

use super::HiveEnvelopeRoute;

pub(crate) fn is_public_route(feed_key: &str, scope_hint: &str) -> bool {
    feed_key != "wattswarm.dm" && !crate::routes::mcp::is_local_private_hive(feed_key, scope_hint)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MessageIdentity {
    author_agent_did: Option<String>,
    mention: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mention_display_name: Option<String>,
}

impl MessageIdentity {
    fn from_envelope(envelope: Option<&SwarmAgentEnvelope>) -> Self {
        Self {
            author_agent_did: envelope.and_then(|envelope| envelope.source_agent_id.clone()),
            mention: envelope.and_then(|envelope| envelope.target_agent_id.clone()),
            mention_display_name: envelope
                .and_then(|envelope| envelope.message.pointer("/payload/mention_display_name"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        }
    }
}

pub(crate) fn message_identity(message: &SwarmTopicMessageView) -> Option<MessageIdentity> {
    is_public_route(&message.feed_key, &message.scope_hint)
        .then(|| MessageIdentity::from_envelope(message.agent_envelope.as_ref()))
}

pub(super) fn append_post_identity(
    payload: &mut Value,
    hive: &HiveProfile,
    envelope: &SwarmAgentEnvelope,
) {
    if is_public_route(&hive.feed_key, &hive.scope_hint) {
        payload.as_object_mut().expect("message payload").extend(
            serde_json::to_value(MessageIdentity::from_envelope(Some(envelope)))
                .expect("message identity")
                .as_object()
                .expect("identity fields")
                .clone(),
        );
    }
}

pub(super) async fn mentioned_message_envelope(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    hive: &HiveProfile,
    network_id: &str,
    body: &HiveMessageBody,
) -> Result<SwarmAgentEnvelope, Response> {
    let mention = body
        .mention
        .as_deref()
        .map(|mention| mention.trim().trim_start_matches('@').trim());
    let target = resolve_board_target(
        state,
        network_id,
        &hive.feed_key,
        &hive.scope_hint,
        mention,
        None,
    )
    .await
    .map_err(|error| match error {
        BoardTargetError::InvalidMention(message) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": message})),
        )
            .into_response(),
        BoardTargetError::Bridge(error) => internal_error(&error),
    })?;
    let mention_display_name = mention.filter(|mention| !mention.starts_with("did:"));
    super::hive_agent_envelope(
        state,
        context,
        "hive.message.post",
        target.clone(),
        super::hive_envelope_message(
            state,
            context,
            &HiveEnvelopeRoute {
                hive_id: &hive.topic_id,
                network_id,
                feed_key: &hive.feed_key,
                scope_hint: &hive.scope_hint,
            },
            "message.post",
            &json!({
                "content": body.content,
                "reply_to_message_id": body.reply_to_message_id,
                "mention": target,
                "mention_display_name": mention_display_name,
            }),
        ),
    )
    .await
    .map_err(|error| internal_error(&error))
}

pub(crate) fn receive_message(
    state: &ControlPlaneState,
    event: &mut AgentEventEnvelope,
    acked_at: u64,
) -> Option<Response> {
    if event.event_type != "topic_message_requires_reply"
        || is_mission_event(event)
        || is_collective_mission_topic_event(event)
    {
        return None;
    }
    let envelope = event.agent_envelope.as_ref()?;
    if envelope.capability.as_deref() != Some("hive.message.post") {
        return None;
    }
    let feed_key = envelope
        .message
        .get("feed_key")
        .or_else(|| event.payload.get("feed_key"))?
        .as_str()?;
    let scope_hint = envelope
        .message
        .get("scope_hint")
        .or_else(|| event.payload.get("scope_hint"))?
        .as_str()?;
    if !is_public_route(feed_key, scope_hint) {
        return None;
    }
    if envelope.target_agent_id.as_deref() == Some(state.agent_did.as_str()) {
        event.target_agent_id.clone_from(&envelope.target_agent_id);
        event.allowed_actions = vec!["reply".to_owned(), "ignore".to_owned()];
        return None;
    }
    Some(
        Json(AgentEventCallbackResponse {
            ok: true,
            acked_at: Some(acked_at),
            detail: Some("public Hive message received without local mention".to_owned()),
            decision: None,
        })
        .into_response(),
    )
}
