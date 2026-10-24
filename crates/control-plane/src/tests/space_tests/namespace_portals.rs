use super::*;
use wattetheria_gateway_contract::SpacePresenceRecord;
use wattetheria_kernel::space::{SpaceCatalog, SpacePackage};

pub(super) fn rehash_package(package: &mut SpacePackage) {
    let mut value = serde_json::to_value(&*package).unwrap();
    value.as_object_mut().unwrap().remove("space_version");
    package.space_version = format!(
        "sha256:{:x}",
        sha2::Sha256::digest(serde_jcs::to_vec(&value).unwrap())
    );
    package.validate().unwrap();
}

pub(super) fn configured_catalog(default: &str, packages: &[SpacePackage]) -> SpaceCatalog {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("assets")).unwrap();
    let mut entries = BTreeMap::<String, Value>::new();
    for (index, package) in packages.iter().enumerate() {
        let filename = format!("package-{index}.json");
        fs::write(
            dir.path().join(&filename),
            serde_json::to_vec(package).unwrap(),
        )
        .unwrap();
        let entry = entries.entry(package.space_id.clone()).or_insert_with(|| {
            json!({
                "space_id":package.space_id,"current_version":package.space_version,"versions":[]
            })
        });
        entry["current_version"] = json!(package.space_version);
        entry["versions"].as_array_mut().unwrap().push(json!({
            "space_version":package.space_version,"package_path":filename,"assets_path":"assets"
        }));
    }
    let path = dir.path().join("catalog.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "default_space_id":default,"spaces":entries.into_values().collect::<Vec<_>>()
        }))
        .unwrap(),
    )
    .unwrap();
    SpaceCatalog::load(&path).unwrap()
}

