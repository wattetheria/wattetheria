use axum::http::StatusCode;
use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::McpEvents;
use super::service::rotation_proof;
use super::store::{NewSubscription, OutboxRecord, SubscriptionDiagnostic};
use super::webhook::{WebhookFailure, validate_callback_url, validate_secret};
use crate::state::ControlPlaneState;

pub(crate) const EVENT_NAME: &str = "wattetheria.agent.event";
const VERIFICATION_SECONDS: i64 = 10 * 60;

pub(crate) struct EventError {
    pub status: StatusCode,
    pub code: i32,
    pub message: &'static str,
    pub data: Value,
}

impl EventError {
    fn invalid(message: &'static str) -> Self {
        Self {
            status: StatusCode::OK,
            code: -32602,
            message,
            data: json!({}),
        }
    }

    fn internal(_: anyhow::Error) -> Self {
        Self {
            status: StatusCode::OK,
            code: -32603,
            message: "event subscription storage failed",
            data: json!({}),
        }
    }
}

pub(crate) async fn handle_request(
    state: &ControlPlaneState,
    method: &str,
    params: &Value,
) -> Result<Value, EventError> {
    if state.agent_event_mode != super::AgentEventMode::McpEvents {
        return Err(EventError {
            status: StatusCode::NOT_FOUND,
            code: -32601,
            message: "MCP Events is not enabled",
            data: json!({}),
        });
    }
    if method == "events/list" {
        return Ok(event_catalog());
    }
    let events = state
        .mcp_events
        .as_ref()
        .ok_or_else(|| EventError::internal(anyhow::anyhow!("events unavailable")))?;
    let url = subscription_url(params, method)?;
    if method == "events/unsubscribe" {
        events.unsubscribe(&url).await?;
        Ok(json!({}))
    } else {
        events.subscribe(params, &url).await
    }
}

fn event_catalog() -> Value {
    json!({"events": [{
        "name": EVENT_NAME,
        "description": "An incoming agent event requiring an external decision. The business event type is data.type.",
        "delivery": ["webhook"],
        "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "payloadSchema": {"type": "object", "properties": {
            "type": {"type": "string"}, "event_id": {"type": "string"},
            "requires_action": {"type": "boolean"}, "decision_status": {"type": "string"}
        }, "required": ["type", "event_id", "requires_action", "decision_status"]}
    }]})
}

fn subscription_url(params: &Value, method: &str) -> Result<String, EventError> {
    if params["name"].as_str() != Some(EVENT_NAME) {
        return Err(EventError {
            status: StatusCode::OK,
            code: -32011,
            message: "unknown event",
            data: json!({"kind": "event"}),
        });
    }
    if !params
        .get("arguments")
        .is_none_or(|v| v.as_object().is_some_and(serde_json::Map::is_empty))
    {
        return Err(EventError::invalid("arguments must be an empty object"));
    }
    let mode = params["delivery"].get("mode");
    if !(mode.and_then(Value::as_str) == Some("webhook")
        || method == "events/unsubscribe" && mode.is_none())
    {
        return Err(EventError {
            status: StatusCode::OK,
            code: -32014,
            message: "only webhook delivery is supported",
            data: json!({"feature": "deliveryMode"}),
        });
    }
    let url = params["delivery"]["url"]
        .as_str()
        .ok_or_else(|| EventError::invalid("delivery.url is required"))?;
    validate_callback_url(url)
        .map_err(|_| EventError::invalid("callback must be a public HTTPS URL"))?;
    Ok(url.to_owned())
}

fn subscription_id(owner: &str, url: &str) -> String {
    let key = serde_json::to_vec(&json!([owner, url, EVENT_NAME, {}]))
        .expect("subscription key is serializable");
    format!("sub_{}", hex::encode(Sha256::digest(key)))
}

