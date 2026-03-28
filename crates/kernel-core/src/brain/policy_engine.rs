//! Executable policy engine with pending approvals and grants.

use anyhow::{Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::capabilities::{CapabilityPolicy, TrustLevel};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrantScope {
    Once,
    Session,
    Permanent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityGrant {
    pub grant_id: String,
    pub created_at: i64,
    pub approved_by: String,
    pub subject_pattern: String,
    pub capability_pattern: String,
    pub scope: GrantScope,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRequest {
    pub request_id: String,
    pub timestamp: i64,
    pub subject: String,
    pub trust: TrustLevel,
    pub capability: String,
    pub reason: Option<String>,
    pub input_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Allowed,
    DeniedPendingApproval,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub decision: DecisionKind,
    pub reason: String,
    pub request_id: Option<String>,
    pub grant_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PolicyState {
    pub grants: Vec<CapabilityGrant>,
    pub pending: Vec<CapabilityRequest>,
}

#[derive(Debug, Clone)]
pub struct PolicyEngine {
    session_id: String,
    base: CapabilityPolicy,
    state: PolicyState,
}

impl PolicyEngine {
    #[must_use]
    pub fn new(session_id: impl Into<String>, base: CapabilityPolicy, state: PolicyState) -> Self {
        Self {
            session_id: session_id.into(),
            base,
            state,
        }
    }

    pub fn evaluate(&mut self, request: CapabilityRequest) -> PolicyDecision {
        if let Some(grant) = self.find_matching_grant(&request) {
            let grant_id = grant.grant_id.clone();
            let scope = grant.scope;
            let decision = PolicyDecision {
                decision: DecisionKind::Allowed,
                reason: "approved_by_grant".to_string(),
                request_id: None,
                grant_id: Some(grant_id.clone()),
            };
            if scope == GrantScope::Once {
                self.state.grants.retain(|g| g.grant_id != grant_id);
            }
            return decision;
        }

        if self.base.is_allowed(request.trust, &request.capability)
            && !is_high_risk(&request.capability)
        {
            return PolicyDecision {
                decision: DecisionKind::Allowed,
                reason: "allowed_by_baseline_policy".to_string(),
                request_id: None,
                grant_id: None,
            };
        }

        if let Some(existing) = self
            .state
            .pending
            .iter()
            .find(|pending| {
                pending.subject == request.subject
                    && pending.capability == request.capability
                    && pending.trust == request.trust
            })
            .cloned()
        {
            return PolicyDecision {
                decision: DecisionKind::DeniedPendingApproval,
                reason: "pending_existing_approval".to_string(),
                request_id: Some(existing.request_id),
                grant_id: None,
            };
        }

        let mut pending = request;
        if pending.request_id.is_empty() {
            pending.request_id = Uuid::new_v4().to_string();
        }
        if pending.timestamp == 0 {
            pending.timestamp = Utc::now().timestamp();
        }

        self.state.pending.push(pending.clone());

        PolicyDecision {
            decision: DecisionKind::DeniedPendingApproval,
            reason: "approval_required".to_string(),
            request_id: Some(pending.request_id),
            grant_id: None,
        }
    }

    pub fn approve_pending(
        &mut self,
        request_id: &str,
        approved_by: &str,
        scope: GrantScope,
    ) -> Result<CapabilityGrant> {
        let index = self
            .state
            .pending
            .iter()
            .position(|req| req.request_id == request_id)
            .ok_or_else(|| anyhow::anyhow!("pending request not found"))?;

        let request = self.state.pending.remove(index);
        let grant = CapabilityGrant {
            grant_id: Uuid::new_v4().to_string(),
            created_at: Utc::now().timestamp(),
            approved_by: approved_by.to_string(),
            subject_pattern: request.subject,
            capability_pattern: request.capability,
            scope,
            session_id: if scope == GrantScope::Session {
                Some(self.session_id.clone())
            } else {
                None
            },
        };

        self.state.grants.push(grant.clone());
        Ok(grant)
    }

    pub fn reject_pending(&mut self, request_id: &str) -> Result<()> {
        let before = self.state.pending.len();
        self.state
            .pending
            .retain(|request| request.request_id != request_id);
        if self.state.pending.len() == before {
            bail!("pending request not found");
        }
        Ok(())
    }

    pub fn revoke_grant(&mut self, grant_id: &str) -> Result<()> {
        let before = self.state.grants.len();
        self.state.grants.retain(|grant| grant.grant_id != grant_id);
        if self.state.grants.len() == before {
            bail!("grant not found");
        }
        Ok(())
    }

    #[must_use]
    pub fn list_pending(&self) -> Vec<CapabilityRequest> {
        self.state.pending.clone()
    }

    #[must_use]
    pub fn list_grants(&self) -> Vec<CapabilityGrant> {
        self.state.grants.clone()
    }

    fn find_matching_grant(&self, request: &CapabilityRequest) -> Option<&CapabilityGrant> {
        self.state.grants.iter().find(|grant| {
            matches_pattern(&grant.subject_pattern, &request.subject)
                && matches_pattern(&grant.capability_pattern, &request.capability)
                && match grant.scope {
                    GrantScope::Session => grant
                        .session_id
                        .as_ref()
                        .is_some_and(|session| session == &self.session_id),
                    GrantScope::Once | GrantScope::Permanent => true,
                }
        })
    }

    #[must_use]
    pub fn state(&self) -> &PolicyState {
        &self.state
    }
}

fn is_high_risk(capability: &str) -> bool {
    let base = capability.split(':').next().unwrap_or(capability);
    matches!(
        base,
        "proc.exec"
            | "wallet.sign"
            | "wallet.send"
            | "fs.write"
            | "net.outbound"
            | "oracle.publish"
            | "p2p.publish"
    )
}

fn matches_pattern(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    pattern == value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_risk_needs_approval_and_once_grant_is_consumed() {
        let mut engine = PolicyEngine::new(
            "session-a",
            CapabilityPolicy::default(),
            PolicyState::default(),
        );

        let first = engine.evaluate(CapabilityRequest {
            request_id: String::new(),
            timestamp: 0,
            subject: "controller:echo".to_string(),
            trust: TrustLevel::Verified,
            capability: "p2p.publish".to_string(),
            reason: Some("test".to_string()),
            input_digest: None,
        });
        assert_eq!(first.decision, DecisionKind::DeniedPendingApproval);
        let pending_id = first.request_id.clone().unwrap();

        let grant = engine
            .approve_pending(&pending_id, "operator", GrantScope::Once)
            .unwrap();
        assert_eq!(grant.scope, GrantScope::Once);

        let second = engine.evaluate(CapabilityRequest {
            request_id: String::new(),
            timestamp: 0,
            subject: "controller:echo".to_string(),
            trust: TrustLevel::Verified,
            capability: "p2p.publish".to_string(),
            reason: Some("test".to_string()),
            input_digest: None,
        });
        assert_eq!(second.decision, DecisionKind::Allowed);

        let third = engine.evaluate(CapabilityRequest {
            request_id: String::new(),
            timestamp: 0,
            subject: "controller:echo".to_string(),
            trust: TrustLevel::Verified,
            capability: "p2p.publish".to_string(),
            reason: Some("test".to_string()),
            input_digest: None,
        });
        assert_eq!(third.decision, DecisionKind::DeniedPendingApproval);
    }

    #[test]
    fn revoke_grant_removes_access() {
        let mut engine = PolicyEngine::new(
            "session-a",
            CapabilityPolicy::default(),
            PolicyState::default(),
        );

        // Create a pending request for a high-risk capability.
        let decision = engine.evaluate(CapabilityRequest {
            request_id: String::new(),
            timestamp: 0,
            subject: "controller:test".to_string(),
            trust: TrustLevel::Verified,
            capability: "wallet.sign".to_string(),
            reason: Some("revoke-test".to_string()),
            input_digest: None,
        });
        let request_id = decision.request_id.unwrap();

        // Approve it permanently.
        let grant = engine
            .approve_pending(&request_id, "operator", GrantScope::Permanent)
            .unwrap();
        let grant_id = grant.grant_id.clone();

        // Verify it grants access.
        let allowed = engine.evaluate(CapabilityRequest {
            request_id: String::new(),
            timestamp: 0,
            subject: "controller:test".to_string(),
            trust: TrustLevel::Verified,
            capability: "wallet.sign".to_string(),
            reason: Some("test".to_string()),
            input_digest: None,
        });
        assert_eq!(allowed.decision, DecisionKind::Allowed);

        // Revoke it.
        engine.revoke_grant(&grant_id).unwrap();

        // Now access should be denied again.
        let denied = engine.evaluate(CapabilityRequest {
            request_id: String::new(),
            timestamp: 0,
            subject: "controller:test".to_string(),
            trust: TrustLevel::Verified,
            capability: "wallet.sign".to_string(),
            reason: Some("test".to_string()),
            input_digest: None,
        });
        assert_eq!(denied.decision, DecisionKind::DeniedPendingApproval);

        // Revoking a non-existent grant should fail.
        assert!(engine.revoke_grant("nonexistent").is_err());
    }

    #[test]
    fn low_risk_uses_baseline_policy() {
        let mut engine = PolicyEngine::new(
            "session-a",
            CapabilityPolicy::default(),
            PolicyState::default(),
        );

        let result = engine.evaluate(CapabilityRequest {
            request_id: String::new(),
            timestamp: 0,
            subject: "controller:reader".to_string(),
            trust: TrustLevel::Untrusted,
            capability: "fs.read:/galaxy".to_string(),
            reason: None,
            input_digest: None,
        });

        assert_eq!(result.decision, DecisionKind::Allowed);
        assert!(engine.list_pending().is_empty());
    }
}
