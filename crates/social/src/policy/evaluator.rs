use crate::policy::decisions::{PolicyDecision, PolicyEvaluation};
use crate::policy::rules::{PolicyRule, PolicyRuleType, PolicyScope};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEvaluationContext {
    pub scope: PolicyScope,
    pub owner_public_id: String,
    pub target_public_id: String,
    pub target_node_id: Option<String>,
    pub has_block: bool,
    pub has_active_friendship: bool,
    pub has_pending_request: bool,
}

pub fn evaluate(rule_set: &[PolicyRule], context: &PolicyEvaluationContext) -> PolicyEvaluation {
    let mut ordered_rules = rule_set
        .iter()
        .filter(|rule| rule.enabled && rule_applies_to_scope(rule, context.scope))
        .collect::<Vec<_>>();
    ordered_rules.sort_by_key(|rule| (rule.priority, rule.rule_id.as_str()));

    for rule in ordered_rules {
        if !matcher_matches(rule.matcher_json.as_object(), context) {
            continue;
        }
        let decision = decision_for_rule_type(rule.rule_type);
        let reason = rule
            .config_json
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_else(|| default_reason_for_rule_type(rule.rule_type))
            .to_string();
        return PolicyEvaluation {
            decision,
            matched_rule_id: Some(rule.rule_id.clone()),
            reason,
            context_json: context_to_json(context),
        };
    }

    PolicyEvaluation {
        decision: PolicyDecision::Allow,
        matched_rule_id: None,
        reason: "no_matching_rule".to_string(),
        context_json: context_to_json(context),
    }
}

fn rule_applies_to_scope(rule: &PolicyRule, scope: PolicyScope) -> bool {
    rule.scope == PolicyScope::Global || rule.scope == scope
}

fn matcher_matches(
    matcher: Option<&serde_json::Map<String, Value>>,
    context: &PolicyEvaluationContext,
) -> bool {
    let Some(matcher) = matcher else {
        return true;
    };

    if let Some(expected) = matcher.get("target_public_id").and_then(Value::as_str)
        && expected != context.target_public_id
    {
        return false;
    }
    if let Some(prefix) = matcher
        .get("target_public_id_prefix")
        .and_then(Value::as_str)
        && !context.target_public_id.starts_with(prefix)
    {
        return false;
    }
    if let Some(expected) = matcher.get("target_node_id").and_then(Value::as_str)
        && context.target_node_id.as_deref() != Some(expected)
    {
        return false;
    }
    if let Some(required) = matcher.get("requires_blocked").and_then(Value::as_bool)
        && context.has_block != required
    {
        return false;
    }
    if let Some(required) = matcher
        .get("requires_active_friendship")
        .and_then(Value::as_bool)
        && context.has_active_friendship != required
    {
        return false;
    }
    if let Some(required) = matcher
        .get("requires_pending_request")
        .and_then(Value::as_bool)
        && context.has_pending_request != required
    {
        return false;
    }

    true
}

fn decision_for_rule_type(rule_type: PolicyRuleType) -> PolicyDecision {
    match rule_type {
        PolicyRuleType::AllowDirectMessageForFriends => PolicyDecision::Allow,
        PolicyRuleType::RejectBlockedAgent
        | PolicyRuleType::RejectDuplicatePendingRequest
        | PolicyRuleType::RejectActiveFriendship
        | PolicyRuleType::DenyDirectMessageWhenBlocked
        | PolicyRuleType::DenyDirectMessageWhenNotFriends => PolicyDecision::Deny,
    }
}

fn default_reason_for_rule_type(rule_type: PolicyRuleType) -> &'static str {
    match rule_type {
        PolicyRuleType::RejectBlockedAgent => "blocked_agent",
        PolicyRuleType::RejectDuplicatePendingRequest => "duplicate_pending_request",
        PolicyRuleType::RejectActiveFriendship => "already_friends",
        PolicyRuleType::AllowDirectMessageForFriends => "active_friendship",
        PolicyRuleType::DenyDirectMessageWhenBlocked => "blocked",
        PolicyRuleType::DenyDirectMessageWhenNotFriends => "friendship_required",
    }
}

fn context_to_json(context: &PolicyEvaluationContext) -> Value {
    json!({
        "scope": context.scope,
        "owner_public_id": context.owner_public_id,
        "target_public_id": context.target_public_id,
        "target_node_id": context.target_node_id,
        "has_block": context.has_block,
        "has_active_friendship": context.has_active_friendship,
        "has_pending_request": context.has_pending_request,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::rules::PolicyRule;
    use serde_json::json;

    fn context() -> PolicyEvaluationContext {
        PolicyEvaluationContext {
            scope: PolicyScope::DirectMessagesOutbound,
            owner_public_id: "did:key:alice".to_string(),
            target_public_id: "did:key:bob".to_string(),
            target_node_id: Some("node-bob".to_string()),
            has_block: false,
            has_active_friendship: true,
            has_pending_request: false,
        }
    }

    #[test]
    fn matching_rule_returns_decision_and_reason() {
        let rules = vec![PolicyRule {
            rule_id: "allow-friends".to_string(),
            owner_public_id: Some("did:key:alice".to_string()),
            rule_type: PolicyRuleType::AllowDirectMessageForFriends,
            scope: PolicyScope::DirectMessagesOutbound,
            matcher_json: json!({"requires_active_friendship": true}),
            config_json: json!({"reason":"active_friendship"}),
            priority: 10,
            enabled: true,
            created_at: 1,
            updated_at: 1,
        }];

        let evaluation = evaluate(&rules, &context());
        assert_eq!(evaluation.decision, PolicyDecision::Allow);
        assert_eq!(evaluation.matched_rule_id.as_deref(), Some("allow-friends"));
        assert_eq!(evaluation.reason, "active_friendship");
    }

    #[test]
    fn no_matching_rule_falls_back_to_allow() {
        let rules = vec![PolicyRule {
            rule_id: "block-only".to_string(),
            owner_public_id: Some("did:key:alice".to_string()),
            rule_type: PolicyRuleType::DenyDirectMessageWhenBlocked,
            scope: PolicyScope::DirectMessagesOutbound,
            matcher_json: json!({"requires_blocked": true}),
            config_json: json!({"reason":"blocked"}),
            priority: 10,
            enabled: true,
            created_at: 1,
            updated_at: 1,
        }];

        let evaluation = evaluate(&rules, &context());
        assert_eq!(evaluation.decision, PolicyDecision::Allow);
        assert_eq!(evaluation.reason, "no_matching_rule");
    }
}
