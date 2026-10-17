use super::*;

#[tokio::test]
async fn reset_higher_raw_false_restores_signed_floor_once_without_resigning() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("raw-false-floor.db");
    let mut state = disk_state(state, &path);
    let mut local = super::sequence_recovery::signed_floor(&state, 5);
    local.record.entered = false;
    local.signature = state.sign_payload(&local.record).unwrap();
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([(
            local.record.network_id.clone(),
            BTreeMap::from([(local.record.space_id.clone(), local.clone())]),
        )]),
        active_space: None,
    };
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    state.own_space_presence = Arc::new(Mutex::new(registry));
    let mut remote = local.clone();
    remote.record.seq = 20;
    remote.signature = state.sign_payload(&remote.record).unwrap();
    let original_bytes = canonical_bytes(&remote).unwrap();
    let (url, server) = super::sequence_recovery::floor_server(remote.clone()).await;
    state.gateway_urls = vec![url];
    let mut events = state.stream_tx.subscribe();
    crate::reset_space_presence(&state).await.unwrap();
    let restored = state
        .own_space_presence
        .lock()
        .await
        .stored(&remote.record.network_id, &remote.record.space_id)
        .unwrap()
        .clone();
    assert_eq!(canonical_bytes(&restored).unwrap(), original_bytes);
    assert_eq!(restored, remote);
    assert!(verify_payload(&restored.record, &restored.signature, &restored.public_key).unwrap());
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    let saved = state
        .local_db
        .load_domain::<Value>("own_space_presence")
        .unwrap();
    rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TRIGGER forbid_false_reimport BEFORE UPDATE ON own_space_presence BEGIN SELECT RAISE(ABORT,'raw false imported twice'); END;").unwrap();
    for _ in 0..3 {
        crate::reset_space_presence(&state).await.unwrap();
        crate::reconcile_space_presence(&state).await.unwrap();
        assert_eq!(
            state
                .local_db
                .load_domain::<Value>("own_space_presence")
                .unwrap(),
            saved
        );
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        assert!(state.own_space_presence.lock().await.active_space.is_none());
    }
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TRIGGER forbid_false_reimport;")
        .unwrap();
    let entered = initialize_space_presence(&state).await.unwrap();
    assert_eq!(entered.record.seq, 21);
    assert_eq!(
        entered.record.destination_place_id,
        current_package(&state).spawn_place_id
    );
    assert_eq!(entered.record.path, "");
    current_package(&state)
        .validate_record(&entered.record)
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn reset_reconciliation_of_multiple_false_spaces_is_idempotent() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("reset-idempotency.db");
    let mut state = disk_state(state, &path);
    let identity = wattetheria_kernel::agent_identity::load_agent_identity(dir.path()).unwrap();
    let source = current_package(&state).clone();
    let mut target = source.clone();
    target.space_id = "second-space".to_owned();
    for place in &mut target.places {
        if place.place_id == source.space_id {
            place.place_id = target.space_id.clone();
        }
        if place.parent_place_id.as_deref() == Some(&source.space_id) {
            place.parent_place_id = Some(target.space_id.clone());
        }
    }
    super::namespace_portals::rehash_package(&mut target);
    state.space_catalog = Some(Arc::new(super::namespace_portals::configured_catalog(
        &source.space_id,
        &[source.clone(), target.clone()],
    )));
    let mut first = super::map_upgrade::signed_spawn(&state, &identity, &source, 5);
    let mut second = super::map_upgrade::signed_spawn(&state, &identity, &target, 6);
    for record in [&mut first, &mut second] {
        record.record.entered = false;
        record.signature = state.sign_payload(&record.record).unwrap();
    }
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([(
            first.record.network_id.clone(),
            BTreeMap::from([
                (source.space_id.clone(), first.clone()),
                (target.space_id.clone(), second.clone()),
            ]),
        )]),
        active_space: None,
    };
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    state.own_space_presence = Arc::new(Mutex::new(registry));
    let raw = Arc::new(Mutex::new(second.clone()));
    let shared = raw.clone();
    let router = Router::new().route(
        "/api/spaces/presence/{agent_id}/record",
        get(move || {
            let shared = shared.clone();
            async move { Json(shared.lock().await.clone()) }
        }),
    );
    let (url, server) = super::query_resilience::serve(router).await;
    state.gateway_urls = vec![url];
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TRIGGER forbid_extra_reset_write BEFORE UPDATE ON own_space_presence BEGIN SELECT RAISE(ABORT,'unexpected reset rewrite'); END;").unwrap();
    let before = state
        .local_db
        .load_domain::<Value>("own_space_presence")
        .unwrap();
    let mut events = state.stream_tx.subscribe();
    for _ in 0..3 {
        crate::reset_space_presence(&state).await.unwrap();
        crate::reconcile_space_presence(&state).await.unwrap();
        assert_eq!(
            state
                .local_db
                .load_domain::<Value>("own_space_presence")
                .unwrap(),
            before
        );
        let registry = state.own_space_presence.lock().await;
        assert_eq!(Some(json!(&*registry)), before);
        assert!(registry.active_space.is_none());
        *raw.lock().await = registry.networks[&first.record.network_id]
            .values()
            .max_by_key(|record| record.record.seq)
            .unwrap()
            .clone();
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }
    let mut older = first.clone();
    older.record.entered = true;
    older.record.seq = 4;
    older.signature = state.sign_payload(&older.record).unwrap();
    *raw.lock().await = older;
    crate::reset_space_presence(&state).await.unwrap();
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        before
    );
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    server.abort();
}

