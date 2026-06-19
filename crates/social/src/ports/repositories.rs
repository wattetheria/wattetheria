use crate::domain::blocks::SocialBlock;
use crate::domain::friend_requests::FriendRequest;
use crate::domain::friendships::Friendship;
use crate::domain::identities::RemoteIdentityProfile;
use crate::domain::messages::DirectMessage;
use crate::domain::receipts::MessageReceipt;
use crate::domain::threads::{DirectThread, ThreadState};
use crate::domain::transport_bindings::RemoteTransportBinding;
use crate::policy::decisions::PolicyDecisionLog;
use crate::policy::rules::PolicyRule;
use crate::types::SocialResult;

pub trait RemoteIdentityRepository {
    fn upsert_remote_identity(&self, identity: &RemoteIdentityProfile) -> SocialResult<()>;
    fn update_remote_identity_display_name(
        &self,
        public_id: &str,
        display_name: &str,
    ) -> SocialResult<()>;
    fn get_remote_identity(&self, public_id: &str) -> SocialResult<Option<RemoteIdentityProfile>>;
    fn list_remote_identities(&self) -> SocialResult<Vec<RemoteIdentityProfile>>;
}

pub trait TransportBindingRepository {
    fn upsert_transport_binding(&self, binding: &RemoteTransportBinding) -> SocialResult<()>;
    fn list_transport_bindings_for_public_id(
        &self,
        public_id: &str,
    ) -> SocialResult<Vec<RemoteTransportBinding>>;
    fn list_transport_bindings(&self) -> SocialResult<Vec<RemoteTransportBinding>>;
}

pub trait FriendRequestRepository {
    fn upsert_friend_request(&self, request: &FriendRequest) -> SocialResult<()>;
    fn list_friend_requests(&self, local_public_id: &str) -> SocialResult<Vec<FriendRequest>>;
}

pub trait ReliabilityTaskRepository {
    fn clear_reliability_task(&self, object_kind: &str, object_id: &str) -> SocialResult<()>;
}

pub trait FriendshipRepository {
    fn upsert_friendship(&self, friendship: &Friendship) -> SocialResult<()>;
    fn find_friendship(
        &self,
        local_public_id: &str,
        remote_public_id: &str,
    ) -> SocialResult<Option<Friendship>>;
    fn list_friendships(&self, local_public_id: &str) -> SocialResult<Vec<Friendship>>;
}

pub trait BlockRepository {
    fn upsert_block(&self, block: &SocialBlock) -> SocialResult<()>;
    fn remove_block(&self, owner_public_id: &str, blocked_public_id: &str) -> SocialResult<()>;
    fn find_block(
        &self,
        owner_public_id: &str,
        blocked_public_id: &str,
    ) -> SocialResult<Option<SocialBlock>>;
    fn list_blocks(&self, owner_public_id: &str) -> SocialResult<Vec<SocialBlock>>;
}

pub trait ThreadRepository {
    fn upsert_thread(&self, thread: &DirectThread) -> SocialResult<()>;
    fn find_thread(
        &self,
        local_public_id: &str,
        remote_public_id: &str,
    ) -> SocialResult<Option<DirectThread>>;
    fn list_threads_by_state(
        &self,
        local_public_id: &str,
        state: ThreadState,
    ) -> SocialResult<Vec<DirectThread>>;
    fn list_threads(&self, local_public_id: &str) -> SocialResult<Vec<DirectThread>>;
}

pub trait MessageRepository {
    fn upsert_message(&self, message: &DirectMessage) -> SocialResult<()>;
    fn get_message(&self, thread_id: &str, message_id: &str)
    -> SocialResult<Option<DirectMessage>>;
    fn list_thread_messages(&self, thread_id: &str) -> SocialResult<Vec<DirectMessage>>;
}

pub trait MessageReceiptRepository {
    fn upsert_message_receipt(&self, receipt: &MessageReceipt) -> SocialResult<()>;
    fn list_message_receipts(&self, message_id: &str) -> SocialResult<Vec<MessageReceipt>>;
}

pub trait PolicyRuleRepository {
    fn upsert_policy_rule(&self, rule: &PolicyRule) -> SocialResult<()>;
    fn list_policy_rules(&self, owner_public_id: Option<&str>) -> SocialResult<Vec<PolicyRule>>;
}

pub trait PolicyDecisionLogRepository {
    fn append_policy_decision_log(&self, log: &PolicyDecisionLog) -> SocialResult<()>;
    fn list_policy_decision_logs(
        &self,
        owner_public_id: &str,
    ) -> SocialResult<Vec<PolicyDecisionLog>>;
}