#[tokio::test]
async fn absent_catalog_keeps_nonspace_routes_available() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    state.space_catalog = None;
    let mut events = state.stream_tx.subscribe();
    crate::reset_space_presence(&state).await.unwrap();
    crate::reconcile_space_presence(&state).await.unwrap();
    crate::emit_current_space_presence(&state).await.unwrap();
    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({}),
    )
    .await;
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("Space is unavailable")
    );
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
async fn bridge_network_mismatch_rejects_entry_without_persistence() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    state.swarm_bridge = Arc::new(MockSwarmBridge::default_for("different-node".to_owned()));
    let mut events = state.stream_tx.subscribe();
    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({}),
    )
    .await;
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("active network agent permission")
    );
    assert!(state.own_space_presence.lock().await.active_space.is_none());
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
async fn network_namespace_preserves_same_space_and_removed_space_sequence_floor() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let state = disk_state(state, &dir.path().join("namespace.db"));
    let initial = initialize_space_presence(&state).await.unwrap();
    let mut foreign = initial.clone();
    foreign.record.network_id = "another-network".to_owned();
    foreign.record.seq = 1000;
    foreign.signature = state.sign_payload(&foreign.record).unwrap();
    let mut removed = initial.clone();
    removed.record.space_id = "removed-space".to_owned();
    removed.record.space_version = "removed-version".to_owned();
    removed.record.seq = 500;
    removed.signature = state.sign_payload(&removed.record).unwrap();
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([
            (
                test_network_id(&state),
                BTreeMap::from([
                    (initial.record.space_id.clone(), initial.clone()),
                    (removed.record.space_id.clone(), removed.clone()),
                ]),
            ),
            (
                "another-network".to_owned(),
                BTreeMap::from([(foreign.record.space_id.clone(), foreign.clone())]),
            ),
        ]),
        active_space: None,
    };
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    let loaded = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(loaded.sequence_floor(&test_network_id(&state)), 500);
    assert_eq!(loaded.sequence_floor("another-network"), 1000);
    assert!(loaded.active_space.is_none());
    let state = ControlPlaneState {
        own_space_presence: Arc::new(Mutex::new(loaded)),
        ..state
    };
    let mut events = state.stream_tx.subscribe();
    let entered = initialize_space_presence(&state).await.unwrap();
    super::reentry_last::assert_default_spawn(&state, &entered, 501);
    assert_eq!(events.try_recv().unwrap().payload, json!(entered));
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
    let reopened = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert!(reopened.active_space.is_none());
    assert_eq!(
        reopened.stored("another-network", &foreign.record.space_id),
        Some(&foreign)
    );
    assert_eq!(
        reopened.stored(&test_network_id(&state), &removed.record.space_id),
        Some(&removed)
    );
    assert_eq!(
        reopened.stored(&test_network_id(&state), &initial.record.space_id),
        Some(&entered)
    );
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("unrelated-existing-state")
            .unwrap(),
        Some(json!({"retained":true}))
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn portal_transition_is_atomic_and_uses_network_sequence_floor() {
    for reject_write in [true, false] {
        let (dir, _app, token, _policy, state) = build_test_app(100);
        let path = dir.path().join("portal.db");
        let mut state = disk_state(state, &path);
        let mut source = current_package(&state).clone();
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
        source.portals = serde_json::from_value(json!([{"portal_id":"next","source_place_id":source.spawn_place_id,"target_space_id":target.space_id,"target_place_id":target.spawn_place_id}])).unwrap();
        rehash_package(&mut source);
        rehash_package(&mut target);
        state.space_catalog = Some(Arc::new(configured_catalog(
            &source.space_id,
            &[source.clone(), target.clone()],
        )));
        let initial = initialize_space_presence(&state).await.unwrap();
        let before = state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap();
        if reject_write {
            rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TRIGGER reject_portal BEFORE UPDATE ON own_space_presence BEGIN SELECT RAISE(ABORT,'portal write rejected'); END;").unwrap();
        }
        let mut events = state.stream_tx.subscribe();
        let result = authed_post_json(
            app(state.clone()),
            &token,
            "/v1/wattetheria/space/enter",
            json!({"space_id":target.space_id,"portal_id":"next"}),
        )
        .await;
        if reject_write {
            assert!(
                result["error"]
                    .as_str()
                    .unwrap()
                    .contains("upsert domain table")
            );
            assert_eq!(
                state
                    .local_db
                    .load_domain::<Value>("own_space_presence")
                    .unwrap(),
                before
            );
            let registry = state.own_space_presence.lock().await;
            assert_eq!(
                registry.active_space.as_ref(),
                Some(&(test_network_id(&state), source.space_id.clone()))
            );
            assert_eq!(
                registry.stored(&test_network_id(&state), &source.space_id),
                Some(&initial)
            );
            assert_eq!(
                registry.stored(&test_network_id(&state), &target.space_id),
                None
            );
            assert!(matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
        } else {
            assert_eq!(result["ok"], true);
            let left: SignedSpacePresence =
                serde_json::from_value(events.recv().await.unwrap().payload).unwrap();
            let arrived: SignedSpacePresence =
                serde_json::from_value(events.recv().await.unwrap().payload).unwrap();
            assert!(!left.record.entered);
            assert_eq!(left.record.space_id, source.space_id);
            assert_eq!(left.record.seq, initial.record.seq + 1);
            assert!(arrived.record.entered);
            assert_eq!(arrived.record.space_id, target.space_id);
            assert_eq!(arrived.record.seq, left.record.seq + 1);
            assert_eq!(arrived.record.destination_place_id, target.spawn_place_id);
            for signed in [&left, &arrived] {
                assert!(
                    verify_payload(&signed.record, &signed.signature, &signed.public_key).unwrap()
                );
            }
            assert_eq!(result["presence"], json!(arrived));
            let saved = OwnSpacePresence::load(
                &state.local_db,
                state.space_catalog.as_deref(),
                &state.agent_did,
            )
            .unwrap();
            assert!(saved.active_space.is_none());
            assert_eq!(
                saved.stored(&test_network_id(&state), &source.space_id),
                Some(&left)
            );
            assert_eq!(
                saved.stored(&test_network_id(&state), &target.space_id),
                Some(&arrived)
            );
            assert!(matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
        }
    }
}

#[tokio::test]
async fn portal_requires_actual_arrival_and_same_space_entry_is_idempotent() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    let mut source = current_package(&state).clone();
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
    source.portals = serde_json::from_value(json!([{"portal_id":"park","source_place_id":"hobbs-cafe/cafe","target_space_id":target.space_id,"target_place_id":target.spawn_place_id}])).unwrap();
    rehash_package(&mut source);
    rehash_package(&mut target);
    state.space_catalog = Some(Arc::new(configured_catalog(
        &source.space_id,
        &[source.clone(), target.clone()],
    )));
    let initial = initialize_space_presence(&state).await.unwrap();
    let mut events = state.stream_tx.subscribe();
    for body in [
        json!({"space_id":target.space_id}),
        json!({"space_id":target.space_id,"portal_id":"park"}),
    ] {
        let result = authed_post_json(
            app(state.clone()),
            &token,
            "/v1/wattetheria/space/enter",
            body,
        )
        .await;
        assert!(result["error"].as_str().unwrap().contains("portal"));
        assert_eq!(
            state
                .own_space_presence
                .lock()
                .await
                .stored(&test_network_id(&state), &source.space_id),
            Some(&initial)
        );
    }
    let again = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({"space_id":source.space_id}),
    )
    .await;
    assert_eq!(again["presence"], json!(initial));
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"hobbs-cafe/cafe"}),
    )
    .await;
    assert_eq!(moved["ok"], true);
    let moving: SignedSpacePresence = serde_json::from_value(moved["presence"].clone()).unwrap();
    assert!(moving.record.path.len() > 10);
    assert_eq!(events.recv().await.unwrap().payload, json!(moving));
    let rejected = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({"space_id":target.space_id,"portal_id":"park"}),
    )
    .await;
    assert!(
        rejected["error"]
            .as_str()
            .unwrap()
            .contains("arrived at the source portal")
    );
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .stored(&test_network_id(&state), &source.space_id),
        Some(&moving)
    );
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn legacy_network_binding_requires_verified_checkpoint_and_preserves_signed_bytes() {
    let (_dir, _app, _token, _policy, state) = build_test_app(100);
    let mut legacy = initialize_space_presence(&state).await.unwrap();
    legacy.record.network_id.clear();
    legacy.record.seq = 75;
    legacy.record.destination_place_id = "hobbs-cafe/cafe".to_owned();
    legacy.record.path = current_package(&state)
        .path(
            legacy.record.from_tile,
            current_package(&state)
                .destination_tile(&state.agent_did, "hobbs-cafe/cafe")
                .unwrap(),
        )
        .unwrap();
    legacy.signature = state.sign_payload(&legacy.record).unwrap();
    let signed_bytes = canonical_bytes(&legacy).unwrap();
    assert!(
        serde_json::to_value(&legacy.record)
            .unwrap()
            .get("network_id")
            .is_none()
    );
    let db = LocalDb::open_in_memory().unwrap();
    db.save_domain(
        "own_space_presence",
        &json!({"spaces":{(legacy.record.space_id.clone()):legacy}}),
    )
    .unwrap();
    let unbound =
        OwnSpacePresence::load(&db, state.space_catalog.as_deref(), &state.agent_did).unwrap();
    assert_eq!(unbound.stored("", &legacy.record.space_id), Some(&legacy));
    assert_eq!(
        unbound.stored(&test_network_id(&state), &legacy.record.space_id),
        None
    );
    let credential = state
        .local_db
        .load_network_agent_credential(&state.agent_did, None)
        .unwrap()
        .unwrap();
    let checkpoint = state
        .local_db
        .load_network_permission_checkpoint(&state.agent_did, None, None)
        .unwrap()
        .unwrap();
    let initially_verified = LocalDb::open_in_memory().unwrap();
    initially_verified
        .upsert_network_agent_credential(&credential)
        .unwrap();
    initially_verified
        .upsert_network_permission_checkpoint(&checkpoint)
        .unwrap();
    initially_verified
        .save_domain(
            "own_space_presence",
            &json!({"spaces":{(legacy.record.space_id.clone()):legacy}}),
        )
        .unwrap();
    let bound = OwnSpacePresence::load(
        &initially_verified,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        canonical_bytes(
            bound
                .stored(&checkpoint.network_id, &legacy.record.space_id)
                .unwrap()
        )
        .unwrap(),
        signed_bytes
    );
    assert_eq!(bound.stored("", &legacy.record.space_id), None);
    let before = db.load_domain::<Value>("own_space_presence").unwrap();
    db.upsert_network_agent_credential(&credential).unwrap();
    db.upsert_network_permission_checkpoint(&checkpoint)
        .unwrap();
    let still_unbound =
        OwnSpacePresence::load(&db, state.space_catalog.as_deref(), &state.agent_did).unwrap();
    assert_eq!(
        canonical_bytes(still_unbound.stored("", &legacy.record.space_id).unwrap()).unwrap(),
        signed_bytes
    );
    assert_eq!(
        still_unbound.stored(&checkpoint.network_id, &legacy.record.space_id),
        None
    );
    assert!(still_unbound.active_space.is_none());
    OwnSpacePresence::load(&db, state.space_catalog.as_deref(), &state.agent_did).unwrap();
    assert_eq!(
        db.load_domain::<Value>("own_space_presence").unwrap(),
        before
    );
    let state = ControlPlaneState {
        local_db: Arc::new(db),
        own_space_presence: Arc::new(Mutex::new(still_unbound)),
        ..state
    };
    let entered = initialize_space_presence(&state).await.unwrap();
    assert_eq!(entered.record.seq, 76);
    assert_eq!(entered.record.network_id, checkpoint.network_id);
    assert_eq!(
        entered.record.destination_place_id,
        current_package(&state).spawn_place_id
    );
    assert_eq!(entered.record.path, "");
    assert_ne!(
        entered.record.destination_place_id,
        legacy.record.destination_place_id
    );
    assert!(verify_payload(&entered.record, &entered.signature, &entered.public_key).unwrap());
    let reloaded = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        canonical_bytes(reloaded.stored("", &legacy.record.space_id).unwrap()).unwrap(),
        signed_bytes
    );
}

