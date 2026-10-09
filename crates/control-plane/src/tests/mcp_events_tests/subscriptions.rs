use super::subscription_receiver::Receiver;
use super::*;
use crate::mcp_events::AgentEventMode;
use std::sync::atomic::Ordering;

fn attach_receiver(state: &mut ControlPlaneState, receiver: &Receiver) {
    let events = Arc::try_unwrap(state.mcp_events.take().unwrap())
        .ok()
        .expect("fixture owns Events");
    state.mcp_events = Some(Arc::new(
        events.with_test_callback_client(receiver.client.clone()),
    ));
}

async fn subscribe_to_receiver(
    state: &ControlPlaneState,
    token: &str,
    receiver: &Receiver,
) -> Value {
    let mut params = parameters();
    params["delivery"]["url"] = json!(receiver.url);
    let (status, result) = modern_call(
        app(state.clone()),
        Some(token),
        "events/subscribe",
        params,
        |_| {},
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(result.get("error").is_none(), "{result}");
    result["result"].clone()
}

async fn publish_pending(state: &ControlPlaneState, input: &AgentEventEnvelope) {
    state
        .mcp_events
        .as_ref()
        .unwrap()
        .publish(
            input,
            &ProjectOutcome {
                decision_status: "pending_external",
                commit_status: "not_requested",
                chosen_action: None,
                route: None,
                requires_action: true,
            },
        )
        .await
        .unwrap();
}

async fn modern_call(
    router: Router,
    token: Option<&str>,
    method: &str,
    mut params: Value,
    change_headers: impl FnOnce(&mut HeaderMap),
) -> (StatusCode, Value) {
    params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {}});
    mcp_call(router, token, method, params, |headers| {
        headers.insert(
            "mcp-protocol-version",
            HeaderValue::from_static("2026-07-28"),
        );
        headers.insert("mcp-method", HeaderValue::from_str(method).unwrap());
        change_headers(headers);
    })
    .await
}

async fn subscription_app() -> (tempfile::TempDir, Router, String, ControlPlaneState) {
    let (dir, _, token, mut state) = events_app().await;
    state.agent_event_mode = AgentEventMode::McpEvents;
    (dir, app(state.clone()), token, state)
}

fn parameters() -> Value {
    json!({"name": "wattetheria.agent.event", "arguments": {}, "cursor": null,
        "delivery": {"mode": "webhook", "url": "https://callback.example/events",
        "secret": "whsec_AAECAwQFBgcICQoLDA0ODxAREhMUFRYX"}})
}

