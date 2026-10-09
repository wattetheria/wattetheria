use super::*;
use crate::mcp_events::EnvWebhookConfig;
use std::sync::atomic::Ordering;
use std::time::Duration;

async fn env_receiver(
    events: &McpEvents,
) -> (
    tokio::task::JoinHandle<()>,
    Arc<tokio::sync::Mutex<Vec<Value>>>,
) {
    let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let captured = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new().route(
        "/hook",
        axum::routing::post(move |headers: HeaderMap, Json(payload): Json<Value>| {
            let captured = captured.clone();
            async move {
                assert_eq!(headers["x-receiver"], "env-authority");
                captured.lock().await.push(payload);
                StatusCode::OK
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    events
        .apply_env_webhook(
            EnvWebhookConfig::from_values(
                Some(&format!("http://{address}/hook")),
                Some("x-receiver: env-authority"),
                parameters()["delivery"]["secret"].as_str(),
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    (server, requests)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn public_mcp_rotation_revokes_all_mcp_subscriptions_and_preserves_env_webhook() {
    let receiver = Receiver::start().await;
    let (dir, mut state, secret) = public_app().await;
    attach_receiver(&mut state, &receiver);
    let events = state.mcp_events.as_ref().unwrap().clone();
    let (env_server, env_requests) = env_receiver(&events).await;
    let mut ids = Vec::new();
    for url in [
        receiver.url.clone(),
        receiver.url.replace("webhook.test", "WEBHOOK.TEST"),
    ] {
        let mut params = parameters();
        params["delivery"]["url"] = json!(url);
        let (status, subscribed) =
            public_call(&state, &secret, "events/subscribe", params, true).await;
        assert_eq!(status, StatusCode::OK);
        assert!(subscribed.get("error").is_none(), "{subscribed}");
        ids.push(subscribed["result"]["id"].as_str().unwrap().to_owned());
    }
    let store = EventStore::open(
        &dir.path().join("mcp-events.sqlite"),
        &dir.path().join("mcp-events.key"),
    )
    .unwrap();
    store
        .upsert_subscription(NewSubscription {
            id: "previous-owner-subscription".into(),
            source_kind: "mcp_subscription".into(),
            event_type: "*".into(),
            target_agent_id: "previous-owner".into(),
            credential_fingerprint: "previous-credential".into(),
            owner_id: "previous-owner".into(),
            callback_url: receiver.url.clone(),
            callback_secret: parameters()["delivery"]["secret"].as_str().unwrap().into(),
            old_callback_secret: None,
            old_secret_expires_at: None,
            canonical_arguments: b"{}".to_vec(),
            expires_at: Some(1),
        })
        .unwrap();
    ids.push("previous-owner-subscription".into());
    publish_pending(&state, &event(&state.agent_did)).await;
    assert_eq!(events.diagnostics().await.unwrap()["pendingOutbox"], 3);
    let (status, rotated) = rotate(&state, Some(&state.auth_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rotated["revokedSubscriptions"], 3);
    let next = rotated["path"]
        .as_str()
        .unwrap()
        .strip_prefix("/mcp/")
        .unwrap();
    assert_eq!(
        public_call(&state, &secret, "events/list", json!({}), true)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        public_call(&state, next, "events/list", json!({}), true)
            .await
            .0,
        StatusCode::OK
    );
    for id in &ids {
        assert!(!store.get_subscription(id).unwrap().unwrap().active);
    }
    let diagnostic = events.diagnostics().await.unwrap();
    assert_eq!(diagnostic["subscriptions"].as_array().unwrap().len(), 1);
    let env_id = diagnostic["subscriptions"][0]["id"].as_str().unwrap();
    let env = store.get_subscription(env_id).unwrap().unwrap();
    assert!(env.active);
    assert_eq!(env.callback_secret, parameters()["delivery"]["secret"]);
    assert!(
        !events
            .delivery_tick(Some("mcp_subscription"))
            .await
            .unwrap()
    );
    assert!(events.delivery_tick(Some("env_webhook")).await.unwrap());
    assert_eq!(env_requests.lock().await.len(), 1);

    // A previously verified URL must challenge again after rotation.
    receiver.wrong_challenge.store(true, Ordering::SeqCst);
    let mut params = parameters();
    params["delivery"]["url"] = json!(receiver.url);
    let (_, rejected) = public_call(&state, next, "events/subscribe", params.clone(), true).await;
    assert_eq!(rejected["error"]["code"], -32015);
    receiver.wrong_challenge.store(false, Ordering::SeqCst);
    let (_, subscribed) = public_call(&state, next, "events/subscribe", params, true).await;
    assert!(subscribed.get("error").is_none(), "{subscribed}");
    assert_eq!(subscribed["result"]["id"], ids[0]);
    assert!(
        !events
            .delivery_tick(Some("mcp_subscription"))
            .await
            .unwrap()
    );
    let mut new_event = event(&state.agent_did);
    new_event.event_id = "after-rotation".into();
    publish_pending(&state, &new_event).await;
    assert!(
        events
            .delivery_tick(Some("mcp_subscription"))
            .await
            .unwrap()
    );
    assert!(events.delivery_tick(Some("env_webhook")).await.unwrap());
    assert_eq!(env_requests.lock().await.len(), 2);
    let delivered = receiver
        .requests
        .lock()
        .await
        .iter()
        .filter(|request| {
            serde_json::from_slice::<Value>(&request.body).unwrap()["type"] != "verification"
        })
        .count();
    assert_eq!(delivered, 1);
    drop(store);
    drop(events);
    drop(state.mcp_events.take());
    let reopened = McpEvents::open(
        dir.path().join("mcp-events.sqlite"),
        dir.path().join("mcp-events.key"),
        state.agent_did.clone(),
        &state.auth_token,
    )
    .await
    .unwrap();
    assert_eq!(
        reopened.diagnostics().await.unwrap()["subscriptions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    env_server.abort();
}

#[tokio::test]
async fn public_mcp_rotation_drains_an_old_url_request_before_revoking_its_subscription() {
    use tokio::sync::Notify;
    let receiver = Receiver::start().await;
    let (_dir, mut state, secret) = public_app().await;
    attach_receiver(&mut state, &receiver);
    let body_started = Arc::new(Notify::new());
    let body_release = Arc::new(Notify::new());
    let mut params = parameters();
    params["delivery"]["url"] = json!(receiver.url);
    params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {}});
    let payload =
        json!({"jsonrpc": "2.0", "id": 1, "method": "events/subscribe", "params": params})
            .to_string();
    let (started, release) = (body_started.clone(), body_release.clone());
    let body = Body::from_stream(futures_util::stream::once(async move {
        started.notify_one();
        release.notified().await;
        Ok::<_, std::io::Error>(payload)
    }));
    let router = public_mcp_app(state.clone());
    let pending = tokio::spawn(async move {
        router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/mcp/{secret}"))
                    .header("content-type", "application/json")
                    .header("mcp-protocol-version", "2026-07-28")
                    .header("mcp-method", "events/subscribe")
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    body_started.notified().await;
    let rotating_state = state.clone();
    let rotation =
        tokio::spawn(
            async move { rotate(&rotating_state, Some(&rotating_state.auth_token)).await },
        );
    let (status, result) = tokio::time::timeout(Duration::from_secs(2), rotation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.await.unwrap().status(), StatusCode::NOT_FOUND);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["revokedSubscriptions"], 0);
    assert_eq!(
        state
            .mcp_events
            .as_ref()
            .unwrap()
            .diagnostics()
            .await
            .unwrap()["subscriptions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn public_mcp_rotation_waits_for_inflight_mcp_delivery_without_blocking_env_delivery() {
    let receiver = Receiver::start().await;
    let (_dir, mut state, secret) = public_app().await;
    attach_receiver(&mut state, &receiver);
    let mut params = parameters();
    params["delivery"]["url"] = json!(receiver.url);
    let (_, subscribed) = public_call(&state, &secret, "events/subscribe", params, true).await;
    assert!(subscribed.get("error").is_none());
    let events = state.mcp_events.as_ref().unwrap().clone();
    let (env_server, env_requests) = env_receiver(&events).await;
    publish_pending(&state, &event(&state.agent_did)).await;
    let held = receiver.delivery_gate.acquire().await.unwrap();
    let delivering = events.clone();
    let delivery = tokio::spawn(async move {
        delivering
            .delivery_tick(Some("mcp_subscription"))
            .await
            .unwrap()
    });
    receiver.delivery_started.notified().await;
    let rotating_state = state.clone();
    let mut rotation =
        tokio::spawn(
            async move { rotate(&rotating_state, Some(&rotating_state.auth_token)).await },
        );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut rotation)
            .await
            .is_err()
    );
    assert!(events.delivery_tick(Some("env_webhook")).await.unwrap());
    assert_eq!(env_requests.lock().await.len(), 1);
    drop(held);
    assert!(delivery.await.unwrap());
    assert_eq!(rotation.await.unwrap().0, StatusCode::OK);
    assert!(
        !events
            .delivery_tick(Some("mcp_subscription"))
            .await
            .unwrap()
    );
    env_server.abort();
}
