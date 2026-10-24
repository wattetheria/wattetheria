use super::namespace_portals::{configured_catalog, rehash_package};
use super::*;
use wattetheria_gateway_contract::SpacePresenceRecord;

#[tokio::test]
async fn gateway_highest_unknown_or_invalid_target_suppresses_old_source() {
    for failure in ["unknown-space", "unknown-version", "invalid-geometry"] {
        let (_dir, _app, token, _policy, mut state) = build_test_app(100);
        let remote = Identity::new_random();
        let package = current_package(&state);
        let now = Utc::now().timestamp_millis();
        let record = SpacePresenceRecord {
            agent_did: remote.agent_did.clone(),
            avatar_id: None,
            network_id: test_network_id(&state),
            space_id: package.space_id.clone(),
            space_version: package.space_version.clone(),
            seq: 10,
            entered: true,
            from_tile: package
                .destination_tile(&remote.agent_did, &package.spawn_place_id)
                .unwrap(),
            path: String::new(),
            depart_at: now,
            destination_place_id: package.spawn_place_id.clone(),
            activity: String::new(),
            object_place_id: None,
            updated_at: now,
        };
        let source = SignedSpacePresence {
            signature: remote.sign(&canonical_bytes(&record).unwrap()).unwrap(),
            public_key: remote.public_key.clone(),
            record,
        };
        let mut newer = source.clone();
        newer.record.seq += 1;
        match failure {
            "unknown-space" => {
                newer.record.space_id = "unknown-target".to_owned();
            }
            "unknown-version" => {
                newer.record.space_version = "unknown-version".to_owned();
            }
            "invalid-geometry" => {
                newer.record.from_tile = [u32::MAX, u32::MAX];
            }
            _ => unreachable!(),
        }
        newer.signature = remote
            .sign(&canonical_bytes(&newer.record).unwrap())
            .unwrap();
        let payload = json!({"presences":[source,newer]});
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        state.gateway_urls = vec![format!("http://{}", listener.local_addr().unwrap())];
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let result = authed_get_json(
            app(state.clone()),
            &token,
            &format!(
                "/v1/wattetheria/space/presence?agent_id={}",
                remote.agent_did
            ),
        )
        .await;
        assert_eq!(result["error"], "agent presence not found", "{failure}");
        let place = authed_get_json(
            app(state),
            &token,
            "/v1/wattetheria/space/place?place_id=the-ville",
        )
        .await;
        assert_eq!(place["presences"], json!([]), "{failure}");
        server.abort();
    }
}

#[tokio::test]
async fn place_occupancy_uses_actual_tile_and_trusted_record_speed() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    let mut old = current_package(&state).clone();
    old.walk_speed = 0.001;
    rehash_package(&mut old);
    let mut current = old.clone();
    current.walk_speed = 1000.0;
    rehash_package(&mut current);
    state.space_catalog = Some(Arc::new(configured_catalog(
        &old.space_id,
        &[old.clone(), current],
    )));
    let remote = Identity::new_random();
    let from = old
        .destination_tile(&remote.agent_did, "johnson-park")
        .unwrap();
    let to = old
        .destination_tile(&remote.agent_did, "hobbs-cafe/cafe")
        .unwrap();
    let now = Utc::now().timestamp_millis();
    let record = SpacePresenceRecord {
        agent_did: remote.agent_did.clone(),
        avatar_id: None,
        network_id: test_network_id(&state),
        space_id: old.space_id.clone(),
        space_version: old.space_version.clone(),
        seq: 10,
        entered: true,
        from_tile: from,
        path: old.path(from, to).unwrap(),
        depart_at: now - 1_000,
        destination_place_id: "hobbs-cafe/cafe".to_owned(),
        activity: "walking".to_owned(),
        object_place_id: None,
        updated_at: now,
    };
    assert_eq!(record.tile_at(old.walk_speed, now), from);
    assert_eq!(record.tile_at(1000.0, now), to);
    assert_ne!(from, to);
    let signed = SignedSpacePresence {
        signature: remote.sign(&canonical_bytes(&record).unwrap()).unwrap(),
        public_key: remote.public_key.clone(),
        record,
    };
    let expected = signed.clone();
    let payload = json!({"presences":[signed]});
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
    let current_place = authed_get_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/place?place_id=johnson-park",
    )
    .await;
    assert_eq!(current_place["presences"], json!([expected.clone()]));
    let destination_place = authed_get_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/place?place_id=hobbs-cafe/cafe",
    )
    .await;
    assert_eq!(destination_place["presences"], json!([]));
    let observed = authed_get_json(
        app(state.clone()),
        &token,
        &format!(
            "/v1/wattetheria/space/presence?agent_id={}",
            remote.agent_did
        ),
    )
    .await;
    assert_eq!(
        observed["position"],
        json!(
            expected
                .record
                .position_at(old.walk_speed, observed["server_time_ms"].as_i64().unwrap())
        )
    );
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        None
    );
    server.abort();
}

