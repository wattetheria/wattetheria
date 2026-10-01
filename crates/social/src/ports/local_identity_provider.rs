use crate::domain::identities::LocalIdentityContext;
use crate::types::SocialResult;
use async_trait::async_trait;

#[async_trait]
#[allow(
    clippy::double_must_use,
    reason = "async_trait adds must_use to boxed futures"
)]
pub trait LocalIdentityProvider: Send + Sync {
    async fn active_identity(&self) -> SocialResult<LocalIdentityContext>;
}
