use crate::domain::blocks::SocialBlock;
use crate::ports::repositories::BlockRepository;
use crate::types::{SocialError, SocialResult};

pub fn upsert_block<R>(repository: &R, block: &SocialBlock) -> SocialResult<()>
where
    R: BlockRepository,
{
    if block.block_id.trim().is_empty() {
        return Err(SocialError::InvalidInput("block_id is required".to_owned()));
    }
    if block.owner_public_id.trim().is_empty() || block.blocked_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "owner_public_id and blocked_public_id are required".to_owned(),
        ));
    }
    if block.created_at > block.updated_at {
        return Err(SocialError::InvalidInput(
            "updated_at must be >= created_at".to_owned(),
        ));
    }
    repository.upsert_block(block)
}

pub fn remove_block<R>(
    repository: &R,
    owner_public_id: &str,
    blocked_public_id: &str,
) -> SocialResult<()>
where
    R: BlockRepository,
{
    if owner_public_id.trim().is_empty() || blocked_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "owner_public_id and blocked_public_id are required".to_owned(),
        ));
    }
    repository.remove_block(owner_public_id, blocked_public_id)
}

pub fn list_blocks<R>(repository: &R, owner_public_id: &str) -> SocialResult<Vec<SocialBlock>>
where
    R: BlockRepository,
{
    if owner_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "owner_public_id is required".to_owned(),
        ));
    }
    repository.list_blocks(owner_public_id)
}