#[tokio::test]
async fn trusted_old_speed_projects_position_and_replans_legal_current_destination() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    let mut old = current_package(&state).clone();
    old.walk_speed = 1.0;
    rehash_package(&mut old);
    let mut current = old.clone();
    current.walk_speed = 4.0;
    rehash_package(&mut current);
    state.space_catalog = Some(Arc::new(configured_catalog(
        &old.space_id,
        &[old.clone(), current.clone()],
    )));
    let now = Utc::now().timestamp_millis();
    let from = old
        .destination_tile(&state.agent_did, &old.spawn_place_id)
        .unwrap();
    let destination = old
        .destination_tile(&state.agent_did, "hobbs-cafe/cafe")
        .unwrap();
    let record = SpacePresenceRecord {
        agent_did: state.agent_did.clone(),
        avatar_id: None,
        network_id: test_network_id(&state),
        space_id: old.space_id.clone(),
        space_version: old.space_version.clone(),
        seq: 30,
        entered: true,
        from_tile: from,
        path: old.path(from, destination).unwrap(),
        depart_at: now - 2000,
        destination_place_id: "hobbs-cafe/cafe".to_owned(),
        activity: "walking".to_owned(),
        object_place_id: None,
        updated_at: now - 2000,
    };
    assert!(record.path.len() > 2);
    let saved = SignedSpacePresence {
        signature: state.sign_payload(&record).unwrap(),
        public_key: state.signer.public_key().to_owned(),
        record,
    };
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([(
            test_network_id(&state),
            BTreeMap::from([(old.space_id.clone(), saved.clone())]),
        )]),
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
    let entered = initialize_space_presence(&state).await.unwrap();
    assert_eq!(entered.record.seq, 31);
    assert_eq!(
        entered.record.from_tile,
        saved
            .record
            .tile_at(old.walk_speed, entered.record.updated_at)
    );
    assert_eq!(entered.record.depart_at, entered.record.updated_at);
    assert_eq!(entered.record.space_version, current.space_version);
    assert_eq!(
        entered.record.destination_place_id,
        saved.record.destination_place_id
    );
    assert_eq!(entered.record.activity, saved.record.activity);
    current.validate_record(&entered.record).unwrap();
    assert!(verify_payload(&entered.record, &entered.signature, &entered.public_key).unwrap());
}
