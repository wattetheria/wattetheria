use crate::domain::friend_requests::FriendRequestState;
use crate::domain::friendships::FriendshipState;
use crate::policy::bootstrap::default_policy_rules;
use crate::policy::decisions::{PolicyDecision, PolicyDecisionLog, PolicyEvaluation};
use crate::policy::evaluator::{PolicyEvaluationContext, evaluate};
use crate::policy::rules::{PolicyRule, PolicyRuleType, PolicyScope};
use crate::ports::repositories::{
    BlockRepository, FriendRequestRepository, FriendshipRepository, PolicyDecisionLogRepository,
    PolicyRuleRepository,
};
use crate::types::SocialResult;

pub fn ensure_default_policy_rules<R>(
    repository: &R,
    owner_public_id: &str,
    now: i64,
) -> SocialResult<Vec<PolicyRule>>
where
    R: PolicyRuleRepository,
{
    let existing = repository.list_policy_rules(Some(owner_public_id))?;
    let defaults = default_policy_rules(owner_public_id, now);

    if existing.is_empty() {
        for rule in defaults {
            repository.upsert_policy_rule(&rule)?;
        }
    } else {
        disable_legacy_duplicate_pending_default(repository, owner_public_id, &existing, now)?;
        for rule in defaults {
            if !existing
                .iter()
                .any(|existing_rule| existing_rule.rule_id == rule.rule_id)
            {
                repository.upsert_policy_rule(&rule)?;
            }
        }
    }

    repository.list_policy_rules(Some(owner_public_id))
}

fn disable_legacy_duplicate_pending_default<R>(
    repository: &R,
    owner_public_id: &str,
    existing: &[PolicyRule],
    now: i64,
) -> SocialResult<()>
where
    R: PolicyRuleRepository,
{
    let legacy_rule_id = format!("{owner_public_id}:reject-duplicate-pending-request");
    let Some(legacy_rule) = existing
        .iter()
        .find(|rule| rule.rule_id == legacy_rule_id && rule.enabled)
    else {
        return Ok(());
    };
    if legacy_rule.rule_type != PolicyRuleType::RejectDuplicatePendingRequest
        || legacy_rule.matcher_json != serde_json::json!({"requires_pending_request": true})
    {
        return Ok(());
    }
    let mut disabled = legacy_rule.clone();
    disabled.enabled = false;
    disabled.updated_at = now;
    disabled.config_json =
        serde_json::json!({"reason":"legacy_duplicate_pending_request_disabled"});
    repository.upsert_policy_rule(&disabled)
}

pub fn evaluate_outbound_friend_request_policy<R>(
    repository: &R,
    owner_public_id: &str,
    target_public_id: &str,
    target_node_id: Option<&str>,
    now: i64,
) -> SocialResult<PolicyEvaluation>
where
    R: PolicyRuleRepository
        + PolicyDecisionLogRepository
        + BlockRepository
        + FriendRequestRepository
        + FriendshipRepository,
{
    ensure_default_policy_rules(repository, owner_public_id, now)?;
    let all_rules = repository.list_policy_rules(None)?;
    let rules = rules_for_owner(&all_rules, owner_public_id);
    let blocked = repository
        .find_block(owner_public_id, target_public_id)?
        .is_some();
    let has_pending_request = repository
        .list_friend_requests(owner_public_id)?
        .into_iter()
        .any(|request| {
            request.remote_public_id == target_public_id
                && request.state == FriendRequestState::Pending
        });
    let has_active_friendship = repository
        .find_friendship(owner_public_id, target_public_id)?
        .map(|friendship| friendship.state == FriendshipState::Active)
        .unwrap_or(false);
    let context = PolicyEvaluationContext {
        scope: PolicyScope::FriendRequestsOutbound,
        owner_public_id: owner_public_id.to_string(),
        target_public_id: target_public_id.to_string(),
        target_node_id: target_node_id.map(ToOwned::to_owned),
        has_block: blocked,
        has_active_friendship,
        has_pending_request,
    };
    let evaluation = evaluate(&rules, &context);
    append_decision_log(
        repository,
        owner_public_id,
        PolicyScope::FriendRequestsOutbound,
        target_public_id,
        target_node_id,
        &evaluation,
        now,
    )?;
    Ok(evaluation)
}

