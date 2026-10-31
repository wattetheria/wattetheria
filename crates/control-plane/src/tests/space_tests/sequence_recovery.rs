use super::*;
use crate::routes::space::reconcile_space_presence;

#[tokio::test]
async fn space_inactive_reconciliation_recovers_remote_only_saved_position_without_spawning() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    initialize_space_presence(&state).await.unwrap();
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"hobbs-cafe/cafe","activity":"coffee"}),
    )
    .await;
    let mut saved: SignedSpacePresence = serde_json::from_value(moved["presence"].clone()).unwrap();
    saved.record.seq = 500;
    saved.signature = state.sign_payload(&saved.record).unwrap();
    let (url, server) = floor_server(saved.clone()).await;
    let mut state = disk_state(state, &dir.path().join("remote-only.db"));
    state.own_space_presence = Arc::new(Mutex::new(OwnSpacePresence::default()));
    state.gateway_urls = vec![url];
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        None
    );
    let mut events = state.stream_tx.subscribe();
    crate::reset_space_presence(&state).await.unwrap();
    let inactive = state
        .own_space_presence
        .lock()
        .await
        .get(&test_network_id(&state), current_package(&state))
        .cloned()
        .unwrap();
    let mut expected = saved.record.clone();
    expected.entered = false;
    expected.seq += 1;
    expected.updated_at = inactive.record.updated_at;
    assert_eq!(inactive.record, expected);
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert_eq!(events.recv().await.unwrap().payload, json!(inactive));
    assert!(
        wattetheria_kernel::signing::verify_payload(
            &inactive.record,
            &inactive.signature,
            &inactive.public_key
        )
        .unwrap()
    );
    reconcile_space_presence(&state).await.unwrap();
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(
        crate::routes::space::snapshot::export(&state)
            .await
            .unwrap()
            .space_presences,
        vec![inactive.clone()]
    );
    let response =
        authed_get_json(app(state.clone()), &token, "/v1/wattetheria/space/presence").await;
    assert_eq!(response["error"], "agent presence not found");
    let entered = initialize_space_presence(&state).await.unwrap();
    expected.entered = true;
    expected.seq += 1;
    expected.updated_at = entered.record.updated_at;
    assert_eq!(entered.record, expected);
    assert_eq!(events.recv().await.unwrap().payload, json!(entered));
    assert_eq!(initialize_space_presence(&state).await.unwrap(), entered);
    server.abort();
}

