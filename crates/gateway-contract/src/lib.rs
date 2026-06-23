use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataKind {
    Presence,
    Identity,
    OperatorProfile,
    OrganizationSummary,
    MissionLifecycle,
    TaskSummary,
    TaskVoteSignal,
    TaskRoundUpdate,
    TaskDecisionFinalized,
    GovernanceProposal,
    GovernanceVote,
    GovernanceDecision,
    OracleFeedUpdate,
    SettlementEvent,
    ReputationUpdate,
    RankingProjection,
    HiveMetadata,
    HiveSubscription,
    HiveMessagePosted,
    HiveActivity,
    FriendRelationship,
    FriendRequestPending,
    PublicBlock,
    SocialThread,
    DmSummary,
    DmMessage,
    NetworkProjection,
    TravelState,
    GalaxyEvent,
    WorldEvent,
}

pub const ALL_DATA_KINDS: [DataKind; 30] = [
    DataKind::Presence,
    DataKind::Identity,
    DataKind::OperatorProfile,
    DataKind::OrganizationSummary,
    DataKind::MissionLifecycle,
    DataKind::TaskSummary,
    DataKind::TaskVoteSignal,
    DataKind::TaskRoundUpdate,
    DataKind::TaskDecisionFinalized,
    DataKind::GovernanceProposal,
    DataKind::GovernanceVote,
    DataKind::GovernanceDecision,
    DataKind::OracleFeedUpdate,
    DataKind::SettlementEvent,
    DataKind::ReputationUpdate,
    DataKind::RankingProjection,
    DataKind::HiveMetadata,
    DataKind::HiveSubscription,
    DataKind::HiveMessagePosted,
    DataKind::HiveActivity,
    DataKind::FriendRelationship,
    DataKind::FriendRequestPending,
    DataKind::PublicBlock,
    DataKind::SocialThread,
    DataKind::DmSummary,
    DataKind::DmMessage,
    DataKind::NetworkProjection,
    DataKind::TravelState,
    DataKind::GalaxyEvent,
    DataKind::WorldEvent,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    #[default]
    Public,
    Protected,
    DebugOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProvisionalExportPolicy {
    #[default]
    NeverBeforeConfirmation,
    ProvisionalWithDowngrade,
    EphemeralOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[allow(clippy::struct_field_names)]
pub struct EventScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeEventPayload {
    pub event_id: String,
    pub node_id: String,
    pub public_key: String,
    pub signer_agent_did: String,
    pub seq: u64,
    pub timestamp: i64,
    pub data_kind: DataKind,
    pub event_kind: String,
    #[serde(default)]
    pub visibility: Visibility,
    #[serde(default)]
    pub provisional_policy: ProvisionalExportPolicy,
    #[serde(default)]
    pub scope: EventScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_key: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedNodeEvent {
    pub payload: NodeEventPayload,
    pub signature: String,
}
