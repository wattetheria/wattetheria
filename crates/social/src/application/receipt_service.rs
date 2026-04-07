use crate::domain::receipts::MessageReceipt;
use crate::ports::repositories::MessageReceiptRepository;
use crate::types::{SocialError, SocialResult};

pub fn upsert_message_receipt<R>(repository: &R, receipt: &MessageReceipt) -> SocialResult<()>
where
    R: MessageReceiptRepository,
{
    if receipt.message_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "message_id is required".to_owned(),
        ));
    }
    repository.upsert_message_receipt(receipt)
}

pub fn list_message_receipts<R>(
    repository: &R,
    message_id: &str,
) -> SocialResult<Vec<MessageReceipt>>
where
    R: MessageReceiptRepository,
{
    if message_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "message_id is required".to_owned(),
        ));
    }
    repository.list_message_receipts(message_id)
}
