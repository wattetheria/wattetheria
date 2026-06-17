use crate::domain::friend_requests::{FriendRequest, FriendRequestDirection, FriendRequestState};
use crate::ports::repositories::{FriendRequestRepository, ReliabilityTaskRepository};
use crate::types::{SocialError, SocialResult};
use std::collections::{BTreeSet, HashSet};

const FRIEND_REQUEST_OBJECT_KIND: &str = "friend_request";

#[derive(Debug, Clone, Copy)]
pub struct FriendRequestCounterpartRef<'a> {
    pub public_id: &'a str,
    pub remote_node_id: &'a str,
    pub target_agent: &'a str,
}

pub fn upsert_friend_request<R>(repository: &R, request: &FriendRequest) -> SocialResult<()>
where
    R: FriendRequestRepository,
{
    validate_friend_request(request)?;
    let existing_items = repository.list_friend_requests(&request.local_public_id)?;
    let existing = existing_items
        .iter()
        .find(|item| item.request_id == request.request_id);

    if let Some(existing) = existing
        && !existing.can_transition_to(request.state)
    {
        if request.state == FriendRequestState::Pending {
            return Ok(());
        }
        return Err(SocialError::Conflict(format!(
            "invalid friend request transition: {:?} -> {:?}",
            existing.state, request.state
        )));
    }

    let Some(coalesced) = coalesce_friend_request(&existing_items, request) else {
        return Ok(());
    };
    repository.upsert_friend_request(&coalesced)
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
    repository
        .list_friend_requests(local_public_id)
        .map(dedupe_friend_requests)
}

pub fn settle_related_outbound_friend_requests<R>(
    repository: &R,
    local_public_id: &str,
    counterpart: FriendRequestCounterpartRef<'_>,
    state: FriendRequestState,
    decision_reason: &str,
    occurred_at: i64,
) -> SocialResult<()>
where
    R: FriendRequestRepository + ReliabilityTaskRepository,
{
    let counterpart_keys = counterpart_match_keys(counterpart);
    let requests = list_friend_requests(repository, local_public_id)?;
    for request in requests {
        if request.direction != FriendRequestDirection::Outbound
            || request.state != FriendRequestState::Pending
            || !request_matches_counterpart(&request, &counterpart_keys)
        {
            continue;
        }
        upsert_friend_request(
            repository,
            &FriendRequest {
                state,
                decision_reason: Some(decision_reason.to_string()),
                updated_at: request.updated_at.max(occurred_at),
                ..request.clone()
            },
        )?;
        repository.clear_reliability_task(FRIEND_REQUEST_OBJECT_KIND, &request.request_id)?;
    }
    Ok(())
}

fn counterpart_match_keys(counterpart: FriendRequestCounterpartRef<'_>) -> BTreeSet<String> {
    [
        counterpart.public_id,
        counterpart.remote_node_id,
        counterpart.target_agent,
    ]
    .into_iter()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(ToOwned::to_owned)
    .collect()
}

fn request_matches_counterpart(
    request: &FriendRequest,
    counterpart_keys: &BTreeSet<String>,
) -> bool {
    counterpart_keys.contains(request.remote_public_id.trim())
        || request
            .remote_node_id
            .as_deref()
            .map(str::trim)
            .is_some_and(|remote_node_id| counterpart_keys.contains(remote_node_id))
}

fn coalesce_friend_request(
    existing_items: &[FriendRequest],
    request: &FriendRequest,
) -> Option<FriendRequest> {
    if request.state == FriendRequestState::Pending {
        if existing_items.iter().any(|item| {
            same_request_pair(item, request) && item.state != FriendRequestState::Pending
        }) {
            return None;
        }
        return existing_items
            .iter()
            .find(|item| {
                same_request_pair(item, request) && item.state == FriendRequestState::Pending
            })
            .map(|existing| merge_friend_request(existing, request))
            .or_else(|| Some(request.clone()));
    }

    if let Some(existing) = existing_items
        .iter()
        .find(|item| item.request_id == request.request_id)
    {
        return Some(merge_friend_request(existing, request));
    }
    if let Some(existing) = existing_items
        .iter()
        .find(|item| same_request_pair(item, request) && item.state == FriendRequestState::Pending)
    {
        return Some(merge_friend_request(existing, request));
    }
    if let Some(existing) = existing_items
        .iter()
        .find(|item| same_request_pair(item, request) && item.state != FriendRequestState::Pending)
    {
        return (existing.state == request.state).then(|| merge_friend_request(existing, request));
    }
    Some(request.clone())
}