#[tokio::test]
async fn reset_higher_raw_true_retires_once_without_touching_other_false_space() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("raw-higher-true.db");
    let mut state = disk_state(state, &path);
    let mut first = super::sequence_recovery::signed_floor(&state, 5);
    first.record.entered = false;
    first.signature = state.sign_payload(&first.record).unwrap();
    let mut second = first.clone();
    second.record.space_id = "another-space".to_owned();
    second.record.seq = 6;
    second.signature = state.sign_payload(&second.record).unwrap();
    let network = first.record.network_id.clone();
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([(
            network.clone(),
            BTreeMap::from([
                (first.record.space_id.clone(), first.clone()),
                (second.record.space_id.clone(), second.clone()),
            ]),
        )]),
        active_space: None,
    };
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    state.own_space_presence = Arc::new(Mutex::new(registry));
    let mut newer = first.clone();
    newer.record.entered = true;
    newer.record.seq = 7;
    newer.signature = state.sign_payload(&newer.record).unwrap();
    let (url, server) = super::sequence_recovery::floor_server(newer.clone()).await;
    state.gateway_urls = vec![url];
    let mut events = state.stream_tx.subscribe();
    crate::reset_space_presence(&state).await.unwrap();
    let inactive: SignedSpacePresence =
        serde_json::from_value(events.try_recv().unwrap().payload).unwrap();
    let mut expected = newer.record.clone();
    expected.entered = false;
    expected.seq = 8;
    expected.updated_at = inactive.record.updated_at;
    assert_eq!(inactive.record, expected);
    assert!(verify_payload(&inactive.record, &inactive.signature, &inactive.public_key).unwrap());
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .stored(&network, &second.record.space_id),
        Some(&second)
    );
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    let saved = state
        .local_db
        .load_domain::<Value>("own_space_presence")
        .unwrap();
    rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TRIGGER forbid_extra_reset_write BEFORE UPDATE ON own_space_presence BEGIN SELECT RAISE(ABORT,'unexpected reset rewrite'); END;").unwrap();
    for _ in 0..3 {
        crate::reset_space_presence(&state).await.unwrap();
        crate::reconcile_space_presence(&state).await.unwrap();
        assert_eq!(
            state
                .local_db
                .load_domain::<Value>("own_space_presence")
                .unwrap(),
            saved
        );
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }
    server.abort();
}

