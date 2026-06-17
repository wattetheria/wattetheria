use crate::application::{
    block_service, friend_request_service, friendship_service, message_service, receipt_service,
    remote_identity_service, thread_service, transport_binding_service,
};
use crate::domain::blocks::SocialBlock;
use crate::domain::friend_requests::{FriendRequest, FriendRequestDirection, FriendRequestState};
use crate::domain::friendships::{Friendship, FriendshipState};
use crate::domain::identities::RemoteIdentityProfile;
use crate::domain::messages::{
    DeliveryState, DirectMessage, MessageDirection, MessageKind, ReadState,
};
use crate::domain::receipts::{MessageReceipt, ReceiptKind};
use crate::domain::threads::{DirectThread, ThreadState};
use crate::domain::transport_bindings::{RemoteTransportBinding, TransportKind};
use crate::ports::repositories::{
    BlockRepository, FriendRequestRepository, FriendshipRepository, MessageReceiptRepository,
    MessageRepository, ReliabilityTaskRepository, RemoteIdentityRepository, ThreadRepository,
    TransportBindingRepository,
};
use crate::types::{SocialError, SocialResult};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownIdentitySnapshot {
    pub public_id: String,
    pub agent_did: Option<String>,
    pub display_name: String,
    pub active: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownTransportBindingSnapshot {
    pub binding_source: String,
    pub binding_confidence: i32,
    pub binding_verified: bool,
    pub binding_verified_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterpartSnapshot {
    pub counterpart_public_id: String,
    pub target_agent: String,
    pub remote_node_id: String,
    pub known_identity: Option<KnownIdentitySnapshot>,
    pub known_binding: Option<KnownTransportBindingSnapshot>,
    pub observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipSyncView {
    pub counterpart: CounterpartSnapshot,
    pub relationship_state: String,
    pub last_action: Option<String>,
    pub initiated_by: String,
    pub request_id: Option<String>,
    pub correlation_id: Option<String>,
    pub requested_at: Option<i64>,
    pub responded_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmThreadSyncView {
    pub counterpart: CounterpartSnapshot,
    pub transport_thread_id: String,
    pub session_state: String,
    pub relationship_established_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_message_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmMessageSyncView {
    pub counterpart: CounterpartSnapshot,
    pub transport_thread_id: String,
    pub message_id: String,
    pub message_kind: String,
    pub direction: String,
    pub delivery_state: String,
    pub a2a_protocol: String,
    pub content: Value,
    pub encrypted_body: Option<String>,
    pub content_encoding: Option<String>,
    pub agent_envelope_json: Option<Value>,
    pub agent_signature: Option<String>,
    pub created_at: i64,
    pub acknowledged_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationshipAction {
    Request,
    Accept,
    Reject,
    Cancel,
    Remove,
    Block,
    Unblock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistRelationshipActionInput {
    pub local_public_id: String,
    pub counterpart: CounterpartSnapshot,
    pub action: RelationshipAction,
    pub request_id: Option<String>,
    pub correlation_id: Option<String>,
    pub occurred_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistDmMessageInput {
    pub local_public_id: String,
    pub counterpart: CounterpartSnapshot,
    pub thread_id: String,
    pub message_id: String,
    pub content: Value,
    pub agent_envelope_json: Value,
    pub agent_signature: Option<String>,
    pub occurred_at: i64,
}

fn counterpart_display_name(snapshot: &CounterpartSnapshot) -> Option<String> {
    snapshot
        .known_identity
        .as_ref()
        .map(|identity| identity.display_name.trim())
        .filter(|display_name| !display_name.is_empty())
        .map(ToOwned::to_owned)
}

fn normalize_agent_did(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with("did:") {
        Some(value.to_string())
    } else {
        None
    }
}

fn counterpart_agent_did(snapshot: &CounterpartSnapshot) -> Option<String> {
    snapshot
        .known_identity
        .as_ref()
        .and_then(|identity| identity.agent_did.as_deref())
        .and_then(normalize_agent_did)
        .or_else(|| normalize_agent_did(&snapshot.target_agent))
        .or_else(|| normalize_agent_did(&snapshot.counterpart_public_id))
}

pub fn cache_counterpart<R>(repository: &R, snapshot: &CounterpartSnapshot) -> SocialResult<()>
where
    R: RemoteIdentityRepository + TransportBindingRepository,
{
    let agent_did = counterpart_agent_did(snapshot);
    if let Some(agent_did) = agent_did.clone() {
        let existing_identity = repository.get_remote_identity(&snapshot.counterpart_public_id)?;
        let (public_id, display_name, active, created_at) = snapshot
            .known_identity
            .as_ref()
            .map(|identity| {
                (
                    identity.public_id.clone(),
                    identity.display_name.clone(),
                    identity.active,
                    identity.created_at,
                )
            })
            .unwrap_or_else(|| {
                (
                    snapshot.counterpart_public_id.clone(),
                    snapshot.counterpart_public_id.clone(),
                    true,
                    snapshot.observed_at,
                )
            });
        let active = existing_identity
            .as_ref()
            .map_or(active, |existing| existing.active && active);
        let identity_updated_at = snapshot.observed_at.max(created_at);
        remote_identity_service::upsert_remote_identity(
            repository,
            &RemoteIdentityProfile {
                public_id,
                agent_did,
                display_name,
                description: None,
                capabilities: Vec::new(),
                skills: Vec::new(),
                did_document_json: None,
                active,
                last_profile_fetched_at: Some(identity_updated_at),
                created_at,
                updated_at: identity_updated_at,
            },
        )?;
    }

    let (binding_source, binding_confidence, binding_verified, binding_verified_at) = snapshot
        .known_binding
        .as_ref()
        .map(|binding| {
            (
                binding.binding_source.clone(),
                binding.binding_confidence,
                binding.binding_verified,
                binding.binding_verified_at,
            )
        })
        .unwrap_or_else(|| ("derived".to_string(), 50, false, None));
    let binding_updated_at = binding_verified_at
        .unwrap_or(snapshot.observed_at)
        .max(snapshot.observed_at);
    transport_binding_service::upsert_transport_binding(
        repository,
        &RemoteTransportBinding {
            public_id: snapshot.counterpart_public_id.clone(),
            agent_did,
            transport_kind: TransportKind::Wattswarm,
            transport_node_id: snapshot.remote_node_id.clone(),
            binding_source,
            binding_confidence,
            binding_proof_json: None,
            binding_verified,
            binding_verified_at,
            updated_at: binding_updated_at,
        },
    )?;

    Ok(())
}

fn mark_counterpart_identity_state<R>(
    repository: &R,
    counterpart_public_id: &str,
    active: bool,
    updated_at: i64,
) -> SocialResult<()>
where
    R: RemoteIdentityRepository,
{
    let Some(mut identity) = repository.get_remote_identity(counterpart_public_id)? else {
        return Ok(());
    };
    identity.active = active;
    identity.updated_at = identity.updated_at.max(updated_at);
    remote_identity_service::upsert_remote_identity(repository, &identity)
}

pub fn reconcile_relationship_views<R>(
    repository: &R,
    local_public_id: &str,
    views: &[RelationshipSyncView],
) -> SocialResult<()>
where
    R: RemoteIdentityRepository
        + TransportBindingRepository
        + FriendRequestRepository
        + ReliabilityTaskRepository
        + FriendshipRepository
        + BlockRepository
        + ThreadRepository,
{
    let existing_requests =
        friend_request_service::list_friend_requests(repository, local_public_id)
            .unwrap_or_default();
    for view in views {
        cache_counterpart(repository, &view.counterpart)?;
        let request_id = view
            .request_id
            .clone()
            .or_else(|| {
                latest_request_for_counterpart(
                    &existing_requests,
                    &view.counterpart.counterpart_public_id,
                )
                .map(|request| request.request_id.clone())
            })
            .unwrap_or_else(|| {
                stable_pair_id(
                    "friend-request",
                    local_public_id,
                    &view.counterpart.counterpart_public_id,
                )
            });
        let direction = if view.initiated_by == "remote" {
            FriendRequestDirection::Inbound
        } else {
            FriendRequestDirection::Outbound
        };
        let requested_at = view.requested_at.unwrap_or(view.updated_at);
        let updated_at = view
            .responded_at
            .unwrap_or(view.updated_at)
            .max(requested_at);
        let thread_id = thread_service::find_thread(
            repository,
            local_public_id,
            &view.counterpart.counterpart_public_id,
        )?
        .map(|thread| thread.thread_id)
        .unwrap_or_else(|| {
            stable_pair_id(
                "dm",
                local_public_id,
                &view.counterpart.counterpart_public_id,
            )
        });
        let friendship_id = stable_pair_id(
            "friendship",
            local_public_id,
            &view.counterpart.counterpart_public_id,
        );

        match (
            view.relationship_state.as_str(),
            view.last_action.as_deref(),
        ) {
            ("requested", _) => {
                ignore_conflict(friend_request_service::upsert_friend_request(
                    repository,
                    &FriendRequest {
                        request_id,
                        local_public_id: local_public_id.to_string(),
                        remote_public_id: view.counterpart.counterpart_public_id.clone(),
                        remote_node_id: Some(view.counterpart.remote_node_id.clone()),
                        direction,
                        state: FriendRequestState::Pending,
                        decision_reason: None,
                        correlation_id: view.correlation_id.clone(),
                        created_at: requested_at,
                        updated_at,
                        expires_at: None,
                    },
                ))?;
            }
            ("accepted", _) => {
                retire_alias_friendships(
                    repository,
                    local_public_id,
                    &view.counterpart.counterpart_public_id,
                    &view.counterpart.remote_node_id,
                    &request_id,
                    updated_at,
                )?;
                ignore_conflict(friend_request_service::upsert_friend_request(
                    repository,
                    &FriendRequest {
                        request_id: request_id.clone(),
                        local_public_id: local_public_id.to_string(),
                        remote_public_id: view.counterpart.counterpart_public_id.clone(),
                        remote_node_id: Some(view.counterpart.remote_node_id.clone()),
                        direction,
                        state: FriendRequestState::Accepted,
                        decision_reason: Some("accepted".to_string()),
                        correlation_id: view.correlation_id.clone(),
                        created_at: requested_at,
                        updated_at,
                        expires_at: None,
                    },
                ))?;
                ignore_conflict(friendship_service::upsert_friendship(
                    repository,
                    &Friendship {
                        friendship_id,
                        local_public_id: local_public_id.to_string(),
                        remote_public_id: view.counterpart.counterpart_public_id.clone(),
                        display_name: counterpart_display_name(&view.counterpart),
                        state: FriendshipState::Active,
                        established_from_request_id: Some(request_id),
                        thread_id: Some(thread_id),
                        created_at: requested_at,
                        updated_at,
                    },
                ))?;
                mark_counterpart_identity_state(
                    repository,
                    &view.counterpart.counterpart_public_id,
                    true,
                    updated_at,
                )?;
                friend_request_service::settle_related_outbound_friend_requests(
                    repository,
                    local_public_id,
                    friend_request_service::FriendRequestCounterpartRef {
                        public_id: &view.counterpart.counterpart_public_id,
                        remote_node_id: &view.counterpart.remote_node_id,
                        target_agent: &view.counterpart.target_agent,
                    },
                    FriendRequestState::Accepted,
                    "accepted",
                    updated_at,
                )?;
            }
            ("rejected", _) => {
                ignore_conflict(friend_request_service::upsert_friend_request(
                    repository,
                    &FriendRequest {
                        request_id,
                        local_public_id: local_public_id.to_string(),
                        remote_public_id: view.counterpart.counterpart_public_id.clone(),
                        remote_node_id: Some(view.counterpart.remote_node_id.clone()),
                        direction,
                        state: FriendRequestState::Rejected,
                        decision_reason: Some("rejected".to_string()),
                        correlation_id: view.correlation_id.clone(),
                        created_at: requested_at,
                        updated_at,
                        expires_at: None,
                    },
                ))?;
                friend_request_service::settle_related_outbound_friend_requests(
                    repository,
                    local_public_id,
                    friend_request_service::FriendRequestCounterpartRef {
                        public_id: &view.counterpart.counterpart_public_id,
                        remote_node_id: &view.counterpart.remote_node_id,
                        target_agent: &view.counterpart.target_agent,
                    },
                    FriendRequestState::Rejected,
                    "rejected",
                    updated_at,
                )?;
            }
            ("blocked", _) => {
                ignore_conflict(friend_request_service::upsert_friend_request(
                    repository,
                    &FriendRequest {
                        request_id: request_id.clone(),
                        local_public_id: local_public_id.to_string(),
                        remote_public_id: view.counterpart.counterpart_public_id.clone(),
                        remote_node_id: Some(view.counterpart.remote_node_id.clone()),
                        direction,
                        state: FriendRequestState::Blocked,
                        decision_reason: Some("blocked".to_string()),
                        correlation_id: view.correlation_id.clone(),
                        created_at: requested_at,
                        updated_at,
                        expires_at: None,
                    },
                ))?;
                if view.initiated_by == "local" {
                    ignore_conflict(block_service::upsert_block(
                        repository,
                        &SocialBlock {
                            block_id: stable_pair_id(
                                "block",
                                local_public_id,
                                &view.counterpart.counterpart_public_id,
                            ),
                            owner_public_id: local_public_id.to_string(),
                            blocked_public_id: view.counterpart.counterpart_public_id.clone(),
                            blocked_node_id: Some(view.counterpart.remote_node_id.clone()),
                            reason: Some("blocked".to_string()),
                            created_at: requested_at,
                            updated_at,
                        },
                    ))?;
                }
                ignore_conflict(friendship_service::upsert_friendship(
                    repository,
                    &Friendship {
                        friendship_id,
                        local_public_id: local_public_id.to_string(),
                        remote_public_id: view.counterpart.counterpart_public_id.clone(),
                        display_name: counterpart_display_name(&view.counterpart),
                        state: FriendshipState::Blocked,
                        established_from_request_id: Some(request_id),
                        thread_id: Some(thread_id),
                        created_at: requested_at,
                        updated_at,
                    },
                ))?;
                friend_request_service::settle_related_outbound_friend_requests(
                    repository,
                    local_public_id,
                    friend_request_service::FriendRequestCounterpartRef {
                        public_id: &view.counterpart.counterpart_public_id,
                        remote_node_id: &view.counterpart.remote_node_id,
                        target_agent: &view.counterpart.target_agent,
                    },
                    FriendRequestState::Blocked,
                    "blocked",
                    updated_at,
                )?;
            }
            ("none", Some("remove")) => {
                let existing_friendship =
                    friendship_service::list_friendships(repository, local_public_id)?
                        .into_iter()
                        .find(|friendship| {
                            friendship.remote_public_id == view.counterpart.counterpart_public_id
                                && friendship.state == FriendshipState::Active
                        });
                if existing_friendship
                    .as_ref()
                    .is_some_and(|friendship| friendship.created_at > updated_at)
                {
                    continue;
                }
                let existing_thread = thread_service::find_thread(
                    repository,
                    local_public_id,
                    &view.counterpart.counterpart_public_id,
                )?;
                let friendship_id = existing_friendship
                    .as_ref()
                    .map(|friendship| friendship.friendship_id.clone())
                    .unwrap_or(friendship_id);
                let display_name = counterpart_display_name(&view.counterpart).or_else(|| {
                    existing_friendship
                        .as_ref()
                        .and_then(|item| item.display_name.clone())
                });
                let established_from_request_id = existing_friendship
                    .as_ref()
                    .and_then(|friendship| friendship.established_from_request_id.clone())
                    .or(Some(request_id));
                let thread_id = existing_friendship
                    .as_ref()
                    .and_then(|friendship| friendship.thread_id.clone())
                    .or_else(|| {
                        existing_thread
                            .as_ref()
                            .map(|thread| thread.thread_id.clone())
                    })
                    .unwrap_or_else(|| thread_id.clone());
                let created_at = existing_friendship
                    .as_ref()
                    .map(|friendship| friendship.created_at)
                    .unwrap_or(requested_at);
                ignore_conflict(friendship_service::upsert_friendship(
                    repository,
                    &Friendship {
                        friendship_id,
                        local_public_id: local_public_id.to_string(),
                        remote_public_id: view.counterpart.counterpart_public_id.clone(),
                        display_name,
                        state: FriendshipState::Removed,
                        established_from_request_id,
                        thread_id: Some(thread_id.clone()),
                        created_at,
                        updated_at,
                    },
                ))?;
                let (thread_created_at, transport_thread_id, last_message_at, thread_updated_at) =
                    existing_thread
                        .as_ref()
                        .map(|thread| {
                            (
                                thread.created_at,
                                thread.transport_thread_id.clone(),
                                thread.last_message_at,
                                thread.updated_at,
                            )
                        })
                        .unwrap_or((requested_at, thread_id.clone(), None, requested_at));
                let (thread_created_at, thread_updated_at) = normalize_thread_lifetime(
                    thread_created_at,
                    thread_updated_at.max(updated_at),
                    last_message_at,
                );
                ignore_conflict(thread_service::upsert_thread(
                    repository,
                    &DirectThread {
                        thread_id: thread_id.clone(),
                        local_public_id: local_public_id.to_string(),
                        remote_public_id: view.counterpart.counterpart_public_id.clone(),
                        transport_thread_id,
                        state: ThreadState::Closed,
                        last_message_at,
                        created_at: thread_created_at,
                        updated_at: thread_updated_at,
                    },
                ))?;
                mark_counterpart_identity_state(
                    repository,
                    &view.counterpart.counterpart_public_id,
                    false,
                    updated_at,
                )?;
            }
            ("none", Some("cancel")) => {
                ignore_conflict(friend_request_service::upsert_friend_request(
                    repository,
                    &FriendRequest {
                        request_id,
                        local_public_id: local_public_id.to_string(),
                        remote_public_id: view.counterpart.counterpart_public_id.clone(),
                        remote_node_id: Some(view.counterpart.remote_node_id.clone()),
                        direction,
                        state: FriendRequestState::Cancelled,
                        decision_reason: Some("cancelled".to_string()),
                        correlation_id: view.correlation_id.clone(),
                        created_at: requested_at,
                        updated_at,
                        expires_at: None,
                    },
                ))?;
            }
            ("none", Some("unblock")) => {
                ignore_conflict(block_service::remove_block(
                    repository,
                    local_public_id,
                    &view.counterpart.counterpart_public_id,
                ))?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn reconcile_dm_threads<R>(
    repository: &R,
    local_public_id: &str,
    views: &[DmThreadSyncView],
) -> SocialResult<()>
where
    R: RemoteIdentityRepository
        + TransportBindingRepository
        + ThreadRepository
        + FriendshipRepository,
{
    for view in views {
        cache_counterpart(repository, &view.counterpart)?;
        let existing_thread = thread_service::find_thread(
            repository,
            local_public_id,
            &view.counterpart.counterpart_public_id,
        )?;
        let social_thread_id = existing_thread
            .as_ref()
            .map(|thread| thread.thread_id.clone())
            .unwrap_or_else(|| view.transport_thread_id.clone());
        let created_at = existing_thread
            .as_ref()
            .map(|thread| thread.created_at.min(view.created_at))
            .unwrap_or(view.created_at);
        let last_message_at = max_optional_i64(
            existing_thread
                .as_ref()
                .and_then(|thread| thread.last_message_at),
            view.last_message_at,
        );
        let updated_at = last_message_at
            .unwrap_or(view.updated_at)
            .max(
                existing_thread
                    .as_ref()
                    .map(|thread| thread.updated_at)
                    .unwrap_or(view.updated_at),
            )
            .max(view.updated_at)
            .max(created_at);
        ignore_conflict(thread_service::upsert_thread(
            repository,
            &DirectThread {
                thread_id: social_thread_id.clone(),
                local_public_id: local_public_id.to_string(),
                remote_public_id: view.counterpart.counterpart_public_id.clone(),
                transport_thread_id: view.transport_thread_id.clone(),
                state: thread_state_from_swarm(&view.session_state),
                last_message_at,
                created_at,
                updated_at,
            },
        ))?;
        if view.relationship_established_at.is_some() {
            ignore_conflict(friendship_service::upsert_friendship(
                repository,
                &Friendship {
                    friendship_id: stable_pair_id(
                        "friendship",
                        local_public_id,
                        &view.counterpart.counterpart_public_id,
                    ),
                    local_public_id: local_public_id.to_string(),
                    remote_public_id: view.counterpart.counterpart_public_id.clone(),
                    display_name: counterpart_display_name(&view.counterpart),
                    state: FriendshipState::Active,
                    established_from_request_id: None,
                    thread_id: Some(social_thread_id),
                    created_at,
                    updated_at,
                },
            ))?;
        }
    }
    Ok(())
}

pub fn reconcile_dm_messages<R>(
    repository: &R,
    local_public_id: &str,
    views: &[DmMessageSyncView],
) -> SocialResult<()>
where
    R: RemoteIdentityRepository
        + TransportBindingRepository
        + FriendshipRepository
        + ThreadRepository
        + MessageRepository
        + MessageReceiptRepository,
{
    for view in views {
        cache_counterpart(repository, &view.counterpart)?;
        ensure_inbound_dm_allowed(repository, local_public_id, view)?;
        let existing_thread = thread_service::find_thread(
            repository,
            local_public_id,
            &view.counterpart.counterpart_public_id,
        )?;
        let social_thread_id = existing_thread
            .as_ref()
            .map(|thread| thread.thread_id.clone())
            .unwrap_or_else(|| view.transport_thread_id.clone());
        let created_at = existing_thread
            .as_ref()
            .map(|thread| thread.created_at.min(view.created_at))
            .unwrap_or(view.created_at);
        let message_updated_at = view
            .acknowledged_at
            .unwrap_or(view.created_at)
            .max(view.created_at);
        let last_message_at_value = existing_thread
            .as_ref()
            .and_then(|thread| thread.last_message_at)
            .unwrap_or(view.created_at)
            .max(view.created_at);
        let last_message_at = Some(last_message_at_value);
        let thread_updated_at = message_updated_at
            .max(
                existing_thread
                    .as_ref()
                    .map(|thread| thread.updated_at)
                    .unwrap_or(message_updated_at),
            )
            .max(last_message_at_value)
            .max(created_at);
        ignore_conflict(thread_service::upsert_thread(
            repository,
            &DirectThread {
                thread_id: social_thread_id.clone(),
                local_public_id: local_public_id.to_string(),
                remote_public_id: view.counterpart.counterpart_public_id.clone(),
                transport_thread_id: view.transport_thread_id.clone(),
                state: ThreadState::Ready,
                last_message_at,
                created_at,
                updated_at: thread_updated_at,
            },
        ))?;
        let delivery_state = delivery_state_from_swarm(&view.delivery_state);
        ignore_conflict(message_service::upsert_message(
            repository,
            &DirectMessage {
                thread_id: social_thread_id,
                message_id: view.message_id.clone(),
                transport_message_id: Some(view.message_id.clone()),
                local_public_id: local_public_id.to_string(),
                remote_public_id: view.counterpart.counterpart_public_id.clone(),
                direction: message_direction_from_swarm(&view.direction),
                message_kind: message_kind_from_swarm(&view.message_kind),
                content_json: view.content.clone(),
                encrypted_body: view.encrypted_body.clone(),
                content_encoding: view.content_encoding.clone(),
                agent_envelope_json: view.agent_envelope_json.clone(),
                agent_signature: view.agent_signature.clone(),
                delivery_state,
                read_state: ReadState::Unread,
                created_at: view.created_at,
                updated_at: message_updated_at,
            },
        ))?;
        if matches!(
            delivery_state,
            DeliveryState::Delivered | DeliveryState::Acknowledged
        ) {
            ignore_conflict(receipt_service::upsert_message_receipt(
                repository,
                &MessageReceipt {
                    message_id: view.message_id.clone(),
                    receipt_kind: ReceiptKind::Delivered,
                    recorded_at: view.created_at,
                    detail: Some("reconciled from transport delivery state".to_string()),
                },
            ))?;
        }
        if let Some(acknowledged_at) = view.acknowledged_at {
            ignore_conflict(receipt_service::upsert_message_receipt(
                repository,
                &MessageReceipt {
                    message_id: view.message_id.clone(),
                    receipt_kind: ReceiptKind::Acknowledged,
                    recorded_at: acknowledged_at,
                    detail: Some("reconciled from transport acknowledgment".to_string()),
                },
            ))?;
        }
    }
    Ok(())
}

fn ensure_inbound_dm_allowed<R>(
    repository: &R,
    local_public_id: &str,
    view: &DmMessageSyncView,
) -> SocialResult<()>
where
    R: FriendshipRepository,
{
    if message_direction_from_swarm(&view.direction) != MessageDirection::Inbound {
        return Ok(());
    }
    let counterpart_public_id = &view.counterpart.counterpart_public_id;
    let Some(friendship) = repository.find_friendship(local_public_id, counterpart_public_id)?
    else {
        return Err(SocialError::Conflict(format!(
            "inbound_dm_rejected_not_active_friendship: local_public_id={local_public_id}, remote_public_id={counterpart_public_id}, message_id={}",
            view.message_id
        )));
    };
    if friendship.state != FriendshipState::Active {
        return Err(SocialError::Conflict(format!(
            "inbound_dm_rejected_not_active_friendship: local_public_id={local_public_id}, remote_public_id={counterpart_public_id}, state={:?}, message_id={}",
            friendship.state, view.message_id
        )));
    }
    Ok(())
}

pub fn persist_relationship_action<R>(
    repository: &R,
    input: &PersistRelationshipActionInput,
) -> SocialResult<()>
where
    R: RemoteIdentityRepository
        + TransportBindingRepository
        + FriendRequestRepository
        + ReliabilityTaskRepository
        + FriendshipRepository
        + BlockRepository
        + ThreadRepository,
{
    cache_counterpart(repository, &input.counterpart)?;
    let request_id = input
        .request_id
        .clone()
        .or_else(|| {
            friend_request_service::list_friend_requests(repository, &input.local_public_id)
                .ok()
                .and_then(|items| {
                    let counterpart_public_id = &input.counterpart.counterpart_public_id;
                    let request = if input.action == RelationshipAction::Request {
                        latest_pending_request_for_counterpart(&items, counterpart_public_id)
                    } else {
                        latest_request_for_counterpart(&items, counterpart_public_id)
                    };
                    request.map(|item| item.request_id.clone())
                })
        })
        .unwrap_or_else(|| format!("request-{}", input.occurred_at));
    let thread_id = stable_pair_id(
        "dm",
        &input.local_public_id,
        &input.counterpart.counterpart_public_id,
    );
    let friendship_id = stable_pair_id(
        "friendship",
        &input.local_public_id,
        &input.counterpart.counterpart_public_id,
    );
    match input.action {
        RelationshipAction::Request => friend_request_service::upsert_friend_request(
            repository,
            &FriendRequest {
                request_id,
                local_public_id: input.local_public_id.clone(),
                remote_public_id: input.counterpart.counterpart_public_id.clone(),
                remote_node_id: Some(input.counterpart.remote_node_id.clone()),
                direction: FriendRequestDirection::Outbound,
                state: FriendRequestState::Pending,
                decision_reason: None,
                correlation_id: input.correlation_id.clone(),
                created_at: input.occurred_at,
                updated_at: input.occurred_at,
                expires_at: None,
            },
        )?,
        RelationshipAction::Accept => {
            friend_request_service::upsert_friend_request(
                repository,
                &FriendRequest {
                    request_id: request_id.clone(),
                    local_public_id: input.local_public_id.clone(),
                    remote_public_id: input.counterpart.counterpart_public_id.clone(),
                    remote_node_id: Some(input.counterpart.remote_node_id.clone()),
                    direction: FriendRequestDirection::Inbound,
                    state: FriendRequestState::Accepted,
                    decision_reason: Some("accepted".to_string()),
                    correlation_id: input.correlation_id.clone(),
                    created_at: input.occurred_at,
                    updated_at: input.occurred_at,
                    expires_at: None,
                },
            )?;
            friendship_service::upsert_friendship(
                repository,
                &Friendship {
                    friendship_id,
                    local_public_id: input.local_public_id.clone(),
                    remote_public_id: input.counterpart.counterpart_public_id.clone(),
                    display_name: counterpart_display_name(&input.counterpart),
                    state: FriendshipState::Active,
                    established_from_request_id: Some(request_id),
                    thread_id: Some(thread_id.clone()),
                    created_at: input.occurred_at,
                    updated_at: input.occurred_at,
                },
            )?;
            thread_service::upsert_thread(
                repository,
                &DirectThread {
                    thread_id: thread_id.clone(),
                    local_public_id: input.local_public_id.clone(),
                    remote_public_id: input.counterpart.counterpart_public_id.clone(),
                    transport_thread_id: thread_id,
                    state: ThreadState::Ready,
                    last_message_at: None,
                    created_at: input.occurred_at,
                    updated_at: input.occurred_at,
                },
            )?;
            mark_counterpart_identity_state(
                repository,
                &input.counterpart.counterpart_public_id,
                true,
                input.occurred_at,
            )?;
            friend_request_service::settle_related_outbound_friend_requests(
                repository,
                &input.local_public_id,
                friend_request_service::FriendRequestCounterpartRef {
                    public_id: &input.counterpart.counterpart_public_id,
                    remote_node_id: &input.counterpart.remote_node_id,
                    target_agent: &input.counterpart.target_agent,
                },
                FriendRequestState::Accepted,
                "accepted",
                input.occurred_at,
            )?;
        }
        RelationshipAction::Reject => {
            friend_request_service::upsert_friend_request(
                repository,
                &FriendRequest {
                    request_id,
                    local_public_id: input.local_public_id.clone(),
                    remote_public_id: input.counterpart.counterpart_public_id.clone(),
                    remote_node_id: Some(input.counterpart.remote_node_id.clone()),
                    direction: FriendRequestDirection::Inbound,
                    state: FriendRequestState::Rejected,
                    decision_reason: Some("rejected".to_string()),
                    correlation_id: input.correlation_id.clone(),
                    created_at: input.occurred_at,
                    updated_at: input.occurred_at,
                    expires_at: None,
                },
            )?;
            friend_request_service::settle_related_outbound_friend_requests(
                repository,
                &input.local_public_id,
                friend_request_service::FriendRequestCounterpartRef {
                    public_id: &input.counterpart.counterpart_public_id,
                    remote_node_id: &input.counterpart.remote_node_id,
                    target_agent: &input.counterpart.target_agent,
                },
                FriendRequestState::Rejected,
                "rejected",
                input.occurred_at,
            )?;
        }
        RelationshipAction::Cancel => friend_request_service::upsert_friend_request(
            repository,
            &FriendRequest {
                request_id,
                local_public_id: input.local_public_id.clone(),
                remote_public_id: input.counterpart.counterpart_public_id.clone(),
                remote_node_id: Some(input.counterpart.remote_node_id.clone()),
                direction: FriendRequestDirection::Outbound,
                state: FriendRequestState::Cancelled,
                decision_reason: Some("cancelled".to_string()),
                correlation_id: input.correlation_id.clone(),
                created_at: input.occurred_at,
                updated_at: input.occurred_at,
                expires_at: None,
            },
        )?,
        RelationshipAction::Remove => {
            let existing_friendship =
                friendship_service::list_friendships(repository, &input.local_public_id)?
                    .into_iter()
                    .find(|friendship| {
                        friendship.remote_public_id == input.counterpart.counterpart_public_id
                            && friendship.state == FriendshipState::Active
                    });
            let friendship_id = existing_friendship
                .as_ref()
                .map(|friendship| friendship.friendship_id.clone())
                .unwrap_or(friendship_id);
            let display_name = counterpart_display_name(&input.counterpart).or_else(|| {
                existing_friendship
                    .as_ref()
                    .and_then(|item| item.display_name.clone())
            });
            let established_from_request_id = existing_friendship
                .as_ref()
                .and_then(|friendship| friendship.established_from_request_id.clone())
                .or(Some(request_id));
            let thread_id = existing_friendship
                .as_ref()
                .and_then(|friendship| friendship.thread_id.clone())
                .unwrap_or_else(|| thread_id.clone());
            let created_at = existing_friendship
                .as_ref()
                .map(|friendship| friendship.created_at)
                .unwrap_or(input.occurred_at);
            friendship_service::upsert_friendship(
                repository,
                &Friendship {
                    friendship_id,
                    local_public_id: input.local_public_id.clone(),
                    remote_public_id: input.counterpart.counterpart_public_id.clone(),
                    display_name,
                    state: FriendshipState::Removed,
                    established_from_request_id,
                    thread_id: Some(thread_id.clone()),
                    created_at,
                    updated_at: input.occurred_at,
                },
            )?;
            thread_service::upsert_thread(
                repository,
                &DirectThread {
                    thread_id: thread_id.clone(),
                    local_public_id: input.local_public_id.clone(),
                    remote_public_id: input.counterpart.counterpart_public_id.clone(),
                    transport_thread_id: thread_id,
                    state: ThreadState::Closed,
                    last_message_at: None,
                    created_at: input.occurred_at,
                    updated_at: input.occurred_at,
                },
            )?;
            mark_counterpart_identity_state(
                repository,
                &input.counterpart.counterpart_public_id,
                false,
                input.occurred_at,
            )?;
        }
        RelationshipAction::Block => {
            friend_request_service::upsert_friend_request(
                repository,
                &FriendRequest {
                    request_id: request_id.clone(),
                    local_public_id: input.local_public_id.clone(),
                    remote_public_id: input.counterpart.counterpart_public_id.clone(),
                    remote_node_id: Some(input.counterpart.remote_node_id.clone()),
                    direction: FriendRequestDirection::Inbound,
                    state: FriendRequestState::Blocked,
                    decision_reason: Some("blocked".to_string()),
                    correlation_id: input.correlation_id.clone(),
                    created_at: input.occurred_at,
                    updated_at: input.occurred_at,
                    expires_at: None,
                },
            )?;
            block_service::upsert_block(
                repository,
                &SocialBlock {
                    block_id: stable_pair_id(
                        "block",
                        &input.local_public_id,
                        &input.counterpart.counterpart_public_id,
                    ),
                    owner_public_id: input.local_public_id.clone(),
                    blocked_public_id: input.counterpart.counterpart_public_id.clone(),
                    blocked_node_id: Some(input.counterpart.remote_node_id.clone()),
                    reason: Some("blocked".to_string()),
                    created_at: input.occurred_at,
                    updated_at: input.occurred_at,
                },
            )?;
            friendship_service::upsert_friendship(
                repository,
                &Friendship {
                    friendship_id,
                    local_public_id: input.local_public_id.clone(),
                    remote_public_id: input.counterpart.counterpart_public_id.clone(),
                    display_name: counterpart_display_name(&input.counterpart),
                    state: FriendshipState::Blocked,
                    established_from_request_id: Some(request_id),
                    thread_id: Some(thread_id.clone()),
                    created_at: input.occurred_at,
                    updated_at: input.occurred_at,
                },
            )?;
            thread_service::upsert_thread(
                repository,
                &DirectThread {
                    thread_id: thread_id.clone(),
                    local_public_id: input.local_public_id.clone(),
                    remote_public_id: input.counterpart.counterpart_public_id.clone(),
                    transport_thread_id: thread_id,
                    state: ThreadState::Blocked,
                    last_message_at: None,
                    created_at: input.occurred_at,
                    updated_at: input.occurred_at,
                },
            )?;
            friend_request_service::settle_related_outbound_friend_requests(
                repository,
                &input.local_public_id,
                friend_request_service::FriendRequestCounterpartRef {
                    public_id: &input.counterpart.counterpart_public_id,
                    remote_node_id: &input.counterpart.remote_node_id,
                    target_agent: &input.counterpart.target_agent,
                },
                FriendRequestState::Blocked,
                "blocked",
                input.occurred_at,
            )?;
        }
        RelationshipAction::Unblock => block_service::remove_block(
            repository,
            &input.local_public_id,
            &input.counterpart.counterpart_public_id,
        )?,
    }
    Ok(())
}

pub fn persist_dm_message<R>(repository: &R, input: &PersistDmMessageInput) -> SocialResult<()>
where
    R: RemoteIdentityRepository
        + TransportBindingRepository
        + ThreadRepository
        + MessageRepository
        + MessageReceiptRepository,
{
    cache_counterpart(repository, &input.counterpart)?;
    let existing = thread_service::find_thread(
        repository,
        &input.local_public_id,
        &input.counterpart.counterpart_public_id,
    )?;
    let created_at = existing
        .as_ref()
        .map_or(input.occurred_at, |thread| thread.created_at);
    thread_service::upsert_thread(
        repository,
        &DirectThread {
            thread_id: input.thread_id.clone(),
            local_public_id: input.local_public_id.clone(),
            remote_public_id: input.counterpart.counterpart_public_id.clone(),
            transport_thread_id: existing.as_ref().map_or_else(
                || input.thread_id.clone(),
                |thread| thread.transport_thread_id.clone(),
            ),
            state: ThreadState::Ready,
            last_message_at: Some(input.occurred_at),
            created_at,
            updated_at: input.occurred_at,
        },
    )?;
    message_service::upsert_message(
        repository,
        &DirectMessage {
            thread_id: input.thread_id.clone(),
            message_id: input.message_id.clone(),
            transport_message_id: Some(input.message_id.clone()),
            local_public_id: input.local_public_id.clone(),
            remote_public_id: input.counterpart.counterpart_public_id.clone(),
            direction: MessageDirection::Outbound,
            message_kind: MessageKind::Message,
            content_json: input.content.clone(),
            encrypted_body: None,
            content_encoding: None,
            agent_envelope_json: Some(input.agent_envelope_json.clone()),
            agent_signature: input.agent_signature.clone(),
            delivery_state: DeliveryState::Pending,
            read_state: ReadState::Unread,
            created_at: input.occurred_at,
            updated_at: input.occurred_at,
        },
    )?;
    receipt_service::upsert_message_receipt(
        repository,
        &MessageReceipt {
            message_id: input.message_id.clone(),
            receipt_kind: ReceiptKind::Sent,
            recorded_at: input.occurred_at,
            detail: Some("transport accepted".to_string()),
        },
    )?;
    Ok(())
}

fn stable_pair_id(prefix: &str, left: &str, right: &str) -> String {
    if left <= right {
        format!("{prefix}:{left}:{right}")
    } else {
        format!("{prefix}:{right}:{left}")
    }
}

fn max_optional_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn normalize_thread_lifetime(
    mut created_at: i64,
    mut updated_at: i64,
    last_message_at: Option<i64>,
) -> (i64, i64) {
    if let Some(last_message_at) = last_message_at {
        created_at = created_at.min(last_message_at);
        updated_at = updated_at.max(last_message_at);
    }
    updated_at = updated_at.max(created_at);
    (created_at, updated_at)
}

fn retire_alias_friendships<R>(
    repository: &R,
    local_public_id: &str,
    canonical_remote_public_id: &str,
    remote_node_id: &str,
    request_id: &str,
    updated_at: i64,
) -> SocialResult<()>
where
    R: FriendshipRepository + TransportBindingRepository,
{
    let bindings = transport_binding_service::list_transport_bindings(repository)?;
    for friendship in friendship_service::list_friendships(repository, local_public_id)? {
        if friendship.state != FriendshipState::Active
            || friendship.remote_public_id == canonical_remote_public_id
        {
            continue;
        }
        let matches_request = friendship.established_from_request_id.as_deref() == Some(request_id);
        let matches_node = friendship.remote_public_id == remote_node_id
            || bindings.iter().any(|binding| {
                binding.public_id == friendship.remote_public_id
                    && binding.transport_kind == TransportKind::Wattswarm
                    && binding.transport_node_id == remote_node_id
            });
        if !matches_request && !matches_node {
            continue;
        }
        let mut retired = friendship;
        retired.state = FriendshipState::Removed;
        retired.updated_at = retired.updated_at.max(updated_at);
        friendship_service::upsert_friendship(repository, &retired)?;
    }
    Ok(())
}

fn latest_request_for_counterpart<'a>(
    items: &'a [FriendRequest],
    counterpart_public_id: &str,
) -> Option<&'a FriendRequest> {
    items
        .iter()
        .filter(|item| item.remote_public_id == counterpart_public_id)
        .max_by_key(|item| (item.updated_at, item.created_at))
}

fn latest_pending_request_for_counterpart<'a>(
    items: &'a [FriendRequest],
    counterpart_public_id: &str,
) -> Option<&'a FriendRequest> {
    items
        .iter()
        .filter(|item| {
            item.remote_public_id == counterpart_public_id
                && item.state == FriendRequestState::Pending
        })
        .max_by_key(|item| (item.updated_at, item.created_at))
}

fn ignore_conflict(result: SocialResult<()>) -> SocialResult<()> {
    match result {
        Ok(()) | Err(SocialError::Conflict(_)) => Ok(()),
        Err(error) => Err(error),
    }
}

fn thread_state_from_swarm(state: &str) -> ThreadState {
    match state {
        "ready" => ThreadState::Ready,
        "blocked" => ThreadState::Blocked,
        "closed" => ThreadState::Closed,
        _ => ThreadState::Pending,
    }
}

fn message_kind_from_swarm(kind: &str) -> MessageKind {
    match kind {
        "relationship_established" => MessageKind::RelationshipEstablished,
        "session_init" => MessageKind::SessionInit,
        _ => MessageKind::Message,
    }
}

fn message_direction_from_swarm(direction: &str) -> MessageDirection {
    match direction {
        "inbound" => MessageDirection::Inbound,
        _ => MessageDirection::Outbound,
    }
}

fn delivery_state_from_swarm(state: &str) -> DeliveryState {
    match state {
        "delivered" => DeliveryState::Delivered,
        "acknowledged" => DeliveryState::Acknowledged,
        "failed" => DeliveryState::Failed,
        _ => DeliveryState::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SocialStore;

    fn counterpart(now: i64) -> CounterpartSnapshot {
        CounterpartSnapshot {
            counterpart_public_id: "did:key:bob".to_string(),
            target_agent: "did:key:bob".to_string(),
            remote_node_id: "node-bob".to_string(),
            known_identity: None,
            known_binding: None,
            observed_at: now,
        }
    }

    fn seed_friendship(store: &SocialStore, state: FriendshipState) {
        seed_friendship_with_times(store, state, 1, 1);
    }

    fn seed_friendship_with_times(
        store: &SocialStore,
        state: FriendshipState,
        created_at: i64,
        updated_at: i64,
    ) {
        friendship_service::upsert_friendship(
            store,
            &Friendship {
                friendship_id: "friendship:did:key:alice:did:key:bob".to_string(),
                local_public_id: "did:key:alice".to_string(),
                remote_public_id: "did:key:bob".to_string(),
                display_name: Some("Bob".to_string()),
                state,
                established_from_request_id: Some("req-1".to_string()),
                thread_id: Some("dm:alice:bob".to_string()),
                created_at,
                updated_at,
            },
        )
        .expect("seed friendship");
    }

    fn seed_friend_request(store: &SocialStore, state: FriendRequestState) {
        friend_request_service::upsert_friend_request(
            store,
            &FriendRequest {
                request_id: "req-1".to_string(),
                local_public_id: "did:key:alice".to_string(),
                remote_public_id: "did:key:bob".to_string(),
                remote_node_id: Some("node-bob".to_string()),
                direction: FriendRequestDirection::Outbound,
                state,
                decision_reason: None,
                correlation_id: Some("corr-1".to_string()),
                created_at: 1,
                updated_at: 1,
                expires_at: None,
            },
        )
        .expect("seed friend request");
    }

    fn seed_block(store: &SocialStore) {
        block_service::upsert_block(
            store,
            &SocialBlock {
                block_id: "block:did:key:alice:did:key:bob".to_string(),
                owner_public_id: "did:key:alice".to_string(),
                blocked_public_id: "did:key:bob".to_string(),
                blocked_node_id: Some("node-bob".to_string()),
                reason: Some("blocked".to_string()),
                created_at: 1,
                updated_at: 1,
            },
        )
        .expect("seed block");
    }

    #[test]
    fn cache_counterpart_rejects_non_did_agent_ids_for_identity_and_binding() {
        let store = SocialStore::open_in_memory().expect("social store");
        let snapshot = CounterpartSnapshot {
            counterpart_public_id: "agent-remote.fingerprint".to_string(),
            target_agent: "wattswarm-agent-remote".to_string(),
            remote_node_id: "node-remote".to_string(),
            known_identity: Some(KnownIdentitySnapshot {
                public_id: "agent-remote.fingerprint".to_string(),
                agent_did: Some("wattswarm-agent-remote".to_string()),
                display_name: "Remote".to_string(),
                active: true,
                created_at: 1,
            }),
            known_binding: None,
            observed_at: 2,
        };

        cache_counterpart(&store, &snapshot).expect("cache counterpart");

        let identities =
            remote_identity_service::list_remote_identities(&store).expect("remote identities");
        assert!(identities.is_empty());
        let bindings =
            transport_binding_service::list_transport_bindings(&store).expect("bindings");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].agent_did, None);
        assert_eq!(bindings[0].transport_node_id, "node-remote");
    }

    #[test]
    fn cache_counterpart_persists_valid_did_for_identity_and_binding() {
        let store = SocialStore::open_in_memory().expect("social store");
        let snapshot = CounterpartSnapshot {
            counterpart_public_id: "agent-remote.fingerprint".to_string(),
            target_agent: "wattswarm-agent-remote".to_string(),
            remote_node_id: "node-remote".to_string(),
            known_identity: Some(KnownIdentitySnapshot {
                public_id: "agent-remote.fingerprint".to_string(),
                agent_did: Some("did:key:zRemote".to_string()),
                display_name: "Remote".to_string(),
                active: true,
                created_at: 1,
            }),
            known_binding: None,
            observed_at: 2,
        };

        cache_counterpart(&store, &snapshot).expect("cache counterpart");

        let identities =
            remote_identity_service::list_remote_identities(&store).expect("remote identities");
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].agent_did, "did:key:zRemote");
        let bindings =
            transport_binding_service::list_transport_bindings(&store).expect("bindings");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].agent_did.as_deref(), Some("did:key:zRemote"));
    }

    #[test]
    fn persist_relationship_accept_creates_friendship_and_thread() {
        let store = SocialStore::open_in_memory().expect("social store");
        persist_relationship_action(
            &store,
            &PersistRelationshipActionInput {
                local_public_id: "did:key:alice".to_string(),
                counterpart: counterpart(10),
                action: RelationshipAction::Accept,
                request_id: Some("req-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                occurred_at: 10,
            },
        )
        .expect("persist relationship accept");

        let friendships =
            friendship_service::list_friendships(&store, "did:key:alice").expect("friendships");
        let threads = thread_service::list_threads(&store, "did:key:alice").expect("threads");
        assert_eq!(friendships.len(), 1);
        assert_eq!(friendships[0].state, FriendshipState::Active);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].state, ThreadState::Ready);
    }

    #[test]
    fn relationship_response_settles_related_outbound_pending_request_and_clears_retry_task() {
        for (action, expected_state, expected_reason) in [
            (
                RelationshipAction::Accept,
                FriendRequestState::Accepted,
                "accepted",
            ),
            (
                RelationshipAction::Reject,
                FriendRequestState::Rejected,
                "rejected",
            ),
            (
                RelationshipAction::Block,
                FriendRequestState::Blocked,
                "blocked",
            ),
        ] {
            let store = SocialStore::open_in_memory().expect("social store");
            friend_request_service::upsert_friend_request(
                &store,
                &FriendRequest {
                    request_id: "outbound-pending".to_string(),
                    local_public_id: "did:key:alice".to_string(),
                    remote_public_id: "node-bob".to_string(),
                    remote_node_id: Some("node-bob".to_string()),
                    direction: FriendRequestDirection::Outbound,
                    state: FriendRequestState::Pending,
                    decision_reason: None,
                    correlation_id: Some("corr-outbound".to_string()),
                    created_at: 1,
                    updated_at: 1,
                    expires_at: None,
                },
            )
            .expect("seed outbound request");
            store
                .record_reliability_attempt("friend_request", "outbound-pending", 2, 3, None)
                .expect("seed retry task");

            persist_relationship_action(
                &store,
                &PersistRelationshipActionInput {
                    local_public_id: "did:key:alice".to_string(),
                    counterpart: counterpart(10),
                    action,
                    request_id: Some(format!("response-{expected_reason}")),
                    correlation_id: Some("corr-response".to_string()),
                    occurred_at: 10,
                },
            )
            .expect("persist relationship response");

            let requests = friend_request_service::list_friend_requests(&store, "did:key:alice")
                .expect("list requests");
            let settled = requests
                .iter()
                .find(|request| request.request_id == "outbound-pending")
                .expect("settled outbound request");
            assert_eq!(settled.state, expected_state);
            assert_eq!(settled.decision_reason.as_deref(), Some(expected_reason));
            assert_eq!(settled.remote_public_id, "node-bob");
            assert!(
                store
                    .get_reliability_task("friend_request", "outbound-pending")
                    .expect("get retry task")
                    .is_none()
            );
            assert!(
                store
                    .due_outbound_pending_friend_requests(20, 0, 10)
                    .expect("query due requests")
                    .is_empty()
            );
        }
    }

    #[test]
    fn persist_relationship_remove_marks_identity_removed() {
        let store = SocialStore::open_in_memory().expect("social store");
        persist_relationship_action(
            &store,
            &PersistRelationshipActionInput {
                local_public_id: "did:key:alice".to_string(),
                counterpart: counterpart(10),
                action: RelationshipAction::Accept,
                request_id: Some("req-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                occurred_at: 10,
            },
        )
        .expect("persist relationship accept");

        persist_relationship_action(
            &store,
            &PersistRelationshipActionInput {
                local_public_id: "did:key:alice".to_string(),
                counterpart: counterpart(20),
                action: RelationshipAction::Remove,
                request_id: Some("req-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                occurred_at: 20,
            },
        )
        .expect("persist relationship remove");

        let identities =
            remote_identity_service::list_remote_identities(&store).expect("remote identities");
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].public_id, "did:key:bob");
        assert!(!identities[0].active);
        assert_eq!(identities[0].updated_at, 20);
    }

    #[test]
    fn reconcile_relationship_accept_clamps_out_of_order_response_time() {
        let store = SocialStore::open_in_memory().expect("social store");
        reconcile_relationship_views(
            &store,
            "did:key:alice",
            &[RelationshipSyncView {
                counterpart: counterpart(30),
                relationship_state: "accepted".to_string(),
                last_action: Some("accept".to_string()),
                initiated_by: "remote".to_string(),
                request_id: Some("req-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                requested_at: Some(30),
                responded_at: Some(20),
                updated_at: 20,
            }],
        )
        .expect("reconcile accepted relationship");

        let friendships =
            friendship_service::list_friendships(&store, "did:key:alice").expect("friendships");
        assert_eq!(friendships.len(), 1);
        assert_eq!(friendships[0].state, FriendshipState::Active);
        assert_eq!(friendships[0].created_at, 30);
        assert_eq!(friendships[0].updated_at, 30);
    }

    #[test]
    fn reconcile_relationship_request_clamps_out_of_order_update_time() {
        let store = SocialStore::open_in_memory().expect("social store");
        reconcile_relationship_views(
            &store,
            "did:key:alice",
            &[RelationshipSyncView {
                counterpart: counterpart(30),
                relationship_state: "requested".to_string(),
                last_action: Some("request".to_string()),
                initiated_by: "remote".to_string(),
                request_id: Some("req-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                requested_at: Some(30),
                responded_at: None,
                updated_at: 20,
            }],
        )
        .expect("reconcile requested relationship");

        let requests = friend_request_service::list_friend_requests(&store, "did:key:alice")
            .expect("requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].state, FriendRequestState::Pending);
        assert_eq!(requests[0].created_at, 30);
        assert_eq!(requests[0].updated_at, 30);
    }

    #[test]
    fn reconcile_relationship_remove_marks_friendship_removed_and_thread_closed() {
        let store = SocialStore::open_in_memory().expect("social store");
        seed_friendship(&store, FriendshipState::Active);
        thread_service::upsert_thread(
            &store,
            &DirectThread {
                thread_id: "dm:alice:bob".to_string(),
                local_public_id: "did:key:alice".to_string(),
                remote_public_id: "did:key:bob".to_string(),
                transport_thread_id: "transport:alice:bob".to_string(),
                state: ThreadState::Ready,
                last_message_at: Some(70),
                created_at: 1,
                updated_at: 70,
            },
        )
        .expect("seed thread");

        reconcile_relationship_views(
            &store,
            "did:key:alice",
            &[RelationshipSyncView {
                counterpart: counterpart(40),
                relationship_state: "none".to_string(),
                last_action: Some("remove".to_string()),
                initiated_by: "remote".to_string(),
                request_id: Some("req-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                requested_at: Some(10),
                responded_at: Some(40),
                updated_at: 40,
            }],
        )
        .expect("reconcile removed relationship");

        let friendships =
            friendship_service::list_friendships(&store, "did:key:alice").expect("friendships");
        let threads = thread_service::list_threads(&store, "did:key:alice").expect("threads");
        assert_eq!(friendships.len(), 1);
        assert_eq!(friendships[0].state, FriendshipState::Removed);
        assert_eq!(friendships[0].updated_at, 40);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].state, ThreadState::Closed);
        assert_eq!(threads[0].created_at, 1);
        assert_eq!(threads[0].last_message_at, Some(70));
        assert_eq!(threads[0].updated_at, 70);
        let identities =
            remote_identity_service::list_remote_identities(&store).expect("remote identities");
        assert_eq!(identities.len(), 1);
        assert!(!identities[0].active);
        assert_eq!(identities[0].updated_at, 40);
    }

    #[test]
    fn reconcile_relationship_remove_ignores_stale_remove_before_active_friendship() {
        let store = SocialStore::open_in_memory().expect("social store");
        seed_friendship_with_times(&store, FriendshipState::Active, 100, 110);
        thread_service::upsert_thread(
            &store,
            &DirectThread {
                thread_id: "dm:alice:bob".to_string(),
                local_public_id: "did:key:alice".to_string(),
                remote_public_id: "did:key:bob".to_string(),
                transport_thread_id: "transport:alice:bob".to_string(),
                state: ThreadState::Ready,
                last_message_at: Some(110),
                created_at: 100,
                updated_at: 110,
            },
        )
        .expect("seed thread");

        reconcile_relationship_views(
            &store,
            "did:key:alice",
            &[RelationshipSyncView {
                counterpart: counterpart(90),
                relationship_state: "none".to_string(),
                last_action: Some("remove".to_string()),
                initiated_by: "remote".to_string(),
                request_id: Some("req-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                requested_at: Some(10),
                responded_at: Some(90),
                updated_at: 120,
            }],
        )
        .expect("ignore stale remove");

        let friendships =
            friendship_service::list_friendships(&store, "did:key:alice").expect("friendships");
        let threads = thread_service::list_threads(&store, "did:key:alice").expect("threads");
        assert_eq!(friendships.len(), 1);
        assert_eq!(friendships[0].state, FriendshipState::Active);
        assert_eq!(friendships[0].created_at, 100);
        assert_eq!(friendships[0].updated_at, 110);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].state, ThreadState::Ready);
        assert_eq!(threads[0].updated_at, 110);
    }

    #[test]
    fn reconcile_dm_thread_does_not_reactivate_removed_identity() {
        let store = SocialStore::open_in_memory().expect("social store");
        persist_relationship_action(
            &store,
            &PersistRelationshipActionInput {
                local_public_id: "did:key:alice".to_string(),
                counterpart: counterpart(10),
                action: RelationshipAction::Accept,
                request_id: Some("req-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                occurred_at: 10,
            },
        )
        .expect("persist relationship accept");
        persist_relationship_action(
            &store,
            &PersistRelationshipActionInput {
                local_public_id: "did:key:alice".to_string(),
                counterpart: counterpart(20),
                action: RelationshipAction::Remove,
                request_id: Some("req-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                occurred_at: 20,
            },
        )
        .expect("persist relationship remove");

        reconcile_dm_threads(
            &store,
            "did:key:alice",
            &[DmThreadSyncView {
                counterpart: counterpart(30),
                transport_thread_id: "dm:alice:bob".to_string(),
                session_state: "ready".to_string(),
                relationship_established_at: None,
                created_at: 30,
                updated_at: 30,
                last_message_at: None,
            }],
        )
        .expect("reconcile dm thread");

        let identities =
            remote_identity_service::list_remote_identities(&store).expect("remote identities");
        assert_eq!(identities.len(), 1);
        assert!(!identities[0].active);
        assert_eq!(identities[0].updated_at, 30);
    }

    #[test]
    fn reconcile_relationship_cancel_marks_request_cancelled_without_removing_friendship() {
        let store = SocialStore::open_in_memory().expect("social store");
        seed_friend_request(&store, FriendRequestState::Pending);
        seed_friendship(&store, FriendshipState::Active);

        reconcile_relationship_views(
            &store,
            "did:key:alice",
            &[RelationshipSyncView {
                counterpart: counterpart(40),
                relationship_state: "none".to_string(),
                last_action: Some("cancel".to_string()),
                initiated_by: "local".to_string(),
                request_id: Some("req-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                requested_at: Some(10),
                responded_at: Some(40),
                updated_at: 40,
            }],
        )
        .expect("reconcile cancelled relationship");

        let requests = friend_request_service::list_friend_requests(&store, "did:key:alice")
            .expect("requests");
        let friendships =
            friendship_service::list_friendships(&store, "did:key:alice").expect("friendships");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].state, FriendRequestState::Cancelled);
        assert_eq!(friendships.len(), 1);
        assert_eq!(friendships[0].state, FriendshipState::Active);
    }

    #[test]
    fn reconcile_relationship_unblock_removes_block_without_removing_friendship() {
        let store = SocialStore::open_in_memory().expect("social store");
        seed_block(&store);
        seed_friendship(&store, FriendshipState::Blocked);

        reconcile_relationship_views(
            &store,
            "did:key:alice",
            &[RelationshipSyncView {
                counterpart: counterpart(40),
                relationship_state: "none".to_string(),
                last_action: Some("unblock".to_string()),
                initiated_by: "local".to_string(),
                request_id: Some("req-1".to_string()),
                correlation_id: Some("corr-1".to_string()),
                requested_at: Some(10),
                responded_at: Some(40),
                updated_at: 40,
            }],
        )
        .expect("reconcile unblocked relationship");

        let blocks = block_service::list_blocks(&store, "did:key:alice").expect("blocks");
        let friendships =
            friendship_service::list_friendships(&store, "did:key:alice").expect("friendships");
        assert!(blocks.is_empty());
        assert_eq!(friendships.len(), 1);
        assert_eq!(friendships[0].state, FriendshipState::Blocked);
    }

    #[test]
    fn reconcile_dm_message_creates_thread_message_and_receipt() {
        let store = SocialStore::open_in_memory().expect("social store");
        seed_friendship(&store, FriendshipState::Active);
        reconcile_dm_messages(
            &store,
            "did:key:alice",
            &[DmMessageSyncView {
                counterpart: counterpart(20),
                transport_thread_id: "dm:alice:bob".to_string(),
                message_id: "msg-1".to_string(),
                message_kind: "message".to_string(),
                direction: "inbound".to_string(),
                delivery_state: "delivered".to_string(),
                a2a_protocol: "google_a2a".to_string(),
                content: serde_json::json!({"text":"hello"}),
                encrypted_body: None,
                content_encoding: None,
                agent_envelope_json: None,
                agent_signature: None,
                created_at: 20,
                acknowledged_at: Some(21),
            }],
        )
        .expect("reconcile dm messages");

        let threads = thread_service::list_threads(&store, "did:key:alice").expect("threads");
        let messages =
            message_service::list_thread_messages(&store, &threads[0].thread_id).expect("messages");
        let receipts = receipt_service::list_message_receipts(&store, "msg-1").expect("receipts");
        assert_eq!(threads.len(), 1);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].delivery_state, DeliveryState::Delivered);
        assert_eq!(receipts.len(), 2);
    }

    #[test]
    fn reconcile_dm_message_clamps_acknowledged_before_created() {
        let store = SocialStore::open_in_memory().expect("social store");
        seed_friendship(&store, FriendshipState::Active);
        reconcile_dm_messages(
            &store,
            "did:key:alice",
            &[DmMessageSyncView {
                counterpart: counterpart(20),
                transport_thread_id: "dm:alice:bob".to_string(),
                message_id: "msg-early-ack".to_string(),
                message_kind: "message".to_string(),
                direction: "inbound".to_string(),
                delivery_state: "delivered".to_string(),
                a2a_protocol: "google_a2a".to_string(),
                content: serde_json::json!({"text":"hello"}),
                encrypted_body: None,
                content_encoding: None,
                agent_envelope_json: None,
                agent_signature: None,
                created_at: 20,
                acknowledged_at: Some(19),
            }],
        )
        .expect("reconcile dm messages");

        let threads = thread_service::list_threads(&store, "did:key:alice").expect("threads");
        let messages =
            message_service::list_thread_messages(&store, &threads[0].thread_id).expect("messages");
        assert_eq!(threads[0].updated_at, 20);
        assert_eq!(messages[0].updated_at, 20);
    }

    #[test]
    fn reconcile_dm_message_keeps_thread_valid_when_message_predates_thread() {
        let store = SocialStore::open_in_memory().expect("social store");
        seed_friendship(&store, FriendshipState::Active);
        thread_service::upsert_thread(
            &store,
            &DirectThread {
                thread_id: "dm:alice:bob".to_string(),
                local_public_id: "did:key:alice".to_string(),
                remote_public_id: "did:key:bob".to_string(),
                transport_thread_id: "dm:alice:bob".to_string(),
                state: ThreadState::Ready,
                last_message_at: Some(30),
                created_at: 30,
                updated_at: 30,
            },
        )
        .expect("seed thread");

        reconcile_dm_messages(
            &store,
            "did:key:alice",
            &[DmMessageSyncView {
                counterpart: counterpart(20),
                transport_thread_id: "dm:alice:bob".to_string(),
                message_id: "msg-before-thread".to_string(),
                message_kind: "message".to_string(),
                direction: "inbound".to_string(),
                delivery_state: "delivered".to_string(),
                a2a_protocol: "google_a2a".to_string(),
                content: serde_json::json!({"text":"early hello"}),
                encrypted_body: None,
                content_encoding: None,
                agent_envelope_json: None,
                agent_signature: None,
                created_at: 20,
                acknowledged_at: Some(20),
            }],
        )
        .expect("reconcile early dm message");

        let threads = thread_service::list_threads(&store, "did:key:alice").expect("threads");
        let messages =
            message_service::list_thread_messages(&store, "dm:alice:bob").expect("messages");
        assert_eq!(threads[0].created_at, 30);
        assert_eq!(threads[0].last_message_at, Some(30));
        assert_eq!(threads[0].updated_at, 30);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].created_at, 20);
        assert_eq!(messages[0].updated_at, 20);
    }

    #[test]
    fn reconcile_dm_message_rejects_inbound_when_friendship_is_removed() {
        let store = SocialStore::open_in_memory().expect("social store");
        seed_friendship(&store, FriendshipState::Removed);

        let error = reconcile_dm_messages(
            &store,
            "did:key:alice",
            &[DmMessageSyncView {
                counterpart: counterpart(20),
                transport_thread_id: "dm:alice:bob".to_string(),
                message_id: "msg-after-remove".to_string(),
                message_kind: "message".to_string(),
                direction: "inbound".to_string(),
                delivery_state: "delivered".to_string(),
                a2a_protocol: "google_a2a".to_string(),
                content: serde_json::json!({"text":"should be rejected"}),
                encrypted_body: None,
                content_encoding: None,
                agent_envelope_json: None,
                agent_signature: None,
                created_at: 20,
                acknowledged_at: Some(21),
            }],
        )
        .expect_err("reject inbound dm");

        assert!(
            error
                .to_string()
                .contains("inbound_dm_rejected_not_active_friendship")
        );
        assert!(
            thread_service::list_threads(&store, "did:key:alice")
                .expect("threads")
                .is_empty()
        );
        assert!(
            receipt_service::list_message_receipts(&store, "msg-after-remove")
                .expect("receipts")
                .is_empty()
        );
    }
}
