use crate::domain::identities::LocalIdentityContext;
use crate::types::SocialResult;
use async_trait::async_trait;

#[async_trait]
pub trait LocalIdentityProvider: Send + Sync {
    async fn active_identity(&self) -> SocialResult<LocalIdentityContext>;
}