pub fn evaluate_outbound_dm_policy<R>(
    repository: &R,
    owner_public_id: &str,
    target_public_id: &str,
    target_node_id: Option<&str>,
    now: i64,
) -> SocialResult<PolicyEvaluation>
where
    R: PolicyRuleRepository + PolicyDecisionLogRepository + BlockRepository + FriendshipRepository,
{
    ensure_default_policy_rules(repository, owner_public_id, now)?;
    let all_rules = repository.list_policy_rules(None)?;
    let rules = rules_for_owner(&all_rules, owner_public_id);
    let blocked = repository
        .find_block(owner_public_id, target_public_id)?
        .is_some();
    let has_active_friendship = repository
        .find_friendship(owner_public_id, target_public_id)?
        .map(|friendship| friendship.state == FriendshipState::Active)
        .unwrap_or(false);
    let context = PolicyEvaluationContext {
        scope: PolicyScope::DirectMessagesOutbound,
        owner_public_id: owner_public_id.to_string(),
        target_public_id: target_public_id.to_string(),
        target_node_id: target_node_id.map(ToOwned::to_owned),
        has_block: blocked,
        has_active_friendship,
        has_pending_request: false,
    };
    let evaluation = evaluate(&rules, &context);
    append_decision_log(
        repository,
        owner_public_id,
        PolicyScope::DirectMessagesOutbound,
        target_public_id,
        target_node_id,
        &evaluation,
        now,
    )?;
    Ok(evaluation)
}

fn rules_for_owner(rules: &[PolicyRule], owner_public_id: &str) -> Vec<PolicyRule> {
    rules
        .iter()
        .filter(|rule| {
            rule.owner_public_id.is_none()
                || rule.owner_public_id.as_deref() == Some(owner_public_id)
        })
        .cloned()
        .collect()
}