#[tokio::test]
async fn space_inactive_reconciliation_retries_unreachable_gateway_without_implicit_entry() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    let source = initialize_space_presence(&state).await.unwrap();
    state.own_space_presence = Arc::new(Mutex::new(OwnSpacePresence::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    state.gateway_urls = vec![format!("http://{}", listener.local_addr().unwrap())];
    drop(listener);
    crate::reset_space_presence(&state).await.unwrap();
    assert!(state.own_space_presence.lock().await.networks.is_empty());
    let (url, server) = floor_server(source.clone()).await;
    state.gateway_urls = vec![url];
    reconcile_space_presence(&state).await.unwrap();
    let registry = state.own_space_presence.lock().await;
    let inactive = registry
        .get(&test_network_id(&state), current_package(&state))
        .unwrap();
    assert!(registry.active_space.is_none());
    assert!(!inactive.record.entered);
    assert_eq!(inactive.record.seq, source.record.seq + 1);
    assert_eq!(inactive.record.from_tile, source.record.from_tile);
    assert_eq!(inactive.record.path, source.record.path);
    drop(registry);
    server.abort();
}

pub(super) fn signed_floor(state: &ControlPlaneState, seq: u64) -> SignedSpacePresence {
    let record = wattetheria_gateway_contract::SpacePresenceRecord {
        agent_did: state.agent_did.clone(),
        avatar_id: None,
        skin_id: None,
        network_id: test_network_id(state),
        space_id: current_package(state).space_id.clone(),
        space_version: "retained-old-map".to_owned(),
        seq,
        entered: true,
        from_tile: [u32::MAX, u32::MAX],
        path: "R".to_owned(),
        depart_at: 1,
        destination_place_id: "retired-place".to_owned(),
        activity: "retained activity".to_owned(),
        object_place_id: None,
        updated_at: 1,
    };
    SignedSpacePresence {
        signature: state.sign_payload(&record).unwrap(),
        public_key: state.signer.public_key().to_owned(),
        record,
    }
}

pub(super) async fn floor_server(
    floor: SignedSpacePresence,
) -> (String, tokio::task::JoinHandle<()>) {
    let router = Router::new().route(
        "/api/spaces/presence/{agent_id}/record",
        get(move || {
            let floor = floor.clone();
            async move { Json(floor) }
        }),
    );
    recovery_server(router).await
}

async fn recovery_server(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{address}"), server)
}

async fn assert_failed_recovery_preserves_absent_presence(state: &ControlPlaneState, token: &str) {
    let mut events = state.stream_tx.subscribe();
    let rejected = authed_post_json(
        app(state.clone()),
        token,
        "/v1/wattetheria/space/enter",
        json!({}),
    )
    .await;
    assert_eq!(
        rejected["error"],
        "gateway presence sequence recovery unavailable; retry enter_space when a gateway is reachable"
    );
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert!(state.own_space_presence.lock().await.networks.is_empty());
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        None
    );
    assert_eq!(
        crate::routes::space::snapshot::export(state)
            .await
            .unwrap()
            .space_presences,
        [],
    );
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("unrelated-existing-state")
            .unwrap(),
        Some(json!({"retained":true})),
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn space_recovery_rejects_http_failures_malformed_records_and_request_timeout() {
    for (status, body) in [
        (StatusCode::UNAUTHORIZED, "unauthorized"),
        (StatusCode::FORBIDDEN, "forbidden"),
        (StatusCode::INTERNAL_SERVER_ERROR, "unavailable"),
        (StatusCode::BAD_GATEWAY, "unavailable"),
        (StatusCode::OK, "not JSON"),
        (StatusCode::OK, "{}"),
    ] {
        let (dir, _app, token, _policy, state) = build_test_app(100);
        let mut state = disk_state(state, &dir.path().join("failed-response.db"));
        let (url, server) = recovery_server(Router::new().route(
            "/api/spaces/presence/{agent_id}/record",
            get(move || async move { (status, body) }),
        ))
        .await;
        state.gateway_urls = vec![url];
        crate::reset_space_presence(&state).await.unwrap();
        reconcile_space_presence(&state).await.unwrap();
        assert_failed_recovery_preserves_absent_presence(&state, &token).await;
        server.abort();
    }
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let mut state = disk_state(state, &dir.path().join("timeout-response.db"));
    let (url, server) = recovery_server(Router::new().route(
        "/api/spaces/presence/{agent_id}/record",
        get(|| async { std::future::pending::<StatusCode>().await }),
    ))
    .await;
    state.gateway_urls = vec![url];
    assert_failed_recovery_preserves_absent_presence(&state, &token).await;
    server.abort();
}

#[tokio::test]
async fn space_recovery_one_confirmed_gateway_is_enough_with_other_failures() {
    for remote_floor in [None, Some(500)] {
        let (dir, _app, token, _policy, state) = build_test_app(100);
        let mut state = disk_state(state, &dir.path().join("mixed-responses.db"));
        let (bad_url, bad_server) = recovery_server(Router::new().route(
            "/api/spaces/presence/{agent_id}/record",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        ))
        .await;
        let mut invalid = signed_floor(&state, 900);
        invalid.record.seq += 1;
        let (invalid_url, invalid_server) = floor_server(invalid).await;
        let (valid_url, valid_server) = if let Some(floor) = remote_floor {
            floor_server(signed_floor(&state, floor)).await
        } else {
            recovery_server(Router::new().route(
                "/api/spaces/presence/{agent_id}/record",
                get(|| async { StatusCode::NOT_FOUND }),
            ))
            .await
        };
        state.gateway_urls = vec![bad_url, invalid_url, valid_url, "not a URL".to_owned()];
        let mut events = state.stream_tx.subscribe();
        let entered = authed_post_json(
            app(state.clone()),
            &token,
            "/v1/wattetheria/space/enter",
            json!({}),
        )
        .await;
        assert_eq!(entered["ok"], true);
        assert_eq!(
            entered["presence"]["record"]["seq"],
            remote_floor.unwrap_or(0) + 1
        );
        assert_eq!(entered["presence"]["record"]["entered"], true);
        assert_eq!(events.recv().await.unwrap().payload, entered["presence"]);
        let persisted = OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did,
        )
        .unwrap();
        assert_eq!(
            json!(
                persisted
                    .get(&test_network_id(&state), current_package(&state))
                    .unwrap()
            ),
            entered["presence"]
        );
        assert!(state.own_space_presence.lock().await.active_space.is_some());
        for server in [bad_server, invalid_server, valid_server] {
            server.abort();
        }
    }
}

