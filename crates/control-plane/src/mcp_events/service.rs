use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};

use super::catalog::{self, ProjectOutcome};
use super::env_webhook::{EnvWebhookConfig, body_with_text, generate_secret};
use super::store::{
    DeliveryErrorCategory, DeliveryResult, EventStore, NewOccurrence, NewSubscription,
    OutboxRecord, Subscription, SubscriptionDiagnostic,
};
use super::webhook::{WebhookClient, WebhookFailure, validate_secret};
use crate::routes::agent_events::AgentEventEnvelope;
use reqwest::header::HeaderMap;

const ROTATION_SECONDS: i64 = 10 * 60;
const RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;
const MAX_WEBHOOK_BYTES: usize = 256 * 1024;
const CONTENT_PREVIEW_CHARS: usize = 4096;
/// The event webhook is the operator's own always-on receiver, so delivery keeps
/// retrying through restarts and short outages (about an hour) before giving up.
const MAX_DELIVERY_ATTEMPTS: i32 = 20;
const WILDCARD_EVENT_TYPE: &str = "*";
const ENV_WEBHOOK_SOURCE_KIND: &str = "env_webhook";

#[derive(Clone)]
pub struct McpEvents {
    store: EventStore,
    pub(super) owner_id: String,
    pub(super) credential_fingerprint: String,
    pub(super) webhook: Arc<WebhookClient>,
    pub(super) subscriptions: Arc<Mutex<HashMap<String, SubscriptionDiagnostic>>>,
    pub(super) verified_callbacks: Arc<Mutex<HashMap<String, i64>>>,
    pub(super) subscription_lifecycle: Arc<RwLock<()>>,
    env_webhook: Arc<Mutex<Option<EnvWebhook>>>,
}

/// Request headers and body format come from the deployment environment on
/// every start and are never written to the store.
struct EnvWebhook {
    subscription_id: String,
    headers: HeaderMap,
    include_text: bool,
}

impl McpEvents {
    pub async fn open(
        path: PathBuf,
        key_path: PathBuf,
        owner_id: String,
        token: &str,
    ) -> Result<Self> {
        let store = tokio::task::spawn_blocking(move || EventStore::open(&path, &key_path))
            .await
            .context("join MCP Events store open")??;
        let credential_fingerprint = hex::encode(Sha256::digest(token.as_bytes()));
        let fingerprint = credential_fingerprint.clone();
        let owner = owner_id.clone();
        let cloned = store.clone();
        tokio::task::spawn_blocking(move || cloned.revoke_credentials_except(&owner, &fingerprint))
            .await
            .context("join MCP Events credential check")??;
        let owner = owner_id.clone();
        let cloned = store.clone();
        let subscriptions = tokio::task::spawn_blocking(move || {
            cloned.active_subscriptions(&owner, Utc::now().timestamp())
        })
        .await
        .context("join MCP Events subscriptions load")??
        .into_iter()
        .map(|subscription| (subscription.id.clone(), subscription))
        .collect();
        Ok(Self {
            store,
            owner_id,
            credential_fingerprint,
            webhook: Arc::new(WebhookClient::default()),
            subscriptions: Arc::new(Mutex::new(subscriptions)),
            verified_callbacks: Arc::new(Mutex::new(HashMap::new())),
            subscription_lifecycle: Arc::new(RwLock::new(())),
            env_webhook: Arc::new(Mutex::new(None)),
        })
    }

