//! Capability policy model for controlled extension execution.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    Trusted,
    Verified,
    Untrusted,
}

#[derive(Debug, Clone)]
pub struct CapabilityPolicy {
    allowed: HashMap<TrustLevel, BTreeSet<&'static str>>,
}

impl Default for CapabilityPolicy {
    fn default() -> Self {
        let mut allowed = HashMap::new();
        allowed.insert(
            TrustLevel::Trusted,
            BTreeSet::from([
                "fs.read",
                "fs.write",
                "net.outbound",
                "proc.exec",
                "wallet.sign",
                "wallet.send",
                "mcp.call",
                "model.invoke",
                "oracle.publish",
                "oracle.subscribe",
                "p2p.publish",
            ]),
        );
        allowed.insert(
            TrustLevel::Verified,
            BTreeSet::from([
                "fs.read",
                "fs.write",
                "net.outbound",
                "mcp.call",
                "model.invoke",
                "oracle.subscribe",
                "p2p.publish",
            ]),
        );
        allowed.insert(
            TrustLevel::Untrusted,
            BTreeSet::from(["fs.read", "model.invoke"]),
        );
        Self { allowed }
    }
}

impl CapabilityPolicy {
    #[must_use]
    pub fn is_allowed(&self, trust: TrustLevel, capability: &str) -> bool {
        let base = capability.split(':').next().unwrap_or(capability);
        self.allowed
            .get(&trust)
            .is_some_and(|set| set.contains(capability) || set.contains(base))
    }

    pub fn assert_allowed(&self, trust: TrustLevel, capability: &str) -> anyhow::Result<()> {
        if !self.is_allowed(trust, capability) {
            anyhow::bail!("capability denied: {trust:?}:{capability}");
        }
        Ok(())
    }

    #[must_use]
    pub fn list_allowed(&self, trust: TrustLevel) -> Vec<String> {
        self.allowed
            .get(&trust)
            .map(|set| set.iter().map(|s| (*s).to_string()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_level_boundaries() {
        let policy = CapabilityPolicy::default();
        assert!(policy.is_allowed(TrustLevel::Trusted, "proc.exec"));
        assert!(!policy.is_allowed(TrustLevel::Untrusted, "proc.exec"));
        assert!(
            policy
                .assert_allowed(TrustLevel::Untrusted, "wallet.send")
                .is_err()
        );
    }
}