#[tokio::test]
async fn space_recovery_saved_local_floor_allows_offline_restart_and_explicit_reentry() {
    for old_map in [false, true] {
        let (dir, _app, _token, _policy, state) = build_test_app(100);
        let path = dir.path().join("saved-floor.db");
        let mut state = disk_state(state, &path);
        let local = initialize_space_presence(&state).await.unwrap();
        let old = old_map.then(|| signed_floor(&state, 600));
        if let Some(old) = &old {
            let mut registry = state.own_space_presence.lock().await;
            registry
                .networks
                .entry(test_network_id(&state))
                .or_default()
                .insert(current_package(&state).space_id.clone(), old.clone());
            state
                .local_db
                .save_domain("own_space_presence", &*registry)
                .unwrap();
        }
        let reopened = Arc::new(LocalDb::open(&path).unwrap());
        let loaded =
            OwnSpacePresence::load(&reopened, state.space_catalog.as_deref(), &state.agent_did)
                .unwrap();
        state.local_db = reopened;
        state.own_space_presence = Arc::new(Mutex::new(loaded));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        state.gateway_urls = vec![format!("http://{}", listener.local_addr().unwrap())];
        drop(listener);
        let mut events = state.stream_tx.subscribe();
        crate::reset_space_presence(&state).await.unwrap();
        reconcile_space_presence(&state).await.unwrap();
        assert!(state.own_space_presence.lock().await.active_space.is_none());
        if let Some(old) = &old {
            let inactive: SignedSpacePresence =
                serde_json::from_value(events.recv().await.unwrap().payload).unwrap();
            let mut expected = old.record.clone();
            expected.entered = false;
            expected.seq += 1;
            expected.updated_at = inactive.record.updated_at;
            assert_eq!(inactive.record, expected);
            assert!(
                verify_payload(&inactive.record, &inactive.signature, &inactive.public_key)
                    .unwrap()
            );
            assert_ne!(inactive.signature, old.signature);
            assert!(matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
        } else {
            let inactive: SignedSpacePresence =
                serde_json::from_value(events.recv().await.unwrap().payload).unwrap();
            assert_eq!(inactive.record.seq, local.record.seq + 1);
            assert!(!inactive.record.entered);
        }
        let entered = initialize_space_presence(&state).await.unwrap();
        assert_eq!(
            entered.record.seq,
            if old_map { 602 } else { local.record.seq + 2 }
        );
        assert!(entered.record.entered);
        if !old_map {
            let mut expected = local.record;
            expected.seq = entered.record.seq;
            expected.updated_at = entered.record.updated_at;
            assert_eq!(entered.record, expected);
        }
        assert_eq!(events.recv().await.unwrap().payload, json!(entered));
        assert_eq!(initialize_space_presence(&state).await.unwrap(), entered);
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert_eq!(
            OwnSpacePresence::load(
                &state.local_db,
                state.space_catalog.as_deref(),
                &state.agent_did
            )
            .unwrap()
            .get(&test_network_id(&state), current_package(&state)),
            Some(&entered)
        );
    }
}

pub(super) fn store_legacy_activity(
    state: &mut ControlPlaneState,
    mut presence: SignedSpacePresence,
    activity: String,
) -> SignedSpacePresence {
    presence.record.activity = activity;
    presence.record.seq = 40;
    presence.signature = state.sign_payload(&presence.record).unwrap();
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([(
            test_network_id(state),
            BTreeMap::from([(current_package(state).space_id.clone(), presence.clone())]),
        )]),
        ..OwnSpacePresence::default()
    };
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    state.own_space_presence = Arc::new(Mutex::new(
        OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did,
        )
        .unwrap(),
    ));
    presence
}

