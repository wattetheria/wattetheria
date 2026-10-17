use super::*;
use crate::mcp_events::{AgentEventMode, EnvWebhookConfig, McpEvents};
use crate::routes::space::world::parse_world_event;
use wattetheria_kernel::swarm_bridge::{EventAnnouncementPayload, HybridSwarmBridge};

fn announcement() -> EventAnnouncementPayload {
    signed_announcement("world-event-real-signature", "mainnet:test", false)
}

fn signed_announcement(id: &str, network_id: &str, expired: bool) -> EventAnnouncementPayload {
    let now = Utc::now().timestamp_millis();
    signed_announcement_ending_at(
        id,
        network_id,
        if expired {
            now - 60_000
        } else {
            now + 3_600_000
        },
    )
}

fn signed_announcement_ending_at(
    id: &str,
    network_id: &str,
    ends_at: i64,
) -> EventAnnouncementPayload {
    let identity = Identity::new_random();
    let now = Utc::now().timestamp_millis();
    let mut announcement = EventAnnouncementPayload {
        announcement_id: id.to_owned(),
        network_id: network_id.to_owned(),
        authority_node_id: hex::encode(STANDARD.decode(&identity.public_key).unwrap()),
        issued_at: Utc::now().timestamp_millis().try_into().unwrap(),
        content: json!({"event_id":id,"space_id":"the-ville","title":"Park gathering","description":"Meet at the park","place_id":"johnson-park","starts_at":now-120_000,"ends_at":ends_at}),
        authority_signature: String::new(),
    };
    announcement.authority_signature = hex::encode(
        STANDARD
            .decode(
                identity
                    .sign(&canonical_bytes(&announcement.unsigned_payload()).unwrap())
                    .unwrap(),
            )
            .unwrap(),
    );
    announcement
}

async fn global_projection_server(
    announcement: EventAnnouncementPayload,
) -> (String, tokio::task::JoinHandle<()>) {
    let fixture = Arc::new(Mutex::new(GlobalFixture {
        network_id: announcement.network_id.clone(),
        messages: vec![projection_message(1, &announcement)],
        ..GlobalFixture::default()
    }));
    projection_fixture_server(fixture).await
}

#[derive(Default)]
struct GlobalFixture {
    network_id: String,
    messages: Vec<Value>,
    calls: Vec<std::collections::HashMap<String, String>>,
    pages: std::collections::BTreeMap<u64, Value>,
    delay_after: Option<u64>,
}

fn projection_message(sequence: u64, announcement: &EventAnnouncementPayload) -> Value {
    json!({"sequence":sequence,"message_id":format!("outer-{sequence}"),"kind":"EventAnnouncement","lane":"events","scope_hint":"global","author_node_id":"relay-node",
        "content":{"type":"EventAnnouncement","payload":announcement},"created_at":1})
}

async fn projection_fixture_server(
    fixture: Arc<Mutex<GlobalFixture>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new().route("/api/global/messages",get(move |axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>| {
        let fixture = fixture.clone();
        async move {
                let delay = {
                    let mut fixture = fixture.lock().await;
                    fixture.calls.push(query.clone());
                    query.get("after_sequence").and_then(|value| value.parse::<u64>().ok())
                        .is_some_and(|after| fixture.delay_after == Some(after))
                };
                if delay {
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                }
                let fixture = fixture.lock().await;
            if let Some(id) = query.get("announcement_id") {
                let announcement = fixture.messages.iter().map(|message| &message["content"]["payload"]).find(|announcement| {
                    announcement["announcement_id"].as_str() == Some(id.as_str())
                        && announcement["network_id"].as_str() == query.get("network_id").map(String::as_str)
                });
                return Json(json!({"announcement":announcement}));
            }
            if let Some(after) = query.get("after_sequence") {
                let after: u64 = after.parse().unwrap();
                if let Some(page) = fixture.pages.get(&after) {
                    return Json(page.clone());
                }
                let all: Vec<_> = fixture.messages.iter().filter(|message| message["sequence"].as_u64().unwrap() > after).cloned().collect();
                let limit: usize = query.get("limit").unwrap().parse().unwrap();
                let page: Vec<_> = all.iter().take(limit).cloned().collect();
                let next = (all.len() > limit).then(|| page.last().unwrap()["sequence"].as_u64().unwrap());
                return Json(json!({"network_id":fixture.network_id,"messages":page,"next_after_sequence":next}));
            }
            let before = query.get("before_sequence").map(|value| value.parse::<u64>().unwrap());
            let messages: Vec<_> = fixture.messages.iter().rev().filter(|message| before.is_none_or(|before| message["sequence"].as_u64().unwrap() < before)).cloned().collect();
            Json(json!({"messages":messages}))
        }
    })).route("/api/wattetheria/network/snapshot",get(|| async { Json(json!({"network_id":"mainnet:test"})) }));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{address}"), server)
}