fn merge_friend_request(existing: &FriendRequest, incoming: &FriendRequest) -> FriendRequest {
    FriendRequest {
        request_id: existing.request_id.clone(),
        local_public_id: incoming.local_public_id.clone(),
        remote_public_id: incoming.remote_public_id.clone(),
        remote_node_id: incoming
            .remote_node_id
            .clone()
            .or_else(|| existing.remote_node_id.clone()),
        direction: incoming.direction,
        state: incoming.state,
        decision_reason: incoming
            .decision_reason
            .clone()
            .or_else(|| existing.decision_reason.clone()),
        correlation_id: incoming
            .correlation_id
            .clone()
            .or_else(|| existing.correlation_id.clone()),
        created_at: existing.created_at.min(incoming.created_at),
        updated_at: existing.updated_at.max(incoming.updated_at),
        expires_at: incoming.expires_at.or(existing.expires_at),
    }
}

fn same_request_pair(left: &FriendRequest, right: &FriendRequest) -> bool {
    left.local_public_id == right.local_public_id
        && left.remote_public_id == right.remote_public_id
        && left.direction == right.direction
}

fn dedupe_friend_requests(mut items: Vec<FriendRequest>) -> Vec<FriendRequest> {
    let terminal_pairs = items
        .iter()
        .filter(|item| item.state != FriendRequestState::Pending)
        .map(request_pair_key)
        .collect::<HashSet<_>>();
    let mut pending_pairs = HashSet::new();
    items.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.request_id.cmp(&right.request_id))
    });
    items
        .into_iter()
        .filter(|item| {
            if item.state != FriendRequestState::Pending {
                return true;
            }
            let key = request_pair_key(item);
            !terminal_pairs.contains(&key) && pending_pairs.insert(key)
        })
        .collect()
}

fn request_pair_key(request: &FriendRequest) -> (String, String, &'static str) {
    let direction = match request.direction {
        crate::domain::friend_requests::FriendRequestDirection::Inbound => "inbound",
        crate::domain::friend_requests::FriendRequestDirection::Outbound => "outbound",
    };
    (
        request.local_public_id.clone(),
        request.remote_public_id.clone(),
        direction,
    )
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

    #[test]
    fn coalesces_duplicate_pending_request_for_same_pair() {
        let repository = FakeRepository::default();
        let request = pending_request();
        upsert_friend_request(&repository, &request).expect("save pending");

        let mut duplicate = request;
        duplicate.request_id = "request-2".to_owned();
        duplicate.updated_at = 2;
        upsert_friend_request(&repository, &duplicate).expect("coalesce duplicate pending");

        let requests =
            list_friend_requests(&repository, "did:key:alice").expect("list friend requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].request_id, "request-1");
        assert_eq!(requests[0].updated_at, 2);
    }

    #[test]
    fn ignores_pending_replay_after_terminal_decision() {
        let repository = FakeRepository::default();
        let mut request = pending_request();
        upsert_friend_request(&repository, &request).expect("save pending");
        request.state = FriendRequestState::Rejected;
        request.decision_reason = Some("rejected".to_owned());
        request.updated_at = 2;
        upsert_friend_request(&repository, &request).expect("save rejected");

        let mut replay = pending_request();
        replay.request_id = "request-2".to_owned();
        replay.updated_at = 3;
        upsert_friend_request(&repository, &replay).expect("ignore pending replay");

        let requests =
            list_friend_requests(&repository, "did:key:alice").expect("list friend requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].request_id, "request-1");
        assert_eq!(requests[0].state, FriendRequestState::Rejected);
    }
}