pub(super) fn registry_bytes(path: &std::path::Path) -> String {
    let table = "own_space_presence";
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row(
            &format!("SELECT payload FROM {table} WHERE id=1"),
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn space_legacy_activity_load_is_read_only_and_authorized_repair_preserves_movement() {
    for activity in [
        "界".repeat(2001),
        "previously accepted\u{0007}activity".to_owned(),
    ] {
        let (dir, _app, token, _policy, state) = build_test_app(100);
        let path = dir.path().join("legacy-activity.db");
        let mut state = disk_state(state, &path);
        initialize_space_presence(&state).await.unwrap();
        let moved = authed_post_json(
            app(state.clone()),
            &token,
            "/v1/wattetheria/space/move",
            json!({"place_id":"hobbs-cafe/cafe","activity":"coffee"}),
        )
        .await;
        let original = store_legacy_activity(
            &mut state,
            serde_json::from_value(moved["presence"].clone()).unwrap(),
            activity,
        );
        let bytes = registry_bytes(&path);
        let checkpoint = state
            .local_db
            .load_network_permission_checkpoint(&state.agent_did, None, None)
            .unwrap()
            .unwrap();
        let mut pending = checkpoint.clone();
        pending.permission_status = "pending".to_owned();
        state
            .local_db
            .upsert_network_permission_checkpoint(&pending)
            .unwrap();
        state.local_db = Arc::new(LocalDb::open(&path).unwrap());
        let loaded = OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did,
        )
        .unwrap();
        assert_eq!(
            loaded.networks[&test_network_id(&state)][&current_package(&state).space_id],
            original
        );
        assert_eq!(registry_bytes(&path), bytes);
        assert!(
            loaded
                .get(&test_network_id(&state), current_package(&state))
                .is_none()
        );
        assert_eq!(
            loaded.projection(&test_network_id(&state), state.space_catalog.as_deref()),
            vec![]
        );
        let mut events = state.stream_tx.subscribe();
        crate::emit_current_space_presence(&state).await.unwrap();
        assert_eq!(
            crate::routes::space::snapshot::export(&state)
                .await
                .unwrap()
                .space_presences,
            []
        );
        assert!(initialize_space_presence(&state).await.is_err());
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert_eq!(registry_bytes(&path), bytes);
        state
            .local_db
            .upsert_network_permission_checkpoint(&checkpoint)
            .unwrap();
        let repaired = initialize_space_presence(&state).await.unwrap();
        let mut expected = original.record.clone();
        expected.activity.clear();
        expected.seq += 1;
        expected.updated_at = repaired.record.updated_at;
        assert_ne!(repaired.signature, original.signature);
        assert_eq!(repaired.record, expected);
        assert_eq!(events.recv().await.unwrap().payload, json!(repaired));
        assert!(
            verify_payload(&repaired.record, &repaired.signature, &repaired.public_key).unwrap()
        );
        let restarted = ControlPlaneState {
            local_db: Arc::new(LocalDb::open(&path).unwrap()),
            own_space_presence: Arc::new(Mutex::new(
                OwnSpacePresence::load(
                    &state.local_db,
                    state.space_catalog.as_deref(),
                    &state.agent_did,
                )
                .unwrap(),
            )),
            ..state
        };
        let restored = initialize_space_presence(&restarted).await.unwrap();
        expected.seq += 1;
        expected.updated_at = restored.record.updated_at;
        assert_eq!(restored.record, expected);
        assert_eq!(events.recv().await.unwrap().payload, json!(restored));
        assert_eq!(
            initialize_space_presence(&restarted).await.unwrap(),
            restored
        );
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let retained = restarted
            .local_db
            .load_domain::<Value>("unrelated-existing-state")
            .unwrap();
        assert_eq!(retained, Some(json!({"retained":true})));
    }
}

#[tokio::test]
async fn space_valid_legacy_activity_and_signed_bytes_are_not_rewritten() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("valid-legacy-activity.db");
    let mut state = disk_state(state, &path);
    let spawn = initialize_space_presence(&state).await.unwrap();
    let original = store_legacy_activity(&mut state, spawn, "界".repeat(2000));
    let bytes = registry_bytes(&path);
    state.local_db = Arc::new(LocalDb::open(&path).unwrap());
    let loaded = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        loaded.get(&test_network_id(&state), current_package(&state)),
        Some(&original)
    );
    assert_eq!(
        loaded.projection(&test_network_id(&state), state.space_catalog.as_deref()),
        vec![]
    );
    assert_eq!(registry_bytes(&path), bytes);
    assert!(loaded.active_space.is_none());
    let mut events = state.stream_tx.subscribe();
    let restored = initialize_space_presence(&state).await.unwrap();
    let mut expected = original.record.clone();
    expected.seq += 1;
    expected.updated_at = restored.record.updated_at;
    assert_eq!(restored.record, expected);
    assert_eq!(events.recv().await.unwrap().payload, json!(restored));
    let restored_bytes = registry_bytes(&path);
    assert_eq!(initialize_space_presence(&state).await.unwrap(), restored);
    assert_eq!(registry_bytes(&path), restored_bytes);
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn space_legacy_activity_failed_sqlite_repair_preserves_original_bytes_and_signature() {
    for activity in ["界".repeat(2001), "old\u{0007}activity".to_owned()] {
        let (dir, _app, _token, _policy, state) = build_test_app(100);
        let path = dir.path().join("legacy-activity-failed-write.db");
        let mut state = disk_state(state, &path);
        let spawn = initialize_space_presence(&state).await.unwrap();
        let original = store_legacy_activity(&mut state, spawn, activity);
        let bytes = registry_bytes(&path);
        let table = "own_space_presence";
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(&format!("CREATE TRIGGER reject_legacy_repair BEFORE UPDATE ON {table} BEGIN SELECT RAISE(ABORT,'legacy repair rejected'); END;")).unwrap();
        let mut events = state.stream_tx.subscribe();
        assert!(
            initialize_space_presence(&state)
                .await
                .unwrap_err()
                .to_string()
                .contains("upsert domain table")
        );
        assert_eq!(
            state.own_space_presence.lock().await.networks[&test_network_id(&state)]
                [&current_package(&state).space_id],
            original
        );
        assert_eq!(registry_bytes(&path), bytes);
        assert_eq!(
            OwnSpacePresence::load(
                &state.local_db,
                state.space_catalog.as_deref(),
                &state.agent_did
            )
            .unwrap()
            .networks[&test_network_id(&state)][&current_package(&state).space_id],
            original
        );
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
}

#[tokio::test]
async fn space_legacy_activity_repair_uses_remote_floor_and_checked_overflow() {
    for floor in [80, u64::MAX] {
        let (dir, _app, _token, _policy, state) = build_test_app(100);
        let path = dir.path().join("legacy-activity-remote-floor.db");
        let mut state = disk_state(state, &path);
        let spawn = initialize_space_presence(&state).await.unwrap();
        let original = store_legacy_activity(&mut state, spawn, "bad\u{0007}activity".to_owned());
        let bytes = registry_bytes(&path);
        let (url, server) = floor_server(signed_floor(&state, floor)).await;
        state.gateway_urls = vec![url];
        let mut events = state.stream_tx.subscribe();
        if floor == u64::MAX {
            assert!(
                initialize_space_presence(&state)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("sequence exhausted")
            );
            assert_eq!(registry_bytes(&path), bytes);
            assert_eq!(
                state.own_space_presence.lock().await.networks[&test_network_id(&state)]
                    [&current_package(&state).space_id],
                original
            );
            assert!(matches!(
                events.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
        } else {
            let repaired = initialize_space_presence(&state).await.unwrap();
            let mut expected = original.record.clone();
            expected.seq = floor + 1;
            expected.activity.clear();
            expected.depart_at = repaired.record.updated_at;
            expected.updated_at = repaired.record.updated_at;
            assert_eq!(repaired.record, expected);
            assert_eq!(events.recv().await.unwrap().payload, json!(repaired));
            assert_eq!(initialize_space_presence(&state).await.unwrap(), repaired);
        }
        server.abort();
    }
}

#[tokio::test]
async fn space_missing_local_registry_recovers_signed_high_floor_without_old_geometry() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let mut state = disk_state(state, &dir.path().join("lost-space-registry.db"));
    let floor = signed_floor(&state, u64::MAX - 2);
    let (url, server) = floor_server(floor).await;
    state.gateway_urls = vec![url];
    let spawn = initialize_space_presence(&state).await.unwrap();
    assert_eq!(spawn.record.seq, u64::MAX - 1);
    assert_eq!(
        spawn.record.space_version,
        current_package(&state).space_version
    );
    assert_eq!(
        spawn.record.destination_place_id,
        current_package(&state).spawn_place_id
    );
    current_package(&state)
        .validate_record(&spawn.record)
        .unwrap();
    assert!(
        wattetheria_kernel::signing::verify_payload(
            &spawn.record,
            &spawn.signature,
            &spawn.public_key,
        )
        .unwrap()
    );
    let reopened = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        reopened.get(&test_network_id(&state), current_package(&state)),
        Some(&spawn)
    );
    assert_eq!(initialize_space_presence(&state).await.unwrap(), spawn);
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("unrelated-existing-state")
            .unwrap(),
        Some(json!({"retained":true}))
    );
    server.abort();
}