fn granted_ttl(params: &Value) -> Result<Option<i64>, EventError> {
    let requested = match params.get("ttlMs") {
        None | Some(Value::Null) => return Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|v| *v > 0)
            .ok_or_else(|| EventError::invalid("ttlMs must be a positive integer or null"))?,
    };
    Ok(Some(
        i64::try_from(requested.clamp(1_000, 24 * 60 * 60 * 1000))
            .expect("bounded subscription lifetime"),
    ))
}

impl McpEvents {
    #[cfg(test)]
    pub(crate) fn with_test_callback_client(mut self, client: reqwest::Client) -> Self {
        self.webhook = std::sync::Arc::new(super::webhook::WebhookClient {
            callback_client: Some(client),
        });
        self
    }

    async fn subscribe(&self, params: &Value, url: &str) -> Result<Value, EventError> {
        let lifecycle = self.subscription_lifecycle.clone().read_owned().await;
        let secret = params["delivery"]["secret"]
            .as_str()
            .ok_or_else(|| EventError::invalid("delivery.secret is required"))?;
        validate_secret(secret).map_err(|_| {
            EventError::invalid("secret must be whsec_ with 24 to 64 decoded bytes")
        })?;
        let ttl = granted_ttl(params)?;
        if params.get("cursor").is_some_and(|v| !v.is_null()) {
            return Err(EventError::invalid("this event does not support replay"));
        }
        let id = subscription_id(&self.owner_id, url);
        self.verify_subscription(url, secret, &id).await?;
        let mut subscriptions = self.subscriptions.lock().await;
        let now = Utc::now().timestamp();
        let previous = self
            .store_call({
                let id = id.clone();
                move |store| {
                    store.cleanup(now)?;
                    store.get_subscription(&id)
                }
            })
            .await
            .map_err(EventError::internal)?;
        let expires_at = ttl.map(|ttl| (Utc::now().timestamp_millis() + ttl) / 1000);
        let (old_callback_secret, old_secret_expires_at) =
            rotation_proof(previous.as_ref(), secret, now);
        let input = NewSubscription {
            id: id.clone(),
            source_kind: "mcp_subscription".into(),
            event_type: "*".into(),
            target_agent_id: self.owner_id.clone(),
            credential_fingerprint: self.credential_fingerprint.clone(),
            owner_id: self.owner_id.clone(),
            callback_url: url.to_owned(),
            callback_secret: secret.to_owned(),
            old_callback_secret,
            old_secret_expires_at,
            canonical_arguments: b"{}".to_vec(),
            expires_at,
        };
        self.store_call(move |store| {
            // A canceled HTTP request must not release the gate before its blocking write.
            let _lifecycle = lifecycle;
            store.upsert_subscription(input)
        })
        .await
        .map_err(EventError::internal)?;
        subscriptions.retain(|_, s| s.expires_at.is_none_or(|expiry| expiry > now));
        subscriptions.insert(
            id.clone(),
            SubscriptionDiagnostic {
                id: id.clone(),
                event_type: "*".into(),
                target_agent_id: self.owner_id.clone(),
                expires_at,
            },
        );
        let expiration = expires_at.map(|expires_at| {
            Utc.timestamp_opt(expires_at, 0)
                .single()
                .expect("bounded subscription expiration")
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        });
        Ok(json!({"id": id, "refreshBefore": expiration, "cursor": null, "truncated": false}))
    }

    async fn verify_subscription(
        &self,
        url: &str,
        secret: &str,
        id: &str,
    ) -> Result<(), EventError> {
        let now = Utc::now().timestamp();
        let mut verified = self.verified_callbacks.lock().await;
        verified.retain(|_, expiry| *expiry > now);
        if verified.contains_key(url) {
            return Ok(());
        }
        self.webhook
            .verify_callback(url, secret, id)
            .await
            .map_err(|reason| EventError {
                status: StatusCode::OK,
                code: -32015,
                message: "callback verification failed",
                data: json!({"reason": reason}),
            })?;
        verified.insert(
            url.to_owned(),
            Utc::now().timestamp() + VERIFICATION_SECONDS,
        );
        Ok(())
    }

