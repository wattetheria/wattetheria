use super::*;
use wattetheria_kernel::space::SpacePackage;

pub(super) fn two_spaces(state: &mut ControlPlaneState) -> (SpacePackage, SpacePackage) {
    let mut source = current_package(state).clone();
    let mut target = source.clone();
    target.space_id = "zz-second-space".to_owned();
    for place in &mut target.places {
        if place.place_id == source.space_id {
            place.place_id.clone_from(&target.space_id);
        }
        if place.parent_place_id.as_deref() == Some(&source.space_id) {
            place.parent_place_id = Some(target.space_id.clone());
        }
    }
    source.portals = serde_json::from_value(json!([{
        "portal_id":"next", "source_place_id":source.spawn_place_id,
        "target_space_id":target.space_id, "target_place_id":target.spawn_place_id
    }]))
    .unwrap();
    super::namespace_portals::rehash_package(&mut source);
    super::namespace_portals::rehash_package(&mut target);
    state.space_catalog = Some(Arc::new(super::namespace_portals::configured_catalog(
        &source.space_id,
        &[source.clone(), target.clone()],
    )));
    (source, target)
}

fn save_inactive_registry(state: &mut ControlPlaneState, records: &[SignedSpacePresence]) {
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([(
            test_network_id(state),
            records
                .iter()
                .map(|signed| (signed.record.space_id.clone(), signed.clone()))
                .collect(),
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
}

pub(super) fn assert_default_spawn(
    state: &ControlPlaneState,
    signed: &SignedSpacePresence,
    seq: u64,
) {
    let package = current_package(state);
    let avatar = signed
        .record
        .avatar_id
        .expect("entry assigns an appearance");
    assert!(avatar < wattetheria_gateway_contract::space::SPACE_AVATAR_COUNT);
    assert_eq!(
        signed.record,
        wattetheria_gateway_contract::SpacePresenceRecord {
            agent_did: state.agent_did.clone(),
            avatar_id: Some(avatar),
            network_id: test_network_id(state),
            space_id: package.space_id.clone(),
            space_version: package.space_version.clone(),
            entered: true,
            seq,
            from_tile: package
                .destination_tile(&state.agent_did, &package.spawn_place_id)
                .unwrap(),
            path: String::new(),
            depart_at: signed.record.updated_at,
            destination_place_id: package.spawn_place_id.clone(),
            activity: String::new(),
            object_place_id: None,
            updated_at: signed.record.updated_at,
        }
    );
    assert!(verify_payload(&signed.record, &signed.signature, &signed.public_key).unwrap());
}

#[tokio::test]
async fn removed_last_space_falls_back_to_default_spawn_without_allowing_other_spaces() {
    for explicit_default in [false, true] {
        let (dir, _app, token, _policy, state) = build_test_app(100);
        let mut state = disk_state(state, &dir.path().join("retired-last.db"));
        let (default, other) = two_spaces(&mut state);
        let mut previous = initialize_space_presence(&state).await.unwrap();
        previous.record.from_tile = default
            .destination_tile(&state.agent_did, "hobbs-cafe/cafe")
            .unwrap();
        previous.record.destination_place_id = "hobbs-cafe/cafe".to_owned();
        previous.record.activity = "old default location".to_owned();
        previous.signature = state.sign_payload(&previous.record).unwrap();
        default.validate_record(&previous.record).unwrap();
        let mut removed = previous.clone();
        removed.record.space_id = "removed-space".to_owned();
        removed.record.seq = 20;
        removed.signature = state.sign_payload(&removed.record).unwrap();
        save_inactive_registry(&mut state, &[previous.clone(), removed.clone()]);
        let before = state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap();
        let mut events = state.stream_tx.subscribe();
        for requested in [&other.space_id, &removed.record.space_id] {
            let rejected = authed_post_json(
                app(state.clone()),
                &token,
                "/v1/wattetheria/space/enter",
                json!({"space_id":requested,"portal_id":"next"}),
            )
            .await;
            assert_eq!(
                rejected["error"],
                "last Space is unavailable; enter the default Space first"
            );
            assert_eq!(
                state
                    .local_db
                    .load_domain::<Value>("own_space_presence")
                    .unwrap(),
                before
            );
            assert!(state.own_space_presence.lock().await.active_space.is_none());
            assert!(matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
        }
        let body = if explicit_default {
            json!({"space_id":default.space_id})
        } else {
            json!({})
        };
        let entered = authed_post_json(
            app(state.clone()),
            &token,
            "/v1/wattetheria/space/enter",
            body,
        )
        .await;
        assert_eq!(entered["ok"], true);
        let entered: SignedSpacePresence =
            serde_json::from_value(entered["presence"].clone()).unwrap();
        assert_default_spawn(&state, &entered, removed.record.seq + 1);
        assert_ne!(entered.record.from_tile, previous.record.from_tile);
        assert_eq!(events.try_recv().unwrap().payload, json!(entered));
        let persisted = OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did,
        )
        .unwrap();
        assert_eq!(
            persisted.stored(&test_network_id(&state), &removed.record.space_id),
            Some(&removed)
        );
        assert_eq!(
            persisted.stored(&test_network_id(&state), &default.space_id),
            Some(&entered)
        );
        assert_eq!(initialize_space_presence(&state).await.unwrap(), entered);
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }
}

#[tokio::test]
async fn first_entry_only_allows_default_space_without_writes_on_rejection() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    let (source, target) = two_spaces(&mut state);
    let mut events = state.stream_tx.subscribe();
    let rejected = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({"space_id":target.space_id,"portal_id":"next"}),
    )
    .await;
    assert!(
        rejected["error"]
            .as_str()
            .unwrap()
            .contains("default Space")
    );
    assert!(state.own_space_presence.lock().await.networks.is_empty());
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
    let entered = initialize_space_presence(&state).await.unwrap();
    assert_eq!(entered.record.space_id, source.space_id);
    assert_eq!(entered.record.destination_place_id, source.spawn_place_id);
    assert_eq!(entered.record.path, "");
}

#[tokio::test]
async fn portal_restart_returns_last_space_and_rejects_inactive_bypass() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let mut state = disk_state(state, &dir.path().join("last-space-restart.db"));
    let (source, target) = two_spaces(&mut state);
    initialize_space_presence(&state).await.unwrap();
    let unknown = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({"space_id":"unknown-space"}),
    )
    .await;
    assert_eq!(unknown["error"], "unknown configured Space");
    let arrived = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({"space_id":target.space_id,"portal_id":"next"}),
    )
    .await;
    assert_eq!(arrived["ok"], true);
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"hobbs-cafe/cafe","activity":"return here"}),
    )
    .await;
    let moved: SignedSpacePresence = serde_json::from_value(moved["presence"].clone()).unwrap();
    assert_ne!(moved.record.path, "");
    state.own_space_presence = Arc::new(Mutex::new(
        OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did,
        )
        .unwrap(),
    ));
    crate::reset_space_presence(&state).await.unwrap();
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    let before = state
        .local_db
        .load_domain::<Value>("own_space_presence")
        .unwrap();
    let mut events = state.stream_tx.subscribe();
    let rejected = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({"space_id":source.space_id,"portal_id":"next"}),
    )
    .await;
    assert_eq!(
        rejected["error"],
        "return to the last Space before using a portal"
    );
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        before
    );
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    let restored = initialize_space_presence(&state).await.unwrap();
    assert_eq!(restored.record.space_id, target.space_id);
    assert_eq!(restored.record.from_tile, moved.record.from_tile);
    assert_eq!(restored.record.path, moved.record.path);
    assert_eq!(restored.record.depart_at, moved.record.depart_at);
    assert_eq!(restored.record.activity, moved.record.activity);
    assert_eq!(restored.record.seq, moved.record.seq + 2);
}