#[tokio::test]
async fn space_recovery_raises_sequence_only_and_equal_floor_does_not_emit() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    initialize_space_presence(&state).await.unwrap();
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"hobbs-cafe/cafe","activity":"coffee"}),
    )
    .await;
    let moved: SignedSpacePresence = serde_json::from_value(moved["presence"].clone()).unwrap();
    let (url, server) = floor_server(signed_floor(&state, 90)).await;
    state.gateway_urls = vec![url];
    let mut events = state.stream_tx.subscribe();
    reconcile_space_presence(&state).await.unwrap();
    let recovered = state
        .own_space_presence
        .lock()
        .await
        .get(&test_network_id(&state), current_package(&state))
        .cloned()
        .unwrap();
    let mut expected = moved.record.clone();
    expected.seq = 91;
    expected.updated_at = recovered.record.updated_at;
    assert_eq!(recovered.record, expected);
    assert_ne!(recovered.signature, moved.signature);
    assert_eq!(events.recv().await.unwrap().payload, json!(recovered));
    let (equal_url, equal_server) = floor_server(recovered.clone()).await;
    state.gateway_urls = vec![equal_url];
    reconcile_space_presence(&state).await.unwrap();
    assert_eq!(initialize_space_presence(&state).await.unwrap(), recovered);
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    server.abort();
    equal_server.abort();
}

