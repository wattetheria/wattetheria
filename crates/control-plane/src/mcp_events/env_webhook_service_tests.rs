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
            None,
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

fn stored_encrypted_payload(dir: &Path, event_id: &str) -> Vec<u8> {
    rusqlite::Connection::open(dir.join("events.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT o.payload_enc FROM mcp_event_outbox o
         JOIN mcp_event_occurrences p ON p.id = o.occurrence_id WHERE p.event_id = ?1",
            [event_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn assert_signed_text(request: &Received) -> Value {
    Webhook::new(SECRET)
        .unwrap()
        .verify(&request.body, &request.headers)
        .unwrap();
    let mut body: Value = serde_json::from_slice(&request.body).unwrap();
    let text = body.as_object_mut().unwrap().remove("text").unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(text.as_str().unwrap()).unwrap(),
        body
    );
    assert_eq!(body["data"]["requires_action"], true);
    assert_eq!(body["data"]["decision_status"], "pending_external");
    body
}

#[tokio::test]
async fn unset_and_false_text_flags_send_original_bytes() {
    for text in [None, Some("false")] {
        let dir = tempfile::tempdir().unwrap();
        let mut receiver = Receiver::start().await;
        let events = open(dir.path(), "token").await;
        let config = EnvWebhookConfig::from_values(Some(&receiver.url), None, Some(SECRET), text)
            .unwrap()
            .unwrap();
        events.apply_env_webhook(Some(config)).await.unwrap();
        let input = event("friend_request", "no-text");
        events.publish(&input, &pending_external()).await.unwrap();
        assert!(events.delivery_tick(None).await.unwrap());
        let requests = receiver.drain();
        assert_eq!(requests.len(), 1);
        let projected = catalog::project(&input, &pending_external()).unwrap();
        let expected = webhook_body(
            &event_id("callback", "no-text", "owner"),
            projected.name,
            "2023-11-14T22:13:20.000Z",
            &projected.data,
        )
        .unwrap();
        assert_eq!(requests[0].body.as_ref(), expected);
        assert!(
            serde_json::from_slice::<Value>(&requests[0].body)
                .unwrap()
                .get("text")
                .is_none()
        );
        Webhook::new(SECRET)
            .unwrap()
            .verify(&requests[0].body, &requests[0].headers)
            .unwrap();
        assert_eq!(counts(&events).await, (1, 0, 0));
    }
}

#[tokio::test]
async fn text_mode_delivers_every_type_with_original_fields_and_final_body_signatures() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    let mut config = receiver.config(Some(SECRET));
    config.include_text = true;
    events.apply_env_webhook(Some(config)).await.unwrap();
    for kind in EVENT_TYPES {
        events
            .publish(&event(kind, kind), &pending_external())
            .await
            .unwrap();
    }
    deliver_pending(&events, EVENT_TYPES.len()).await;
    let requests = receiver.drain();
    assert_eq!(requests.len(), EVENT_TYPES.len());
    let mut names = Vec::new();
    for request in &requests {
        assert_eq!(request.headers["authorization"], "Bearer receiver-key");
        assert_eq!(request.query.as_deref(), Some("key=abc"));
        let body = assert_signed_text(request);
        assert_eq!(body.as_object().unwrap().len(), 4);
        assert_eq!(body["timestamp"], "2023-11-14T22:13:20.000Z");
        names.push(body["name"].as_str().unwrap().to_owned());
    }
    names.sort();
    let mut expected = EVENT_TYPES.map(|kind| format!("wattetheria.agent.{kind}"));
    expected.sort();
    assert_eq!(names, expected);
    assert_eq!(counts(&events).await, (1, 0, 0));
}

#[tokio::test]
async fn text_body_and_webhook_id_are_stable_when_retrying_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    let mut config = receiver.config(Some(SECRET));
    config.include_text = true;
    events
        .apply_env_webhook(Some(config.clone()))
        .await
        .unwrap();
    receiver.status.store(503, Ordering::SeqCst);
    events
        .publish(&event("friend_request", "retry-text"), &pending_external())
        .await
        .unwrap();
    assert!(events.delivery_tick(None).await.unwrap());
    let first = receiver.drain().pop().unwrap();
    assert_signed_text(&first);
    assert_eq!(counts(&events).await, (1, 1, 0));
    drop(events);
    let events = open(dir.path(), "token").await;
    events.apply_env_webhook(Some(config)).await.unwrap();
    receiver.status.store(200, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    assert!(events.delivery_tick(None).await.unwrap());
    let second = receiver.drain().pop().unwrap();
    assert_signed_text(&second);
    assert_eq!(first.body, second.body);
    assert_eq!(first.headers["webhook-id"], second.headers["webhook-id"]);
    assert_eq!(counts(&events).await, (1, 0, 0));
}

#[tokio::test]
async fn text_expansion_is_checked_against_final_request_size() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    let mut config = receiver.config(Some(SECRET));
    config.include_text = true;
    events.apply_env_webhook(Some(config)).await.unwrap();
    for (index, task_id) in [
        "x".repeat(70 * 1024),
        "x".repeat(140 * 1024),
        "x".repeat(255 * 1024),
        "\u{1f600}".repeat(63_000),
        "\"\\\n\u{0000}".repeat(21_000),
    ]
    .into_iter()
    .enumerate()
    {
        let mut input = event("task_claim_received", &format!("large-text-{index}"));
        input.payload = json!({"task_id": task_id});
        events.publish(&input, &pending_external()).await.unwrap();
        let id = event_id("callback", &input.event_id, "owner");
        let stored = stored_encrypted_payload(dir.path(), &id);
        assert!(events.delivery_tick(None).await.unwrap());
        assert_eq!(stored_encrypted_payload(dir.path(), &id), stored);
        let requests = receiver.drain();
        assert_eq!(
            requests.len(),
            1,
            "large event {index} must reach the receiver"
        );
        let request = &requests[0];
        Webhook::new(SECRET)
            .unwrap()
            .verify(&request.body, &request.headers)
            .unwrap();
        assert!(request.body.len() <= super::super::webhook::MAX_REQUEST_BYTES);
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.encode_utf16().count() <= 65_536);
        let preview: Value = serde_json::from_str(text).unwrap();
        assert_eq!(preview["text_truncated"], true);
        assert_eq!(preview["eventId"], body["eventId"]);
        assert_eq!(preview["name"], body["name"]);
        assert_eq!(preview["timestamp"], body["timestamp"]);
        assert_eq!(preview["data"]["requires_action"], true);
        let shortened = preview["data"]["task_id"].as_str().unwrap();
        assert!(task_id.starts_with(shortened));
        assert!(shortened.len() < task_id.len());
        if index < 2 {
            assert_eq!(body["data"]["task_id"], task_id);
            assert!(body.get("data_truncated").is_none());
        } else {
            assert_eq!(body["data_truncated"], true);
            assert_eq!(body["data"]["task_id"], shortened);
        }
        assert_eq!(counts(&events).await, (1, 0, 0));
    }
}

