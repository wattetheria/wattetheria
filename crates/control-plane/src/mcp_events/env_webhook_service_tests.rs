use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};

use axum::body::Bytes;
use axum::extract::RawQuery;
use axum::http::{HeaderMap, StatusCode};
use serde_json::{Value, json};
use standardwebhooks::Webhook;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::JoinHandle;

use super::*;

const SECRET: &str = "whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD";
const EVENT_TYPES: [&str; 10] = [
    "friend_request",
    "payment_request",
    "payment_update",
    "third_party_result",
    "task_claim_received",
    "task_claim_decision_received",
    "task_result_received",
    "task_completion_decision_received",
    "task_settled_received",
    "topic_message_requires_reply",
];

struct Received {
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
}

struct Receiver {
    url: String,
    status: Arc<AtomicU16>,
    received: UnboundedReceiver<Received>,
    server: JoinHandle<()>,
}

impl Receiver {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, received) = unbounded_channel();
        let status = Arc::new(AtomicU16::new(200));
        let reply = Arc::clone(&status);
        let router = axum::Router::new().route(
            "/wake",
            axum::routing::post(
                move |RawQuery(query): RawQuery, headers: HeaderMap, body: Bytes| {
                    let (sender, reply) = (sender.clone(), Arc::clone(&reply));
                    async move {
                        let _ = sender.send(Received {
                            query,
                            headers,
                            body,
                        });
                        StatusCode::from_u16(reply.load(Ordering::SeqCst)).unwrap()
                    }
                },
            ),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url: format!("http://{address}/wake?key=abc"),
            status,
            received,
            server,
        }
    }

    fn config(&self, secret: Option<&str>) -> EnvWebhookConfig {
        EnvWebhookConfig::from_values(
            Some(&self.url),
            Some("Authorization: Bearer receiver-key"),
            secret,
        )
        .unwrap()
        .unwrap()
    }

    fn drain(&mut self) -> Vec<Received> {
        std::iter::from_fn(|| self.received.try_recv().ok()).collect()
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn open(dir: &Path, token: &str) -> McpEvents {
    McpEvents::open(
        dir.join("events.sqlite3"),
        dir.join("events.key"),
        "owner".into(),
        token,
    )
    .await
    .unwrap()
}

fn event(event_type: &str, id: &str) -> AgentEventEnvelope {
    AgentEventEnvelope {
        event_id: id.to_owned(),
        event_type: event_type.to_owned(),
        source_kind: "callback".to_owned(),
        source_node_id: Some("node-1".to_owned()),
        target_agent_id: Some("owner".to_owned()),
        target_executor: None,
        agent_envelope: None,
        payload: json!({}),
        requires_commit: false,
        allowed_actions: Vec::new(),
        correlation_id: None,
        dedupe_key: None,
        created_at: 1_700_000_000_000,
    }
}

fn pending_external() -> ProjectOutcome {
    ProjectOutcome {
        decision_status: "pending_external",
        commit_status: "not_requested",
        chosen_action: None,
        route: None,
        requires_action: true,
    }
}

async fn deliver_pending(events: &McpEvents, ticks: usize) {
    for _ in 0..ticks {
        events.delivery_tick(None).await.unwrap();
    }
}

async fn counts(events: &McpEvents) -> (u64, u64, u64) {
    let value = events.diagnostics().await.unwrap();
    let get = |key: &str| value[key].as_u64().unwrap();
    (
        get("activeSubscriptions"),
        get("pendingOutbox"),
        get("deadLetters"),
    )
}

async fn stored_secret(events: &McpEvents) -> (String, Option<String>) {
    let id = env_webhook_subscription_id("owner");
    let subscription = events
        .store_call(move |store| store.get_subscription(&id))
        .await
        .unwrap()
        .unwrap();
    (
        subscription.callback_secret,
        subscription.old_callback_secret,
    )
}

#[tokio::test]
async fn every_event_type_reaches_the_env_webhook_signed_with_receiver_headers() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    events
        .apply_env_webhook(Some(receiver.config(Some(SECRET))))
        .await
        .unwrap();

    for (index, event_type) in EVENT_TYPES.iter().enumerate() {
        events
            .publish(
                &event(event_type, &format!("e{index}")),
                &pending_external(),
            )
            .await
            .unwrap();
    }
    deliver_pending(&events, EVENT_TYPES.len() + 2).await;

    let delivered = receiver.drain();
    assert_eq!(delivered.len(), EVENT_TYPES.len());
    let webhook = Webhook::new(SECRET).unwrap();
    let mut names: Vec<String> = Vec::new();
    for request in &delivered {
        assert_eq!(request.query.as_deref(), Some("key=abc"));
        assert_eq!(request.headers["authorization"], "Bearer receiver-key");
        webhook.verify(&request.body, &request.headers).unwrap();
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["data"]["requires_action"], true);
        assert_eq!(body["data"]["decision_status"], "pending_external");
        names.push(body["name"].as_str().unwrap().to_owned());
    }
    names.sort();
    let mut expected: Vec<String> = EVENT_TYPES
        .iter()
        .map(|kind| format!("wattetheria.agent.{kind}"))
        .collect();
    expected.sort();
    assert_eq!(names, expected);
    assert_eq!(counts(&events).await, (1, 0, 0));
}