    /// Applies the environment webhook: every event goes to one URL without an
    /// MCP subscription or callback challenge. `None` stops a previously
    /// configured webhook. Call after `open`, on every start.
    pub async fn apply_env_webhook(&self, config: Option<EnvWebhookConfig>) -> Result<()> {
        let id = env_webhook_subscription_id(&self.owner_id);
        let Some(config) = config else {
            let (owner, subscription_id) = (self.owner_id.clone(), id.clone());
            self.store_call(move |store| store.deactivate_subscription(&subscription_id, &owner))
                .await?;
            self.subscriptions.lock().await.remove(&id);
            *self.env_webhook.lock().await = None;
            return Ok(());
        };
        let now = Utc::now().timestamp();
        let previous = self
            .store_call({
                let id = id.clone();
                move |store| store.get_subscription(&id)
            })
            .await?;
        let secret = config
            .secret
            .clone()
            .or_else(|| {
                previous
                    .as_ref()
                    .filter(|current| current.active)
                    .map(|current| current.callback_secret.clone())
            })
            .unwrap_or_else(generate_secret);
        validate_secret(&secret).map_err(|_| {
            anyhow!(
                "{} must look like whsec_<base64 of 24 to 64 bytes>",
                super::env_webhook::ENV_SECRET
            )
        })?;
        let (old_secret, old_secret_expires_at) = rotation_proof(previous.as_ref(), &secret, now);
        let input = NewSubscription {
            id: id.clone(),
            source_kind: ENV_WEBHOOK_SOURCE_KIND.to_owned(),
            event_type: WILDCARD_EVENT_TYPE.to_owned(),
            target_agent_id: self.owner_id.clone(),
            credential_fingerprint: self.credential_fingerprint.clone(),
            owner_id: self.owner_id.clone(),
            callback_url: config.url,
            callback_secret: secret,
            old_secret_expires_at,
            old_callback_secret: old_secret,
            canonical_arguments: b"{}".to_vec(),
            expires_at: None,
        };
        let diagnostic = SubscriptionDiagnostic {
            id: id.clone(),
            event_type: input.event_type.clone(),
            target_agent_id: input.target_agent_id.clone(),
            expires_at: None,
        };
        self.store_call(move |store| store.upsert_subscription(input))
            .await?;
        self.subscriptions
            .lock()
            .await
            .insert(id.clone(), diagnostic);
        *self.env_webhook.lock().await = Some(EnvWebhook {
            subscription_id: id,
            headers: config.headers,
            include_text: config.include_text,
        });
        Ok(())
    }