#[tokio::test]
async fn raw_gateway_latest_restores_last_before_default_even_with_stale_local() {
    for local_kind in ["absent", "same-space", "other-space"] {
        let (dir, _app, _token, _policy, state) = build_test_app(100);
        let mut state = disk_state(state, &dir.path().join("raw-last.db"));
        let (source, target) = two_spaces(&mut state);
        let identity = wattetheria_kernel::agent_identity::load_agent_identity(dir.path()).unwrap();
        let mut remote = super::map_upgrade::signed_spawn(&state, &identity, &target, 20);
        remote.record.avatar_id = Some(24);
        remote.record.entered = false;
        remote.record.destination_place_id = "hobbs-cafe/cafe".to_owned();
        remote.record.path = target
            .path(
                remote.record.from_tile,
                target
                    .destination_tile(&state.agent_did, "hobbs-cafe/cafe")
                    .unwrap(),
            )
            .unwrap();
        remote.record.activity = "latest remote position".to_owned();
        remote.signature = state.sign_payload(&remote.record).unwrap();
        if local_kind != "absent" {
            let package = if local_kind == "same-space" {
                &target
            } else {
                &source
            };
            let local = super::map_upgrade::signed_spawn(&state, &identity, package, 5);
            save_inactive_registry(&mut state, &[local]);
        }
        let mut legacy = remote.clone();
        legacy.record.network_id.clear();
        legacy.record.space_id.clone_from(&source.space_id);
        legacy.record.seq = 100;
        legacy.signature = state.sign_payload(&legacy.record).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let raw = remote.clone();
        let router = Router::new().route(
            "/api/spaces/presence/{agent_id}/record",
            get(move |Query(query): Query<BTreeMap<String, String>>| {
                let captured = captured.clone();
                let raw = raw.clone();
                let legacy = legacy.clone();
                async move {
                    captured.lock().await.push(query.clone());
                    if query.get("network_id").is_some_and(String::is_empty) {
                        Json(legacy)
                    } else {
                        Json(raw)
                    }
                }
            }),
        );
        let (url, server) = super::query_resilience::serve(router).await;
        let older = super::map_upgrade::signed_spawn(&state, &identity, &source, 10);
        let (older_url, older_server) = super::sequence_recovery::floor_server(older).await;
        let mut foreign = remote.clone();
        foreign.record.network_id = "foreign-network".to_owned();
        foreign.record.seq = 1_000;
        foreign.signature = state.sign_payload(&foreign.record).unwrap();
        let (foreign_url, foreign_server) = super::sequence_recovery::floor_server(foreign).await;
        state.gateway_urls = vec![older_url, url, foreign_url];
        if local_kind == "same-space" {
            state.gateway_urls.reverse();
        }
        let mut events = state.stream_tx.subscribe();
        let entered = initialize_space_presence(&state).await.unwrap();
        let mut expected = remote.record.clone();
        expected.entered = true;
        expected.seq = 101;
        expected.updated_at = entered.record.updated_at;
        assert_eq!(entered.record, expected, "local_kind={local_kind}");
        assert!(verify_payload(&entered.record, &entered.signature, &entered.public_key).unwrap());
        assert_eq!(events.try_recv().unwrap().payload, json!(entered));
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let queries = seen.lock().await;
        assert_eq!(queries.len(), 2);
        assert!(
            queries
                .iter()
                .all(|query| query.len() == 1 && query.contains_key("network_id"))
        );
        assert_eq!(queries[0]["network_id"], test_network_id(&state));
        assert_eq!(queries[1]["network_id"], "");
        server.abort();
        older_server.abort();
        foreign_server.abort();
    }
}

