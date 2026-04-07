use crate::domain::identities::RemoteIdentityProfile;
use crate::ports::repositories::RemoteIdentityRepository;
use crate::types::{SocialError, SocialResult};

pub fn upsert_remote_identity<R>(
    repository: &R,
    identity: &RemoteIdentityProfile,
) -> SocialResult<()>
where
    R: RemoteIdentityRepository,
{
    if identity.public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "public_id is required".to_owned(),
        ));
    }
    if identity.agent_did.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "agent_did is required".to_owned(),
        ));
    }
    if identity.display_name.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "display_name is required".to_owned(),
        ));
    }
    if identity.created_at > identity.updated_at {
        return Err(SocialError::InvalidInput(
            "updated_at must be >= created_at".to_owned(),
        ));
    }
    repository.upsert_remote_identity(identity)
}

pub fn list_remote_identities<R>(repository: &R) -> SocialResult<Vec<RemoteIdentityProfile>>
where
    R: RemoteIdentityRepository,
{
    repository.list_remote_identities()
}
