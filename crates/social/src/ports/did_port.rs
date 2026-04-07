use crate::types::SocialResult;

pub trait DidPort: Send + Sync {
    fn validate_agent_did(&self, agent_did: &str) -> SocialResult<()>;
    fn validate_binding_proof(&self, binding_proof_json: &serde_json::Value) -> SocialResult<()>;
}
