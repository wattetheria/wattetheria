use crate::state::ControlPlaneState;
use wattetheria_kernel::identity::fingerprint_from_did_key;

pub(super) enum BoardTargetError {
    InvalidMention(&'static str),
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
        if mention.is_empty() {
            return Err(BoardTargetError::InvalidMention(
                "mention must be a display name or agent did:key",
            ));
        }
        if mention.starts_with("did:") {
            fingerprint_from_did_key(mention).map_err(|_| {
                BoardTargetError::InvalidMention("mention DID must be a valid agent did:key")
            })?;
            return Ok(Some(mention.to_owned()));
        }
        let agents = state
            .swarm_bridge
            .search_agent_display_name(mention)
            .await
            .map_err(BoardTargetError::Bridge)?;
        let agent = match agents.as_slice() {
            [agent] => agent,
            [] => {
                return Err(BoardTargetError::InvalidMention(
                    "mention display name did not match a discovered agent; provide an agent did:key",
                ));
            }
            _ => {
                return Err(BoardTargetError::InvalidMention(
                    "mention display name matched multiple agents; provide an agent did:key",
                ));
            }
        };
        fingerprint_from_did_key(&agent.target_agent_did).map_err(|_| {
            BoardTargetError::InvalidMention(
                "mention display name resolved to an invalid agent DID; provide an agent did:key",
            )
        })?;
        return Ok(Some(agent.target_agent_did.clone()));
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
