use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FriendRequestDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FriendRequestState {
    Pending,
    Accepted,
    Rejected,
    Blocked,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FriendRequest {
    pub request_id: String,
    pub local_public_id: String,
    pub remote_public_id: String,
    pub remote_node_id: Option<String>,
    pub direction: FriendRequestDirection,
    pub state: FriendRequestState,
    pub decision_reason: Option<String>,
    pub correlation_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: Option<i64>,
}

impl FriendRequest {
    #[must_use]
    pub fn can_transition_to(&self, next: FriendRequestState) -> bool {
        if self.state == next {
            return true;
        }
        match self.state {
            FriendRequestState::Pending => matches!(
                next,
                FriendRequestState::Accepted
                    | FriendRequestState::Rejected
                    | FriendRequestState::Blocked
                    | FriendRequestState::Cancelled
                    | FriendRequestState::Expired
            ),
            FriendRequestState::Accepted
            | FriendRequestState::Rejected
            | FriendRequestState::Blocked
            | FriendRequestState::Cancelled
            | FriendRequestState::Expired => false,
        }
    }
}
