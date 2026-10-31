use super::*;
fn invalidate_own_presence_payload(
    payload: &mut Value,
    state: &ControlPlaneState,
    local: &SignedSpacePresence,
    foreign: &SignedSpacePresence,
    version: i64,
    failure: &str,
) {
    let spaces = payload["spaces"].as_object_mut().unwrap();
    match failure {
        "foreign" | "agent-key" if version == 12 => {
            spaces.insert(local.record.space_id.clone(), json!(foreign));
        }
        "foreign" => {
            spaces
                .get_mut(&local.record.space_id)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(foreign.record.agent_did.clone(), json!(foreign));
        }
        "agent-key" => {
            let records = spaces
                .get_mut(&local.record.space_id)
                .unwrap()
                .as_object_mut()
                .unwrap();
            records.remove(&state.agent_did);
            records.insert("wrong-agent-key".to_owned(), json!(local));
        }
        "space-key" => {
            let record = spaces.remove(&local.record.space_id).unwrap();
            spaces.insert("wrong-space-key".to_owned(), record);
        }
        "archive-signature" => {
            let archive = spaces.get_mut("zz-archive").unwrap();
            let signed = if version == 12 {
                archive
            } else {
                &mut archive[&state.agent_did]
            };
            signed["record"]["seq"] = json!(99);
        }
        "identity" => {}
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn own_presence_upgrade_rejects_foreign_identity_keys_and_signatures_without_overwrite() {
    for version in [10, 11, 12] {
        for failure in [
            "foreign",
            "agent-key",
            "space-key",
            "archive-signature",
            "identity",
        ] {
            let (dir, _app, _token, _policy, state) = build_test_app(100);
            let path = dir.path().join("own-presence-invalid-upgrade.db");
            let state = disk_state(state, &path);
            let local = initialize_space_presence(&state).await.unwrap();
            let foreign =
                signed_spawn(&state, &Identity::new_random(), current_package(&state), 14);
            let mut archive = local.clone();
            archive.record.space_id = "zz-archive".to_owned();
            archive.record.space_version = "obsolete-map".to_owned();
            archive.signature = state.sign_payload(&archive.record).unwrap();
            let mut payload = if version == 12 {
                json!({"spaces":{
                    (local.record.space_id.clone()):local,
                    (archive.record.space_id.clone()):archive
                }})
            } else {
                json!({"spaces":{
                    (local.record.space_id.clone()):{(state.agent_did.clone()):local},
                    (archive.record.space_id.clone()):{(state.agent_did.clone()):archive}
                }})
            };
            invalidate_own_presence_payload(
                &mut payload,
                &state,
                &local,
                &foreign,
                version,
                failure,
            );
            state
                .local_db
                .save_domain("own_space_presence", &payload)
                .unwrap();
            let conn = rusqlite::Connection::open(&path).unwrap();
            let read_row = || -> (String, String) {
                conn.query_row(
                    "SELECT payload, updated_at FROM own_space_presence WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap()
            };
            let before = read_row();
            if version != 12 {
                let table = if version == 10 {
                    format!("domain_{}", hex::encode("space_presence_registry"))
                } else {
                    "space_presence_registry".to_owned()
                };
                conn.execute_batch(&format!(
                    "ALTER TABLE own_space_presence RENAME TO {table}; UPDATE schema_version SET version={version};"
                )).unwrap();
            }
            let db = LocalDb::open(&path).unwrap();
            let did = if failure == "identity" {
                &foreign.record.agent_did
            } else {
                &state.agent_did
            };
            let error = OwnSpacePresence::load(&db, state.space_catalog.as_deref(), did)
                .unwrap_err()
                .to_string();
            let expected = match failure {
                "foreign" | "identity" => "stored presence is not the local agent",
                "agent-key" if version == 12 => "stored presence is not the local agent",
                "agent-key" | "space-key" => "presence registry key does not match signed record",
                "archive-signature" => "invalid presence signature",
                _ => unreachable!(),
            };
            assert_eq!(error, expected, "schema {version}, {failure}");
            assert_eq!(read_row(), before, "schema {version}, {failure}");
            assert_eq!(
                db.load_domain::<Value>("own_space_presence").unwrap(),
                Some(payload)
            );
        }
    }
}

#[tokio::test]
async fn own_presence_upgrade_sqlite_write_failure_preserves_complete_legacy_payload() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("own-presence-upgrade-write-failure.db");
    let state = disk_state(state, &path);
    let signed = initialize_space_presence(&state).await.unwrap();
    let legacy =
        json!({"spaces":{(signed.record.space_id.clone()):{(state.agent_did.clone()):signed}}});
    state
        .local_db
        .save_domain("own_space_presence", &legacy)
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    let read_row = || -> (String, String) {
        conn.query_row(
            "SELECT payload, updated_at FROM own_space_presence WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };
    let before = read_row();
    conn.execute_batch("CREATE TRIGGER reject_presence_upgrade BEFORE UPDATE ON own_space_presence BEGIN SELECT RAISE(ABORT,'presence upgrade rejected'); END;").unwrap();
    let db = LocalDb::open(&path).unwrap();
    assert!(
        OwnSpacePresence::load(&db, state.space_catalog.as_deref(), &state.agent_did)
            .unwrap_err()
            .to_string()
            .contains("upsert domain table")
    );
    assert_eq!(read_row(), before);
    assert_eq!(
        db.load_domain::<Value>("own_space_presence").unwrap(),
        Some(legacy)
    );
    conn.execute_batch("DROP TRIGGER reject_presence_upgrade;")
        .unwrap();
    let loaded =
        OwnSpacePresence::load(&db, state.space_catalog.as_deref(), &state.agent_did).unwrap();
    assert_eq!(
        loaded.get(&test_network_id(&state), current_package(&state)),
        Some(&signed)
    );
    let converted = read_row();
    let reloaded =
        OwnSpacePresence::load(&db, state.space_catalog.as_deref(), &state.agent_did).unwrap();
    assert_eq!(reloaded.networks, loaded.networks);
    assert_eq!(read_row(), converted);
}

struct MapUpgradeRegistrationBridge {
    anchor: NetworkCredentialTrustAnchor,
}

#[async_trait::async_trait]
impl SwarmBridge for MapUpgradeRegistrationBridge {
    async fn agent_view(&self, agent_did: &str) -> anyhow::Result<SwarmAgentView> {
        Ok(SwarmAgentView {
            agent_did: agent_did.to_owned(),
            stats: AgentStats::default(),
        })
    }
    async fn current_network_id(&self) -> anyhow::Result<String> {
        Ok(self.anchor.network_id.clone())
    }
    async fn local_node_id(&self) -> anyhow::Result<String> {
        Ok("map-upgrade-node".to_owned())
    }
    async fn network_credential_trust_anchor(
        &self,
    ) -> anyhow::Result<NetworkCredentialTrustAnchor> {
        Ok(self.anchor.clone())
    }
    async fn update_network_permission(
        &self,
        _update: wattetheria_kernel::network_agent_registration::NetworkPermissionUpdate,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

pub(super) fn upgraded_package(
    package: &wattetheria_kernel::space::SpacePackage,
    blocked_tile: [u32; 2],
) -> wattetheria_kernel::space::SpacePackage {
    let mut package = package.clone();
    package.walkable_grid[blocked_tile[1] as usize][blocked_tile[0] as usize] = 0;
    for place in &mut package.places {
        place.tiles.retain(|tile| *tile != blocked_tile);
    }
    package.places.retain(|place| !place.tiles.is_empty());
    let mut value = serde_json::to_value(&package).unwrap();
    value.as_object_mut().unwrap().remove("space_version");
    package.space_version = format!(
        "sha256:{:x}",
        sha2::Sha256::digest(serde_jcs::to_vec(&value).unwrap())
    );
    package.validate().unwrap();
    package
}

#[tokio::test]
async fn space_map_upgrade_loads_signed_old_record_and_respawns_with_monotonic_sequence() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let path = dir.path().join("space-map-version-upgrade.db");
    let state = disk_state(state, &path);
    initialize_space_presence(&state).await.unwrap();
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"hobbs-cafe/cafe"}),
    )
    .await;
    let old: SignedSpacePresence = serde_json::from_value(moved["presence"].clone()).unwrap();
    assert_eq!(old.record.seq, 2);
    assert!(
        wattetheria_kernel::signing::verify_payload(&old.record, &old.signature, &old.public_key)
            .unwrap()
    );
    let package = Arc::new(upgraded_package(
        current_package(&state),
        old.record.from_tile,
    ));
    assert_ne!(package.space_version, old.record.space_version);
    let reopened = Arc::new(LocalDb::open(&path).unwrap());
    let registry = OwnSpacePresence::load(
        &reopened,
        Some(&test_space_catalog(&package)),
        &state.agent_did,
    )
    .expect("map upgrade must not abort node startup");
    let state = ControlPlaneState {
        local_db: reopened,
        space_catalog: Some(Arc::new(test_space_catalog(&package))),
        own_space_presence: Arc::new(Mutex::new(registry)),
        ..state
    };
    let spawned = initialize_space_presence(&state).await.unwrap();
    assert_eq!(spawned.record.seq, old.record.seq + 1);
    assert_eq!(
        spawned.record.space_version,
        current_package(&state).space_version
    );
    assert_eq!(
        spawned.record.destination_place_id,
        current_package(&state).spawn_place_id
    );
    assert_eq!(spawned.record.path, "");
    assert!(
        wattetheria_kernel::signing::verify_payload(
            &spawned.record,
            &spawned.signature,
            &spawned.public_key
        )
        .unwrap()
    );
    assert_ne!(spawned.signature, old.signature);
    assert!(current_package(&state).is_walkable(spawned.record.from_tile));
    let restarted = ControlPlaneState {
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
    assert!(
        restarted
            .own_space_presence
            .lock()
            .await
            .active_space
            .is_none()
    );
    let restored = initialize_space_presence(&restarted).await.unwrap();
    let mut expected = spawned.record.clone();
    expected.seq += 1;
    expected.updated_at = restored.record.updated_at;
    assert_eq!(restored.record, expected);
    for _ in 0..3 {
        assert_eq!(
            initialize_space_presence(&restarted).await.unwrap(),
            restored
        );
    }
    assert_eq!(
        restarted
            .local_db
            .load_domain::<Value>("unrelated-existing-state")
            .unwrap(),
        Some(json!({"retained":true}))
    );
}

#[tokio::test]
async fn space_map_upgrade_approved_registration_and_reopened_reapproval_preserve_respawn() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("map-upgrade-approval.db");
    let state = disk_state(state, &path);
    let old = initialize_space_presence(&state).await.unwrap();
    let stored = state
        .local_db
        .load_network_agent_credential(&state.agent_did, None)
        .unwrap()
        .unwrap();
    let package = Arc::new(upgraded_package(
        current_package(&state),
        old.record.from_tile,
    ));
    let registry = OwnSpacePresence::load(
        &state.local_db,
        Some(&test_space_catalog(&package)),
        &state.agent_did,
    )
    .unwrap();
    let state = ControlPlaneState {
        space_catalog: Some(Arc::new(test_space_catalog(&package))),
        own_space_presence: Arc::new(Mutex::new(registry)),
        swarm_bridge: Arc::new(MapUpgradeRegistrationBridge {
            anchor: serde_json::from_str(stored.trust_anchor_json.as_ref().unwrap()).unwrap(),
        }),
        ..state
    };
    let request = crate::build_registration_request(&state).await.unwrap();
    let approved = json!({"request":request,"status":"approved","credential":serde_json::from_str::<Value>(&stored.credential_json).unwrap()});
    let mut events = state.stream_tx.subscribe();
    crate::apply_registry_registration_record(&state, &approved)
        .await
        .unwrap();
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert!(
        state
            .own_space_presence
            .lock()
            .await
            .get(&test_network_id(&state), current_package(&state))
            .is_none()
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    initialize_space_presence(&state).await.unwrap();
    let respawned = state
        .own_space_presence
        .lock()
        .await
        .get(&test_network_id(&state), current_package(&state))
        .cloned()
        .unwrap();
    assert_eq!(respawned.record.seq, old.record.seq + 1);
    assert_eq!(
        respawned.record.space_version,
        current_package(&state).space_version
    );
    assert_eq!(
        events.recv().await.unwrap().payload,
        serde_json::to_value(&respawned).unwrap()
    );
    let reopened = Arc::new(LocalDb::open(&path).unwrap());
    let registry =
        OwnSpacePresence::load(&reopened, state.space_catalog.as_deref(), &state.agent_did)
            .unwrap();
    let restarted = ControlPlaneState {
        local_db: reopened,
        own_space_presence: Arc::new(Mutex::new(registry)),
        ..state
    };
    crate::apply_registry_registration_record(&restarted, &approved)
        .await
        .unwrap();
    assert!(
        restarted
            .own_space_presence
            .lock()
            .await
            .active_space
            .is_none()
    );
    assert_eq!(
        restarted
            .own_space_presence
            .lock()
            .await
            .get(&test_network_id(&restarted), current_package(&restarted)),
        Some(&respawned)
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

pub(super) fn signed_spawn(
    state: &ControlPlaneState,
    identity: &Identity,
    package: &wattetheria_kernel::space::SpacePackage,
    seq: u64,
) -> SignedSpacePresence {
    let record = wattetheria_gateway_contract::SpacePresenceRecord {
        agent_did: identity.agent_did.clone(),
        avatar_id: None,
        skin_id: None,
        network_id: test_network_id(state),
        space_id: package.space_id.clone(),
        space_version: package.space_version.clone(),
        seq,
        entered: true,
        from_tile: package
            .destination_tile(&identity.agent_did, &package.spawn_place_id)
            .unwrap(),
        path: String::new(),
        depart_at: Utc::now().timestamp_millis(),
        destination_place_id: package.spawn_place_id.clone(),
        activity: String::new(),
        object_place_id: None,
        updated_at: Utc::now().timestamp_millis(),
    };
    SignedSpacePresence {
        signature: identity.sign(&canonical_bytes(&record).unwrap()).unwrap(),
        public_key: identity.public_key.clone(),
        record,
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn space_map_upgrade_pending_record_stays_private_and_preserves_own_archives() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let state = disk_state(state, &dir.path().join("space-map-upgrade-pending.db"));
    let old = initialize_space_presence(&state).await.unwrap();
    let package = Arc::new(upgraded_package(
        current_package(&state),
        old.record.from_tile,
    ));
    let mut archive = old.clone();
    archive.record.space_id = "archived-space".to_owned();
    archive.record.space_version = "archived-map-version".to_owned();
    archive.record.seq = 14;
    archive.signature = state.sign_payload(&archive.record).unwrap();
    let mut registry = state.own_space_presence.lock().await.clone();
    registry
        .networks
        .entry(test_network_id(&state))
        .or_default()
        .insert(archive.record.space_id.clone(), archive.clone());
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    let mut checkpoint = state
        .local_db
        .load_network_permission_checkpoint(&state.agent_did, None, None)
        .unwrap()
        .unwrap();
    checkpoint.permission_status = "pending".to_owned();
    state
        .local_db
        .upsert_network_permission_checkpoint(&checkpoint)
        .unwrap();
    let registry = OwnSpacePresence::load(
        &state.local_db,
        Some(&test_space_catalog(&package)),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(registry.get(&test_network_id(&state), &package), None);
    assert_eq!(
        registry.networks[&test_network_id(&state)][&archive.record.space_id],
        archive
    );
    let state = ControlPlaneState {
        space_catalog: Some(Arc::new(test_space_catalog(&package))),
        own_space_presence: Arc::new(Mutex::new(registry)),
        ..state
    };
    let mut events = state.stream_tx.subscribe();
    crate::emit_current_space_presence(&state).await.unwrap();
    assert!(
        initialize_space_presence(&state)
            .await
            .unwrap_err()
            .to_string()
            .contains("active network agent permission")
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(
        crate::routes::space::snapshot::export(&state)
            .await
            .unwrap()
            .space_presences,
        []
    );
    let place = authed_get_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/place?place_id=the-ville",
    )
    .await;
    assert_eq!(place["presences"], json!([]));
    checkpoint.permission_status = "active".to_owned();
    state
        .local_db
        .upsert_network_permission_checkpoint(&checkpoint)
        .unwrap();
    let entered = initialize_space_presence(&state).await.unwrap();
    super::reentry_last::assert_default_spawn(&state, &entered, 15);
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .active_space_id(&test_network_id(&state)),
        Some(entered.record.space_id.as_str())
    );
    assert_eq!(events.try_recv().unwrap().payload, json!(entered));
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(
        state.own_space_presence.lock().await.networks[&test_network_id(&state)]
            [&archive.record.space_id],
        archive
    );
    let persisted = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        persisted.networks[&test_network_id(&state)][&archive.record.space_id],
        archive
    );
    assert_eq!(
        persisted.get(&test_network_id(&state), current_package(&state)),
        Some(&entered)
    );
    assert_eq!(
        persisted.stored(&test_network_id(&state), &old.record.space_id),
        Some(&entered)
    );
}

#[tokio::test]
async fn space_map_upgrade_sequence_overflow_keeps_old_record_and_emits_nothing() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let state = disk_state(state, &dir.path().join("space-map-upgrade-overflow.db"));
    let mut old = initialize_space_presence(&state).await.unwrap();
    old.record.seq = u64::MAX;
    old.signature = state.sign_payload(&old.record).unwrap();
    let package = Arc::new(upgraded_package(
        current_package(&state),
        old.record.from_tile,
    ));
    let mut registry = state.own_space_presence.lock().await.clone();
    registry
        .networks
        .entry(test_network_id(&state))
        .or_default()
        .insert(old.record.space_id.clone(), old.clone());
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    let registry = OwnSpacePresence::load(
        &state.local_db,
        Some(&test_space_catalog(&package)),
        &state.agent_did,
    )
    .unwrap();
    let state = ControlPlaneState {
        space_catalog: Some(Arc::new(test_space_catalog(&package))),
        own_space_presence: Arc::new(Mutex::new(registry)),
        ..state
    };
    let mut events = state.stream_tx.subscribe();
    assert_eq!(
        initialize_space_presence(&state)
            .await
            .unwrap_err()
            .to_string(),
        "presence sequence exhausted"
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(
        state.own_space_presence.lock().await.networks[&test_network_id(&state)]
            [&old.record.space_id],
        old
    );
    let stored: OwnSpacePresence = state
        .local_db
        .load_domain("own_space_presence")
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.networks[&test_network_id(&state)][&old.record.space_id],
        old
    );
}

#[tokio::test]
async fn space_map_upgrade_real_sqlite_respawn_failure_keeps_original_signature_and_sequence() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("space-map-upgrade-write-failure.db");
    let state = disk_state(state, &path);
    let old = initialize_space_presence(&state).await.unwrap();
    let package = Arc::new(upgraded_package(
        current_package(&state),
        old.record.from_tile,
    ));
    let registry = OwnSpacePresence::load(
        &state.local_db,
        Some(&test_space_catalog(&package)),
        &state.agent_did,
    )
    .unwrap();
    let state = ControlPlaneState {
        space_catalog: Some(Arc::new(test_space_catalog(&package))),
        own_space_presence: Arc::new(Mutex::new(registry)),
        ..state
    };
    let table = "own_space_presence";
    rusqlite::Connection::open(&path).unwrap().execute_batch(&format!("CREATE TRIGGER reject_upgrade_write BEFORE UPDATE ON {table} BEGIN SELECT RAISE(ABORT,'upgrade write rejected'); END;")).unwrap();
    let mut events = state.stream_tx.subscribe();
    assert!(
        initialize_space_presence(&state)
            .await
            .unwrap_err()
            .to_string()
            .contains("upsert domain table")
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(
        state.own_space_presence.lock().await.networks[&test_network_id(&state)]
            [&old.record.space_id],
        old
    );
    let stored: OwnSpacePresence = state
        .local_db
        .load_domain("own_space_presence")
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.networks[&test_network_id(&state)][&old.record.space_id],
        old
    );
}

#[tokio::test]
async fn space_map_upgrade_rejects_invalid_old_signatures_and_current_geometry_or_corruption() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let state = disk_state(
        state,
        &dir.path().join("space-map-upgrade-invalid-record.db"),
    );
    let old = initialize_space_presence(&state).await.unwrap();
    let package = upgraded_package(current_package(&state), old.record.from_tile);
    let mut registry = state.own_space_presence.lock().await.clone();
    let mut tampered = old.clone();
    tampered.record.activity = "tampered".to_owned();
    registry
        .networks
        .entry(test_network_id(&state))
        .or_default()
        .insert(old.record.space_id.clone(), tampered);
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    assert_eq!(
        OwnSpacePresence::load(
            &state.local_db,
            Some(&test_space_catalog(&package)),
            &state.agent_did
        )
        .unwrap_err()
        .to_string(),
        "invalid presence signature"
    );
    let mut bad_geometry = old.clone();
    bad_geometry
        .record
        .space_version
        .clone_from(&package.space_version);
    bad_geometry.signature = state.sign_payload(&bad_geometry.record).unwrap();
    registry
        .networks
        .entry(test_network_id(&state))
        .or_default()
        .insert(old.record.space_id.clone(), bad_geometry);
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    assert_eq!(
        OwnSpacePresence::load(
            &state.local_db,
            Some(&test_space_catalog(&package)),
            &state.agent_did
        )
        .unwrap_err()
        .to_string(),
        "presence starts on blocked or out-of-bounds tile"
    );
    state
        .local_db
        .save_domain("own_space_presence", &json!({"spaces":42}))
        .unwrap();
    assert!(
        OwnSpacePresence::load(
            &state.local_db,
            Some(&test_space_catalog(&package)),
            &state.agent_did
        )
        .is_err()
    );
}

#[tokio::test]
async fn space_map_upgrade_gateway_read_filters_verified_old_records_but_rejects_old_tampering() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let identity = Identity::new_random();
    let old = signed_spawn(&state, &identity, current_package(&state), 9);
    let package = Arc::new(upgraded_package(
        current_package(&state),
        old.record.from_tile,
    ));
    let current = signed_spawn(&state, &identity, &package, 10);
    let records = Arc::new(Mutex::new(vec![old, current.clone()]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let shared = records.clone();
    let router = Router::new()
        .route(
            "/api/spaces/presence",
            get(move || {
                let shared = shared.clone();
                async move { Json(json!({"presences":shared.lock().await.clone()})) }
            }),
        )
        .route(
            "/api/spaces/events",
            get(|| async { Json(json!({"events":[]})) }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let state = ControlPlaneState {
        space_catalog: Some(Arc::new(test_space_catalog(&package))),
        gateway_urls: vec![format!("http://{address}")],
        ..state
    };
    let url = format!(
        "/v1/wattetheria/space/presence?agent_id={}",
        identity.agent_did
    );
    let queried = authed_get_json(app(state.clone()), &token, &url).await;
    assert_eq!(queried["presence"], serde_json::to_value(&current).unwrap());
    let place = authed_get_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/place?place_id=the-ville",
    )
    .await;
    assert_eq!(place["presences"], json!([current]));
    records.lock().await[0].record.activity = "unsigned tampering".to_owned();
    let queried = authed_get_json(app(state.clone()), &token, &url).await;
    assert!(queried.get("error").is_none());
    assert_eq!(queried["presence"], serde_json::to_value(&current).unwrap());
    let place = authed_get_json(
        app(state),
        &token,
        "/v1/wattetheria/space/place?place_id=the-ville",
    )
    .await;
    assert!(place.get("error").is_none());
    assert_eq!(place["presences"], json!([current]));
    server.abort();
}