#[tokio::test]
async fn large_text_preview_is_stable_on_retry_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    let mut config = receiver.config(Some(SECRET));
    config.include_text = true;
    events
        .apply_env_webhook(Some(config.clone()))
        .await
        .unwrap();
    let mut input = event("task_claim_received", "large-retry");
    input.payload = json!({"task_id": "x".repeat(255 * 1024)});
    events.publish(&input, &pending_external()).await.unwrap();
    let id = event_id("callback", &input.event_id, "owner");
    let stored = stored_encrypted_payload(dir.path(), &id);
    receiver.status.store(503, Ordering::SeqCst);
    assert!(events.delivery_tick(None).await.unwrap());
    let first = receiver.drain().pop().unwrap();
    Webhook::new(SECRET)
        .unwrap()
        .verify(&first.body, &first.headers)
        .unwrap();
    let body: Value = serde_json::from_slice(&first.body).unwrap();
    assert_eq!(body["data_truncated"], true);
    assert_eq!(
        serde_json::from_str::<Value>(body["text"].as_str().unwrap()).unwrap()["text_truncated"],
        true
    );
    assert_eq!(counts(&events).await, (1, 1, 0));
    drop(events);

    let events = open(dir.path(), "token").await;
    events.apply_env_webhook(Some(config)).await.unwrap();
    receiver.status.store(200, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    assert!(events.delivery_tick(None).await.unwrap());
    let second = receiver.drain().pop().unwrap();
    Webhook::new(SECRET)
        .unwrap()
        .verify(&second.body, &second.headers)
        .unwrap();
    assert_eq!(second.body, first.body);
    assert_eq!(second.headers["webhook-id"], first.headers["webhook-id"]);
    assert_eq!(stored_encrypted_payload(dir.path(), &id), stored);
    assert_eq!(counts(&events).await, (1, 0, 0));
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
