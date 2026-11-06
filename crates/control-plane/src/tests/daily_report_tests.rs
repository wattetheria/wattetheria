use super::*;
use crate::mcp_events::{AgentEventMode, EnvWebhookConfig, McpEvents};
use crate::routes::space::daily::send_due_report;
use chrono::Timelike;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Default)]
struct MockDaily {
    down: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
}

/// A gateway serving both halves of the daily report, which can be taken down.
async fn spawn_mock_gateway(mock: MockDaily) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let answer = |mock: MockDaily, body: Value| {
        mock.reads.fetch_add(1, Ordering::SeqCst);
        if mock.down.load(Ordering::SeqCst) {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "down"})),
            )
        } else {
            (StatusCode::OK, Json(body))
        }
    };
    let router = Router::new()
        .route(
            "/api/spaces/daily",
            get(
                move |axum::extract::State(mock): axum::extract::State<MockDaily>,
                      axum::extract::Query(query): axum::extract::Query<
                    BTreeMap<String, String>,
                >| async move {
                    answer(mock, json!({"day": query["day"], "new_agents": 2}))
                },
            ),
        )
        .route(
            "/api/agents/{did}/daily",
            get(
                move |axum::extract::State(mock): axum::extract::State<MockDaily>,
                      axum::extract::Path(did): axum::extract::Path<String>,
                      axum::extract::Query(query): axum::extract::Query<
                    BTreeMap<String, String>,
                >| async move {
                    let offset: i32 = query["offset_secs"].parse().unwrap();
                    answer(
                        mock,
                        json!({"day": query["day"], "offset_secs": offset, "agent_did": did, "watt": {"net": 5}}),
                    )
                },
            ),
        )
        .with_state(mock);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{address}"), server)
}

/// A webhook the node notifies, and the node state wired to it and to `gateway_url`.
async fn node_with_webhook(
    gateway_url: String,
) -> (
    tempfile::TempDir,
    ControlPlaneState,
    Arc<McpEvents>,
    tokio::sync::mpsc::UnboundedReceiver<axum::body::Bytes>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let receiver_router = Router::new().route(
        "/wake",
        post(move |body: axum::body::Bytes| {
            let sender = sender.clone();
            async move {
                sender.send(body).unwrap();
                StatusCode::OK
            }
        }),
    );
    let webhook = tokio::spawn(async move {
        axum::serve(listener, receiver_router).await.unwrap();
    });
    let (dir, _router, token, _policy, mut state) = build_test_app(100);
    let events = Arc::new(
        McpEvents::open(
            dir.path().join("daily-events.sqlite"),
            dir.path().join("daily-events.key"),
            state.agent_did.clone(),
            &token,
        )
        .await
        .unwrap(),
    );
    events
        .apply_env_webhook(Some(EnvWebhookConfig {
            url: format!("http://{address}/wake"),
            headers: axum::http::HeaderMap::new(),
            secret: Some("whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD".to_owned()),
            include_text: false,
            request: None,
        }))
        .await
        .unwrap();
    state.mcp_events = Some(events.clone());
    state.agent_event_mode = AgentEventMode::McpEvents;
    state.gateway_urls = vec![gateway_url];
    (dir, state, events, receiver, webhook)
}

