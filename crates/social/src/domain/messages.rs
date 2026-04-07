use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Message,
    RelationshipEstablished,
    SessionInit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Delivered,
    Acknowledged,
    Failed,
}

impl DeliveryState {
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        match (self, next) {
            (Self::Pending, Self::Pending) => true,
            (Self::Delivered, Self::Delivered) => true,
            (Self::Acknowledged, Self::Acknowledged) => true,
            (Self::Failed, Self::Failed) => true,
            (Self::Pending, Self::Delivered | Self::Acknowledged | Self::Failed) => true,
            (Self::Delivered, Self::Acknowledged | Self::Failed) => true,
            (Self::Acknowledged | Self::Failed, _) => false,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadState {
    Unread,
    Read,
}

impl ReadState {
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        match (self, next) {
            (Self::Unread, Self::Unread) => true,
            (Self::Read, Self::Read) => true,
            (Self::Unread, Self::Read) => true,
            (Self::Read, _) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectMessage {
    pub thread_id: String,
    pub message_id: String,
    pub transport_message_id: Option<String>,
    pub local_public_id: String,
    pub remote_public_id: String,
    pub direction: MessageDirection,
    pub message_kind: MessageKind,
    pub content_json: serde_json::Value,
    pub encrypted_body: Option<String>,
    pub content_encoding: Option<String>,
    pub agent_envelope_json: Option<serde_json::Value>,
    pub agent_signature: Option<String>,
    pub delivery_state: DeliveryState,
    pub read_state: ReadState,
    pub created_at: i64,
    pub updated_at: i64,
}