async fn callback_state(
    projection_url: &str,
    receiver_url: &str,
) -> (tempfile::TempDir, ControlPlaneState, Arc<McpEvents>) {
    let (dir, _router, token, _policy, mut state) = build_test_app(100);
    let events = Arc::new(
        McpEvents::open(
            dir.path().join("world-events.sqlite"),
            dir.path().join("world-events.key"),
            state.agent_did.clone(),
            &token,
        )
        .await
        .unwrap(),
    );
    events
        .apply_env_webhook(Some(EnvWebhookConfig {
            url: receiver_url.to_owned(),
            headers: axum::http::HeaderMap::new(),
            secret: Some("whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD".to_owned()),
            include_text: false,
        }))
        .await
        .unwrap();
    state.mcp_events = Some(events.clone());
    state.agent_event_mode = AgentEventMode::McpEvents;
    state.swarm_bridge = Arc::new(HybridSwarmBridge::new(
        dir.path().join("missions/state.json"),
        Some(projection_url),
    ));
    (dir, state, events)
}

fn callback_body(
    announcement: &EventAnnouncementPayload,
    agent_did: &str,
    event_id: &str,
) -> Value {
    json!({"event": {
        "event_id":event_id,"event_type":"global_announcement_received","source_kind":"global_announcement","source_node_id":"relay-node",
        "target_agent_id":agent_did,"payload":announcement,"requires_commit":false,"allowed_actions":["ignore"],"created_at":announcement.issued_at
    }})
}

#[tokio::test]
async fn world_event_two_real_callbacks_deliver_same_authority_announcement_to_mcp_receivers() {
    let announcement = announcement();
    let (projection_url, projection) = global_projection_server(announcement.clone()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let receiver_router = Router::new().route(
        "/wake",
        post(
            move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let sender = sender.clone();
                async move {
                    sender.send((headers, body)).unwrap();
                    StatusCode::OK
                }
            },
        ),
    );
    let webhook = tokio::spawn(async move {
        axum::serve(listener, receiver_router).await.unwrap();
    });
    let receiver_url = format!("http://{address}/wake");
    for _ in 0..2 {
        let (_dir, state, events) = callback_state(&projection_url, &receiver_url).await;
        let mut stream = state.stream_tx.subscribe();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = app(state.clone());
        let callback = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = reqwest::Client::new();
        for id in ["outer-first-delivery", "outer-replay-delivery"] {
            let response: Value = client
                .post(format!("http://{address}/agent-events"))
                .json(&callback_body(&announcement, &state.agent_did, id))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(response["ok"], true);
        }
        assert!(events.delivery_tick(Some("env_webhook")).await.unwrap());
        let (headers, body) =
            tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
                .await
                .unwrap()
                .unwrap();
        standardwebhooks::Webhook::new("whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD")
            .unwrap()
            .verify(&body, &headers)
            .unwrap();
        let notification: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(notification["name"], "wattetheria.world.event");
        assert_eq!(
            notification["data"]["announcement"],
            serde_json::to_value(&announcement).unwrap()
        );
        assert!(!events.delivery_tick(Some("env_webhook")).await.unwrap());
        let event = stream.recv().await.unwrap();
        let gateway = crate::build_signed_node_event(&state, &event)
            .unwrap()
            .unwrap();
        assert_eq!(
            gateway.payload.data_kind,
            wattetheria_gateway_contract::DataKind::WorldEvent
        );
        assert_eq!(
            gateway.payload.payload,
            serde_json::to_value(&announcement).unwrap()
        );
        callback.abort();
    }
    assert!(receiver.try_recv().is_err());
    webhook.abort();
    projection.abort();
}