async fn next_notification(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<axum::body::Bytes>,
) -> Value {
    let body = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn the_daily_report_reaches_the_agent_once_a_day_and_retries_while_the_gateway_is_down() {
    let mock = MockDaily::default();
    mock.down.store(true, Ordering::SeqCst);
    let (gateway_url, gateway) = spawn_mock_gateway(mock.clone()).await;
    let (_dir, state, events, mut receiver, webhook) = node_with_webhook(gateway_url).await;

    // The gateway is down: nothing is sent, and the report stays due.
    assert_eq!(send_due_report(&state).await, 0);
    assert!(!events.delivery_tick(Some("env_webhook")).await.unwrap());

    mock.down.store(false, Ordering::SeqCst);
    assert_eq!(send_due_report(&state).await, 1);
    assert!(events.delivery_tick(Some("env_webhook")).await.unwrap());
    let notification = next_notification(&mut receiver).await;
    let yesterday = (Utc::now() - chrono::TimeDelta::days(1))
        .date_naive()
        .to_string();
    assert_eq!(notification["name"], "wattetheria.agent.daily_report");
    assert_eq!(notification["data"]["day"], yesterday);
    assert_eq!(notification["data"]["space"]["new_agents"], 2);
    assert_eq!(notification["data"]["agent"]["agent_did"], state.agent_did);
    assert_eq!(notification["data"]["agent"]["day"], yesterday);
    // The owner's time zone is unknown: the UTC day, and no timezone in the event.
    assert_eq!(notification["data"]["agent"]["offset_secs"], 0);
    assert!(notification["data"].get("timezone").is_none());

    // Sent for the day: later ticks neither read the gateway nor notify again.
    let reads = mock.reads.load(Ordering::SeqCst);
    assert_eq!(send_due_report(&state).await, 0);
    assert_eq!(mock.reads.load(Ordering::SeqCst), reads);
    assert!(!events.delivery_tick(Some("env_webhook")).await.unwrap());
    gateway.abort();
    webhook.abort();
}

/// The fixed-offset zone where it is `hour` o'clock now, and its offset in hours.
fn zone_at_hour(hour: u32) -> (String, i32) {
    let now = i32::try_from(Utc::now().hour()).unwrap();
    let mut offset = (i32::try_from(hour).unwrap() - now).rem_euclid(24);
    if offset > 14 {
        offset -= 24;
    }
    // IANA's Etc zones count the other way round: Etc/GMT-10 is ten hours ahead of UTC.
    let name = match offset {
        0 => "Etc/GMT".to_owned(),
        ahead if ahead > 0 => format!("Etc/GMT-{ahead}"),
        behind => format!("Etc/GMT+{}", -behind),
    };
    (name, offset)
}

async fn set_timezone(state: &ControlPlaneState, token: &str, timezone: &str) -> StatusCode {
    app(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/daily/timezone")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"timezone": timezone}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn the_daily_report_waits_for_the_owners_morning_and_covers_the_owners_day() {
    let mock = MockDaily::default();
    let (gateway_url, gateway) = spawn_mock_gateway(mock.clone()).await;
    let (_dir, state, events, mut receiver, webhook) = node_with_webhook(gateway_url).await;
    let token = state.auth_token.clone();
    assert_eq!(
        set_timezone(&state, &token, "Mars/Olympus").await,
        StatusCode::BAD_REQUEST
    );

    // Three in the owner's night: nothing is read or sent.
    let (night, _) = zone_at_hour(3);
    assert_eq!(set_timezone(&state, &token, &night).await, StatusCode::OK);
    assert_eq!(send_due_report(&state).await, 0);
    assert_eq!(mock.reads.load(Ordering::SeqCst), 0);

    // Ten in the owner's morning: the owner's yesterday goes out, cut at the owner's midnight.
    let (morning, hours) = zone_at_hour(10);
    assert_eq!(set_timezone(&state, &token, &morning).await, StatusCode::OK);
    assert_eq!(send_due_report(&state).await, 1);
    assert!(events.delivery_tick(Some("env_webhook")).await.unwrap());
    let notification = next_notification(&mut receiver).await;
    let local = Utc::now() + chrono::TimeDelta::hours(hours.into());
    let yesterday = (local - chrono::TimeDelta::days(1))
        .date_naive()
        .to_string();
    assert_eq!(notification["data"]["day"], yesterday);
    assert_eq!(notification["data"]["timezone"], morning);
    assert_eq!(notification["data"]["agent"]["day"], yesterday);
    assert_eq!(notification["data"]["agent"]["offset_secs"], hours * 3600);
    assert_eq!(send_due_report(&state).await, 0);
    gateway.abort();
    webhook.abort();
}

async fn set_enabled(state: &ControlPlaneState, token: &str, enabled: bool) -> StatusCode {
    app(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/daily/enabled")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"enabled": enabled}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn the_daily_report_is_on_by_default_and_stays_quiet_while_turned_off() {
    let mock = MockDaily::default();
    let (gateway_url, gateway) = spawn_mock_gateway(mock.clone()).await;
    let (_dir, state, events, mut receiver, webhook) = node_with_webhook(gateway_url).await;
    let token = state.auth_token.clone();

    // Turned off: the gateway is not read and nothing reaches the agent.
    assert_eq!(set_enabled(&state, &token, false).await, StatusCode::OK);
    assert_eq!(send_due_report(&state).await, 0);
    assert_eq!(mock.reads.load(Ordering::SeqCst), 0);
    assert!(!events.delivery_tick(Some("env_webhook")).await.unwrap());

    // Turned back on: yesterday's report goes out on the next tick.
    assert_eq!(set_enabled(&state, &token, true).await, StatusCode::OK);
    assert_eq!(send_due_report(&state).await, 1);
    assert!(events.delivery_tick(Some("env_webhook")).await.unwrap());
    let notification = next_notification(&mut receiver).await;
    assert_eq!(notification["name"], "wattetheria.agent.daily_report");
    gateway.abort();
    webhook.abort();
}

/// A Board message in `category`, made at `created_at_ms`, by Bob, mentioning `mention`.
fn board_message(
    network_id: &str,
    category: &str,
    message_id: &str,
    created_at_ms: i64,
    mention: Option<&str>,
) -> SwarmTopicMessageView {
    SwarmTopicMessageView {
        message_id: message_id.to_owned(),
        network_id: network_id.to_owned(),
        feed_key: format!("wattetheria.board.{category}"),
        scope_hint: format!("group:board-{category}"),
        author_node_id: "node-bob".to_owned(),
        agent_envelope: Some(
            serde_json::from_value(json!({
                "protocol": "wattetheria.agent.envelope.v1",
                "source_agent_id": "did:key:z6Mkbob",
                "target_agent_id": mention,
                "message": {"author_display_name": "Bob"},
            }))
            .unwrap(),
        ),
        content: json!(format!("{message_id} text")),
        reply_to_message_id: None,
        created_at: created_at_ms.cast_unsigned(),
    }
}

#[tokio::test]
async fn the_daily_report_carries_the_agents_own_activity_and_the_boards_day() {
    let (gateway_url, gateway) = spawn_mock_gateway(MockDaily::default()).await;
    let (_dir, mut state, events, mut receiver, webhook) = node_with_webhook(gateway_url).await;
    let bridge = Arc::new(MockSwarmBridge::default_for(state.agent_did.clone()));
    state.swarm_bridge = bridge.clone();
    let network_id = state.swarm_bridge.current_network_id().await.unwrap();

    let today = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let yesterday_ms = (today - chrono::TimeDelta::hours(12)).timestamp_millis();
    let today_ms = (today + chrono::TimeDelta::minutes(1)).timestamp_millis();
    let old_ms = (today - chrono::TimeDelta::days(40)).timestamp_millis();
    for (kind, created_at_ms) in [
        ("post_hive_message", yesterday_ms),
        ("publish_mission", yesterday_ms + 1),
        ("post_hive_message", yesterday_ms + 2),
        ("send_agent_dm_message", today_ms),
        ("create_hive", old_ms),
    ] {
        state
            .local_db
            .append_agent_activity(&wattetheria_kernel::local_db::AgentActivity {
                kind: kind.to_owned(),
                subject: Some("hive-1".to_owned()),
                detail: json!({}),
                created_at_ms,
            })
            .unwrap();
    }
    let me = state.agent_did.clone();
    bridge.messages.lock().await.extend([
        board_message(&network_id, "general", "g-1", yesterday_ms, None),
        board_message(&network_id, "general", "g-2", yesterday_ms + 5, Some(&me)),
        board_message(
            &network_id,
            "trade",
            "t-1",
            yesterday_ms,
            Some("did:key:z6Mkcarol"),
        ),
        board_message(&network_id, "general", "g-today", today_ms, Some(&me)),
    ]);

    assert_eq!(send_due_report(&state).await, 1);
    assert!(events.delivery_tick(Some("env_webhook")).await.unwrap());
    let data = next_notification(&mut receiver).await["data"].clone();

    // Yesterday's activity only, counted by tool, oldest first.
    let activity = &data["agent"]["activity"];
    assert_eq!(activity["total"], 3);
    assert_eq!(activity["counts"]["post_hive_message"], 2);
    assert_eq!(activity["counts"]["publish_mission"], 1);
    assert!(activity["counts"].get("send_agent_dm_message").is_none());
    assert_eq!(activity["items"][1]["kind"], "publish_mission");
    assert!(
        activity["items"][0]["at"].as_str().unwrap().starts_with(
            &(today - chrono::TimeDelta::days(1))
                .date_naive()
                .to_string()
        )
    );

    // The Board's day: per-channel counts and previews, and the one message for this agent.
    let board = &data["space"]["board"];
    assert_eq!(board["channels"]["general"]["messages"], 2);
    assert_eq!(board["channels"]["trade"]["messages"], 1);
    assert_eq!(board["channels"]["search"]["messages"], 0);
    assert_eq!(
        board["channels"]["general"]["latest"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(board["mentioned"], 1);
    assert_eq!(board["mentions"][0]["message_id"], "g-2");
    assert_eq!(board["mentions"][0]["from"], "Bob");
    assert_eq!(board["mentions"][0]["text"], "g-2 text");
    assert_eq!(board["mentions"][0]["category"], "general");

    // Sending the report drops activity older than the days kept.
    let kept = state.local_db.agent_activity_between(0, i64::MAX).unwrap();
    assert_eq!(kept.len(), 4);
    assert!(kept.iter().all(|row| row.kind != "create_hive"));
    gateway.abort();
    webhook.abort();
}