#[tokio::test]
async fn a_failing_receiver_keeps_the_event_pending_and_a_replay_is_not_duplicated() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    events
        .apply_env_webhook(Some(receiver.config(Some(SECRET))))
        .await
        .unwrap();
    receiver.status.store(503, Ordering::SeqCst);

    let request = event("friend_request", "same-source-event");
    events.publish(&request, &pending_external()).await.unwrap();
    events.publish(&request, &pending_external()).await.unwrap();
    deliver_pending(&events, 3).await;

    assert_eq!(receiver.drain().len(), 1, "one attempt, retried later");
    assert_eq!(counts(&events).await, (1, 1, 0));
}

#[tokio::test]
async fn pending_events_are_delivered_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    {
        let events = open(dir.path(), "token").await;
        events
            .apply_env_webhook(Some(receiver.config(Some(SECRET))))
            .await
            .unwrap();
        events
            .publish(&event("friend_request", "queued"), &pending_external())
            .await
            .unwrap();
    }

    let events = open(dir.path(), "token").await;
    assert_eq!(counts(&events).await, (1, 1, 0));
    events
        .apply_env_webhook(Some(receiver.config(Some(SECRET))))
        .await
        .unwrap();
    deliver_pending(&events, 2).await;

    let delivered = receiver.drain();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].headers["authorization"], "Bearer receiver-key");
    assert_eq!(counts(&events).await, (1, 0, 0));
}

#[tokio::test]
async fn removing_the_env_webhook_stops_new_events_and_it_can_be_enabled_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    events
        .apply_env_webhook(Some(receiver.config(Some(SECRET))))
        .await
        .unwrap();
    events.apply_env_webhook(None).await.unwrap();

    events
        .publish(&event("friend_request", "while-off"), &pending_external())
        .await
        .unwrap();
    deliver_pending(&events, 2).await;
    assert!(receiver.drain().is_empty());
    assert_eq!(counts(&events).await, (0, 0, 0));

    // A restart with the variable still removed keeps it off.
    drop(events);
    let events = open(dir.path(), "token").await;
    events.apply_env_webhook(None).await.unwrap();
    assert_eq!(counts(&events).await, (0, 0, 0));

    events
        .apply_env_webhook(Some(receiver.config(Some(SECRET))))
        .await
        .unwrap();
    events
        .publish(&event("friend_request", "back-on"), &pending_external())
        .await
        .unwrap();
    deliver_pending(&events, 2).await;
    assert_eq!(receiver.drain().len(), 1);
}

#[tokio::test]
async fn generated_secret_is_stable_and_a_configured_secret_rotates_with_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    events
        .apply_env_webhook(Some(receiver.config(None)))
        .await
        .unwrap();
    let (generated, old) = stored_secret(&events).await;
    assert!(generated.starts_with("whsec_") && old.is_none());

    drop(events);
    let events = open(dir.path(), "token").await;
    events
        .apply_env_webhook(Some(receiver.config(None)))
        .await
        .unwrap();
    assert_eq!(stored_secret(&events).await, (generated.clone(), None));

    events
        .apply_env_webhook(Some(receiver.config(Some(SECRET))))
        .await
        .unwrap();
    assert_eq!(
        stored_secret(&events).await,
        (SECRET.to_owned(), Some(generated.clone()))
    );
    events
        .publish(&event("friend_request", "rotated"), &pending_external())
        .await
        .unwrap();
    deliver_pending(&events, 2).await;
    let delivered = receiver.drain();
    assert_eq!(delivered.len(), 1);
    for secret in [SECRET, generated.as_str()] {
        Webhook::new(secret)
            .unwrap()
            .verify(&delivered[0].body, &delivered[0].headers)
            .unwrap();
    }
}

#[tokio::test]
async fn an_invalid_secret_is_rejected_without_creating_a_subscription() {
    let dir = tempfile::tempdir().unwrap();
    let receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    assert!(
        events
            .apply_env_webhook(Some(receiver.config(Some("not-a-whsec"))))
            .await
            .is_err()
    );
    assert_eq!(counts(&events).await, (0, 0, 0));
}

#[tokio::test]
async fn a_changed_control_token_revokes_the_webhook_until_it_is_applied_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    {
        let events = open(dir.path(), "old-token").await;
        events
            .apply_env_webhook(Some(receiver.config(Some(SECRET))))
            .await
            .unwrap();
    }
    let events = open(dir.path(), "new-token").await;
    assert_eq!(counts(&events).await, (0, 0, 0));
    events
        .publish(
            &event("friend_request", "before-apply"),
            &pending_external(),
        )
        .await
        .unwrap();
    assert_eq!(counts(&events).await, (0, 0, 0));

    events
        .apply_env_webhook(Some(receiver.config(Some(SECRET))))
        .await
        .unwrap();
    events
        .publish(&event("friend_request", "after-apply"), &pending_external())
        .await
        .unwrap();
    deliver_pending(&events, 2).await;
    assert_eq!(receiver.drain().len(), 1);
}