    async fn unsubscribe(&self, url: &str) -> Result<(), EventError> {
        let id = subscription_id(&self.owner_id, url);
        let mut subscriptions = self.subscriptions.lock().await;
        let (owner, subscription) = (self.owner_id.clone(), id.clone());
        self.store_call(move |store| {
            store.cancel_subscription(&subscription, &owner, Utc::now().timestamp())
        })
        .await
        .map_err(EventError::internal)?;
        subscriptions.remove(&id);
        Ok(())
    }

    pub(crate) async fn revoke_mcp_subscriptions(&self) -> anyhow::Result<usize> {
        let _lifecycle = self.subscription_lifecycle.write().await;
        let mut subscriptions = self.subscriptions.lock().await;
        let mut verified = self.verified_callbacks.lock().await;
        let revoked = self
            .store_call(move |store| {
                store
                    .cancel_subscriptions_by_source_kind("mcp_subscription", Utc::now().timestamp())
            })
            .await?;
        for id in &revoked {
            subscriptions.remove(id);
        }
        verified.clear();
        Ok(revoked.len())
    }

    pub(super) async fn deliver_subscription(
        &self,
        item: &OutboxRecord,
        old_secret: Option<&str>,
    ) -> Result<u16, WebhookFailure> {
        let mut payload: Value =
            serde_json::from_slice(&item.payload).map_err(|_| WebhookFailure::InvalidPayload)?;
        let event_type = payload["name"]
            .as_str()
            .and_then(|name| name.strip_prefix("wattetheria.agent."))
            .ok_or(WebhookFailure::InvalidPayload)?
            .to_owned();
        payload["name"] = json!(EVENT_NAME);
        payload["data"]["type"] = json!(event_type);
        payload["cursor"] = Value::Null;
        let body = super::service::bounded_webhook_body(payload)
            .map_err(|_| WebhookFailure::RequestTooLarge)?;
        self.webhook
            .post_subscription(
                &item.callback_url,
                &item.callback_secret,
                old_secret,
                &item.event_id,
                &body,
                &item.subscription_id,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_identity_and_lifetime_are_bounded_and_owner_scoped() {
        let id = subscription_id("owner-a", "https://callback.example/hook");
        assert_eq!(
            id,
            subscription_id("owner-a", "https://callback.example/hook")
        );
        assert_ne!(
            id,
            subscription_id("owner-b", "https://callback.example/hook")
        );
        assert_ne!(
            id,
            subscription_id("owner-a", "https://callback.example/other")
        );
        assert_eq!(granted_ttl(&json!({})).ok(), Some(None));
        assert_eq!(granted_ttl(&json!({"ttlMs": null})).ok(), Some(None));
        assert_eq!(
            granted_ttl(&json!({"ttlMs": u64::MAX})).ok(),
            Some(Some(86_400_000))
        );
        assert_eq!(granted_ttl(&json!({"ttlMs": 1})).ok(), Some(Some(1_000)));
        assert!(granted_ttl(&json!({"ttlMs": 0})).is_err());
    }

    #[test]
    fn subscription_payload_limit_includes_the_added_protocol_fields() {
        let mut payload = json!({"eventId": "evt_large", "name": "wattetheria.agent.topic_message_requires_reply",
            "timestamp": "2026-10-06T12:00:00Z", "data": {"content": ""}});
        let overhead = serde_json::to_vec(&payload).unwrap().len();
        payload["data"]["content"] = json!("x".repeat(256 * 1024 - overhead - 1));
        assert_eq!(serde_json::to_vec(&payload).unwrap().len(), 256 * 1024 - 1);
        payload["name"] = json!(EVENT_NAME);
        payload["data"]["type"] = json!("topic_message_requires_reply");
        payload["cursor"] = Value::Null;
        let body = super::super::service::bounded_webhook_body(payload).unwrap();
        assert!(body.len() <= 256 * 1024);
        let delivered: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(delivered["data"]["content_truncated"], true);
        assert_eq!(delivered["data"]["content"].as_str().unwrap().len(), 4_096);
    }
}