#[tokio::test]
async fn mcp_events_worker_drains_each_source_without_blocking_the_other() {
    use crate::mcp_events::env_webhook::EnvWebhookConfig;
    use std::time::Duration;
    use tokio::sync::{Notify, Semaphore};

    for slow_mcp in [false, true] {
        let receiver = Receiver::start().await;
        let (_dir, router, token, mut state) = subscription_app().await;
        drop(router);
        attach_receiver(&mut state, &receiver);
        let subscription = subscribe_to_receiver(&state, &token, &receiver).await;
        let env_gate = Arc::new(Semaphore::new(1));
        let env_started = Arc::new(Notify::new());
        let (gate, started) = (env_gate.clone(), env_started.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let env_router = Router::new().route(
            "/hook",
            axum::routing::post(move || {
                let (gate, started) = (gate.clone(), started.clone());
                async move {
                    started.notify_one();
                    let _permit = gate.acquire().await.unwrap();
                    StatusCode::OK
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, env_router).await.unwrap() });
        let events = state.mcp_events.as_ref().unwrap().clone();
        events
            .apply_env_webhook(
                EnvWebhookConfig::from_values(
                    Some(&format!("http://{address}/hook")),
                    None,
                    parameters()["delivery"]["secret"].as_str(),
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let slow_gate = if slow_mcp {
            receiver.delivery_gate.clone()
        } else {
            env_gate
        };
        let held = slow_gate.acquire().await.unwrap();
        for index in 0..4 {
            let mut input = event(&state.agent_did);
            input.event_id = format!("burst-{index}");
            publish_pending(&state, &input).await;
        }
        let worker_events = events.clone();
        let worker = tokio::spawn(async move { worker_events.run_worker().await });
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let slow_started = if slow_mcp {
                &receiver.delivery_started
            } else {
                &env_started
            };
            slow_started.notified().await;
            loop {
                let diagnostics = events.diagnostics().await.unwrap();
                let deliveries = diagnostics["recentDeliveries"].as_array().unwrap();
                let delivered = deliveries
                    .iter()
                    .filter(|item| {
                        (item["subscriptionId"] == subscription["id"]) != slow_mcp
                            && item["status"] == "delivered"
                    })
                    .count();
                if delivered == 4 {
                    assert!(deliveries.iter().any(|item| (item["subscriptionId"]
                        == subscription["id"])
                        == slow_mcp
                        && item["status"] == "leased"));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        drop(held);
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        server.abort();
        let _ = server.await;
        assert!(result.is_ok(), "slow_mcp={slow_mcp}: {result:?}");
    }
}

#[tokio::test]
async fn mcp_events_subscription_verifies_then_delivers_a_signed_total_event() {
    let receiver = Receiver::start().await;
    let (_dir, router, token, mut state) = subscription_app().await;
    drop(router);
    attach_receiver(&mut state, &receiver);
    let subscription = subscribe_to_receiver(&state, &token, &receiver).await;
    assert_eq!(subscription["cursor"], Value::Null);
    assert_eq!(subscription["truncated"], false);
    assert_eq!(subscription.get("refreshBefore"), Some(&Value::Null));
    publish_pending(&state, &event(&state.agent_did)).await;
    state
        .mcp_events
        .as_ref()
        .unwrap()
        .delivery_tick(None)
        .await
        .unwrap();
    let records = receiver.requests.lock().await;
    assert_eq!(records.len(), 2);
    for record in records.iter() {
        assert_eq!(
            record.headers["x-mcp-subscription-id"],
            subscription["id"].as_str().unwrap()
        );
        standardwebhooks::Webhook::new(parameters()["delivery"]["secret"].as_str().unwrap())
            .unwrap()
            .verify(&record.body, &record.headers)
            .unwrap();
    }
    let verification: Value = serde_json::from_slice(&records[0].body).unwrap();
    assert_eq!(verification["type"], "verification");
    assert!(
        records[0].headers["webhook-id"]
            .to_str()
            .unwrap()
            .starts_with("msg_verification_")
    );
    let delivered: Value = serde_json::from_slice(&records[1].body).unwrap();
    assert_eq!(delivered["name"], "wattetheria.agent.event");
    assert_eq!(delivered["data"]["type"], "third_party_result");
    assert_eq!(delivered["data"]["requires_action"], true);
    assert_eq!(delivered["cursor"], Value::Null);
    assert!(delivered.get("type").is_none());
    assert_eq!(
        delivered["eventId"],
        records[1].headers["webhook-id"].to_str().unwrap()
    );
}

#[tokio::test]
async fn mcp_events_callback_failure_never_activates_a_subscription() {
    let receiver = Receiver::start().await;
    receiver.wrong_challenge.store(true, Ordering::SeqCst);
    let (_dir, router, token, mut state) = subscription_app().await;
    drop(router);
    attach_receiver(&mut state, &receiver);
    let mut params = parameters();
    params["delivery"]["url"] = json!(receiver.url);
    let (_, rejected) = modern_call(
        app(state.clone()),
        Some(&token),
        "events/subscribe",
        params.clone(),
        |_| {},
    )
    .await;
    assert_eq!(rejected["error"]["code"], -32015);
    assert_eq!(rejected["error"]["data"]["reason"], "challenge_failed");
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["activeSubscriptions"],
        0
    );
    receiver.wrong_challenge.store(false, Ordering::SeqCst);
    receiver.status.store(503, Ordering::SeqCst);
    let (_, unavailable) = modern_call(
        app(state.clone()),
        Some(&token),
        "events/subscribe",
        params,
        |_| {},
    )
    .await;
    assert_eq!(unavailable["error"]["code"], -32015);
    assert_eq!(unavailable["error"]["data"]["reason"], "http_5xx");
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["activeSubscriptions"],
        0
    );
    receiver.status.store(200, Ordering::SeqCst);
    subscribe_to_receiver(&state, &token, &receiver).await;
    assert_eq!(receiver.requests.lock().await.len(), 3);
}

#[tokio::test]
async fn mcp_events_refresh_reuses_id_and_rotates_secret_without_repeating_challenge() {
    let receiver = Receiver::start().await;
    let (_dir, router, token, mut state) = subscription_app().await;
    drop(router);
    attach_receiver(&mut state, &receiver);
    let first = subscribe_to_receiver(&state, &token, &receiver).await;
    let mut params = parameters();
    params["delivery"]["url"] = json!(receiver.url);
    let new_secret = "whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD";
    params["delivery"]["secret"] = json!(new_secret);
    params["ttlMs"] = json!(60_000);
    let (_, refreshed) = modern_call(
        app(state.clone()),
        Some(&token),
        "events/subscribe",
        params,
        |_| {},
    )
    .await;
    assert_eq!(refreshed["result"]["id"], first["id"]);
    let expires = chrono::DateTime::parse_from_rfc3339(
        refreshed["result"]["refreshBefore"].as_str().unwrap(),
    )
    .unwrap()
    .timestamp();
    assert!((58..=60).contains(&(expires - chrono::Utc::now().timestamp())));
    assert_eq!(receiver.requests.lock().await.len(), 1);
    let input = event(&state.agent_did);
    publish_pending(&state, &input).await;
    publish_pending(&state, &input).await;
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["pendingOutbox"],
        1
    );
    state
        .mcp_events
        .as_ref()
        .unwrap()
        .delivery_tick(None)
        .await
        .unwrap();
    let records = receiver.requests.lock().await;
    assert_eq!(records.len(), 2);
    for secret in [
        new_secret,
        parameters()["delivery"]["secret"].as_str().unwrap(),
    ] {
        standardwebhooks::Webhook::new(secret)
            .unwrap()
            .verify(&records[1].body, &records[1].headers)
            .unwrap();
    }
}

#[tokio::test]
async fn mcp_events_subscriptions_and_pending_deliveries_survive_restart() {
    let receiver = Receiver::start().await;
    let (dir, router, token, mut state) = subscription_app().await;
    drop(router);
    attach_receiver(&mut state, &receiver);
    let first = subscribe_to_receiver(&state, &token, &receiver).await;
    publish_pending(&state, &event(&state.agent_did)).await;
    state.mcp_events = None;
    let reopened = McpEvents::open(
        dir.path().join("mcp-events.sqlite"),
        dir.path().join("mcp-events.key"),
        state.agent_did.clone(),
        &token,
    )
    .await
    .unwrap()
    .with_test_callback_client(receiver.client.clone());
    state.mcp_events = Some(Arc::new(reopened));
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["activeSubscriptions"],
        1
    );
    state
        .mcp_events
        .as_ref()
        .unwrap()
        .delivery_tick(None)
        .await
        .unwrap();
    let records = receiver.requests.lock().await;
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[1].headers["x-mcp-subscription-id"],
        first["id"].as_str().unwrap()
    );
    drop(records);
    let renewed = subscribe_to_receiver(&state, &token, &receiver).await;
    assert_eq!(renewed["id"], first["id"]);
    assert_eq!(receiver.requests.lock().await.len(), 3);
}

#[tokio::test]
async fn mcp_events_unsubscribe_is_idempotent_and_stops_pending_and_new_deliveries() {
    let receiver = Receiver::start().await;
    let (_dir, router, token, mut state) = subscription_app().await;
    drop(router);
    attach_receiver(&mut state, &receiver);
    subscribe_to_receiver(&state, &token, &receiver).await;
    publish_pending(&state, &event(&state.agent_did)).await;
    let mut params = parameters();
    params["delivery"]["url"] = json!(receiver.url);
    params["delivery"].as_object_mut().unwrap().remove("secret");
    for omit_mode in [true, false] {
        let mut request_params = params.clone();
        if omit_mode {
            request_params["delivery"]
                .as_object_mut()
                .unwrap()
                .remove("mode");
        }
        let (_, result) = modern_call(
            app(state.clone()),
            Some(&token),
            "events/unsubscribe",
            request_params,
            |_| {},
        )
        .await;
        assert_eq!(
            result["result"],
            json!({"resultType": "complete", "_meta": {"io.modelcontextprotocol/serverInfo": {
            "name": "wattetheria-local-control-plane", "version": env!("CARGO_PKG_VERSION")}}})
        );
    }
    let mut later = event(&state.agent_did);
    later.event_id = "new-event".into();
    publish_pending(&state, &later).await;
    state
        .mcp_events
        .as_ref()
        .unwrap()
        .delivery_tick(None)
        .await
        .unwrap();
    assert_eq!(receiver.requests.lock().await.len(), 1);
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["activeSubscriptions"],
        0
    );
}