fn append_decision_log<R>(
    repository: &R,
    owner_public_id: &str,
    scope: PolicyScope,
    target_public_id: &str,
    target_node_id: Option<&str>,
    evaluation: &PolicyEvaluation,
    now: i64,
) -> SocialResult<()>
where
    R: PolicyDecisionLogRepository,
{
    let rule_fragment = evaluation
        .matched_rule_id
        .as_deref()
        .unwrap_or("no_rule")
        .replace(':', "_");
    let decision_fragment = match evaluation.decision {
        PolicyDecision::Allow => "allow",
        PolicyDecision::Deny => "deny",
    };
    repository.append_policy_decision_log(&PolicyDecisionLog {
        decision_id: format!(
            "policy:{owner_public_id}:{target_public_id}:{decision_fragment}:{rule_fragment}:{now}"
        ),
        owner_public_id: owner_public_id.to_string(),
        scope,
        target_public_id: target_public_id.to_string(),
        target_node_id: target_node_id.map(ToOwned::to_owned),
        rule_id: evaluation.matched_rule_id.clone(),
        decision: evaluation.decision,
        reason: evaluation.reason.clone(),
        context_json: evaluation.context_json.clone(),
        created_at: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::blocks::SocialBlock;
    use crate::domain::friend_requests::{FriendRequest, FriendRequestDirection};
    use crate::domain::friendships::Friendship;
    use crate::policy::decisions::PolicyDecisionLog;
    use crate::ports::repositories::{
        BlockRepository, FriendRequestRepository, FriendshipRepository,
        PolicyDecisionLogRepository, PolicyRuleRepository,
    };
    use crate::types::SocialResult;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakePolicyRepository {
        rules: Mutex<Vec<PolicyRule>>,
        blocks: Mutex<Vec<SocialBlock>>,
        requests: Mutex<Vec<FriendRequest>>,
        friendships: Mutex<Vec<Friendship>>,
        logs: Mutex<Vec<PolicyDecisionLog>>,
    }

    impl PolicyRuleRepository for FakePolicyRepository {
        fn upsert_policy_rule(&self, rule: &PolicyRule) -> SocialResult<()> {
            let mut rules = self.rules.lock().expect("rules mutex");
            if let Some(existing) = rules.iter_mut().find(|item| item.rule_id == rule.rule_id) {
                *existing = rule.clone();
            } else {
                rules.push(rule.clone());
            }
            Ok(())
        }

        fn list_policy_rules(
            &self,
            owner_public_id: Option<&str>,
        ) -> SocialResult<Vec<PolicyRule>> {
            let rules = self.rules.lock().expect("rules mutex");
            Ok(rules
                .iter()
                .filter(|rule| {
                    owner_public_id.is_none() || rule.owner_public_id.as_deref() == owner_public_id
                })
                .cloned()
                .collect())
        }
    }

    impl PolicyDecisionLogRepository for FakePolicyRepository {
        fn append_policy_decision_log(&self, log: &PolicyDecisionLog) -> SocialResult<()> {
            self.logs.lock().expect("logs mutex").push(log.clone());
            Ok(())
        }

        fn list_policy_decision_logs(
            &self,
            owner_public_id: &str,
        ) -> SocialResult<Vec<PolicyDecisionLog>> {
            Ok(self
                .logs
                .lock()
                .expect("logs mutex")
                .iter()
                .filter(|log| log.owner_public_id == owner_public_id)
                .cloned()
                .collect())
        }
    }

    impl BlockRepository for FakePolicyRepository {
        fn upsert_block(&self, block: &SocialBlock) -> SocialResult<()> {
            self.blocks
                .lock()
                .expect("blocks mutex")
                .push(block.clone());
            Ok(())
        }
        fn remove_block(
            &self,
            _owner_public_id: &str,
            _blocked_public_id: &str,
        ) -> SocialResult<()> {
            Ok(())
        }
        fn find_block(
            &self,
            owner_public_id: &str,
            blocked_public_id: &str,
        ) -> SocialResult<Option<SocialBlock>> {
            Ok(self
                .blocks
                .lock()
                .expect("blocks mutex")
                .iter()
                .find(|block| {
                    block.owner_public_id == owner_public_id
                        && block.blocked_public_id == blocked_public_id
                })
                .cloned())
        }
        fn list_blocks(&self, owner_public_id: &str) -> SocialResult<Vec<SocialBlock>> {
            Ok(self
                .blocks
                .lock()
                .expect("blocks mutex")
                .iter()
                .filter(|block| block.owner_public_id == owner_public_id)
                .cloned()
                .collect())
        }
    }

    impl FriendRequestRepository for FakePolicyRepository {
        fn upsert_friend_request(&self, request: &FriendRequest) -> SocialResult<()> {
            self.requests
                .lock()
                .expect("requests mutex")
                .push(request.clone());
            Ok(())
        }
        fn list_friend_requests(&self, local_public_id: &str) -> SocialResult<Vec<FriendRequest>> {
            Ok(self
                .requests
                .lock()
                .expect("requests mutex")
                .iter()
                .filter(|request| request.local_public_id == local_public_id)
                .cloned()
                .collect())
        }
    }

    impl FriendshipRepository for FakePolicyRepository {
        fn upsert_friendship(&self, friendship: &Friendship) -> SocialResult<()> {
            self.friendships
                .lock()
                .expect("friendships mutex")
                .push(friendship.clone());
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
                .find(|friendship| {
                    friendship.local_public_id == local_public_id
                        && friendship.remote_public_id == remote_public_id
                })
                .cloned())
        }
        fn list_friendships(&self, local_public_id: &str) -> SocialResult<Vec<Friendship>> {
            Ok(self
                .friendships
                .lock()
                .expect("friendships mutex")
                .iter()
                .filter(|friendship| friendship.local_public_id == local_public_id)
                .cloned()
                .collect())
        }
    }

    #[test]
    fn bootstrap_defaults_is_idempotent() {
        let repository = FakePolicyRepository::default();

        let first = ensure_default_policy_rules(&repository, "did:key:test", 1).expect("bootstrap");
        let second =
            ensure_default_policy_rules(&repository, "did:key:test", 2).expect("bootstrap again");

        assert_eq!(first.len(), second.len());
        assert_eq!(first, second);
    }

    #[test]
    fn outbound_friend_request_denies_when_blocked_and_logs_decision() {
        let repository = FakePolicyRepository::default();
        repository
            .upsert_block(&SocialBlock {
                block_id: "block-1".to_string(),
                owner_public_id: "did:key:alice".to_string(),
                blocked_public_id: "did:key:bob".to_string(),
                blocked_node_id: Some("node-bob".to_string()),
                reason: Some("blocked".to_string()),
                created_at: 1,
                updated_at: 1,
            })
            .expect("insert block");

        let evaluation = evaluate_outbound_friend_request_policy(
            &repository,
            "did:key:alice",
            "did:key:bob",
            Some("node-bob"),
            10,
        )
        .expect("evaluate friend request");

        assert_eq!(evaluation.decision, PolicyDecision::Deny);
        assert_eq!(evaluation.reason, "blocked_agent");
        assert_eq!(
            repository
                .list_policy_decision_logs("did:key:alice")
                .expect("logs")
                .len(),
            1
        );
    }

    #[test]
    fn outbound_friend_request_allows_pending_retry_by_default() {
        let repository = FakePolicyRepository::default();
        repository
            .upsert_friend_request(&FriendRequest {
                request_id: "request-1".to_string(),
                local_public_id: "did:key:alice".to_string(),
                remote_public_id: "did:key:bob".to_string(),
                remote_node_id: Some("node-bob".to_string()),
                direction: FriendRequestDirection::Outbound,
                state: FriendRequestState::Pending,
                decision_reason: None,
                correlation_id: None,
                created_at: 1,
                updated_at: 1,
                expires_at: None,
            })
            .expect("insert pending request");

        let evaluation = evaluate_outbound_friend_request_policy(
            &repository,
            "did:key:alice",
            "did:key:bob",
            Some("node-bob"),
            10,
        )
        .expect("evaluate friend request");

        assert_eq!(evaluation.decision, PolicyDecision::Allow);
        assert_eq!(evaluation.reason, "no_matching_rule");
        assert_eq!(
            repository
                .list_policy_decision_logs("did:key:alice")
                .expect("logs")
                .len(),
            1
        );
    }

    #[test]
    fn outbound_friend_request_denies_active_friendship_by_default() {
        let repository = FakePolicyRepository::default();
        repository
            .upsert_friendship(&Friendship {
                friendship_id: "friendship-1".to_string(),
                local_public_id: "did:key:alice".to_string(),
                remote_public_id: "did:key:bob".to_string(),
                display_name: None,
                state: FriendshipState::Active,
                established_from_request_id: Some("request-1".to_string()),
                thread_id: Some("dm:1".to_string()),
                created_at: 1,
                updated_at: 1,
            })
            .expect("insert friendship");

        let evaluation = evaluate_outbound_friend_request_policy(
            &repository,
            "did:key:alice",
            "did:key:bob",
            Some("node-bob"),
            10,
        )
        .expect("evaluate friend request");

        assert_eq!(evaluation.decision, PolicyDecision::Deny);
        assert_eq!(evaluation.reason, "already_friends");
    }

    #[test]
    fn default_policy_migration_disables_legacy_duplicate_pending_rule() {
        let repository = FakePolicyRepository::default();
        repository
            .upsert_policy_rule(&PolicyRule {
                rule_id: "did:key:alice:reject-duplicate-pending-request".to_string(),
                owner_public_id: Some("did:key:alice".to_string()),
                rule_type: PolicyRuleType::RejectDuplicatePendingRequest,
                scope: PolicyScope::FriendRequestsOutbound,
                matcher_json: serde_json::json!({"requires_pending_request": true}),
                config_json: serde_json::json!({"reason":"duplicate_pending_request"}),
                priority: 20,
                enabled: true,
                created_at: 1,
                updated_at: 1,
            })
            .expect("insert legacy rule");
        repository
            .upsert_friend_request(&FriendRequest {
                request_id: "request-1".to_string(),
                local_public_id: "did:key:alice".to_string(),
                remote_public_id: "did:key:bob".to_string(),
                remote_node_id: Some("node-bob".to_string()),
                direction: FriendRequestDirection::Outbound,
                state: FriendRequestState::Pending,
                decision_reason: None,
                correlation_id: None,
                created_at: 1,
                updated_at: 1,
                expires_at: None,
            })
            .expect("insert pending request");

        let evaluation = evaluate_outbound_friend_request_policy(
            &repository,
            "did:key:alice",
            "did:key:bob",
            Some("node-bob"),
            10,
        )
        .expect("evaluate friend request");

        assert_eq!(evaluation.decision, PolicyDecision::Allow);
        assert_eq!(evaluation.reason, "no_matching_rule");
        let rules = repository
            .list_policy_rules(Some("did:key:alice"))
            .expect("rules");
        assert!(rules.iter().any(|rule| {
            rule.rule_id == "did:key:alice:reject-duplicate-pending-request" && !rule.enabled
        }));
        assert!(rules.iter().any(|rule| {
            rule.rule_id == "did:key:alice:reject-active-friendship" && rule.enabled
        }));
    }

    #[test]
    fn outbound_dm_allows_active_friendship_and_logs_decision() {
        let repository = FakePolicyRepository::default();
        repository
            .upsert_friendship(&Friendship {
                friendship_id: "friendship-1".to_string(),
                local_public_id: "did:key:alice".to_string(),
                remote_public_id: "did:key:bob".to_string(),
                display_name: None,
                state: FriendshipState::Active,
                established_from_request_id: None,
                thread_id: Some("dm:1".to_string()),
                created_at: 1,
                updated_at: 1,
            })
            .expect("insert friendship");

        let evaluation = evaluate_outbound_dm_policy(
            &repository,
            "did:key:alice",
            "did:key:bob",
            Some("node-bob"),
            10,
        )
        .expect("evaluate dm");

        assert_eq!(evaluation.decision, PolicyDecision::Allow);
        assert_eq!(evaluation.reason, "active_friendship");
        assert_eq!(
            repository
                .list_policy_decision_logs("did:key:alice")
                .expect("logs")
                .len(),
            1
        );
    }

    #[test]
    fn outbound_dm_denies_when_not_friends_by_default_and_logs_decision() {
        let repository = FakePolicyRepository::default();

        let evaluation = evaluate_outbound_dm_policy(
            &repository,
            "did:key:alice",
            "did:key:bob",
            Some("node-bob"),
            10,
        )
        .expect("evaluate dm");

        assert_eq!(evaluation.decision, PolicyDecision::Deny);
        assert_eq!(evaluation.reason, "friendship_required");
        assert_eq!(
            repository
                .list_policy_decision_logs("did:key:alice")
                .expect("logs")
                .len(),
            1
        );
    }
}
