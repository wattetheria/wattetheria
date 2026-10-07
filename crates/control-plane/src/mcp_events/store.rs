use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use super::crypto::EventCipher;
use super::sqlite::SqliteBackend;

#[derive(Clone, Debug)]
pub struct NewSubscription {
    pub id: String,
    pub source_kind: String,
    pub event_type: String,
    pub target_agent_id: String,
    pub credential_fingerprint: String,
    pub owner_id: String,
    pub callback_url: String,
    pub callback_secret: String,
    pub old_callback_secret: Option<String>,
    pub old_secret_expires_at: Option<i64>,
    pub canonical_arguments: Vec<u8>,
    pub expires_at: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct Subscription {
    pub callback_secret: String,
    pub old_callback_secret: Option<String>,
    pub old_secret_expires_at: Option<i64>,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct NewOccurrence {
    pub event_id: String,
    pub event_type: String,
    pub owner_id: String,
    pub kind: String,
    pub event_body: Vec<u8>,
    pub private_content_id: Option<String>,
    pub private_content: Option<Vec<u8>>,
    pub expires_at: i64,
}

#[derive(Clone, Debug)]
pub struct OutboxRecord {
    pub id: String,
    pub source_kind: String,
    pub event_id: String,
    pub subscription_id: String,
    pub callback_url: String,
    pub callback_secret: String,
    pub old_callback_secret: Option<String>,
    pub old_secret_expires_at: Option<i64>,
    pub payload: Vec<u8>,
    pub attempts: i32,
    pub lease_token: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryErrorCategory {
    HttpRetryable,
    HttpPermanent,
    Transport,
    UnsafeDestination,
    InvalidSecret,
    InvalidPayload,
    RequestTooLarge,
}

impl DeliveryErrorCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HttpRetryable => "http_retryable",
            Self::HttpPermanent => "http_permanent",
            Self::Transport => "transport",
            Self::UnsafeDestination => "unsafe_destination",
            Self::InvalidSecret => "invalid_secret",
            Self::InvalidPayload => "invalid_payload",
            Self::RequestTooLarge => "request_too_large",
        }
    }

    #[cfg(test)]
    pub(crate) fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "http_retryable" => Self::HttpRetryable,
            "http_permanent" => Self::HttpPermanent,
            "transport" => Self::Transport,
            "unsafe_destination" => Self::UnsafeDestination,
            "invalid_secret" => Self::InvalidSecret,
            "invalid_payload" => Self::InvalidPayload,
            "request_too_large" => Self::RequestTooLarge,
            _ => anyhow::bail!("unknown MCP event delivery error category"),
        })
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryStatus {
    Pending,
    Retry,
    Leased,
    Delivered,
    DeadLetter,
}

#[cfg(test)]
impl DeliveryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Retry => "retry",
            Self::Leased => "leased",
            Self::Delivered => "delivered",
            Self::DeadLetter => "dead_letter",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "Pending" => Self::Pending,
            "Retry" => Self::Retry,
            "Leased" => Self::Leased,
            "Delivered" => Self::Delivered,
            "DeadLetter" => Self::DeadLetter,
            _ => anyhow::bail!("unknown MCP event delivery status"),
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Delivered | Self::DeadLetter)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum DeliveryResult {
    Delivered {
        http_status: u16,
        at: i64,
    },
    Retry {
        http_status: Option<u16>,
        category: DeliveryErrorCategory,
        next_at: i64,
        at: i64,
    },
    DeadLetter {
        http_status: Option<u16>,
        category: DeliveryErrorCategory,
        at: i64,
    },
}