#[tokio::test]
async fn reset_without_catalog_tombstones_verified_local_record() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let mut state = disk_state(state, &dir.path().join("catalogless-reset.db"));
    let old = super::sequence_recovery::signed_floor(&state, 40);
    let mut other = old.clone();
    other.record.network_id = "another-network".to_owned();
    other.record.seq = 900;
    other.signature = state.sign_payload(&other.record).unwrap();
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([
            (
                old.record.network_id.clone(),
                BTreeMap::from([(old.record.space_id.clone(), old.clone())]),
            ),
            (
                other.record.network_id.clone(),
                BTreeMap::from([(other.record.space_id.clone(), other.clone())]),
            ),
        ]),
        active_space: None,
    };
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    state.space_catalog = None;
    state.own_space_presence = Arc::new(Mutex::new(
        OwnSpacePresence::load(&state.local_db, None, &state.agent_did).unwrap(),
    ));
    let mut events = state.stream_tx.subscribe();
    crate::reset_space_presence(&state).await.unwrap();
    let event = events
        .try_recv()
        .expect("catalogless restart must publish inactive own presence");
    let inactive: SignedSpacePresence = serde_json::from_value(event.payload.clone()).unwrap();
    let mut expected = old.record.clone();
    expected.entered = false;
    expected.seq = 41;
    expected.updated_at = inactive.record.updated_at;
    assert_eq!(inactive.record, expected);
    assert!(verify_payload(&inactive.record, &inactive.signature, &inactive.public_key).unwrap());
    assert_eq!(
        crate::build_signed_node_event(&state, &event)
            .unwrap()
            .unwrap()
            .payload
            .payload,
        json!(inactive)
    );
    let loaded = OwnSpacePresence::load(&state.local_db, None, &state.agent_did).unwrap();
    assert_eq!(
        loaded.stored(&old.record.network_id, &old.record.space_id),
        Some(&inactive)
    );
    assert_eq!(
        loaded.stored(&other.record.network_id, &other.record.space_id),
        Some(&other)
    );
    assert!(loaded.active_space.is_none());
    assert_eq!(
        crate::routes::space::snapshot::export(&state)
            .await
            .unwrap()
            .space_presences,
        vec![inactive.clone()]
    );
    crate::emit_current_space_presence(&state).await.unwrap();
    assert_eq!(events.try_recv().unwrap().payload, json!(inactive));
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("unrelated-existing-state")
            .unwrap(),
        Some(json!({"retained":true}))
    );
}

#[tokio::test]
async fn reset_raw_unknown_record_declares_inactive_without_spawning() {
    for catalog_present in [false, true] {
        let (dir, _app, _token, _policy, state) = build_test_app(100);
        let mut state = disk_state(state, &dir.path().join("raw-unknown-reset.db"));
        let mut old = super::sequence_recovery::signed_floor(&state, 50);
        old.record.space_id = "removed-space".to_owned();
        old.signature = state.sign_payload(&old.record).unwrap();
        let (url, server) = super::sequence_recovery::floor_server(old.clone()).await;
        state.gateway_urls = vec![url];
        if !catalog_present {
            state.space_catalog = None;
        }
        assert_eq!(
            state
                .local_db
                .load_domain::<Value>("own_space_presence")
                .unwrap(),
            None
        );
        let mut events = state.stream_tx.subscribe();
        crate::reset_space_presence(&state).await.unwrap();
        let inactive: SignedSpacePresence = serde_json::from_value(
            events
                .try_recv()
                .expect("verified raw unknown true must be retired")
                .payload,
        )
        .unwrap();
        let mut expected = old.record.clone();
        expected.entered = false;
        expected.seq = 51;
        expected.updated_at = inactive.record.updated_at;
        assert_eq!(inactive.record, expected);
        assert!(
            verify_payload(&inactive.record, &inactive.signature, &inactive.public_key).unwrap()
        );
        assert_eq!(inactive.record.from_tile, [u32::MAX, u32::MAX]);
        assert_eq!(inactive.record.destination_place_id, "retired-place");
        let loaded = OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did,
        )
        .unwrap();
        assert!(loaded.active_space.is_none());
        assert_eq!(
            loaded.stored(&old.record.network_id, &old.record.space_id),
            Some(&inactive)
        );
        assert_eq!(
            crate::routes::space::snapshot::export(&state)
                .await
                .unwrap()
                .space_presences,
            vec![inactive]
        );
        let saved = state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap();
        crate::reset_space_presence(&state).await.unwrap();
        assert_eq!(
            state
                .local_db
                .load_domain::<Value>("own_space_presence")
                .unwrap(),
            saved
        );
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        server.abort();
    }
}