#[tokio::test]
async fn reset_only_retires_global_latest_and_keeps_lower_signed_bytes_hidden() {
    for latest_state in ["entered", "inactive", "invalid-activity"] {
        let (dir, _app, token, _policy, state) = build_test_app(100);
        let path = dir.path().join("reset-latest-only.db");
        let mut state = disk_state(state, &path);
        let public_id =
            bootstrap_broker_identity(app(state.clone()), &token, &state.agent_did).await;
        let (source, target) = two_spaces(&mut state);
        let identity = wattetheria_kernel::agent_identity::load_agent_identity(dir.path()).unwrap();
        let mut latest = super::map_upgrade::signed_spawn(&state, &identity, &source, 20);
        let lower = super::map_upgrade::signed_spawn(&state, &identity, &target, 10);
        latest.record.entered = latest_state != "inactive";
        if latest_state == "invalid-activity" {
            latest.record.activity = "old\u{7}activity".to_owned();
        }
        latest.signature = state.sign_payload(&latest.record).unwrap();
        save_inactive_registry(&mut state, &[latest.clone(), lower.clone()]);
        let mut events = state.stream_tx.subscribe();
        crate::reset_space_presence(&state).await.unwrap();
        let registry = state.own_space_presence.lock().await;
        assert_eq!(
            registry.stored(&lower.record.network_id, &target.space_id),
            Some(&lower)
        );
        let saved_latest = registry
            .stored(&latest.record.network_id, &source.space_id)
            .unwrap();
        if latest_state == "entered" {
            let mut expected = latest.record.clone();
            expected.entered = false;
            expected.seq = 21;
            expected.updated_at = saved_latest.record.updated_at;
            assert_eq!(saved_latest.record, expected);
            assert_eq!(events.try_recv().unwrap().payload, json!(saved_latest));
        } else {
            assert_eq!(saved_latest, &latest);
        }
        assert!(registry.active_space.is_none());
        for package in [&source, &target] {
            assert!(
                registry
                    .visible_get(&latest.record.network_id, package)
                    .is_none()
            );
        }
        assert!(
            registry
                .projection(&latest.record.network_id, state.space_catalog.as_deref())
                .iter()
                .all(|signed| !signed.record.entered)
        );
        drop(registry);
        let saved = state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap();
        rusqlite::Connection::open(&path).unwrap().execute_batch(
            "CREATE TRIGGER forbid_repeated_reset BEFORE UPDATE ON own_space_presence BEGIN SELECT RAISE(ABORT,'repeated reset write'); END;"
        ).unwrap();
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
        }
        crate::emit_current_space_presence(&state).await.unwrap();
        while let Ok(event) = events.try_recv() {
            let signed: SignedSpacePresence = serde_json::from_value(event.payload).unwrap();
            assert!(!signed.record.entered);
        }
        assert_public_views_hide_inactive_entries(&state, &token, &public_id, &[&source, &target])
            .await;
        assert_eq!(
            state
                .local_db
                .load_domain::<Value>("own_space_presence")
                .unwrap(),
            saved
        );
    }
}

