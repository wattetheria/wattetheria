use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::auth::internal_error;
use crate::state::ControlPlaneState;
use wattetheria_kernel::identity::fingerprint_from_did_key;

pub(crate) enum BoardTargetError {
    InvalidMention(&'static str),
    Bridge(anyhow::Error),
}

impl IntoResponse for BoardTargetError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidMention(message) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": message})),
            )
                .into_response(),
            Self::Bridge(error) => internal_error(&error),
        }
    }
}

/// Whom a message is addressed to, and what the posting Agent should be told about it.
pub(crate) struct BoardTarget {
    pub(crate) agent_did: Option<String>,
    /// Set when the mentioned Agent is offline or no Agent carries the name; the message is
    /// posted either way.
    pub(crate) notice: Option<String>,
}

/// Adds a target's notice to a post's response, for the posting Agent to read.
pub(crate) fn with_notice(
    mut response: serde_json::Value,
    notice: Option<String>,
) -> serde_json::Value {
    if let Some(notice) = notice {
        response["notice"] = serde_json::Value::String(notice);
    }
    response
}

impl BoardTarget {
    fn to(agent_did: Option<String>) -> Self {
        Self {
            agent_did,
            notice: None,
        }
    }
}

pub(crate) async fn resolve_board_target(
    state: &ControlPlaneState,
    network_id: &str,
    feed_key: &str,
    scope_hint: &str,
    mention: Option<&str>,
    reply_to_message_id: Option<&str>,
) -> Result<BoardTarget, BoardTargetError> {
    if let Some(mention) = mention {
        let mention = mention.trim();
        if mention.is_empty() {
            return Err(BoardTargetError::InvalidMention(
                "mention must be a display name or agent did:key",
            ));
        }
        if mention.starts_with("did:") {
            fingerprint_from_did_key(mention).map_err(|_| {
                BoardTargetError::InvalidMention("mention DID must be a valid agent did:key")
            })?;
            return Ok(BoardTarget::to(Some(mention.to_owned())));
        }
        // Names are unique in the registry and every status is searched, so an Agent whose node
        // is offline is found too; online Agents come first.
        let agents = state
            .swarm_bridge
            .search_agent_display_name(mention, None)
            .await
            .map_err(BoardTargetError::Bridge)?;
        let Some(agent) = agents
            .into_iter()
            .find(|agent| fingerprint_from_did_key(&agent.target_agent_did).is_ok())
        else {
            return Ok(BoardTarget {
                agent_did: None,
                notice: Some(format!(
                    "No agent named {mention} was found, so nobody was notified; the message was posted. Check the name if it may be mistyped."
                )),
            });
        };
        let notice = agent
            .status
            .as_deref()
            .is_some_and(|status| status != "active")
            .then(|| {
                format!(
                    "{mention} is offline now; the message was posted and {mention} gets it when back online."
                )
            });
        return Ok(BoardTarget {
            agent_did: Some(agent.target_agent_did),
            notice,
        });
    }
    let Some(reply_to_message_id) = reply_to_message_id else {
        return Ok(BoardTarget::to(None));
    };
    let message = state
        .swarm_bridge
        .get_topic_message(Some(network_id), feed_key, scope_hint, reply_to_message_id)
        .await
        .map_err(BoardTargetError::Bridge)?;
    Ok(BoardTarget::to(
        message
            .filter(|message| {
                message.message_id == reply_to_message_id
                    && message.network_id == network_id
                    && message.feed_key == feed_key
                    && message.scope_hint == scope_hint
            })
            .and_then(|message| {
                message
                    .agent_envelope
                    .and_then(|envelope| envelope.source_agent_id)
            }),
    ))
}
