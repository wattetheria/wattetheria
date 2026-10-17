use super::*;
use wattetheria_kernel::swarm_bridge::{SwarmAgentView, SwarmBridge};

struct UnavailableNetworkBridge;

#[async_trait::async_trait]
impl SwarmBridge for UnavailableNetworkBridge {
    async fn agent_view(&self, _agent_did: &str) -> anyhow::Result<SwarmAgentView> {
        anyhow::bail!("network unavailable")
    }
}

#[tokio::test]
async fn leave_without_entry_needs_neither_catalog_nor_network_and_writes_nothing() {
    let (dir, _app, token, _policy, mut state) = build_test_app(100);
    state.space_catalog = None;
    state.swarm_bridge = Arc::new(UnavailableNetworkBridge);
    state.local_db = Arc::new(LocalDb::open(dir.path().join("no-network.db")).unwrap());
    assert!(state.swarm_bridge.current_network_id().await.is_err());
    assert!(
        wattetheria_kernel::network_agent_registration::load_network_permission_checkpoint(
            &state.local_db,
            &state.agent_did,
            None,
        )
        .unwrap()
        .is_none()
    );
    let mut events = state.stream_tx.subscribe();
    let left = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/leave",
        json!({}),
    )
    .await;
    assert_eq!(left, json!({"ok":true,"presence":null}));
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert!(state.own_space_presence.lock().await.networks.is_empty());
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        None
    );
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn leave_signs_one_inactive_record_and_repeat_is_a_strict_noop() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let state = disk_state(state, &dir.path().join("leave.db"));
    let entered = initialize_space_presence(&state).await.unwrap();
    let mut events = state.stream_tx.subscribe();
    let unauthorized = app(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/space/leave")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert!(state.own_space_presence.lock().await.active_space.is_some());

    let left = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/leave",
        json!({}),
    )
    .await;
    assert_eq!(left["ok"], true);
    let signed: SignedSpacePresence = serde_json::from_value(left["presence"].clone()).unwrap();
    assert!(!signed.record.entered);
    assert_eq!(signed.record.seq, entered.record.seq + 1);
    assert_eq!(signed.record.from_tile, entered.record.from_tile);
    assert_eq!(signed.record.path, "");
    assert_eq!(signed.record.depart_at, signed.record.updated_at);
    assert_eq!(signed.record.network_id, test_network_id(&state));
    assert!(verify_payload(&signed.record, &signed.signature, &signed.public_key).unwrap());
    assert_eq!(events.try_recv().unwrap().payload, json!(signed));
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    let persisted = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        persisted.stored(&test_network_id(&state), &signed.record.space_id),
        Some(&signed)
    );
    let db_after_leave = state
        .local_db
        .load_domain::<Value>("own_space_presence")
        .unwrap();
    let repeated = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/leave",
        json!({}),
    )
    .await;
    assert_eq!(repeated, json!({"ok":true,"presence":null}));
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        db_after_leave
    );
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("unrelated-existing-state")
            .unwrap(),
        Some(json!({"retained":true}))
    );
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn leave_advances_past_verified_gateway_sequence_floor() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    let entered = initialize_space_presence(&state).await.unwrap();
    let floor = super::sequence_recovery::signed_floor(&state, 43);
    let (url, server) = super::sequence_recovery::floor_server(floor).await;
    state.gateway_urls = vec![url];
    let left = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/leave",
        json!({}),
    )
    .await;
    let signed: SignedSpacePresence = serde_json::from_value(left["presence"].clone()).unwrap();
    assert!(!signed.record.entered);
    assert_eq!(signed.record.seq, 44);
    assert_eq!(signed.record.from_tile, entered.record.from_tile);
    assert!(verify_payload(&signed.record, &signed.signature, &signed.public_key).unwrap());
    server.abort();
}