async fn assert_public_views_hide_inactive_entries(
    state: &ControlPlaneState,
    token: &str,
    public_id: &str,
    packages: &[&SpacePackage],
) {
    let snapshot = crate::routes::space::snapshot::export(state).await.unwrap();
    assert!(
        snapshot
            .space_presences
            .iter()
            .all(|signed| !signed.record.entered)
    );
    let exported = crate::routes::client_api::build_signed_public_client_snapshot(
        state,
        &crate::ClientExportQuery {
            public_id: Some(public_id.to_owned()),
            ..crate::ClientExportQuery::default()
        },
    )
    .await
    .unwrap();
    assert!(
        exported
            .payload
            .space_presences
            .iter()
            .all(|signed| !signed.record.entered)
    );
    assert!(
        verify_payload(
            &exported.payload,
            &exported.signature,
            &state.identity.public_key
        )
        .unwrap()
    );
    for package in packages {
        let own = authed_get_json(
            app(state.clone()),
            token,
            &format!(
                "/v1/wattetheria/space/presence?space_id={}",
                package.space_id
            ),
        )
        .await;
        assert_eq!(own["error"], "agent presence not found");
        let place = authed_get_json(
            app(state.clone()),
            token,
            &format!(
                "/v1/wattetheria/space/place?space_id={}&place_id={}",
                package.space_id, package.space_id
            ),
        )
        .await;
        assert_eq!(place["presences"], json!([]));
    }
}

