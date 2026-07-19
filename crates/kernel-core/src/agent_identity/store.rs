use crate::signing::PayloadSigner;
use anyhow::Result;

/// Storage boundary for the local Agent's long-lived identity.
///
/// Implementations may use a development file, an OS keychain, secure
/// hardware, or a remote key-management service.
pub trait AgentIdentityStore: Send + Sync {
    type Signer: PayloadSigner;

    fn load(&self) -> Result<Self::Signer>;
    fn load_or_create(&self) -> Result<Self::Signer>;
}