#[tokio::test]
async fn space_equal_sequence_conflicts_across_gateways_recover_once_under_concurrency() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    let local = initialize_space_presence(&state).await.unwrap();
    let mut conflict = local.clone();
    conflict.record.activity = "other process at the same sequence".to_owned();
    conflict.signature = state.sign_payload(&conflict.record).unwrap();
    let mut servers = Vec::new();
    for record in [
        local.clone(),
        conflict,
        signed_floor(&state, local.record.seq),
    ] {
        let (url, server) = floor_server(record).await;
        state.gateway_urls.push(url);
        servers.push(server);
    }
    let mut events = state.stream_tx.subscribe();
    let (initialized, first, second) = tokio::join!(
        initialize_space_presence(&state),
        reconcile_space_presence(&state),
        reconcile_space_presence(&state),
    );
    first.unwrap();
    second.unwrap();
    let recovered = initialized.unwrap();
    let mut expected = local.record.clone();
    expected.seq = 2;
    expected.updated_at = recovered.record.updated_at;
    assert_eq!(recovered.record, expected);
    assert_eq!(events.recv().await.unwrap().payload, json!(recovered));
    assert_eq!(initialize_space_presence(&state).await.unwrap(), recovered);
    reconcile_space_presence(&state).await.unwrap();
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(
        OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did
        )
        .unwrap()
        .get(&test_network_id(&state), current_package(&state)),
        Some(&recovered)
    );
    for server in servers {
        server.abort();
    }
}

#[tokio::test]
async fn space_recovery_ignores_wrong_identity_wrong_space_and_invalid_signature() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let mut state = disk_state(state, &dir.path().join("invalid-recovery.db"));
    let (_other_dir, _other_app, _other_token, _other_policy, other) = build_test_app(101);
    let mut bad = signed_floor(&state, 900);
    bad.record.seq += 1;
    let mut wrong_network = signed_floor(&state, 800);
    wrong_network.record.network_id = "other-network".to_owned();
    wrong_network.signature = state.sign_payload(&wrong_network.record).unwrap();
    let mut handles = Vec::new();
    for floor in [
        bad,
        wrong_network,
        signed_floor(&other, 700),
        signed_floor(&state, 0),
    ] {
        let (url, server) = floor_server(floor).await;
        state.gateway_urls.push(url);
        handles.push(server);
    }
    state.gateway_urls.push("not a gateway URL".to_owned());
    assert_failed_recovery_preserves_absent_presence(&state, &token).await;
    for handle in handles {
        handle.abort();
    }
}

