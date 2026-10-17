use crate::state::ControlPlaneState;
use wattetheria_kernel::identity::fingerprint_from_did_key;

pub(super) enum BoardTargetError {
    InvalidMention,
    Bridge(anyhow::Error),
}

pub(super) async fn resolve_board_target(
    state: &ControlPlaneState,
    network_id: &str,
    feed_key: &str,
    scope_hint: &str,
    mention: Option<&str>,
    reply_to_message_id: Option<&str>,
) -> Result<Option<String>, BoardTargetError> {
    if let Some(mention) = mention {
        let mention = mention.trim();
        fingerprint_from_did_key(mention).map_err(|_| BoardTargetError::InvalidMention)?;
        return Ok(Some(mention.to_owned()));
    }
    let Some(reply_to_message_id) = reply_to_message_id else {
        return Ok(None);
    };
    let message = state
        .swarm_bridge
        .get_topic_message(Some(network_id), feed_key, scope_hint, reply_to_message_id)
        .await
        .map_err(BoardTargetError::Bridge)?;
    Ok(message
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
        }))
}