#[tokio::test]
async fn reset_without_catalog_empty_or_failed_gateway_does_not_create_presence() {
    for status in [StatusCode::NOT_FOUND, StatusCode::SERVICE_UNAVAILABLE] {
        let (_dir, _app, token, _policy, mut state) = build_test_app(100);
        let router = Router::new().route(
            "/api/spaces/presence/{agent_id}/record",
            get(move || async move { status }),
        );
        let (url, server) = super::query_resilience::serve(router).await;
        state.gateway_urls = vec![url];
        state.space_catalog = None;
        let mut events = state.stream_tx.subscribe();
        crate::reset_space_presence(&state).await.unwrap();
        crate::reconcile_space_presence(&state).await.unwrap();
        assert_eq!(
            state
                .local_db
                .load_domain::<Value>("own_space_presence")
                .unwrap(),
            None
        );
        assert!(state.own_space_presence.lock().await.networks.is_empty());
        assert!(state.own_space_presence.lock().await.active_space.is_none());
        assert_eq!(
            crate::routes::space::snapshot::export(&state)
                .await
                .unwrap()
                .space_presences,
            Vec::<SignedSpacePresence>::new()
        );
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let response = app(state.clone())
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/health")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        server.abort();
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn restart_tombstones_local_removed_and_unknown_versions_atomically() {
    for (space, reject_write) in [
        ("removed-space", true),
        ("removed-space", false),
        ("the-ville", false),
    ] {
        let (dir, _app, token, _policy, state) = build_test_app(100);
        let path = dir.path().join("unknown-restart.db");
        let mut state = disk_state(state, &path);
        let mut old = initialize_space_presence(&state).await.unwrap();
        let network = old.record.network_id.clone();
        old.record.space_id = space.to_owned();
        old.record.space_version = "retired-unknown-version".to_owned();
        old.record.seq = 100;
        old.record.from_tile = [u32::MAX, u32::MAX];
        old.record.path = "R".to_owned();
        old.record.destination_place_id = "retired-place".to_owned();
        old.signature = state.sign_payload(&old.record).unwrap();
        let mut other = old.clone();
        other.record.network_id = "another-network".to_owned();
        other.record.seq = 999;
        other.signature = state.sign_payload(&other.record).unwrap();
        let registry = OwnSpacePresence {
            networks: BTreeMap::from([
                (
                    network.clone(),
                    BTreeMap::from([(space.to_owned(), old.clone())]),
                ),
                (
                    "another-network".to_owned(),
                    BTreeMap::from([(space.to_owned(), other.clone())]),
                ),
            ]),
            active_space: None,
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
        let before = state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap();
        if reject_write {
            rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TRIGGER reject_tombstone BEFORE UPDATE ON own_space_presence BEGIN SELECT RAISE(ABORT,'tombstone rejected'); END;").unwrap();
        }
        let mut events = state.stream_tx.subscribe();
        let result = crate::reset_space_presence(&state).await;
        if reject_write {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("upsert domain table")
            );
            assert_eq!(
                state
                    .local_db
                    .load_domain::<Value>("own_space_presence")
                    .unwrap(),
                before
            );
            assert_eq!(
                state
                    .own_space_presence
                    .lock()
                    .await
                    .stored(&network, space),
                Some(&old)
            );
            assert!(matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
        } else {
            result.unwrap();
            let inactive: SignedSpacePresence =
                serde_json::from_value(events.recv().await.unwrap().payload).unwrap();
            let mut expected = old.record.clone();
            expected.entered = false;
            expected.seq = 101;
            expected.updated_at = inactive.record.updated_at;
            assert_eq!(inactive.record, expected);
            assert_ne!(inactive.signature, old.signature);
            assert!(
                verify_payload(&inactive.record, &inactive.signature, &inactive.public_key)
                    .unwrap()
            );
            let loaded = OwnSpacePresence::load(
                &state.local_db,
                state.space_catalog.as_deref(),
                &state.agent_did,
            )
            .unwrap();
            assert!(loaded.active_space.is_none());
            assert_eq!(loaded.stored(&network, space), Some(&inactive));
            assert_eq!(loaded.stored("another-network", space), Some(&other));
            assert_eq!(
                crate::routes::space::snapshot::export(&state)
                    .await
                    .unwrap()
                    .space_presences,
                vec![inactive.clone()]
            );
            let saved = state
                .local_db
                .load_domain::<Value>("own_space_presence")
                .unwrap();
            crate::reset_space_presence(&state).await.unwrap();
            assert_eq!(
                state
                    .local_db
                    .load_domain::<Value>("own_space_presence")
                    .unwrap(),
                saved
            );
            assert!(matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
            let payload = json!({"presences":[inactive,old]});
            let router = Router::new()
                .route(
                    "/api/spaces/presence",
                    get(move || {
                        let payload = payload.clone();
                        async move { Json(payload) }
                    }),
                )
                .route(
                    "/api/spaces/events",
                    get(|| async { Json(json!({"events":[]})) }),
                );
            let (url, server) = super::query_resilience::serve(router).await;
            state.gateway_urls = vec![url];
            let queried =
                authed_get_json(app(state.clone()), &token, "/v1/wattetheria/space/presence").await;
            assert_eq!(queried["error"], "agent presence not found");
            let place = authed_get_json(
                app(state.clone()),
                &token,
                "/v1/wattetheria/space/place?place_id=the-ville",
            )
            .await;
            assert_eq!(place["presences"], json!([]));
            assert_eq!(
                state
                    .local_db
                    .load_domain::<Value>("own_space_presence")
                    .unwrap(),
                saved
            );
            server.abort();
        }
        assert!(state.own_space_presence.lock().await.active_space.is_none());
        assert_eq!(
            state
                .local_db
                .load_domain::<Value>("unrelated-existing-state")
                .unwrap(),
            Some(json!({"retained":true}))
        );
    }
}

#[tokio::test]
async fn runtime_network_change_does_not_activate_same_named_space_archive() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    let current = initialize_space_presence(&state).await.unwrap();
    let other_bridge = MockSwarmBridge::default_for("another-node".to_owned());
    let other_network = other_bridge.current_network_id().await.unwrap();
    let mut other = current.clone();
    other.record.network_id = other_network.clone();
    other.record.seq = 900;
    other.signature = state.sign_payload(&other.record).unwrap();
    state.own_space_presence.lock().await.networks.insert(
        other_network.clone(),
        BTreeMap::from([(other.record.space_id.clone(), other.clone())]),
    );
    state
        .local_db
        .save_domain(
            "own_space_presence",
            &*state.own_space_presence.lock().await,
        )
        .unwrap();
    let before = state
        .local_db
        .load_domain::<Value>("own_space_presence")
        .unwrap();
    let mut events = state.stream_tx.subscribe();
    state.swarm_bridge = Arc::new(other_bridge);
    let registry = state.own_space_presence.lock().await;
    assert_eq!(
        registry.active_space.as_ref(),
        Some(&(
            current.record.network_id.clone(),
            current.record.space_id.clone()
        ))
    );
    assert_eq!(
        registry.visible_get(&other_network, current_package(&state)),
        None
    );
    assert_eq!(
        registry.projection(&other_network, state.space_catalog.as_deref()),
        vec![]
    );
    assert_eq!(
        registry.stored(&other_network, &other.record.space_id),
        Some(&other)
    );
    drop(registry);
    let queried =
        authed_get_json(app(state.clone()), &token, "/v1/wattetheria/space/presence").await;
    assert!(queried.get("presence").is_none());
    assert!(
        initialize_space_presence(&state)
            .await
            .unwrap_err()
            .to_string()
            .contains("active network agent permission")
    );
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        before
    );
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}