#[tokio::test]
async fn space_recovery_local_old_map_sequence_remains_the_larger_floor() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    let old = signed_floor(&state, 600);
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([(
            test_network_id(&state),
            BTreeMap::from([(current_package(&state).space_id.clone(), old)]),
        )]),
        ..OwnSpacePresence::default()
    };
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    state.own_space_presence = Arc::new(Mutex::new(
        OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did,
        )
        .unwrap(),
    ));
    let (url, server) = floor_server(signed_floor(&state, 500)).await;
    state.gateway_urls = vec![url];
    let current = initialize_space_presence(&state).await.unwrap();
    assert_eq!(current.record.seq, 601);
    assert_eq!(
        current.record.space_version,
        current_package(&state).space_version
    );
    assert_eq!(initialize_space_presence(&state).await.unwrap(), current);
    server.abort();
}

#[tokio::test]
async fn space_recovery_unknown_space_contributes_floor_without_position() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    let mut unknown = signed_floor(&state, 900);
    unknown.record.space_id = "removed-space".to_owned();
    unknown.signature = state.sign_payload(&unknown.record).unwrap();
    let (url, server) = floor_server(unknown.clone()).await;
    state.gateway_urls = vec![url];
    let mut events = state.stream_tx.subscribe();
    crate::reset_space_presence(&state).await.unwrap();
    let inactive: SignedSpacePresence =
        serde_json::from_value(events.try_recv().unwrap().payload).unwrap();
    let mut expected = unknown.record.clone();
    expected.entered = false;
    expected.seq = 901;
    expected.updated_at = inactive.record.updated_at;
    assert_eq!(inactive.record, expected);
    assert!(verify_payload(&inactive.record, &inactive.signature, &inactive.public_key).unwrap());
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .stored(&unknown.record.network_id, &unknown.record.space_id),
        Some(&inactive)
    );
    let entered = initialize_space_presence(&state).await.unwrap();
    super::reentry_last::assert_default_spawn(&state, &entered, 902);
    assert_eq!(events.try_recv().unwrap().payload, json!(entered));
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .stored(&unknown.record.network_id, &unknown.record.space_id),
        Some(&inactive)
    );
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .active_space_id(&test_network_id(&state)),
        Some(entered.record.space_id.as_str())
    );
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    server.abort();
}

#[tokio::test]
async fn space_recovery_concurrent_first_join_and_move_recheck_current_record() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    let (url, server) = floor_server(signed_floor(&state, 500)).await;
    state.gateway_urls = vec![url];
    let (first, second, moved) = tokio::join!(
        initialize_space_presence(&state),
        initialize_space_presence(&state),
        authed_post_json(
            app(state.clone()),
            &token,
            "/v1/wattetheria/space/move",
            json!({"place_id":"hobbs-cafe/cafe","activity":"coffee"})
        ),
    );
    assert!(first.unwrap().record.seq >= 501);
    assert!(second.unwrap().record.seq >= 501);
    let moved: SignedSpacePresence = serde_json::from_value(moved["presence"].clone()).unwrap();
    assert_eq!(moved.record.seq, 502);
    assert_eq!(moved.record.activity, "coffee");
    assert_eq!(moved.record.destination_place_id, "hobbs-cafe/cafe");
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .get(&test_network_id(&state), current_package(&state)),
        Some(&moved)
    );
    assert_eq!(
        OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did
        )
        .unwrap()
        .get(&test_network_id(&state), current_package(&state)),
        Some(&moved)
    );
    server.abort();
}

