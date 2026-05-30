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
    MessageRepository, RemoteIdentityRepository, ThreadRepository, TransportBindingRepository,
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

pub fn cache_counterpart<R>(repository: &R, snapshot: &CounterpartSnapshot) -> SocialResult<()>
where
    R: RemoteIdentityRepository + TransportBindingRepository,
{
    let (public_id, agent_did, display_name, active, created_at) = snapshot
        .known_identity
        .as_ref()
        .map(|identity| {
            (
                identity.public_id.clone(),
                identity
                    .agent_did
                    .clone()
                    .unwrap_or_else(|| snapshot.target_agent.clone()),
                identity.display_name.clone(),
                identity.active,
                identity.created_at,
            )
        })
        .unwrap_or_else(|| {
            (
                snapshot.counterpart_public_id.clone(),
                snapshot.target_agent.clone(),
                snapshot.counterpart_public_id.clone(),
                true,
                snapshot.observed_at,
            )
        });
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
            agent_did: Some(snapshot.target_agent.clone()),
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

pub fn reconcile_relationship_views<R>(
    repository: &R,
    local_public_id: &str,
    views: &[RelationshipSyncView],
) -> SocialResult<()>
where
    R: RemoteIdentityRepository
        + TransportBindingRepository
        + FriendRequestRepository
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
        let updated_at = view.responded_at.unwrap_or(view.updated_at);
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

        match view.relationship_state.as_str() {
            "requested" => {
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
                        updated_at: view.updated_at,
                        expires_at: None,
                    },
                ))?;
            }
            "accepted" => {
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
                        state: FriendshipState::Active,
                        established_from_request_id: Some(request_id),
                        thread_id: Some(thread_id),
                        created_at: requested_at,
                        updated_at,
                    },
                ))?;
            }
            "rejected" => {
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
            }
            "blocked" => {
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
                        state: FriendshipState::Blocked,
                        established_from_request_id: Some(request_id),
                        thread_id: Some(thread_id),
                        created_at: requested_at,
                        updated_at,
                    },
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
            .map(|thread| thread.created_at)
            .unwrap_or(view.created_at);
        ignore_conflict(thread_service::upsert_thread(
            repository,
            &DirectThread {
                thread_id: social_thread_id.clone(),
                local_public_id: local_public_id.to_string(),
                remote_public_id: view.counterpart.counterpart_public_id.clone(),
                transport_thread_id: view.transport_thread_id.clone(),
                state: thread_state_from_swarm(&view.session_state),
                last_message_at: view.last_message_at,
                created_at,
                updated_at: view.updated_at,
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
                    state: FriendshipState::Active,
                    established_from_request_id: None,
                    thread_id: Some(social_thread_id),
                    created_at,
                    updated_at: view.updated_at,
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
        + ThreadRepository
        + MessageRepository
        + MessageReceiptRepository,
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
            .map(|thread| thread.created_at)
            .unwrap_or(view.created_at);
        let updated_at = view
            .acknowledged_at
            .unwrap_or(view.created_at)
            .max(view.created_at);
        ignore_conflict(thread_service::upsert_thread(
            repository,
            &DirectThread {
                thread_id: social_thread_id.clone(),
                local_public_id: local_public_id.to_string(),
                remote_public_id: view.counterpart.counterpart_public_id.clone(),
                transport_thread_id: view.transport_thread_id.clone(),
                state: ThreadState::Ready,
                last_message_at: Some(view.created_at),
                created_at,
                updated_at,
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
                updated_at,
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

pub fn persist_relationship_action<R>(
    repository: &R,
    input: &PersistRelationshipActionInput,
) -> SocialResult<()>
where
    R: RemoteIdentityRepository
        + TransportBindingRepository
        + FriendRequestRepository
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
                    latest_request_for_counterpart(&items, &input.counterpart.counterpart_public_id)
                        .map(|item| item.request_id.clone())
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
        }
        RelationshipAction::Reject => friend_request_service::upsert_friend_request(
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
        )?,
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
            friendship_service::upsert_friendship(
                repository,
                &Friendship {
                    friendship_id,
                    local_public_id: input.local_public_id.clone(),
                    remote_public_id: input.counterpart.counterpart_public_id.clone(),
                    state: FriendshipState::Removed,
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
                    state: ThreadState::Closed,
                    last_message_at: None,
                    created_at: input.occurred_at,
                    updated_at: input.occurred_at,
                },
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
    fn reconcile_dm_message_creates_thread_message_and_receipt() {
        let store = SocialStore::open_in_memory().expect("social store");
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
}
