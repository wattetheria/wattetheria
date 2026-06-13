use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FriendshipState {
    Active,
    Removed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Friendship {
    pub friendship_id: String,
    pub local_public_id: String,
    pub remote_public_id: String,
    pub display_name: Option<String>,
    pub state: FriendshipState,
    pub established_from_request_id: Option<String>,
    pub thread_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Friendship {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state == FriendshipState::Active
    }

    #[must_use]
    pub fn can_transition_to(&self, next: FriendshipState) -> bool {
        match (self.state, next) {
            (FriendshipState::Active, FriendshipState::Active) => true,
            (FriendshipState::Removed, FriendshipState::Removed) => true,
            (FriendshipState::Blocked, FriendshipState::Blocked) => true,
            (FriendshipState::Active, FriendshipState::Removed | FriendshipState::Blocked) => true,
            (FriendshipState::Removed | FriendshipState::Blocked, _) => false,
        }
    }
}