#[tokio::test]
async fn world_event_rejects_tampering_unknown_place_and_unstored_valid_signer() {
    let original = announcement();
    let (projection_url, projection) = global_projection_server(original.clone()).await;
    let (_dir, _router, _token, _policy, mut state) = build_test_app(100);
    state.swarm_bridge = Arc::new(HybridSwarmBridge::new(
        state.data_dir.join("missions/state.json"),
        Some(&projection_url),
    ));
    assert!(parse_world_event(&state, &original).unwrap().is_some());
    let mut tampered = original.clone();
    tampered.content["title"] = json!("tampered");
    assert!(
        parse_world_event(&state, &tampered)
            .unwrap_err()
            .to_string()
            .contains("invalid authority signature")
    );
    let mut unknown = original.clone();
    unknown.content["place_id"] = json!("unknown-place");
    assert!(parse_world_event(&state, &unknown).is_err());
    let rogue = announcement();
    assert_ne!(rogue.authority_node_id, original.authority_node_id);
    let response = app(state.clone())
        .oneshot(
            axum::http::Request::post("/agent-events")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    callback_body(&rogue, &state.agent_did, "rogue-callback").to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    projection.abort();
}

#[tokio::test]
async fn world_event_other_space_generic_announcement_preserves_global_board_and_callback() {
    let mut generic = announcement();
    generic.content = json!({"space_id":"another-space","notice":"generic opaque content"});
    let (projection_url, projection) = global_projection_server(generic.clone()).await;
    let (_dir, _router, token, _policy, mut state) = build_test_app(100);
    state.swarm_bridge = Arc::new(HybridSwarmBridge::new(
        state.data_dir.join("missions/state.json"),
        Some(&projection_url),
    ));
    assert_eq!(parse_world_event(&state, &generic).unwrap(), None);
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        []
    );
    let board = authed_get_json(app(state.clone()), &token, "/v1/client/board?source=global").await;
    assert_eq!(
        board["messages"][0]["content"]["payload"],
        serde_json::to_value(&generic).unwrap()
    );
    assert_eq!(board["messages"][0]["world_event"], Value::Null);
    let response = app(state.clone())
        .oneshot(
            axum::http::Request::post("/agent-events")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    callback_body(&generic, &state.agent_did, "generic").to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["decision"], Value::Null);
    assert_eq!(
        crate::routes::space::snapshot::export(&state)
            .await
            .unwrap()
            .world_events,
        []
    );
    projection.abort();
}

#[tokio::test]
async fn expired_world_events_ack_without_runtime_or_mcp_notification_and_keep_board_history() {
    let expired = signed_announcement("expired-world-event", "mainnet:test", true);
    let fixture = Arc::new(Mutex::new(GlobalFixture {
        network_id: expired.network_id.clone(),
        messages: vec![projection_message(1, &expired)],
        ..GlobalFixture::default()
    }));
    let (url, server) = projection_fixture_server(fixture.clone()).await;
    for mode in [AgentEventMode::ApiRuntime, AgentEventMode::McpEvents] {
        let (_dir, mut state, events) = callback_state(&url, "http://127.0.0.1:1/wake").await;
        state.agent_event_mode = mode;
        let mut stream = state.stream_tx.subscribe();
        let response = app(state.clone())
            .oneshot(
                axum::http::Request::post("/agent-events")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        callback_body(&expired, &state.agent_did, "expired-delivery").to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["decision"], Value::Null);
        assert!(stream.try_recv().is_err());
        assert!(!events.delivery_tick(Some("env_webhook")).await.unwrap());
        assert_eq!(
            crate::routes::space::snapshot::export(&state)
                .await
                .unwrap()
                .world_events,
            []
        );
        let board = authed_get_json(
            app(state.clone()),
            &state.auth_token,
            "/v1/client/board?source=global",
        )
        .await;
        assert_eq!(
            board["messages"][0]["content"]["payload"],
            serde_json::to_value(&expired).unwrap()
        );
        assert_eq!(
            board["messages"][0]["world_event"]["event_id"],
            expired.announcement_id
        );
    }
    let fixture = fixture.lock().await;
    let lookups: Vec<_> = fixture
        .calls
        .iter()
        .filter(|query| query.contains_key("announcement_id"))
        .collect();
    assert_eq!(lookups.len(), 2);
    assert!(lookups.iter().all(
        |query| !query.contains_key("after_sequence") && !query.contains_key("before_sequence")
    ));
    server.abort();
}

