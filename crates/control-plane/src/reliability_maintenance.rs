use crate::routes::civilization::reconcile_swarm_relationship_views;
use crate::social_host::{
    SignedAgentEnvelopeArgs, build_signed_agent_envelope_for_nodes, load_social_identity_maps,
    resolve_social_local_context, with_social_defaults,
};
use crate::state::ControlPlaneState;
use anyhow::Context;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tokio::time::{Duration, interval};
use tracing::{debug, warn};
use wattetheria_kernel::swarm_bridge::{
    SwarmAgentEnvelope, SwarmPeerRelationshipView, SwarmRelationshipAction,
    SwarmRelationshipActionCommand,
};
use wattetheria_social::domain::friend_requests::FriendRequest;

const FRIEND_REQUEST_OBJECT_KIND: &str = "friend_request";
pub const RELIABILITY_MAINTENANCE_INTERVAL_SEC: u64 = 120;
pub const RELIABILITY_MAINTENANCE_BATCH_LIMIT: usize = 10;
pub const FRIEND_REQUEST_MIN_RETRY_DELAY_SEC: i64 = 300;
const FRIEND_REQUEST_RETRY_DELAY_SEC: [i64; 4] = [300, 900, 1800, 3600];

#[must_use]
pub fn spawn_reliability_maintenance_task(state: ControlPlaneState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(RELIABILITY_MAINTENANCE_INTERVAL_SEC));
        loop {
            ticker.tick().await;
            if let Err(error) =
                run_reliability_maintenance_tick_once(&state, RELIABILITY_MAINTENANCE_BATCH_LIMIT)
                    .await
            {
                warn!(%error, "reliability maintenance tick failed");
            }
        }
    })
}

pub async fn run_reliability_maintenance_tick_once(
    state: &ControlPlaneState,
    limit: usize,
) -> anyhow::Result<usize> {
    let now = chrono::Utc::now().timestamp();
    let due = state
        .social_store
        .due_outbound_pending_friend_requests(now, FRIEND_REQUEST_MIN_RETRY_DELAY_SEC, limit)
        .map_err(anyhow::Error::msg)?;
    if due.is_empty() {
        return Ok(0);
    }

    let (identities, bindings) = load_social_identity_maps(state).await;
    let relationship_views = state
        .swarm_bridge
        .list_peer_relationships()
        .await
        .unwrap_or_default();
    let pending_remote_nodes = due
        .iter()
        .filter_map(|request| request.remote_node_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    let pending_relationship_views = relationship_views
        .iter()
        .filter(|view| pending_remote_nodes.contains(view.remote_node_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let peers = state.swarm_bridge.peers().await.unwrap_or_default();
    let connected_nodes = peers
        .into_iter()
        .filter(|peer| peer.connected.unwrap_or(false))
        .map(|peer| peer.node_id)
        .collect::<BTreeSet<_>>();

    let local = resolve_social_local_context(state, None).await;
    reconcile_swarm_relationship_views(
        state,
        &local.public_id,
        &identities,
        &bindings,
        &pending_relationship_views,
    )?;
    let mut processed = 0;
    for request in due {
        maintain_outbound_friend_request(
            state,
            &pending_relationship_views,
            &connected_nodes,
            &request,
            now,
        )
        .await
        .with_context(|| format!("maintain friend request {}", request.request_id))?;
        processed += 1;
    }
    Ok(processed)
}

async fn maintain_outbound_friend_request(
    state: &ControlPlaneState,
    relationship_views: &[SwarmPeerRelationshipView],
    connected_nodes: &BTreeSet<String>,
    request: &FriendRequest,
    now: i64,
) -> anyhow::Result<()> {
    let remote_node_id = request
        .remote_node_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("pending friend request missing remote_node_id")?;
    if !connected_nodes.contains(remote_node_id) {
        state
            .social_store
            .defer_reliability_task(
                FRIEND_REQUEST_OBJECT_KIND,
                &request.request_id,
                now,
                now + FRIEND_REQUEST_MIN_RETRY_DELAY_SEC,
                Some("remote peer is not connected"),
            )
            .map_err(anyhow::Error::msg)?;
        return Ok(());
    }

    let attempt_count = state
        .social_store
        .get_reliability_task(FRIEND_REQUEST_OBJECT_KIND, &request.request_id)
        .map_err(anyhow::Error::msg)?
        .map_or(0, |task| task.attempt_count);
    let next_attempt_at = now + retry_delay_after_attempt(attempt_count);
    let envelope = if let Some(envelope) = relationship_views
        .iter()
        .find(|view| view.remote_node_id == remote_node_id)
        .and_then(|view| view.agent_envelope.clone())
    {
        envelope
    } else {
        build_retry_friend_request_envelope(state, request, remote_node_id, now).await?
    };

    let result = state
        .swarm_bridge
        .send_peer_relationship_action(SwarmRelationshipActionCommand {
            remote_node_id: remote_node_id.to_owned(),
            action: SwarmRelationshipAction::Request,
            agent_envelope: envelope,
        })
        .await;
    let last_error = result.as_ref().err().map(ToString::to_string);
    state
        .social_store
        .record_reliability_attempt(
            FRIEND_REQUEST_OBJECT_KIND,
            &request.request_id,
            now,
            next_attempt_at,
            last_error.as_deref(),
        )
        .map_err(anyhow::Error::msg)?;
    if let Err(error) = result {
        debug!(%error, request_id = %request.request_id, remote_node_id, "friend request retry failed");
    }
    Ok(())
}

fn retry_delay_after_attempt(attempt_count: i64) -> i64 {
    let index = usize::try_from((attempt_count + 1).max(0)).unwrap_or(usize::MAX);
    FRIEND_REQUEST_RETRY_DELAY_SEC
        .get(index)
        .copied()
        .unwrap_or(*FRIEND_REQUEST_RETRY_DELAY_SEC.last().unwrap_or(&3600))
}

async fn build_retry_friend_request_envelope(
    state: &ControlPlaneState,
    request: &FriendRequest,
    remote_node_id: &str,
    now: i64,
) -> anyhow::Result<SwarmAgentEnvelope> {
    let local = resolve_social_local_context(state, Some(&request.local_public_id)).await;
    let (identities, _) = load_social_identity_maps(state).await;
    let target_agent_id = identities
        .get(&request.remote_public_id)
        .and_then(|identity| identity.agent_did.clone())
        .unwrap_or_else(|| request.remote_public_id.clone());
    let local_node_id = state.swarm_bridge.local_node_id().await.ok();
    let message = with_social_defaults(
        json!({
            "kind": "friend_request",
            "request_id": request.request_id,
            "correlation_id": request.correlation_id,
            "retry": true,
        }),
        [
            (
                "source_public_id",
                Value::String(request.local_public_id.clone()),
            ),
            (
                "target_public_id",
                Value::String(request.remote_public_id.clone()),
            ),
            (
                "action",
                serde_json::to_value(&SwarmRelationshipAction::Request).unwrap_or(Value::Null),
            ),
            ("sent_at", json!(now)),
        ],
    );
    build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: local.agent_id,
            source_display_name: local.display_name,
            target_agent_id: Some(target_agent_id),
            source_node_id: local_node_id,
            target_node_id: Some(remote_node_id.to_owned()),
            capability: "social.friend.request".to_owned(),
            message,
            extensions: None,
        },
    )
}