#[tokio::test]
async fn checkpoint_bound_legacy_last_restores_geometry_but_unbound_is_only_floor() {
    for bound in [true, false] {
        let (dir, _app, _token, _policy, state) = build_test_app(100);
        let mut state = disk_state(state, &dir.path().join("legacy-last.db"));
        let (source, target) = two_spaces(&mut state);
        let identity = wattetheria_kernel::agent_identity::load_agent_identity(dir.path()).unwrap();
        let mut legacy = super::map_upgrade::signed_spawn(&state, &identity, &target, 75);
        legacy.record.network_id.clear();
        legacy.record.destination_place_id = "hobbs-cafe/cafe".to_owned();
        legacy.record.path = target
            .path(
                legacy.record.from_tile,
                target
                    .destination_tile(&state.agent_did, "hobbs-cafe/cafe")
                    .unwrap(),
            )
            .unwrap();
        legacy.signature = state.sign_payload(&legacy.record).unwrap();
        let namespace = if bound {
            test_network_id(&state)
        } else {
            String::new()
        };
        let registry = OwnSpacePresence {
            networks: BTreeMap::from([(
                namespace.clone(),
                BTreeMap::from([(target.space_id.clone(), legacy.clone())]),
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
        assert_eq!(
            state
                .own_space_presence
                .lock()
                .await
                .stored(&namespace, &target.space_id),
            Some(&legacy)
        );
        let entered = initialize_space_presence(&state).await.unwrap();
        assert_eq!(entered.record.seq, 76);
        assert_eq!(entered.record.network_id, test_network_id(&state));
        if bound {
            assert_eq!(entered.record.space_id, target.space_id);
            assert_eq!(entered.record.path, legacy.record.path);
            assert_eq!(entered.record.depart_at, legacy.record.depart_at);
            assert_eq!(
                entered.record.destination_place_id,
                legacy.record.destination_place_id
            );
        } else {
            assert_eq!(entered.record.space_id, source.space_id);
            assert_eq!(entered.record.path, "");
            assert_eq!(entered.record.destination_place_id, source.spawn_place_id);
            assert_eq!(
                state
                    .own_space_presence
                    .lock()
                    .await
                    .stored("", &target.space_id),
                Some(&legacy)
            );
        }
        assert!(verify_payload(&entered.record, &entered.signature, &entered.public_key).unwrap());
    }
}

#[tokio::test]
async fn unbound_legacy_floor_cannot_bypass_failed_current_network_recovery() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    let mut legacy = super::sequence_recovery::signed_floor(&state, 75);
    legacy.record.network_id.clear();
    legacy.signature = state.sign_payload(&legacy.record).unwrap();
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([(
            String::new(),
            BTreeMap::from([(legacy.record.space_id.clone(), legacy)]),
        )]),
        active_space: None,
    };
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    state.own_space_presence = Arc::new(Mutex::new(registry));
    state.gateway_urls = vec!["not a gateway URL".to_owned()];
    let before = state
        .local_db
        .load_domain::<Value>("own_space_presence")
        .unwrap();
    let mut events = state.stream_tx.subscribe();
    assert!(
        initialize_space_presence(&state)
            .await
            .unwrap_err()
            .to_string()
            .contains("gateway presence sequence recovery unavailable")
    );
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        before
    );
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn reset_invalid_remote_latest_does_not_resign_a_lower_local_entry() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let path = dir.path().join("remote-invalid-last.db");
    let mut state = disk_state(state, &path);
    let public_id = bootstrap_broker_identity(app(state.clone()), &token, &state.agent_did).await;
    let (source, target) = two_spaces(&mut state);
    let identity = wattetheria_kernel::agent_identity::load_agent_identity(dir.path()).unwrap();
    let lower = super::map_upgrade::signed_spawn(&state, &identity, &target, 10);
    save_inactive_registry(&mut state, std::slice::from_ref(&lower));
    let mut remote = super::map_upgrade::signed_spawn(&state, &identity, &source, 20);
    remote.record.activity = "old\u{7}activity".to_owned();
    remote.signature = state.sign_payload(&remote.record).unwrap();
    let (url, server) = super::sequence_recovery::floor_server(remote.clone()).await;
    state.gateway_urls = vec![url];
    let mut events = state.stream_tx.subscribe();
    crate::reset_space_presence(&state).await.unwrap();
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .stored(&remote.record.network_id, &source.space_id),
        Some(&remote)
    );
    let before = state
        .local_db
        .load_domain::<Value>("own_space_presence")
        .unwrap();
    rusqlite::Connection::open(&path).unwrap().execute_batch(
        "CREATE TRIGGER forbid_invalid_raw_reset BEFORE UPDATE ON own_space_presence BEGIN SELECT RAISE(ABORT,'invalid activity reset write'); END;"
    ).unwrap();
    server.abort();
    state.gateway_urls = vec!["not a gateway URL".to_owned()];
    for _ in 0..3 {
        crate::reset_space_presence(&state).await.unwrap();
        assert_eq!(
            state
                .local_db
                .load_domain::<Value>("own_space_presence")
                .unwrap(),
            before
        );
        let registry = state.own_space_presence.lock().await;
        assert_eq!(
            registry.stored(&lower.record.network_id, &target.space_id),
            Some(&lower)
        );
        assert_eq!(
            registry.projection(&lower.record.network_id, state.space_catalog.as_deref()),
            []
        );
        assert!(registry.active_space.is_none());
        assert_eq!(
            registry.stored(&remote.record.network_id, &source.space_id),
            Some(&remote)
        );
    }
    let rejected = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({"space_id":target.space_id}),
    )
    .await;
    assert_eq!(
        rejected["error"],
        "return to the last Space before using a portal"
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
    assert_public_views_hide_inactive_entries(&state, &token, &public_id, &[&source, &target])
        .await;
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TRIGGER forbid_invalid_raw_reset;")
        .unwrap();
    let restored = initialize_space_presence(&state).await.unwrap();
    assert_eq!(restored.record.space_id, source.space_id);
    assert_eq!(restored.record.seq, 21);
    assert_eq!(restored.record.activity, "");
    assert_eq!(restored.record.from_tile, remote.record.from_tile);
    assert_eq!(restored.record.path, remote.record.path);
    assert_eq!(restored.record.depart_at, remote.record.depart_at);
    assert!(verify_payload(&restored.record, &restored.signature, &restored.public_key).unwrap());
}