#[tokio::test]
async fn leave_during_movement_stops_at_current_tile_and_reentry_does_not_drift() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    initialize_space_presence(&state).await.unwrap();
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"hobbs-cafe/cafe","activity":"walking"}),
    )
    .await;
    let mut moving: SignedSpacePresence =
        serde_json::from_value(moved["presence"].clone()).unwrap();
    assert!(moving.record.path.len() > 4);
    moving.record.depart_at = Utc::now().timestamp_millis() - 2_000;
    moving.signature = state.sign_payload(&moving.record).unwrap();
    current_package(&state)
        .validate_record(&moving.record)
        .unwrap();
    let network = test_network_id(&state);
    let mut registry = state.own_space_presence.lock().await;
    registry
        .networks
        .get_mut(&network)
        .unwrap()
        .insert(moving.record.space_id.clone(), moving.clone());
    state
        .local_db
        .save_domain("own_space_presence", &*registry)
        .unwrap();
    drop(registry);

    let left = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/leave",
        json!({}),
    )
    .await;
    assert_eq!(left["ok"], true);
    let left: SignedSpacePresence = serde_json::from_value(left["presence"].clone()).unwrap();
    let package = current_package(&state);
    assert_eq!(
        left.record.from_tile,
        moving
            .record
            .tile_at(package.walk_speed, left.record.depart_at)
    );
    assert_ne!(left.record.from_tile, moving.record.from_tile);
    assert_eq!(left.record.path, "");
    assert_eq!(left.record.depart_at, left.record.updated_at);
    assert_eq!(
        left.record.destination_place_id,
        package
            .place_at_tile(left.record.from_tile)
            .unwrap()
            .place_id
    );
    assert_eq!(left.record.activity, "walking");
    assert_eq!(left.record.seq, moving.record.seq + 1);
    package.validate_record(&left.record).unwrap();

    let reentered = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({}),
    )
    .await;
    let reentered: SignedSpacePresence =
        serde_json::from_value(reentered["presence"].clone()).unwrap();
    assert!(reentered.record.entered);
    assert_eq!(reentered.record.seq, left.record.seq + 1);
    assert_eq!(reentered.record.from_tile, left.record.from_tile);
    assert_eq!(reentered.record.path, "");
    assert_eq!(reentered.record.depart_at, left.record.depart_at);
    assert_eq!(
        reentered.record.destination_place_id,
        left.record.destination_place_id
    );
    assert_eq!(reentered.record.activity, left.record.activity);
    assert_eq!(
        reentered
            .record
            .tile_at(package.walk_speed, reentered.record.updated_at + 60_000),
        left.record.from_tile
    );
}

#[tokio::test]
async fn concurrent_leave_persists_and_emits_only_one_inactive_record() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let state = disk_state(state, &dir.path().join("concurrent-leave.db"));
    let entered = initialize_space_presence(&state).await.unwrap();
    let mut events = state.stream_tx.subscribe();
    let barrier = Arc::new(tokio::sync::Barrier::new(21));
    let mut requests = Vec::new();
    for _ in 0..20 {
        let state = state.clone();
        let token = token.clone();
        let barrier = barrier.clone();
        requests.push(tokio::spawn(async move {
            barrier.wait().await;
            authed_post_json(app(state), &token, "/v1/wattetheria/space/leave", json!({})).await
        }));
    }
    barrier.wait().await;
    let mut signed = Vec::new();
    for request in requests {
        let response = request.await.unwrap();
        assert_eq!(response["ok"], true);
        if !response["presence"].is_null() {
            signed.push(
                serde_json::from_value::<SignedSpacePresence>(response["presence"].clone())
                    .unwrap(),
            );
        }
    }
    assert_eq!(signed.len(), 1);
    assert!(!signed[0].record.entered);
    assert_eq!(signed[0].record.seq, entered.record.seq + 1);
    assert_eq!(events.try_recv().unwrap().payload, json!(signed[0]));
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    let persisted = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        persisted.stored(&test_network_id(&state), &signed[0].record.space_id),
        Some(&signed[0])
    );
}