#[tokio::test]
async fn world_events_cold_forward_pages_warm_cursor_bad_records_and_restart_preserve_originals() {
    let first = signed_announcement("first-active", "mainnet:test", false);
    let last = signed_announcement("last-active", "mainnet:test", false);
    let expired = signed_announcement("expired-history", "mainnet:test", true);
    let mut bad = first.clone();
    bad.content["title"] = json!("invalid signature");
    let mut messages: Vec<_> = (1..=205).map(|sequence| json!({
        "sequence":sequence,"message_id":format!("generic-{sequence}"),"kind":"TaskCreated","lane":"events","scope_hint":"global","author_node_id":"relay-node","content":{"type":"TaskCreated","payload":{}},"created_at":1
    })).collect();
    messages[0] = projection_message(1, &first);
    messages[1] = projection_message(2, &bad);
    messages[2] = projection_message(3, &expired);
    messages[204] = projection_message(205, &last);
    let fixture = Arc::new(Mutex::new(GlobalFixture {
        network_id: first.network_id.clone(),
        messages,
        ..GlobalFixture::default()
    }));
    let (url, server) = projection_fixture_server(fixture.clone()).await;
    let (_dir, _router, _token, _policy, mut state) = build_test_app(100);
    state.swarm_bridge = Arc::new(HybridSwarmBridge::new(
        state.data_dir.join("missions/state.json"),
        Some(&url),
    ));
    let expected = vec![first.clone(), last.clone()];
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        expected
    );
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        expected
    );
    let new = signed_announcement("new-active", "mainnet:test", false);
    fixture
        .lock()
        .await
        .messages
        .push(projection_message(206, &new));
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        vec![first.clone(), last.clone(), new.clone()]
    );
    let cursors: Vec<_> = fixture
        .lock()
        .await
        .calls
        .iter()
        .map(|query| query["after_sequence"].clone())
        .collect();
    assert_eq!(cursors, ["0", "200", "205", "205"]);
    state.world_event_registry = Arc::new(Mutex::new(crate::WorldEventRegistry::default()));
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        vec![first.clone(), last.clone(), new.clone()]
    );
    server.abort();
    server.await.unwrap_err();
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        vec![first, last, new]
    );
}

#[tokio::test]
async fn world_events_single_callback_lookup_does_not_skip_older_cold_history() {
    let older = signed_announcement("older-active", "mainnet:test", false);
    let current = signed_announcement("current-active", "mainnet:test", false);
    let fixture = Arc::new(Mutex::new(GlobalFixture {
        network_id: current.network_id.clone(),
        messages: vec![
            projection_message(1, &older),
            projection_message(2, &current),
        ],
        ..GlobalFixture::default()
    }));
    let (url, server) = projection_fixture_server(fixture.clone()).await;
    let (_dir, state, _) = callback_state(&url, "http://127.0.0.1:1/wake").await;
    let response = app(state.clone())
        .oneshot(
            axum::http::Request::post("/agent-events")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    callback_body(&current, &state.agent_did, "callback").to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    {
        let fixture = fixture.lock().await;
        assert_eq!(fixture.calls.len(), 1);
        assert_eq!(fixture.calls[0]["announcement_id"], current.announcement_id);
        assert_eq!(fixture.calls[0]["network_id"], current.network_id);
        assert!(!fixture.calls[0].contains_key("after_sequence"));
    }
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        vec![current, older]
    );
    assert_eq!(fixture.lock().await.calls[1]["after_sequence"], "0");
    server.abort();
}