#[tokio::test]
async fn invalid_activity_raw_import_rejects_known_invalid_geometry_atomically() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let mut state = disk_state(state, &dir.path().join("invalid-raw-geometry.db"));
    let (source, target) = two_spaces(&mut state);
    let identity = wattetheria_kernel::agent_identity::load_agent_identity(dir.path()).unwrap();
    let lower = super::map_upgrade::signed_spawn(&state, &identity, &target, 10);
    save_inactive_registry(&mut state, std::slice::from_ref(&lower));
    let mut remote = super::map_upgrade::signed_spawn(&state, &identity, &source, 20);
    remote.record.activity = "old\u{7}activity".to_owned();
    remote.record.from_tile = [u32::MAX, u32::MAX];
    remote.signature = state.sign_payload(&remote.record).unwrap();
    let (url, server) = super::sequence_recovery::floor_server(remote).await;
    state.gateway_urls = vec![url];
    let before = state
        .local_db
        .load_domain::<Value>("own_space_presence")
        .unwrap();
    let mut events = state.stream_tx.subscribe();
    assert_eq!(
        crate::reset_space_presence(&state)
            .await
            .unwrap_err()
            .to_string(),
        "presence starts on blocked or out-of-bounds tile"
    );
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        before
    );
    let reloaded = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        reloaded.stored(&lower.record.network_id, &target.space_id),
        Some(&lower)
    );
    assert_eq!(
        reloaded.stored(&lower.record.network_id, &source.space_id),
        None
    );
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    server.abort();
}

#[tokio::test]
async fn explicit_leave_from_portal_target_remains_the_last_space() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    let (source, target) = two_spaces(&mut state);
    initialize_space_presence(&state).await.unwrap();
    let switched = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({"space_id":target.space_id,"portal_id":"next"}),
    )
    .await;
    let switched: SignedSpacePresence =
        serde_json::from_value(switched["presence"].clone()).unwrap();
    assert_eq!(switched.record.space_id, target.space_id);
    let left = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/leave",
        json!({}),
    )
    .await;
    let left: SignedSpacePresence = serde_json::from_value(left["presence"].clone()).unwrap();
    assert_eq!(left.record.space_id, target.space_id);
    assert!(!left.record.entered);
    assert_eq!(left.record.seq, switched.record.seq + 1);
    let before = state
        .local_db
        .load_domain::<Value>("own_space_presence")
        .unwrap();
    let rejected = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({"space_id":source.space_id,"portal_id":"next"}),
    )
    .await;
    assert_eq!(
        rejected["error"],
        "return to the last Space before using a portal"
    );
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        before
    );
    let returned = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({}),
    )
    .await;
    let returned: SignedSpacePresence =
        serde_json::from_value(returned["presence"].clone()).unwrap();
    assert!(returned.record.entered);
    assert_eq!(returned.record.space_id, target.space_id);
    assert_eq!(returned.record.seq, left.record.seq + 1);
    assert_eq!(returned.record.from_tile, left.record.from_tile);
    assert_eq!(returned.record.path, left.record.path);
    assert_eq!(returned.record.depart_at, left.record.depart_at);
}
