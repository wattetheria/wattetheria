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

    /// A removed friendship means "no relationship": like re-adding a deleted
    /// contact, only a different, newly accepted request may make it active
    /// again. Stale data from the request that formed it can never revive it.
    #[must_use]
    pub fn is_reestablished_by(&self, next: &Friendship) -> bool {
        self.state == FriendshipState::Removed
            && next.state == FriendshipState::Active
            && next
                .established_from_request_id
                .as_deref()
                .is_some_and(|request_id| {
                    self.established_from_request_id.as_deref() != Some(request_id)
                })
    }
}
