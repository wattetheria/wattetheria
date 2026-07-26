use super::ServiceAgentIdentity;
use anyhow::Result;

/// Storage boundary for independently keyed Service Agent identities.
pub trait ServiceAgentIdentityStore: Send + Sync {
    fn load(&self, service_agent_identity_id: &str) -> Result<ServiceAgentIdentity>;
}