#[tokio::test]
async fn rolling_upgrade_keeps_or_replans_and_stops_removed_targets() {
    for mode in ["keep", "replan", "removed-target", "blocked-current"] {
        let (dir, _app, _token, _policy, mut state) = build_test_app(100);
        let identity = wattetheria_kernel::agent_identity::load_agent_identity(dir.path()).unwrap();
        let old = current_package(&state).clone();
        let mut current = old.clone();
        let from = old
            .destination_tile(&state.agent_did, &old.spawn_place_id)
            .unwrap();
        let destination = old
            .destination_tile(&state.agent_did, "hobbs-cafe/cafe")
            .unwrap();
        let path = old.path(from, destination).unwrap();
        assert_ne!(path, "");
        match mode {
            "keep" => {
                current.tile_size += 1;
            }
            "replan" => {
                current.walk_speed *= 2.0;
            }
            "removed-target" => {
                current.places.retain(|place| {
                    place.place_id != "hobbs-cafe/cafe"
                        && place.parent_place_id.as_deref() != Some("hobbs-cafe/cafe")
                });
            }
            "blocked-current" => {
                current.walkable_grid[from[1] as usize][from[0] as usize] = 0;
                for place in &mut current.places {
                    place.tiles.retain(|tile| *tile != from);
                }
                current.places.retain(|place| !place.tiles.is_empty());
            }
            _ => unreachable!(),
        }
        rehash_package(&mut current);
        state.space_catalog = Some(Arc::new(configured_catalog(
            &old.space_id,
            &[old.clone(), current.clone()],
        )));
        let now = Utc::now().timestamp_millis();
        let mut original = super::map_upgrade::signed_spawn(&state, &identity, &old, 20);
        original.record.path.clone_from(&path);
        original.record.depart_at = now + 60_000;
        original.record.destination_place_id = "hobbs-cafe/cafe".to_owned();
        original.record.activity = "walking".to_owned();
        original.signature = state.sign_payload(&original.record).unwrap();
        assert_eq!(original.record.from_tile, from);
        let registry = OwnSpacePresence {
            networks: BTreeMap::from([(
                test_network_id(&state),
                BTreeMap::from([(old.space_id.clone(), original.clone())]),
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
        let restored = initialize_space_presence(&state).await.unwrap();
        assert_eq!(restored.record.seq, 21, "{mode}");
        current.validate_record(&restored.record).unwrap();
        assert_eq!(restored.record.space_version, current.space_version);
        match mode {
            "keep" => {
                assert_eq!(restored.record.path, path);
                assert_eq!(restored.record.depart_at, original.record.depart_at);
                assert_eq!(restored.record.from_tile, from);
            }
            "replan" => {
                assert_eq!(restored.record.from_tile, from);
                assert_eq!(restored.record.depart_at, restored.record.updated_at);
                assert_eq!(restored.record.destination_place_id, "hobbs-cafe/cafe");
            }
            "removed-target" => {
                assert_eq!(restored.record.path, "");
                assert_eq!(restored.record.from_tile, from);
                assert_ne!(restored.record.destination_place_id, "hobbs-cafe/cafe");
                assert_eq!(restored.record.activity, "walking");
            }
            "blocked-current" => {
                assert_eq!(restored.record.path, "");
                assert_eq!(restored.record.destination_place_id, current.spawn_place_id);
                assert!(current.is_walkable(restored.record.from_tile));
                assert_ne!(restored.record.from_tile, from);
            }
            _ => unreachable!(),
        }
        assert!(
            verify_payload(&restored.record, &restored.signature, &restored.public_key).unwrap()
        );
    }
}
