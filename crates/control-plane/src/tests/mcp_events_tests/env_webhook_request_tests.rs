use super::*;

fn request_config() -> Value {
    json!({
        "body_template": {
            "id":{"$ref":"/event/eventId"},
            "token":"receiver-token",
            "content":{"$json":{"text":{"$json":{"$ref":"/event"}}}}
        },
        "headers":{"x-time":{"$ref":"/timestamp"},"x-nonce":{"$ref":"/nonce"}},
        "signature":{"algorithm":"sha256","secret":"receiver-signing-key","input":["$timestamp","$nonce","$secret","$body"],"encoding":"hex","header":"x-signature"}
    })
}

fn assert_request_signature(request: &Received) -> Value {
    Webhook::new(SECRET)
        .unwrap()
        .verify(&request.body, &request.headers)
        .unwrap();
    let mut digest = Sha256::new();
    digest.update(request.headers["x-time"].as_bytes());
    digest.update(request.headers["x-nonce"].as_bytes());
    digest.update(b"receiver-signing-key");
    digest.update(&request.body);
    assert_eq!(
        request.headers["x-signature"],
        hex::encode(digest.finalize())
    );
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["token"], "receiver-token");
    let content: Value = serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
    let event: Value = serde_json::from_str(content["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["id"], event["eventId"]);
    assert_eq!(
        request.headers["webhook-id"],
        event["eventId"].as_str().unwrap()
    );
    event
}

#[tokio::test]
async fn configured_request_wraps_every_event_and_signs_the_final_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    let config = EnvWebhookConfig::from_values(
        Some(&receiver.url),
        Some("Authorization: Bearer receiver-key"),
        Some(SECRET),
        None,
        Some(&request_config().to_string()),
    )
    .unwrap()
    .unwrap();
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
        names.push(
            assert_request_signature(request)["name"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    names.sort();
    let mut expected = EVENT_TYPES.map(|kind| format!("wattetheria.agent.{kind}"));
    expected.sort();
    assert_eq!(names, expected);
    assert_eq!(counts(&events).await, (1, 0, 0));
}

#[tokio::test]
async fn retry_after_restart_keeps_event_identity_and_unmodified_stored_payload() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    let mut config = receiver.config(Some(SECRET));
    config.include_text = true;
    config.request = Some(Arc::new(
        RequestConfig::parse(&request_config().to_string()).unwrap(),
    ));
    events
        .apply_env_webhook(Some(config.clone()))
        .await
        .unwrap();
    events
        .publish(
            &event("friend_request", "request-retry"),
            &pending_external(),
        )
        .await
        .unwrap();
    let id = event_id("callback", "request-retry", "owner");
    let stored = stored_encrypted_payload(dir.path(), &id);
    receiver.status.store(503, Ordering::SeqCst);
    assert!(events.delivery_tick(None).await.unwrap());
    let first = receiver.drain().pop().unwrap();
    let original = assert_request_signature(&first);
    assert!(original["text"].is_string());
    assert_eq!(counts(&events).await, (1, 1, 0));
    drop(events);
    let events = open(dir.path(), "token").await;
    events.apply_env_webhook(Some(config)).await.unwrap();
    receiver.status.store(200, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(events.delivery_tick(None).await.unwrap());
    let second = receiver.drain().pop().unwrap();
    assert_eq!(assert_request_signature(&second), original);
    assert_eq!(first.headers["webhook-id"], second.headers["webhook-id"]);
    assert_ne!(first.headers["x-nonce"], second.headers["x-nonce"]);
    assert_eq!(stored, stored_encrypted_payload(dir.path(), &id));
    assert_eq!(counts(&events).await, (1, 0, 0));
}

#[tokio::test]
async fn receiver_business_rejection_is_retried_despite_http_200() {
    let accepted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let flag = accepted.clone();
    let router = axum::Router::new().route(
        "/wake",
        axum::routing::post(move || {
            let flag = flag.clone();
            async move { axum::Json(json!({"success":flag.load(Ordering::SeqCst)})) }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let events = open(dir.path(), "token").await;
    let config = EnvWebhookConfig::from_values(
        Some(&format!("http://{address}/wake")),
        None,
        Some(SECRET),
        None,
        Some(r#"{"response":{"pointer":"/success","equals":true}}"#),
    )
    .unwrap()
    .unwrap();
    events.apply_env_webhook(Some(config)).await.unwrap();
    events
        .publish(
            &event("friend_request", "business-retry"),
            &pending_external(),
        )
        .await
        .unwrap();
    assert!(events.delivery_tick(None).await.unwrap());
    assert_eq!(counts(&events).await, (1, 1, 0));
    accepted.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(events.delivery_tick(None).await.unwrap());
    assert_eq!(counts(&events).await, (1, 0, 0));
    server.abort();
}

#[tokio::test]
async fn invalid_template_does_not_send_or_acknowledge_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let mut receiver = Receiver::start().await;
    let events = open(dir.path(), "token").await;
    let config = EnvWebhookConfig::from_values(
        Some(&receiver.url),
        None,
        Some(SECRET),
        None,
        Some(r#"{"body_template":{"$ref":"/event/missing"}}"#),
    )
    .unwrap()
    .unwrap();
    events.apply_env_webhook(Some(config)).await.unwrap();
    events
        .publish(
            &event("friend_request", "invalid-template"),
            &pending_external(),
        )
        .await
        .unwrap();
    assert!(events.delivery_tick(None).await.unwrap());
    assert!(receiver.drain().is_empty());
    assert_eq!(counts(&events).await, (1, 0, 1));
}
