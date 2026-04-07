use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptKind {
    Sent,
    Delivered,
    Acknowledged,
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageReceipt {
    pub message_id: String,
    pub receipt_kind: ReceiptKind,
    pub recorded_at: i64,
    pub detail: Option<String>,
}
