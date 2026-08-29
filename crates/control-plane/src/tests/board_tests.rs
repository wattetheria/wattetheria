use super::*;
use crate::routes::board::{
    BOARD_GENERAL_FEED_KEY, BOARD_MESSAGE_MAX_CHARS, BOARD_REQUEST_FEED_KEY, BOARD_SEARCH_FEED_KEY,
    BOARD_TRADE_FEED_KEY, SERVICES_BOARD_FEED_KEY, SERVICES_BOARD_SCOPE_HINT,
    ensure_startup_board_subscriptions,
};
use serde_json::json;

#[tokio::test]
async fn board_lists_fixed_channels_and_defaults_general_subscription() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let payload = authed_get_json(app, &token, "/v1/wattetheria/board/channels").await;
    let channels = payload["channels"].as_array().expect("channels array");

    assert_eq!(channels.len(), 4);
    assert_eq!(
        channels
            .iter()
            .map(|channel| channel["channel"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "board-general",
            "board-trade",
            "board-search",
            "board-request"
        ]
    );
    assert_eq!(
        channels
            .iter()
            .map(|channel| channel["feed_key"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            BOARD_GENERAL_FEED_KEY,
            BOARD_TRADE_FEED_KEY,
            BOARD_SEARCH_FEED_KEY,
            BOARD_REQUEST_FEED_KEY,
        ]
    );
    assert_eq!(channels[0]["subscribed"], true);
    assert_eq!(channels[1]["subscribed"], false);
}

#[tokio::test]
async fn board_default_subscription_does_not_depend_on_topic_bridge_flag() {
    let (_dir, _router, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        agent_topic_bridge_enabled: false,
        ..state
    };

    let payload = authed_get_json(app(state), &token, "/v1/wattetheria/board/channels").await;
    let channels = payload["channels"].as_array().expect("channels array");

    assert_eq!(channels[0]["category"], "general");
    assert_eq!(channels[0]["subscribed"], true);
    assert_eq!(channels[0]["default_subscribed"], true);
}

#[tokio::test]
async fn public_bootstrap_board_subscription_covers_all_board_channels() {
    let (_dir, _router, _token, _policy, state) = build_test_app(100);
    let local_node_id = state.agent_did.clone();
    let mut bridge = MockSwarmBridge::default_for(state.agent_did.clone());
    bridge.public_bootstrap = true;
    let bridge = Arc::new(bridge);
    let state = ControlPlaneState {
        swarm_bridge: bridge.clone(),
        ..state
    };

    ensure_startup_board_subscriptions(&state)
        .await
        .expect("bootstrap Board subscriptions");

    let subscriptions = bridge.subscriptions.lock().await;
    let active_scopes = subscriptions
        .iter()
        .filter(|subscription| subscription.4)
        .map(|subscription| subscription.3.as_str())
        .collect::<Vec<_>>();
    assert_eq!(active_scopes.len(), 5);
    assert!(active_scopes.contains(&"group:board-general"));
    assert!(active_scopes.contains(&"group:board-trade"));
    assert!(active_scopes.contains(&"group:board-search"));
    assert!(active_scopes.contains(&"group:board-request"));
    assert!(active_scopes.contains(&SERVICES_BOARD_SCOPE_HINT));
    let active_feed_keys = subscriptions
        .iter()
        .filter(|subscription| subscription.4)
        .map(|subscription| subscription.2.as_str())
        .collect::<Vec<_>>();
    assert_eq!(active_feed_keys.len(), 5);
    assert!(active_feed_keys.contains(&BOARD_GENERAL_FEED_KEY));
    assert!(active_feed_keys.contains(&BOARD_TRADE_FEED_KEY));
    assert!(active_feed_keys.contains(&BOARD_SEARCH_FEED_KEY));
    assert!(active_feed_keys.contains(&BOARD_REQUEST_FEED_KEY));
    assert!(active_feed_keys.contains(&SERVICES_BOARD_FEED_KEY));
    assert!(
        subscriptions
            .iter()
            .filter(|subscription| subscription.4)
            .all(|subscription| subscription.1 == local_node_id)
    );
    assert!(subscriptions.iter().any(|subscription| {
        subscription.2 == SERVICES_BOARD_FEED_KEY
            && subscription.3 == SERVICES_BOARD_SCOPE_HINT
            && subscription.4
    }));
}

#[tokio::test]
async fn board_publish_and_read_keep_category_group_isolated() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let published = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/board/messages",
        json!({
            "category": "trade",
            "message": {"text": "Looking for a translation service"}
        }),
    )
    .await;
    assert_eq!(published["ok"], true);
    assert_eq!(published["scope_hint"], "group:board-trade");

    let trade = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/board/messages?category=trade",
    )
    .await;
    assert_eq!(trade["channel"], "board-trade");
    assert_eq!(trade["feed_key"], BOARD_TRADE_FEED_KEY);
    assert_eq!(trade["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        trade["messages"][0]["content"]["text"],
        "Looking for a translation service"
    );

    let general = authed_get_json(
        app,
        &token,
        "/v1/wattetheria/board/messages?category=general",
    )
    .await;
    assert!(general["messages"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn board_publish_rejects_overlong_and_control_character_messages() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let overlong = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/board/messages",
        json!({
            "category": "general",
            "message": {"text": "x".repeat(BOARD_MESSAGE_MAX_CHARS + 1)}
        }),
    )
    .await;
    assert_eq!(
        overlong["error"],
        format!("board message must be at most {BOARD_MESSAGE_MAX_CHARS} characters")
    );
    assert_eq!(overlong["max_chars"], BOARD_MESSAGE_MAX_CHARS);

    let control = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/board/messages",
        json!({
            "category": "general",
            "message": {"text": "allowed\u{0000}rejected"}
        }),
    )
    .await;
    assert_eq!(
        control["error"],
        "board message contains unsupported control characters"
    );
}

#[tokio::test]
async fn board_lists_remote_service_agents_for_the_current_provider_did() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _router, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let payload = authed_get_json(
        app(state),
        &token,
        "/v1/wattetheria/board/service-agents/published",
    )
    .await;

    assert_eq!(payload["source"], "servicenet");
    assert_eq!(payload["count"], 1);
    assert_eq!(payload["items"][0]["service_name"], "alpha");
    assert_eq!(payload["items"][0]["service_address"], "alpha@wattetheria");
    assert!(payload["items"][0].get("agent_id").is_none());

    servicenet_server.abort();
}

#[tokio::test]
async fn service_board_messages_use_fixed_channel_and_filter_by_service_name() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _router, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let published = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/board/service-agents/messages",
        json!({
            "acting_as": "alpha",
            "message": {"text": "Service status is available"}
        }),
    )
    .await;
    assert_eq!(published["ok"], true);
    assert_eq!(published["scope_hint"], SERVICES_BOARD_SCOPE_HINT);

    let listed = authed_get_json(
        app,
        &token,
        "/v1/wattetheria/board/service-agents/messages?service_name=alpha",
    )
    .await;
    assert_eq!(listed["feed_key"], SERVICES_BOARD_FEED_KEY);
    assert_eq!(listed["scope_hint"], SERVICES_BOARD_SCOPE_HINT);
    assert_eq!(listed["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        listed["messages"][0]["content"]["service_agent"]["service_address"],
        "alpha@wattetheria"
    );

    servicenet_server.abort();
}
