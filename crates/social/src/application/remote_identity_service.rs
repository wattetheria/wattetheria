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

pub fn refresh_remote_display_name<R>(
    repository: &R,
    public_id: &str,
    display_name: &str,
) -> SocialResult<()>
where
    R: RemoteIdentityRepository,
{
    if public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "public_id is required".to_owned(),
        ));
    }
    let display_name = display_name.trim();
    if display_name.is_empty() {
        return Err(SocialError::InvalidInput(
            "display_name is required".to_owned(),
        ));
    }
    let Some(identity) = repository.get_remote_identity(public_id)? else {
        return Ok(());
    };
    if identity.display_name.trim() == display_name {
        return Ok(());
    }
    repository.update_remote_identity_display_name(public_id, display_name)
}

pub fn list_remote_identities<R>(repository: &R) -> SocialResult<Vec<RemoteIdentityProfile>>
where
    R: RemoteIdentityRepository,
{
    repository.list_remote_identities()
}