#[tokio::test]
async fn world_events_sparse_empty_forward_pages_advance_and_network_change_clears_cache() {
    let first = signed_announcement("main-active", "mainnet:test", false);
    let next = signed_announcement("new-network-active", "different-network", false);
    let fixture = Arc::new(Mutex::new(GlobalFixture {
        network_id: first.network_id.clone(),
        pages: std::collections::BTreeMap::from([
            (
                0,
                json!({"network_id":"mainnet:test","messages":[],"next_after_sequence":10}),
            ),
            (
                10,
                json!({"network_id":"mainnet:test","messages":[projection_message(11, &first)],"next_after_sequence":20}),
            ),
            (
                20,
                json!({"network_id":"mainnet:test","messages":[],"next_after_sequence":null}),
            ),
        ]),
        ..GlobalFixture::default()
    }));
    let (url, server) = projection_fixture_server(fixture.clone()).await;
    let (_dir, _router, _token, _policy, mut state) = build_test_app(100);
    state.swarm_bridge = Arc::new(HybridSwarmBridge::new(
        state.data_dir.join("missions/state.json"),
        Some(&url),
    ));
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        vec![first.clone()]
    );
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        vec![first]
    );
    {
        let mut fixture = fixture.lock().await;
        let cursors: Vec<_> = fixture
            .calls
            .iter()
            .map(|query| query["after_sequence"].clone())
            .collect();
        assert_eq!(cursors, ["0", "10", "20", "20"]);
        fixture.pages.clear();
        fixture.network_id = next.network_id.clone();
        fixture.messages = vec![projection_message(1, &next)];
    }
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        vec![next]
    );
    let fixture = fixture.lock().await;
    assert_eq!(fixture.calls[4]["after_sequence"], "20");
    assert_eq!(fixture.calls[5]["after_sequence"], "0");
    server.abort();
}

#[tokio::test]
async fn world_events_partial_refresh_timeout_keeps_page_cursor_and_valid_payload() {
    let first = signed_announcement("first-page-active", "mainnet:test", false);
    let last = signed_announcement("second-page-active", "mainnet:test", false);
    let fixture = Arc::new(Mutex::new(GlobalFixture {
        network_id: first.network_id.clone(),
        pages: std::collections::BTreeMap::from([
            (
                0,
                json!({"network_id":"mainnet:test","messages":[projection_message(1, &first)],"next_after_sequence":10}),
            ),
            (
                10,
                json!({"network_id":"mainnet:test","messages":[projection_message(11, &last)],"next_after_sequence":null}),
            ),
        ]),
        delay_after: Some(10),
        ..GlobalFixture::default()
    }));
    let (url, server) = projection_fixture_server(fixture.clone()).await;
    let (_dir, _router, _token, _policy, mut state) = build_test_app(100);
    state.swarm_bridge = Arc::new(HybridSwarmBridge::new(
        state.data_dir.join("missions/state.json"),
        Some(&url),
    ));
    let started = std::time::Instant::now();
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        vec![first.clone()]
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    fixture.lock().await.delay_after = None;
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        vec![first, last]
    );
    let fixture = fixture.lock().await;
    let cursors: Vec<_> = fixture
        .calls
        .iter()
        .map(|query| query["after_sequence"].clone())
        .collect();
    assert_eq!(cursors, ["0", "10", "10"]);
    server.abort();
}

#[tokio::test]
async fn world_events_active_read_prunes_expiry_without_mutating_signed_history() {
    let (_dir, _router, _token, _policy, mut state) = build_test_app(100);
    let now = Utc::now().timestamp_millis();
    let short = signed_announcement_ending_at("short-lived", "mainnet:test", now + 1_500);
    let fixture = Arc::new(Mutex::new(GlobalFixture {
        network_id: short.network_id.clone(),
        messages: vec![projection_message(1, &short)],
        ..GlobalFixture::default()
    }));
    let (url, server) = projection_fixture_server(fixture.clone()).await;
    state.swarm_bridge = Arc::new(HybridSwarmBridge::new(
        state.data_dir.join("missions/state.json"),
        Some(&url),
    ));
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        vec![short.clone()]
    );
    tokio::time::sleep(std::time::Duration::from_millis(1_600)).await;
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        []
    );
    let fixture = fixture.lock().await;
    assert_eq!(
        fixture.messages[0]["content"]["payload"],
        serde_json::to_value(short).unwrap()
    );
    assert_eq!(fixture.calls[1]["after_sequence"], "1");
    server.abort();
}
