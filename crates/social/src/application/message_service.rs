use crate::domain::messages::DirectMessage;
use crate::ports::repositories::MessageRepository;
use crate::types::{SocialError, SocialResult};

pub fn upsert_message<R>(repository: &R, message: &DirectMessage) -> SocialResult<()>
where
    R: MessageRepository,
{
    if message.thread_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "thread_id is required".to_owned(),
        ));
    }
    if message.message_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "message_id is required".to_owned(),
        ));
    }
    if message.local_public_id.trim().is_empty() || message.remote_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "local_public_id and remote_public_id are required".to_owned(),
        ));
    }
    if message.created_at > message.updated_at {
        return Err(SocialError::InvalidInput(
            "updated_at must be >= created_at".to_owned(),
        ));
    }
    if let Some(existing) = repository.get_message(&message.thread_id, &message.message_id)?
        && (!existing
            .delivery_state
            .can_transition_to(message.delivery_state)
            || !existing.read_state.can_transition_to(message.read_state))
    {
        return Err(SocialError::Conflict(
            "invalid message delivery/read transition".to_owned(),
        ));
    }
    repository.upsert_message(message)
}

pub fn get_message<R>(
    repository: &R,
    thread_id: &str,
    message_id: &str,
) -> SocialResult<Option<DirectMessage>>
where
    R: MessageRepository,
{
    if thread_id.trim().is_empty() || message_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "thread_id and message_id are required".to_owned(),
        ));
    }
    repository.get_message(thread_id, message_id)
}

pub fn list_thread_messages<R>(repository: &R, thread_id: &str) -> SocialResult<Vec<DirectMessage>>
where
    R: MessageRepository,
{
    if thread_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "thread_id is required".to_owned(),
        ));
    }
    repository.list_thread_messages(thread_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::messages::{DeliveryState, MessageDirection, MessageKind, ReadState};
    use crate::ports::repositories::MessageRepository;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeRepository {
        messages: Mutex<Vec<DirectMessage>>,
    }

    impl MessageRepository for FakeRepository {
        fn upsert_message(&self, message: &DirectMessage) -> SocialResult<()> {
            let mut messages = self.messages.lock().expect("messages mutex");
            if let Some(existing) = messages.iter_mut().find(|item| {
                item.thread_id == message.thread_id && item.message_id == message.message_id
            }) {
                *existing = message.clone();
            } else {
                messages.push(message.clone());
            }
            Ok(())
        }

        fn get_message(
            &self,
            thread_id: &str,
            message_id: &str,
        ) -> SocialResult<Option<DirectMessage>> {
            Ok(self
                .messages
                .lock()
                .expect("messages mutex")
                .iter()
                .find(|item| item.thread_id == thread_id && item.message_id == message_id)
                .cloned())
        }

        fn list_thread_messages(&self, thread_id: &str) -> SocialResult<Vec<DirectMessage>> {
            Ok(self
                .messages
                .lock()
                .expect("messages mutex")
                .iter()
                .filter(|item| item.thread_id == thread_id)
                .cloned()
                .collect())
        }
    }

    #[test]
    fn rejects_invalid_message_transition() {
        let repository = FakeRepository::default();
        let mut message = DirectMessage {
            thread_id: "thread-1".to_owned(),
            message_id: "message-1".to_owned(),
            transport_message_id: Some("transport-1".to_owned()),
            local_public_id: "did:key:alice".to_owned(),
            remote_public_id: "did:key:bob".to_owned(),
            direction: MessageDirection::Outbound,
            message_kind: MessageKind::Message,
            content_json: serde_json::json!({"text":"hello"}),
            encrypted_body: None,
            content_encoding: None,
            agent_envelope_json: None,
            agent_signature: None,
            delivery_state: DeliveryState::Acknowledged,
            read_state: ReadState::Read,
            created_at: 1,
            updated_at: 1,
        };
        upsert_message(&repository, &message).expect("save acknowledged message");

        message.delivery_state = DeliveryState::Pending;
        message.read_state = ReadState::Unread;
        message.updated_at = 2;
        let error = upsert_message(&repository, &message).expect_err("reject invalid state rewind");

        assert!(matches!(error, SocialError::Conflict(_)));
    }
}