#[tokio::test]
async fn mcp_events_expiration_stops_pending_events() {
    let receiver = Receiver::start().await;
    let (_dir, router, token, mut state) = subscription_app().await;
    drop(router);
    attach_receiver(&mut state, &receiver);
    let mut params = parameters();
    params["delivery"]["url"] = json!(receiver.url);
    params["ttlMs"] = json!(1_000);
    let (_, result) = modern_call(
        app(state.clone()),
        Some(&token),
        "events/subscribe",
        params,
        |_| {},
    )
    .await;
    assert!(result.get("error").is_none());
    publish_pending(&state, &event(&state.agent_did)).await;
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    state
        .mcp_events
        .as_ref()
        .unwrap()
        .delivery_tick(None)
        .await
        .unwrap();
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["activeSubscriptions"],
        0
    );
    assert_eq!(receiver.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn mcp_events_all_business_types_reach_one_subscription_and_the_env_webhook() {
    let receiver = Receiver::start().await;
    let (_dir, router, token, mut state) = subscription_app().await;
    drop(router);
    attach_receiver(&mut state, &receiver);
    subscribe_to_receiver(&state, &token, &receiver).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, mut env_requests) = tokio::sync::mpsc::channel(16);
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/hook",
                post(move |headers: HeaderMap, body: axum::body::Bytes| {
                    let sender = sender.clone();
                    async move {
                        sender.send((headers, body)).await.unwrap();
                        StatusCode::OK
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let env = crate::mcp_events::EnvWebhookConfig::from_values(
        Some(&format!("http://{address}/hook")),
        Some("Authorization: Bearer env-key"),
        Some(parameters()["delivery"]["secret"].as_str().unwrap()),
        Some("true"),
    )
    .unwrap()
    .unwrap();
    state
        .mcp_events
        .as_ref()
        .unwrap()
        .apply_env_webhook(Some(env))
        .await
        .unwrap();
    let kinds = [
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
    for kind in kinds {
        let mut input = event(&state.agent_did);
        input.event_id = kind.into();
        input.event_type = kind.into();
        publish_pending(&state, &input).await;
    }
    for _ in 0..20 {
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .delivery_tick(None)
            .await
            .unwrap();
    }
    let records = receiver.requests.lock().await;
    assert_eq!(records.len(), 11);
    let mut actual = records
        .iter()
        .skip(1)
        .map(|record| {
            let body: Value = serde_json::from_slice(&record.body).unwrap();
            assert_eq!(body["name"], "wattetheria.agent.event");
            assert_eq!(body["cursor"], Value::Null);
            assert!(body.get("text").is_none());
            body["data"]["type"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected = kinds.map(str::to_owned);
    expected.sort();
    assert_eq!(actual, expected);
    for _ in 0..10 {
        let (headers, body) = env_requests.try_recv().unwrap();
        assert_eq!(headers["authorization"], "Bearer env-key");
        assert!(!headers.contains_key("x-mcp-subscription-id"));
        let mut body: Value = serde_json::from_slice(&body).unwrap();
        let text = body.as_object_mut().unwrap().remove("text").unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(text.as_str().unwrap()).unwrap(),
            body
        );
        assert_ne!(body["name"], "wattetheria.agent.event");
        assert!(body.get("cursor").is_none());
        assert!(body["data"].get("type").is_none());
    }
    server.abort();
}

#[tokio::test]
async fn mcp_events_subscription_410_and_413_are_terminal() {
    for status in [410, 413] {
        let receiver = Receiver::start().await;
        let (_dir, router, token, mut state) = subscription_app().await;
        drop(router);
        attach_receiver(&mut state, &receiver);
        subscribe_to_receiver(&state, &token, &receiver).await;
        receiver.status.store(status, Ordering::SeqCst);
        publish_pending(&state, &event(&state.agent_did)).await;
        let events = state.mcp_events.as_ref().unwrap();
        events.delivery_tick(None).await.unwrap();
        events.delivery_tick(None).await.unwrap();
        assert_eq!(events.diagnostics().await.unwrap()["deadLetters"], 1);
        assert_eq!(receiver.requests.lock().await.len(), 2);
    }
}

#[tokio::test]
async fn mcp_events_resubscribe_does_not_replay_cancelled_or_expired_pending_events() {
    for expired in [false, true] {
        let receiver = Receiver::start().await;
        let (_dir, router, token, mut state) = subscription_app().await;
        drop(router);
        attach_receiver(&mut state, &receiver);
        let mut params = parameters();
        params["delivery"]["url"] = json!(receiver.url);
        params["ttlMs"] = json!(1_000);
        let (_, first) = modern_call(
            app(state.clone()),
            Some(&token),
            "events/subscribe",
            params.clone(),
            |_| {},
        )
        .await;
        publish_pending(&state, &event(&state.agent_did)).await;
        if expired {
            tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
        } else {
            let (_, cancelled) = modern_call(
                app(state.clone()),
                Some(&token),
                "events/unsubscribe",
                params.clone(),
                |_| {},
            )
            .await;
            assert_eq!(cancelled["result"]["resultType"], "complete");
        }
        params["ttlMs"] = Value::Null;
        let (_, renewed) = modern_call(
            app(state.clone()),
            Some(&token),
            "events/subscribe",
            params,
            |_| {},
        )
        .await;
        assert_eq!(renewed["result"]["id"], first["result"]["id"]);
        assert_eq!(renewed["result"].get("refreshBefore"), Some(&Value::Null));
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .delivery_tick(None)
            .await
            .unwrap();
        assert_eq!(receiver.requests.lock().await.len(), 1);
        let mut later = event(&state.agent_did);
        later.event_id = "fresh".into();
        publish_pending(&state, &later).await;
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .delivery_tick(None)
            .await
            .unwrap();
        assert_eq!(receiver.requests.lock().await.len(), 2);
    }
}

#[tokio::test]
async fn mcp_events_changed_owner_token_revokes_persisted_subscriptions() {
    let receiver = Receiver::start().await;
    let (dir, router, token, mut state) = subscription_app().await;
    drop(router);
    attach_receiver(&mut state, &receiver);
    subscribe_to_receiver(&state, &token, &receiver).await;
    publish_pending(&state, &event(&state.agent_did)).await;
    state.mcp_events = None;
    let reopened = McpEvents::open(
        dir.path().join("mcp-events.sqlite"),
        dir.path().join("mcp-events.key"),
        state.agent_did.clone(),
        "new-node-token",
    )
    .await
    .unwrap()
    .with_test_callback_client(receiver.client.clone());
    assert_eq!(
        reopened.diagnostics().await.unwrap()["activeSubscriptions"],
        0
    );
    reopened.delivery_tick(None).await.unwrap();
    assert_eq!(receiver.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn mcp_events_subscription_retry_keeps_event_identity_and_survives_restart() {
    let receiver = Receiver::start().await;
    let (dir, router, token, mut state) = subscription_app().await;
    drop(router);
    attach_receiver(&mut state, &receiver);
    subscribe_to_receiver(&state, &token, &receiver).await;
    receiver.status.store(503, Ordering::SeqCst);
    publish_pending(&state, &event(&state.agent_did)).await;
    state
        .mcp_events
        .as_ref()
        .unwrap()
        .delivery_tick(None)
        .await
        .unwrap();
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["pendingOutbox"],
        1
    );
    state.mcp_events = None;
    receiver.status.store(200, Ordering::SeqCst);
    let store = crate::mcp_events::store::EventStore::open(
        &dir.path().join("mcp-events.sqlite"),
        &dir.path().join("mcp-events.key"),
    )
    .unwrap();
    let future = chrono::Utc::now().timestamp() + 2 * 24 * 60 * 60;
    store.cleanup(future).unwrap();
    let subscriptions = store
        .active_subscriptions(&state.agent_did, future)
        .unwrap();
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].expires_at, None);
    drop(store);
    let reopened = McpEvents::open(
        dir.path().join("mcp-events.sqlite"),
        dir.path().join("mcp-events.key"),
        state.agent_did.clone(),
        &token,
    )
    .await
    .unwrap()
    .with_test_callback_client(receiver.client.clone());
    assert_eq!(
        reopened.diagnostics().await.unwrap()["subscriptions"][0]["expiresAt"],
        Value::Null
    );
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    reopened.delivery_tick(None).await.unwrap();
    assert_eq!(reopened.diagnostics().await.unwrap()["pendingOutbox"], 0);
    let records = receiver.requests.lock().await;
    assert_eq!(records.len(), 3);
    assert_eq!(
        records[1].headers["webhook-id"],
        records[2].headers["webhook-id"]
    );
    assert_eq!(records[1].body, records[2].body);
    assert_ne!(
        records[1].headers["webhook-timestamp"],
        records[2].headers["webhook-timestamp"]
    );
    let secret = parameters()["delivery"]["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    for record in records.iter().skip(1) {
        standardwebhooks::Webhook::new(&secret)
            .unwrap()
            .verify(&record.body, &record.headers)
            .unwrap();
    }
}

#[path = "subscriptions_protocol.rs"]
mod protocol;

#[path = "public_url.rs"]
mod public_url;
