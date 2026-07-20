use super::ServiceAgentIdentity;
use anyhow::Result;

/// Storage boundary for independently keyed Service Agent identities.
pub trait ServiceAgentIdentityStore: Send + Sync {
    fn load(&self, agent_id: &str) -> Result<ServiceAgentIdentity>;

    fn load_or_create(&self, agent_id: &str, endpoint_url: &str) -> Result<ServiceAgentIdentity>;
}
