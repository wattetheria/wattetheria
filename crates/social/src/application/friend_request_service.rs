use crate::domain::friend_requests::{FriendRequest, FriendRequestState};
use crate::ports::repositories::FriendRequestRepository;
use crate::types::{SocialError, SocialResult};

pub fn upsert_friend_request<R>(repository: &R, request: &FriendRequest) -> SocialResult<()>
where
    R: FriendRequestRepository,
{
    validate_friend_request(request)?;
    let existing = repository
        .list_friend_requests(&request.local_public_id)?
        .into_iter()
        .find(|item| item.request_id == request.request_id);

    if let Some(existing) = existing
        && !existing.can_transition_to(request.state)
    {
        return Err(SocialError::Conflict(format!(
            "invalid friend request transition: {:?} -> {:?}",
            existing.state, request.state
        )));
    }

    repository.upsert_friend_request(request)
}

pub fn list_friend_requests<R>(
    repository: &R,
    local_public_id: &str,
) -> SocialResult<Vec<FriendRequest>>
where
    R: FriendRequestRepository,
{
    if local_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "local_public_id is required".to_owned(),
        ));
    }
    repository.list_friend_requests(local_public_id)
}

fn validate_friend_request(request: &FriendRequest) -> SocialResult<()> {
    if request.request_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "request_id is required".to_owned(),
        ));
    }
    if request.local_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "local_public_id is required".to_owned(),
        ));
    }
    if request.remote_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "remote_public_id is required".to_owned(),
        ));
    }
    if request.created_at > request.updated_at {
        return Err(SocialError::InvalidInput(
            "updated_at must be >= created_at".to_owned(),
        ));
    }
    if request.state != FriendRequestState::Pending && request.decision_reason.is_none() {
        return Err(SocialError::InvalidInput(
            "decision_reason is required for non-pending requests".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::friend_requests::{FriendRequestDirection, FriendRequestState};
    use crate::ports::repositories::FriendRequestRepository;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeRepository {
        requests: Mutex<Vec<FriendRequest>>,
    }

    impl FriendRequestRepository for FakeRepository {
        fn upsert_friend_request(&self, request: &FriendRequest) -> SocialResult<()> {
            let mut requests = self.requests.lock().expect("requests mutex");
            if let Some(existing) = requests
                .iter_mut()
                .find(|item| item.request_id == request.request_id)
            {
                *existing = request.clone();
            } else {
                requests.push(request.clone());
            }
            Ok(())
        }

        fn list_friend_requests(&self, local_public_id: &str) -> SocialResult<Vec<FriendRequest>> {
            let requests = self.requests.lock().expect("requests mutex");
            Ok(requests
                .iter()
                .filter(|item| item.local_public_id == local_public_id)
                .cloned()
                .collect())
        }
    }

    fn pending_request() -> FriendRequest {
        FriendRequest {
            request_id: "request-1".to_owned(),
            local_public_id: "did:key:alice".to_owned(),
            remote_public_id: "did:key:bob".to_owned(),
            remote_node_id: Some("peer-bob".to_owned()),
            direction: FriendRequestDirection::Outbound,
            state: FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: None,
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        }
    }

    #[test]
    fn rejects_invalid_terminal_transition() {
        let repository = FakeRepository::default();
        let mut request = pending_request();
        upsert_friend_request(&repository, &request).expect("save pending");

        request.state = FriendRequestState::Accepted;
        request.decision_reason = Some("accepted".to_owned());
        request.updated_at = 2;
        upsert_friend_request(&repository, &request).expect("save accepted");

        request.state = FriendRequestState::Rejected;
        request.decision_reason = Some("rejected".to_owned());
        request.updated_at = 3;
        let error =
            upsert_friend_request(&repository, &request).expect_err("reject invalid transition");

        assert!(matches!(error, SocialError::Conflict(_)));
    }
}
