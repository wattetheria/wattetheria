use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyScope {
    Global,
    FriendRequestsInbound,
    FriendRequestsOutbound,
    DirectMessagesInbound,
    DirectMessagesOutbound,
    Blocks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyRuleType {
    RejectBlockedAgent,
    RejectDuplicatePendingRequest,
    RejectActiveFriendship,
    AllowDirectMessageForFriends,
    DenyDirectMessageWhenBlocked,
    DenyDirectMessageWhenNotFriends,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub rule_id: String,
    pub owner_public_id: Option<String>,
    pub rule_type: PolicyRuleType,
    pub scope: PolicyScope,
    pub matcher_json: serde_json::Value,
    pub config_json: serde_json::Value,
    pub priority: i32,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}