#[tokio::test]
async fn space_recovery_overflow_never_persists_or_emits() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    let (url, server) = floor_server(signed_floor(&state, u64::MAX)).await;
    state.gateway_urls = vec![url];
    let mut events = state.stream_tx.subscribe();
    assert!(
        initialize_space_presence(&state)
            .await
            .unwrap_err()
            .to_string()
            .contains("sequence exhausted")
    );
    assert!(
        state
            .own_space_presence
            .lock()
            .await
            .get(&test_network_id(&state), current_package(&state))
            .is_none()
    );
    assert!(
        state
            .local_db
            .load_domain::<OwnSpacePresence>("own_space_presence")
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let urls = std::mem::take(&mut state.gateway_urls);
    let original = initialize_space_presence(&state).await.unwrap();
    events.recv().await.unwrap();
    state.gateway_urls = urls;
    assert!(
        reconcile_space_presence(&state)
            .await
            .unwrap_err()
            .to_string()
            .contains("sequence exhausted")
    );
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .get(&test_network_id(&state), current_package(&state)),
        Some(&original)
    );
    assert_eq!(
        OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did
        )
        .unwrap()
        .get(&test_network_id(&state), current_package(&state)),
        Some(&original)
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    server.abort();
}

#[tokio::test]
async fn space_recovery_does_not_update_presence_without_active_permission() {
    let (_dir, _app, _token, _policy, state) = build_test_app(100);
    let original = initialize_space_presence(&state).await.unwrap();
    let (url, server) = floor_server(signed_floor(&state, 900)).await;
    let state = ControlPlaneState {
        local_db: Arc::new(LocalDb::open_in_memory().unwrap()),
        gateway_urls: vec![url],
        ..state
    };
    let mut events = state.stream_tx.subscribe();
    reconcile_space_presence(&state).await.unwrap();
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .get(&original.record.network_id, current_package(&state)),
        Some(&original)
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    server.abort();
}

#[tokio::test]
async fn space_recovery_after_unavailable_startup_requires_existing_authorized_presence() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let mut state = disk_state(state, &dir.path().join("unavailable-recovery.db"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let offline_url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    state.gateway_urls = vec![offline_url];
    crate::reset_space_presence(&state).await.unwrap();
    reconcile_space_presence(&state).await.unwrap();
    assert_failed_recovery_preserves_absent_presence(&state, &token).await;
    let unknown = signed_floor(&state, 150);
    let (url, server) = floor_server(unknown.clone()).await;
    state.gateway_urls = vec![url];
    let mut events = state.stream_tx.subscribe();
    reconcile_space_presence(&state).await.unwrap();
    let inactive: SignedSpacePresence =
        serde_json::from_value(events.try_recv().unwrap().payload).unwrap();
    let mut expected = unknown.record.clone();
    expected.entered = false;
    expected.seq = 151;
    expected.updated_at = inactive.record.updated_at;
    assert_eq!(inactive.record, expected);
    assert!(verify_payload(&inactive.record, &inactive.signature, &inactive.public_key).unwrap());
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .stored(&unknown.record.network_id, &unknown.record.space_id),
        Some(&inactive)
    );
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    let entered = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({}),
    )
    .await;
    assert_eq!(entered["ok"], true);
    assert_eq!(entered["presence"]["record"]["seq"], 152);
    assert_eq!(entered["presence"]["record"]["entered"], true);
    assert_eq!(
        entered["presence"]["record"]["destination_place_id"],
        current_package(&state).spawn_place_id,
    );
    server.abort();
}

#[tokio::test]
async fn space_recovery_real_sqlite_write_failure_retains_existing_record_and_no_event() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("space-sequence-write-failure.db");
    let mut state = disk_state(state, &path);
    let original = initialize_space_presence(&state).await.unwrap();
    let (url, server) = floor_server(signed_floor(&state, 200)).await;
    state.gateway_urls = vec![url];
    let conn = rusqlite::Connection::open(&path).unwrap();
    let table = "own_space_presence";
    conn.execute_batch(&format!("CREATE TRIGGER reject_sequence_write BEFORE UPDATE ON {table} BEGIN SELECT RAISE(ABORT,'sequence write rejected'); END;")).unwrap();
    let mut events = state.stream_tx.subscribe();
    assert!(
        reconcile_space_presence(&state)
            .await
            .unwrap_err()
            .to_string()
            .contains("upsert domain table")
    );
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .get(&test_network_id(&state), current_package(&state)),
        Some(&original)
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(
        OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did
        )
        .unwrap()
        .get(&test_network_id(&state), current_package(&state)),
        Some(&original)
    );
    server.abort();
}
