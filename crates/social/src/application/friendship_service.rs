use crate::domain::friendships::Friendship;
use crate::ports::repositories::FriendshipRepository;
use crate::types::{SocialError, SocialResult};

pub fn upsert_friendship<R>(repository: &R, friendship: &Friendship) -> SocialResult<()>
where
    R: FriendshipRepository,
{
    if friendship.friendship_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "friendship_id is required".to_owned(),
        ));
    }
    if friendship.local_public_id.trim().is_empty() || friendship.remote_public_id.trim().is_empty()
    {
        return Err(SocialError::InvalidInput(
            "local_public_id and remote_public_id are required".to_owned(),
        ));
    }
    if friendship.created_at > friendship.updated_at {
        return Err(SocialError::InvalidInput(
            "updated_at must be >= created_at".to_owned(),
        ));
    }
    if let Some(existing) =
        repository.find_friendship(&friendship.local_public_id, &friendship.remote_public_id)?
        && !existing.can_transition_to(friendship.state)
    {
        return Err(SocialError::Conflict(format!(
            "invalid friendship transition: {:?} -> {:?}",
            existing.state, friendship.state
        )));
    }
    repository.upsert_friendship(friendship)
}

pub fn list_friendships<R>(repository: &R, local_public_id: &str) -> SocialResult<Vec<Friendship>>
where
    R: FriendshipRepository,
{
    if local_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "local_public_id is required".to_owned(),
        ));
    }
    repository.list_friendships(local_public_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::friendships::FriendshipState;
    use crate::ports::repositories::FriendshipRepository;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeRepository {
        friendships: Mutex<Vec<Friendship>>,
    }

    impl FriendshipRepository for FakeRepository {
        fn upsert_friendship(&self, friendship: &Friendship) -> SocialResult<()> {
            let mut friendships = self.friendships.lock().expect("friendships mutex");
            if let Some(existing) = friendships.iter_mut().find(|item| {
                item.local_public_id == friendship.local_public_id
                    && item.remote_public_id == friendship.remote_public_id
            }) {
                *existing = friendship.clone();
            } else {
                friendships.push(friendship.clone());
            }
            Ok(())
        }

        fn find_friendship(
            &self,
            local_public_id: &str,
            remote_public_id: &str,
        ) -> SocialResult<Option<Friendship>> {
            Ok(self
                .friendships
                .lock()
                .expect("friendships mutex")
                .iter()
                .find(|item| {
                    item.local_public_id == local_public_id
                        && item.remote_public_id == remote_public_id
                })
                .cloned())
        }

        fn list_friendships(&self, local_public_id: &str) -> SocialResult<Vec<Friendship>> {
            Ok(self
                .friendships
                .lock()
                .expect("friendships mutex")
                .iter()
                .filter(|item| item.local_public_id == local_public_id)
                .cloned()
                .collect())
        }
    }

    #[test]
    fn rejects_invalid_friendship_transition() {
        let repository = FakeRepository::default();
        let mut friendship = Friendship {
            friendship_id: "friendship-1".to_owned(),
            local_public_id: "did:key:alice".to_owned(),
            remote_public_id: "did:key:bob".to_owned(),
            display_name: None,
            state: FriendshipState::Active,
            established_from_request_id: Some("request-1".to_owned()),
            thread_id: Some("thread-1".to_owned()),
            created_at: 1,
            updated_at: 1,
        };
        upsert_friendship(&repository, &friendship).expect("save active");

        friendship.state = FriendshipState::Removed;
        friendship.updated_at = 2;
        upsert_friendship(&repository, &friendship).expect("save removed");

        friendship.state = FriendshipState::Active;
        friendship.updated_at = 3;
        let error =
            upsert_friendship(&repository, &friendship).expect_err("reject invalid transition");

        assert!(matches!(error, SocialError::Conflict(_)));
    }
}