    pub(super) async fn store_call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(EventStore) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || operation(store))
            .await
            .context("join MCP Events store operation")?
    }

    pub(crate) async fn publish(
        &self,
        event: &AgentEventEnvelope,
        outcome: &ProjectOutcome,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        let interested = self
            .subscriptions
            .lock()
            .await
            .values()
            .any(|subscription| {
                subscription.target_agent_id == self.owner_id
                    && (subscription.event_type == WILDCARD_EVENT_TYPE
                        || subscription.event_type == event.event_type)
                    && subscription
                        .expires_at
                        .is_none_or(|expires_at| expires_at > now)
            });
        if !interested {
            return Ok(());
        }
        let mut projected = catalog::project(event, outcome)
            .ok_or_else(|| anyhow!("callback projection unavailable"))?;
        let content = projected.full_content.take();
        if let (Some(content), Some(data)) = (&content, projected.data.as_object_mut()) {
            data.insert("content".into(), content.clone());
        }
        let event_id = event_id(&event.source_kind, &event.event_id, &self.owner_id);
        let timestamp = Utc
            .timestamp_millis_opt(i64::try_from(event.created_at)?)
            .single()
            .ok_or_else(|| anyhow!("invalid callback occurrence time"))?
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let body = webhook_body(&event_id, projected.name, &timestamp, &projected.data)?;
        let occurrence = NewOccurrence {
            event_id,
            event_type: event.event_type.clone(),
            owner_id: self.owner_id.clone(),
            kind: projected.name.to_owned(),
            event_body: body,
            private_content_id: None,
            private_content: None,
            expires_at: Utc::now().timestamp() + RETENTION_SECONDS,
        };
        self.store_call(move |store| store.publish(occurrence, Utc::now().timestamp()))
            .await
    }

    #[cfg(test)]
    pub(crate) async fn diagnostics(&self) -> Result<Value> {
        let owner = self.owner_id.clone();
        let diagnostics = self
            .store_call(move |store| store.diagnostics(&owner, Utc::now().timestamp()))
            .await?;
        Ok(json!({
            "activeSubscriptions": diagnostics.active_subscriptions,
            "pendingOutbox": diagnostics.pending_outbox,
            "deadLetters": diagnostics.dead_letters,
            "subscriptions": diagnostics.subscriptions.iter().map(|subscription| json!({
                "id": subscription.id,
                "eventType": subscription.event_type,
                "targetAgentId": subscription.target_agent_id,
                "expiresAt": subscription.expires_at.and_then(timestamp_iso),
            })).collect::<Vec<_>>(),
            "recentDeliveries": diagnostics.recent_deliveries.iter().map(|delivery| json!({
                "id": delivery.id,
                "eventId": delivery.event_id,
                "subscriptionId": delivery.subscription_id,
                "status": delivery.status.as_str(),
                "httpStatus": delivery.http_status,
                "errorCategory": delivery.error_category.map(DeliveryErrorCategory::as_str),
                "retryCount": delivery.retry_count,
                "nextAttemptAt": delivery.next_attempt_at.and_then(timestamp_iso),
                "lastAttemptAt": delivery.last_attempt_at.and_then(timestamp_iso),
                "terminal": delivery.terminal,
            })).collect::<Vec<_>>(),
        }))
    }

    pub async fn delivery_tick(&self, source_kind: Option<&'static str>) -> Result<bool> {
        let _subscription_guard = if source_kind == Some(ENV_WEBHOOK_SOURCE_KIND) {
            None
        } else {
            Some(self.subscription_lifecycle.read().await)
        };
        let now = Utc::now().timestamp();
        self.store_call(move |store| store.cleanup(now)).await?;
        let owner = self.owner_id.clone();
        let fingerprint = self.credential_fingerprint.clone();
        let next = self
            .store_call(move |store| {
                store.claim_outbox(
                    &owner,
                    &fingerprint,
                    Utc::now().timestamp(),
                    30,
                    source_kind,
                )
            })
            .await?;
        let Some(item) = next else { return Ok(false) };
        let old_secret = item
            .old_secret_expires_at
            .filter(|until| *until > Utc::now().timestamp())
            .and(item.old_callback_secret.as_deref());
        // Deliveries wait for the current configuration so they always carry
        // the receiver headers from this start's environment.
        let env_config = self
            .env_webhook
            .lock()
            .await
            .as_ref()
            .filter(|env| env.subscription_id == item.subscription_id)
            .map(|env| (env.headers.clone(), env.include_text));
        let outcome = if item.source_kind == ENV_WEBHOOK_SOURCE_KIND {
            let Some((headers, include_text)) = env_config else {
                return Ok(false);
            };
            let body = if include_text {
                body_with_text(&item.payload)
                    .map(Cow::Owned)
                    .map_err(|_| WebhookFailure::InvalidPayload)
            } else {
                Ok(Cow::Borrowed(item.payload.as_slice()))
            };
            match body {
                Ok(body) => {
                    self.webhook
                        .post_event(
                            &item.callback_url,
                            &item.callback_secret,
                            old_secret,
                            &item.event_id,
                            &body,
                            &headers,
                        )
                        .await
                }
                Err(error) => Err(error),
            }
        } else if item.source_kind == "mcp_subscription" {
            self.deliver_subscription(&item, old_secret).await
        } else {
            return Ok(false);
        };
        let completed_at = Utc::now().timestamp();
        let result = classify_delivery(outcome, item.attempts, completed_at);
        log_delivery(&item, &result, MAX_DELIVERY_ATTEMPTS);
        self.complete_delivery(item, result, MAX_DELIVERY_ATTEMPTS)
            .await?;
        Ok(true)
    }

    async fn complete_delivery(
        &self,
        item: OutboxRecord,
        result: DeliveryResult,
        max_attempts: i32,
    ) -> Result<()> {
        self.store_call(move |store| store.complete_delivery(&item, result, max_attempts))
            .await
    }

    pub async fn run_worker(&self) {
        tokio::join!(
            self.run_delivery_worker(ENV_WEBHOOK_SOURCE_KIND),
            self.run_delivery_worker("mcp_subscription")
        );
    }

    async fn run_delivery_worker(&self, source_kind: &'static str) {
        loop {
            match self.delivery_tick(Some(source_kind)).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => tracing::warn!("MCP Events delivery tick failed: {error:#}"),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

fn classify_delivery(
    outcome: Result<u16, WebhookFailure>,
    attempts: i32,
    completed_at: i64,
) -> DeliveryResult {
    match outcome {
        Ok(status) if (200..300).contains(&status) => DeliveryResult::Delivered {
            http_status: status,
            at: completed_at,
        },
        Ok(status @ (410 | 413)) => DeliveryResult::DeadLetter {
            http_status: Some(status),
            category: DeliveryErrorCategory::HttpPermanent,
            at: completed_at,
        },
        Ok(status) => DeliveryResult::Retry {
            http_status: Some(status),
            category: DeliveryErrorCategory::HttpRetryable,
            next_at: completed_at + retry_delay(attempts),
            at: completed_at,
        },
        Err(error) => {
            let category = match error {
                WebhookFailure::InvalidUrl => DeliveryErrorCategory::UnsafeDestination,
                WebhookFailure::InvalidSecret => DeliveryErrorCategory::InvalidSecret,
                WebhookFailure::InvalidPayload => DeliveryErrorCategory::InvalidPayload,
                WebhookFailure::RequestTooLarge => DeliveryErrorCategory::RequestTooLarge,
                WebhookFailure::Transport => DeliveryErrorCategory::Transport,
            };
            if error == WebhookFailure::Transport {
                DeliveryResult::Retry {
                    http_status: None,
                    category,
                    next_at: completed_at + retry_delay(attempts),
                    at: completed_at,
                }
            } else {
                DeliveryResult::DeadLetter {
                    http_status: None,
                    category,
                    at: completed_at,
                }
            }
        }
    }
}

fn log_delivery(item: &OutboxRecord, result: &DeliveryResult, max_attempts: i32) {
    match result {
        DeliveryResult::Delivered { .. } => {}
        DeliveryResult::Retry {
            http_status,
            category,
            ..
        } if item.attempts + 1 < max_attempts => tracing::warn!(
            event_id = %item.event_id,
            attempt = item.attempts + 1,
            http_status = ?http_status,
            error = category.as_str(),
            "event webhook delivery failed; will retry"
        ),
        DeliveryResult::Retry {
            http_status,
            category,
            ..
        }
        | DeliveryResult::DeadLetter {
            http_status,
            category,
            ..
        } => tracing::error!(
            event_id = %item.event_id,
            http_status = ?http_status,
            error = category.as_str(),
            "event webhook delivery gave up"
        ),
    }
}

fn webhook_body(event_id: &str, name: &str, timestamp: &str, data: &Value) -> Result<Vec<u8>> {
    bounded_webhook_body(json!({
        "eventId": event_id,
        "name": name,
        "timestamp": timestamp,
        "data": data,
    }))
}

pub(super) fn bounded_webhook_body(mut payload: Value) -> Result<Vec<u8>> {
    let mut body = serde_json::to_vec(&payload)?;
    if body.len() > MAX_WEBHOOK_BYTES
        && let Some(content) = payload["data"]["content"].as_str()
    {
        let preview: String = content.chars().take(CONTENT_PREVIEW_CHARS).collect();
        payload["data"]["content"] = json!(preview);
        payload["data"]["content_truncated"] = json!(true);
        body = serde_json::to_vec(&payload)?;
    }
    if body.len() > MAX_WEBHOOK_BYTES {
        bail!("projected event exceeds webhook byte limit");
    }
    Ok(body)
}

fn retry_delay(attempts: i32) -> i64 {
    2_i64
        .saturating_pow((attempts + 1).clamp(1, 8).cast_unsigned())
        .min(300)
}

#[cfg(test)]
fn timestamp_iso(seconds: i64) -> Option<String> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn env_webhook_subscription_id(owner: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(ENV_WEBHOOK_SOURCE_KIND.as_bytes());
    digest.update(owner.as_bytes());
    format!("sub_env_{}", hex::encode(digest.finalize()))
}

pub(super) fn rotation_proof(
    previous: Option<&Subscription>,
    secret: &str,
    now: i64,
) -> (Option<String>, Option<i64>) {
    match previous.filter(|current| current.active) {
        Some(current) if current.callback_secret != secret => (
            Some(current.callback_secret.clone()),
            Some(now + ROTATION_SECONDS),
        ),
        Some(current)
            if current
                .old_secret_expires_at
                .is_some_and(|expires_at| expires_at > now) =>
        {
            (
                current.old_callback_secret.clone(),
                current.old_secret_expires_at,
            )
        }
        _ => (None, None),
    }
}

fn event_id(source_kind: &str, source_event_id: &str, owner_id: &str) -> String {
    let mut digest = Sha256::new();
    for part in [source_kind, source_event_id, owner_id] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!("evt_{}", hex::encode(digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_preserves_webhook_secret_rotation_overlap() {
        let mut previous = Subscription {
            callback_secret: "new".into(),
            old_callback_secret: Some("old".into()),
            old_secret_expires_at: Some(110),
            active: true,
        };
        assert_eq!(
            rotation_proof(Some(&previous), "new", 100),
            (Some("old".into()), Some(110))
        );
        assert_eq!(rotation_proof(Some(&previous), "new", 110), (None, None));
        assert_eq!(
            rotation_proof(Some(&previous), "newer", 100),
            (Some("new".into()), Some(100 + ROTATION_SECONDS))
        );
        previous.active = false;
        assert_eq!(rotation_proof(Some(&previous), "newer", 100), (None, None));
    }
}

#[cfg(test)]
#[path = "env_webhook_service_tests.rs"]
mod env_webhook_service_tests;