impl DeliveryResult {
    pub(crate) fn fields(
        self,
        attempts: i32,
        max_attempts: i32,
    ) -> (&'static str, Option<i32>, Option<&'static str>, i64, i64) {
        match self {
            Self::Delivered { http_status, at } => {
                ("Delivered", Some(i32::from(http_status)), None, 0, at)
            }
            Self::Retry {
                http_status,
                category,
                next_at,
                at,
            } => (
                if attempts >= max_attempts {
                    "DeadLetter"
                } else {
                    "Retry"
                },
                http_status.map(i32::from),
                Some(category.as_str()),
                next_at,
                at,
            ),
            Self::DeadLetter {
                http_status,
                category,
                at,
            } => (
                "DeadLetter",
                http_status.map(i32::from),
                Some(category.as_str()),
                0,
                at,
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriptionDiagnostic {
    pub id: String,
    pub event_type: String,
    pub target_agent_id: String,
    pub expires_at: Option<i64>,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryDiagnostic {
    pub id: String,
    pub event_id: String,
    pub subscription_id: String,
    pub status: DeliveryStatus,
    pub http_status: Option<u16>,
    pub error_category: Option<DeliveryErrorCategory>,
    pub retry_count: u32,
    pub next_attempt_at: Option<i64>,
    pub last_attempt_at: Option<i64>,
    pub terminal: bool,
}

#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreDiagnostics {
    pub active_subscriptions: u64,
    pub pending_outbox: u64,
    pub dead_letters: u64,
    pub subscriptions: Vec<SubscriptionDiagnostic>,
    pub recent_deliveries: Vec<DeliveryDiagnostic>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredSubscription {
    pub id: String,
    pub source_kind: String,
    pub event_type: String,
    pub target_agent_id: String,
    pub credential_fingerprint: String,
    pub owner_id: String,
    pub callback_url_enc: Vec<u8>,
    pub callback_secret_enc: Vec<u8>,
    pub old_callback_secret_enc: Option<Vec<u8>>,
    pub old_secret_expires_at: Option<i64>,
    pub canonical_arguments_enc: Vec<u8>,
    pub expires_at: Option<i64>,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredOccurrence {
    pub id: String,
    pub event_id: String,
    pub owner_id: String,
    pub kind: String,
    pub private_content_id: Option<String>,
    pub private_content_enc: Option<Vec<u8>>,
    pub expires_at: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredOutbox {
    pub id: String,
    pub event_id: String,
    pub occurrence_id: String,
    pub subscription: StoredSubscription,
    pub payload_enc: Vec<u8>,
    pub attempts: i32,
    pub lease_token: Option<String>,
}

#[derive(Clone)]
pub struct EventStore {
    backend: Arc<SqliteBackend>,
    cipher: EventCipher,
}

impl EventStore {
    pub fn open(path: &Path, key_path: &Path) -> Result<Self> {
        let cipher = EventCipher::load_or_create(key_path)?;
        let backend = Arc::new(SqliteBackend::open(path, cipher.key_id())?);
        Ok(Self { backend, cipher })
    }

    pub fn upsert_subscription(&self, input: NewSubscription) -> Result<()> {
        let row = StoredSubscription {
            id: input.id.clone(),
            source_kind: input.source_kind,
            event_type: input.event_type,
            target_agent_id: input.target_agent_id,
            credential_fingerprint: input.credential_fingerprint,
            owner_id: input.owner_id,
            callback_url_enc: self
                .cipher
                .encrypt(input.id.as_bytes(), input.callback_url.as_bytes())?,
            callback_secret_enc: self
                .cipher
                .encrypt(input.id.as_bytes(), input.callback_secret.as_bytes())?,
            old_callback_secret_enc: input
                .old_callback_secret
                .map(|secret| self.cipher.encrypt(input.id.as_bytes(), secret.as_bytes()))
                .transpose()?,
            old_secret_expires_at: input.old_secret_expires_at,
            canonical_arguments_enc: self
                .cipher
                .encrypt(input.id.as_bytes(), &input.canonical_arguments)?,
            expires_at: input.expires_at,
            active: true,
        };
        self.backend.upsert_subscription(&row)
    }

    pub fn get_subscription(&self, id: &str) -> Result<Option<Subscription>> {
        self.backend
            .get_subscription(id)?
            .map(|s| self.subscription(s))
            .transpose()
    }

    pub fn deactivate_subscription(&self, id: &str, owner_id: &str) -> Result<()> {
        self.backend.deactivate_subscription(id, owner_id)
    }

    pub(crate) fn cancel_subscription(&self, id: &str, owner_id: &str, now: i64) -> Result<()> {
        self.backend.cancel_subscription(id, owner_id, now)
    }

    pub(crate) fn cancel_subscriptions_by_source_kind(
        &self,
        source_kind: &str,
        now: i64,
    ) -> Result<Vec<String>> {
        self.backend
            .cancel_subscriptions_by_source_kind(source_kind, now)
    }

    pub fn revoke_credentials_except(&self, owner_id: &str, fingerprint: &str) -> Result<u64> {
        self.backend
            .revoke_credentials_except(owner_id, fingerprint)
    }

    pub fn active_subscriptions(
        &self,
        owner_id: &str,
        now: i64,
    ) -> Result<Vec<SubscriptionDiagnostic>> {
        self.backend.active_subscriptions(owner_id, now)
    }

    pub fn publish(&self, input: NewOccurrence, now: i64) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let occurrence = StoredOccurrence {
            id: id.clone(),
            event_id: input.event_id,
            owner_id: input.owner_id,
            kind: input.kind,
            private_content_id: input.private_content_id,
            private_content_enc: input
                .private_content
                .map(|value| self.cipher.encrypt(id.as_bytes(), &value))
                .transpose()?,
            expires_at: input.expires_at,
        };
        let payload = self.cipher.encrypt(id.as_bytes(), &input.event_body)?;
        self.backend
            .publish(&occurrence, &payload, &input.event_type, now)
    }

    pub fn claim_outbox(
        &self,
        owner_id: &str,
        fingerprint: &str,
        now: i64,
        lease_secs: i64,
        source_kind: Option<&str>,
    ) -> Result<Option<OutboxRecord>> {
        let token = Uuid::new_v4().to_string();
        self.backend
            .claim_outbox(
                owner_id,
                fingerprint,
                now,
                now + lease_secs,
                &token,
                source_kind,
            )?
            .map(|o| self.outbox(o))
            .transpose()
    }

    pub fn complete_delivery(
        &self,
        item: &OutboxRecord,
        result: DeliveryResult,
        max_attempts: i32,
    ) -> Result<()> {
        let token = item
            .lease_token
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("outbox item is not leased"))?;
        self.backend
            .complete_delivery(&item.id, token, result, max_attempts)
    }

    pub fn cleanup(&self, now: i64) -> Result<()> {
        self.backend.cleanup(now)
    }

    #[cfg(test)]
    pub fn diagnostics(&self, owner_id: &str, now: i64) -> Result<StoreDiagnostics> {
        self.backend.diagnostics(owner_id, now)
    }

    fn subscription(&self, s: StoredSubscription) -> Result<Subscription> {
        Ok(Subscription {
            callback_secret: String::from_utf8(
                self.cipher
                    .decrypt(s.id.as_bytes(), &s.callback_secret_enc)?,
            )?,
            old_callback_secret: s
                .old_callback_secret_enc
                .map(|value| {
                    self.cipher
                        .decrypt(s.id.as_bytes(), &value)
                        .and_then(|bytes| Ok(String::from_utf8(bytes)?))
                })
                .transpose()?,
            old_secret_expires_at: s.old_secret_expires_at,
            active: s.active,
        })
    }

    fn outbox(&self, o: StoredOutbox) -> Result<OutboxRecord> {
        let payload = self
            .cipher
            .decrypt(o.occurrence_id.as_bytes(), &o.payload_enc)?;
        Ok(OutboxRecord {
            id: o.id.clone(),
            source_kind: o.subscription.source_kind,
            event_id: o.event_id,
            subscription_id: o.subscription.id.clone(),
            callback_url: String::from_utf8(self.cipher.decrypt(
                o.subscription.id.as_bytes(),
                &o.subscription.callback_url_enc,
            )?)?,
            callback_secret: String::from_utf8(self.cipher.decrypt(
                o.subscription.id.as_bytes(),
                &o.subscription.callback_secret_enc,
            )?)?,
            old_callback_secret: o
                .subscription
                .old_callback_secret_enc
                .map(|value| {
                    self.cipher
                        .decrypt(o.subscription.id.as_bytes(), &value)
                        .and_then(|bytes| Ok(String::from_utf8(bytes)?))
                })
                .transpose()?,
            old_secret_expires_at: o.subscription.old_secret_expires_at,
            payload,
            attempts: o.attempts,
            lease_token: o.lease_token,
        })
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
