use super::*;
use crate::routes::space::{OwnSpacePresence, initialize_space_presence};
use wattetheria_gateway_contract::SignedSpacePresence;
use wattetheria_kernel::local_db::LocalDb;

mod background_boundaries;
mod leave;
mod map_upgrade;
mod namespace_portals;
mod query_resilience;
mod reentry_last;
mod reset_conflicts;
mod restart_network;
mod rolling_mapless;
mod sequence_recovery;

fn current_package(state: &ControlPlaneState) -> &wattetheria_kernel::space::SpacePackage {
    state
        .space_catalog
        .as_deref()
        .unwrap()
        .current("the-ville")
        .unwrap()
}

#[tokio::test]
async fn space_requires_explicit_entry_and_auth_without_implicit_writes_or_events() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let mut events = state.stream_tx.subscribe();
    crate::reset_space_presence(&state).await.unwrap();
    crate::reconcile_space_presence(&state).await.unwrap();
    crate::emit_current_space_presence(&state).await.unwrap();
    let response = app(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri("/v1/wattetheria/space/presence")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    for (path, body) in [
        (
            "/v1/wattetheria/space/move",
            json!({"place_id":"johnson-park"}),
        ),
        (
            "/v1/wattetheria/space/activity",
            json!({"activity":"waiting"}),
        ),
    ] {
        let result = authed_post_json(app(state.clone()), &token, path, body).await;
        assert_eq!(
            result["error"],
            "agent has not entered the space; call enter_space first"
        );
    }
    let response = app(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/space/enter")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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
        crate::routes::space::snapshot::export(&state)
            .await
            .unwrap()
            .space_presences,
        []
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let entered = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({}),
    )
    .await;
    assert_eq!(entered["ok"], true);
    assert_eq!(entered["presence"]["record"]["entered"], true);
    assert_eq!(entered["presence"]["record"]["seq"], 1);
    assert_eq!(events.recv().await.unwrap().payload, entered["presence"]);
    let repeated = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({}),
    )
    .await;
    assert_eq!(repeated, entered);
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn space_entry_rejects_forged_credential_with_recomputed_local_hash() {
    use wattetheria_kernel::network_agent_registration::{
        MembershipCredential, membership_credential_hash,
    };
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let mut stored = state
        .local_db
        .load_network_agent_credential(&state.agent_did, None)
        .unwrap()
        .unwrap();
    let mut credential: MembershipCredential =
        serde_json::from_str(&stored.credential_json).unwrap();
    credential.unsigned.issuer_authority_id = "forged-authority".to_owned();
    stored.credential_hash = membership_credential_hash(&credential);
    stored.credential_json = serde_json::to_string(&credential).unwrap();
    state
        .local_db
        .upsert_network_agent_credential(&stored)
        .unwrap();
    assert!(
        wattetheria_kernel::network_agent_registration::network_permission_is_active(
            &state.local_db,
            &state.agent_did
        )
        .unwrap()
    );
    let mut events = state.stream_tx.subscribe();
    let rejected = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({}),
    )
    .await;
    assert!(rejected["error"].as_str().unwrap().contains("issuer proof"));
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
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn space_concurrent_fresh_explicit_entry_persists_and_emits_exactly_once() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let state = disk_state(state, &dir.path().join("concurrent-entry.db"));
    let mut events = state.stream_tx.subscribe();
    let barrier = Arc::new(tokio::sync::Barrier::new(21));
    let mut entries = Vec::new();
    for _ in 0..20 {
        let state = state.clone();
        let token = token.clone();
        let barrier = barrier.clone();
        entries.push(tokio::spawn(async move {
            barrier.wait().await;
            authed_post_json(app(state), &token, "/v1/wattetheria/space/enter", json!({})).await
        }));
    }
    barrier.wait().await;
    let mut first = None;
    for entry in entries {
        let entered = entry.await.unwrap();
        assert_eq!(entered["ok"], true);
        assert_eq!(entered["presence"]["record"]["seq"], 1);
        assert_eq!(entered["presence"]["record"]["entered"], true);
        if let Some(first) = &first {
            assert_eq!(&entered, first);
        } else {
            first = Some(entered);
        }
    }
    let first = first.unwrap();
    assert_eq!(events.recv().await.unwrap().payload, first["presence"]);
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let persisted = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert!(persisted.active_space.is_none());
    assert_eq!(
        json!(
            persisted
                .get(&test_network_id(&state), current_package(&state))
                .unwrap()
        ),
        first["presence"]
    );
}

#[tokio::test]
async fn space_restart_inactive_and_entry_sqlite_failures_preserve_record_and_runtime_gate() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("entry-failure.db");
    let state = disk_state(state, &path);
    let saved = initialize_space_presence(&state).await.unwrap();
    let registry = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    let state = ControlPlaneState {
        own_space_presence: Arc::new(Mutex::new(registry)),
        ..state
    };
    let before = state
        .local_db
        .load_domain::<Value>("own_space_presence")
        .unwrap();
    let table = "own_space_presence";
    rusqlite::Connection::open(&path).unwrap().execute_batch(&format!("CREATE TRIGGER reject_entry BEFORE UPDATE ON {table} BEGIN SELECT RAISE(ABORT,'entry write rejected'); END;")).unwrap();
    let mut events = state.stream_tx.subscribe();
    assert!(crate::reset_space_presence(&state).await.is_err());
    assert!(initialize_space_presence(&state).await.is_err());
    let registry = state.own_space_presence.lock().await;
    assert!(registry.active_space.is_none());
    assert_eq!(
        registry.get(&test_network_id(&state), current_package(&state)),
        Some(&saved)
    );
    assert!(
        registry
            .visible_get(&test_network_id(&state), current_package(&state))
            .is_none()
    );
    drop(registry);
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        before
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

fn disk_state(state: ControlPlaneState, path: &std::path::Path) -> ControlPlaneState {
    let db = Arc::new(LocalDb::open(path).unwrap());
    let checkpoint = state
        .local_db
        .load_network_permission_checkpoint(&state.agent_did, None, None)
        .unwrap()
        .unwrap();
    let credential = state
        .local_db
        .load_network_agent_credential(&state.agent_did, None)
        .unwrap()
        .unwrap();
    db.upsert_network_agent_credential(&credential).unwrap();
    db.upsert_network_permission_checkpoint(&checkpoint)
        .unwrap();
    db.save_domain("unrelated-existing-state", &json!({"retained":true}))
        .unwrap();
    ControlPlaneState {
        local_db: db,
        ..state
    }
}

async fn store_legacy_moving_presence(
    state: &ControlPlaneState,
    token: &str,
) -> (SignedSpacePresence, SignedSpacePresence) {
    initialize_space_presence(state).await.unwrap();
    let moved = authed_post_json(
        app(state.clone()),
        token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"hobbs-cafe/cafe","activity":"walking"}),
    )
    .await;
    let signed: SignedSpacePresence = serde_json::from_value(moved["presence"].clone()).unwrap();
    assert_eq!(signed.record.seq, 2);
    assert_ne!(signed.record.path, "");
    let mut archive = signed.clone();
    archive.record.space_id = "archived-space".to_owned();
    archive.record.space_version = "obsolete-map".to_owned();
    archive.record.seq = 37;
    archive.signature = state.sign_payload(&archive.record).unwrap();
    let legacy = json!({"spaces":{
        (signed.record.space_id.clone()):{(state.agent_did.clone()):signed},
        (archive.record.space_id.clone()):{(state.agent_did.clone()):archive}
    }});
    state
        .local_db
        .save_domain("own_space_presence", &legacy)
        .unwrap();
    (signed, archive)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn space_v10_table_migration_restores_real_signed_record_without_rewriting_or_entering() {
    for (version, legacy_table) in [
        (
            10,
            format!("domain_{}", hex::encode("space_presence_registry")),
        ),
        (11, "space_presence_registry".to_owned()),
    ] {
        let (dir, _app, token, _policy, state) = build_test_app(100);
        let path = dir.path().join("space-table-upgrade.db");
        let state = disk_state(state, &path);
        let (signed, archive) = store_legacy_moving_presence(&state, &token).await;
        let conn = rusqlite::Connection::open(&path).unwrap();
        let read_row = |table: &str| -> (String, String) {
            conn.query_row(
                &format!("SELECT payload, updated_at FROM {table} WHERE id=1"),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        let original = read_row("own_space_presence");
        conn.execute_batch(&format!(
            "ALTER TABLE own_space_presence RENAME TO {legacy_table};
             UPDATE schema_version SET version={version};"
        ))
        .unwrap();
        let mut events = state.stream_tx.subscribe();
        let db = Arc::new(LocalDb::open(&path).unwrap());
        assert_eq!(read_row("own_space_presence"), original);
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [&legacy_table],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        let presence =
            OwnSpacePresence::load(&db, state.space_catalog.as_deref(), &state.agent_did).unwrap();
        assert!(presence.active_space.is_none());
        assert_eq!(
            presence.get(&test_network_id(&state), current_package(&state)),
            Some(&signed)
        );
        assert_eq!(
            presence.networks[&test_network_id(&state)][&archive.record.space_id],
            archive
        );
        assert_eq!(
            presence.visible_get(&test_network_id(&state), current_package(&state)),
            None
        );
        let upgraded = read_row("own_space_presence");
        assert_eq!(
            serde_json::from_str::<Value>(&upgraded.0).unwrap(),
            serde_json::to_value(&presence).unwrap()
        );
        assert_ne!(upgraded.0, original.0);
        assert_eq!(
            signed.record.updated_at,
            presence
                .get(&test_network_id(&state), current_package(&state))
                .unwrap()
                .record
                .updated_at
        );
        assert_eq!(
            db.load_domain::<Value>("unrelated-existing-state").unwrap(),
            Some(json!({"retained":true}))
        );
        let reopened = LocalDb::open(&path).unwrap();
        let reloaded =
            OwnSpacePresence::load(&reopened, state.space_catalog.as_deref(), &state.agent_did)
                .unwrap();
        assert_eq!(reloaded.networks, presence.networks);
        assert_eq!(read_row("own_space_presence"), upgraded);
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let state = ControlPlaneState {
            local_db: db,
            own_space_presence: Arc::new(Mutex::new(presence)),
            ..state
        };
        assert_restart_inactive_and_reentry_preserve_record(&state, &token, &signed).await;
        let inactive: SignedSpacePresence =
            serde_json::from_value(events.recv().await.unwrap().payload).unwrap();
        assert!(!inactive.record.entered);
        assert_eq!(inactive.record.seq, archive.record.seq + 1);
        let mut expected_archive = archive.record.clone();
        expected_archive.entered = false;
        expected_archive.seq += 1;
        expected_archive.updated_at = inactive.record.updated_at;
        assert_eq!(inactive.record, expected_archive);
        assert!(
            verify_payload(&inactive.record, &inactive.signature, &inactive.public_key).unwrap()
        );
        let entered: SignedSpacePresence =
            serde_json::from_value(events.recv().await.unwrap().payload).unwrap();
        reentry_last::assert_default_spawn(&state, &entered, inactive.record.seq + 1);
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let persisted = OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did,
        )
        .unwrap();
        assert_eq!(
            persisted.stored(&test_network_id(&state), &signed.record.space_id),
            Some(&entered)
        );
        assert_eq!(
            persisted.networks[&test_network_id(&state)][&archive.record.space_id],
            inactive
        );
    }
}

async fn assert_restart_inactive_and_reentry_preserve_record(
    restarted: &ControlPlaneState,
    token: &str,
    latest: &SignedSpacePresence,
) {
    let missing = authed_get_json(
        app(restarted.clone()),
        token,
        "/v1/wattetheria/space/presence",
    )
    .await;
    assert_eq!(missing["error"], "agent presence not found");
    let network = test_network_id(restarted);
    let before = restarted.own_space_presence.lock().await.networks[&network].clone();
    let floor = before
        .values()
        .map(|signed| signed.record.seq)
        .max()
        .unwrap();
    crate::reset_space_presence(restarted).await.unwrap();
    let after = restarted.own_space_presence.lock().await.networks[&network].clone();
    let mut next_seq = floor;
    for (space, original) in &before {
        let actual = &after[space];
        if original.record.seq == floor && original.record.entered {
            next_seq += 1;
            let mut expected = original.record.clone();
            expected.entered = false;
            expected.seq = next_seq;
            expected.updated_at = actual.record.updated_at;
            assert_eq!(actual.record, expected);
            assert!(verify_payload(&actual.record, &actual.signature, &actual.public_key).unwrap());
        } else {
            assert_eq!(actual, original);
        }
    }
    let inactive = &after[&latest.record.space_id];
    let mut expected = latest.record.clone();
    expected.entered = latest.record.entered && latest.record.seq != floor;
    expected.seq = inactive.record.seq;
    expected.updated_at = inactive.record.updated_at;
    assert_eq!(inactive.record, expected);
    assert!(
        restarted
            .own_space_presence
            .lock()
            .await
            .active_space
            .is_none()
    );
    assert_eq!(
        crate::routes::space::snapshot::export(restarted)
            .await
            .unwrap()
            .space_presences,
        after
            .values()
            .filter(|signed| !signed.record.entered)
            .cloned()
            .collect::<Vec<_>>()
    );
    let restored = initialize_space_presence(restarted).await.unwrap();
    if latest.record.seq == floor {
        expected.entered = true;
        expected.seq = next_seq + 1;
        expected.updated_at = restored.record.updated_at;
        assert_eq!(restored.record, expected);
    } else {
        reentry_last::assert_default_spawn(restarted, &restored, next_seq + 1);
        for (space, original) in &after {
            if space != &restored.record.space_id {
                assert_eq!(
                    restarted
                        .own_space_presence
                        .lock()
                        .await
                        .stored(&network, space),
                    Some(original)
                );
            }
        }
    }
    assert_eq!(
        initialize_space_presence(restarted).await.unwrap(),
        restored
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn space_spawn_move_activity_and_restart_preserve_signed_latest_and_existing_data() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let path = dir.path().join("space-upgrade.db");
    let state = disk_state(state, &path);
    let spawn = initialize_space_presence(&state).await.unwrap();
    assert_eq!(spawn.record.seq, 1);
    assert_eq!(
        spawn.record.destination_place_id,
        current_package(&state).spawn_place_id
    );
    assert_eq!(spawn.record.path, "");
    let mut events = state.stream_tx.subscribe();
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"hobbs-cafe/cafe","activity":"coffee"}),
    )
    .await;
    assert_eq!(moved["ok"], true);
    let signed: SignedSpacePresence = serde_json::from_value(moved["presence"].clone()).unwrap();
    assert_eq!(signed.record.seq, 2);
    assert_ne!(signed.record.path, "");
    current_package(&state)
        .validate_record(&signed.record)
        .unwrap();
    assert!(
        wattetheria_kernel::signing::verify_payload(
            &signed.record,
            &signed.signature,
            &signed.public_key
        )
        .unwrap()
    );
    let event = events.recv().await.unwrap();
    assert_eq!(event.kind, "space.presence.updated");
    let gateway = crate::build_signed_node_event(&state, &event)
        .unwrap()
        .unwrap();
    assert_eq!(
        gateway.payload.data_kind,
        wattetheria_gateway_contract::DataKind::SpacePresence
    );
    assert_eq!(gateway.payload.payload, moved["presence"]);
    assert_eq!(gateway.payload.timestamp, signed.record.updated_at / 1_000);
    if let Ok(path) = std::env::var("WATTETHERIA_SPACE_PRODUCER_FIXTURE_OUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&gateway).unwrap()).unwrap();
    }
    let changed = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/activity",
        json!({"activity":"reading"}),
    )
    .await;
    assert_eq!(changed["ok"], true);
    let latest: SignedSpacePresence = serde_json::from_value(changed["presence"].clone()).unwrap();
    assert_eq!(latest.record.seq, 3);
    assert_eq!(latest.record.activity, "reading");
    assert_eq!(latest.record.path, "");
    let reopened = Arc::new(LocalDb::open(&path).unwrap());
    let registry =
        OwnSpacePresence::load(&reopened, state.space_catalog.as_deref(), &state.agent_did)
            .unwrap();
    assert!(registry.active_space.is_none());
    assert!(
        registry
            .visible_get(&test_network_id(&state), current_package(&state))
            .is_none()
    );
    assert_eq!(
        registry.get(&test_network_id(&state), current_package(&state)),
        Some(&latest)
    );
    let restarted = ControlPlaneState {
        local_db: reopened,
        own_space_presence: Arc::new(Mutex::new(registry)),
        ..state
    };
    assert_restart_inactive_and_reentry_preserve_record(&restarted, &token, &latest).await;
    let changed_again = authed_post_json(
        app(restarted.clone()),
        &token,
        "/v1/wattetheria/space/activity",
        json!({"activity":"after restart"}),
    )
    .await;
    assert_eq!(changed_again["presence"]["record"]["seq"], 6);
    assert_eq!(
        restarted
            .local_db
            .load_domain::<Value>("unrelated-existing-state")
            .unwrap(),
        Some(json!({"retained":true}))
    );
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        12
    );
}

#[tokio::test]
async fn space_gateway_presence_query_and_target_movement_use_verified_current_tile() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    let remote = Identity::new_random();
    let tile = current_package(&state)
        .destination_tile(&remote.agent_did, "johnson-park")
        .unwrap();
    let record = wattetheria_gateway_contract::SpacePresenceRecord {
        agent_did: remote.agent_did.clone(),
        network_id: test_network_id(&state),
        space_id: current_package(&state).space_id.clone(),
        space_version: current_package(&state).space_version.clone(),
        seq: 9,
        entered: true,
        from_tile: tile,
        path: String::new(),
        depart_at: Utc::now().timestamp_millis(),
        destination_place_id: "johnson-park".to_owned(),
        activity: "waiting".to_owned(),
        object_place_id: None,
        updated_at: Utc::now().timestamp_millis(),
    };
    let signed = SignedSpacePresence {
        signature: remote.sign(&canonical_bytes(&record).unwrap()).unwrap(),
        public_key: remote.public_key.clone(),
        record,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let response = json!({"space_id":"the-ville","presences":[signed]});
    let router = Router::new().route(
        "/api/spaces/presence",
        get(move || {
            let response = response.clone();
            async move { Json(response) }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    state.gateway_urls = vec![format!("http://{address}")];
    let queried = authed_get_json(
        app(state.clone()),
        &token,
        &format!(
            "/v1/wattetheria/space/presence?agent_id={}",
            remote.agent_did
        ),
    )
    .await;
    assert_eq!(queried["presence"], serde_json::to_value(&signed).unwrap());
    assert_eq!(
        queried["position"],
        json!(signed.record.position_at(
            current_package(&state).walk_speed,
            queried["server_time_ms"].as_i64().unwrap()
        ))
    );
    initialize_space_presence(&state).await.unwrap();
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"target_agent_id":remote.agent_did,"activity":"meeting"}),
    )
    .await;
    assert_eq!(moved["ok"], true);
    let moved: SignedSpacePresence = serde_json::from_value(moved["presence"].clone()).unwrap();
    current_package(&state)
        .validate_record(&moved.record)
        .unwrap();
    assert_eq!(
        moved
            .record
            .tile_at(current_package(&state).walk_speed, i64::MAX),
        tile
    );
    assert_eq!(moved.record.activity, "meeting");
    assert_ne!(moved.record.agent_did, remote.agent_did);
    server.abort();
}

#[tokio::test]
async fn space_snapshot_uses_real_persisted_heartbeat_without_refreshing_it() {
    let (_dir, _app, _token, _policy, state) = build_test_app(100);
    assert_eq!(
        crate::routes::space::presence::node_presence(&state).unwrap(),
        None
    );
    let mut manager = wattetheria_kernel::online_proof::OnlineProofManager::default();
    manager.create_lease(&state.agent_did, 300, 20);
    let lease = manager.heartbeat(&state.agent_did).unwrap();
    state
        .local_db
        .save_domain(wattetheria_kernel::local_db::domain::ONLINE_PROOF, &manager)
        .unwrap();
    let first = crate::routes::space::snapshot::export(&state)
        .await
        .unwrap();
    let second = crate::routes::space::snapshot::export(&state)
        .await
        .unwrap();
    let proof = first.node_presence.unwrap();
    assert_eq!(proof.last_heartbeat_ms, lease.last_heartbeat * 1_000);
    assert_eq!(proof.heartbeat_interval_sec, 20);
    assert_eq!(proof.node_id, state.agent_did);
    assert_eq!(second.node_presence, Some(proof));
    assert_eq!(
        manager.get_proof(&state.agent_did).unwrap()["lease_expiry"],
        lease.lease_expiry
    );
}

#[tokio::test]
async fn space_empty_snapshot_fields_preserve_existing_signed_serialization() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let public_id = bootstrap_broker_identity(app(state.clone()), &token, &state.agent_did).await;
    let snapshot = crate::routes::client_api::build_signed_public_client_snapshot(
        &state,
        &crate::ClientExportQuery {
            public_id: Some(public_id),
            ..crate::ClientExportQuery::default()
        },
    )
    .await
    .unwrap();
    let value = serde_json::to_value(&snapshot.payload).unwrap();
    for field in ["space_presences", "world_events", "node_presence"] {
        assert!(value.get(field).is_none(), "{field}");
    }
    let decoded: crate::routes::client_api::PublicClientSnapshot =
        serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
    assert!(
        wattetheria_kernel::signing::verify_payload(
            &decoded,
            &snapshot.signature,
            &state.identity.public_key
        )
        .unwrap()
    );
}

#[tokio::test]
async fn space_concurrent_initialization_and_mutation_do_not_reset_existing_presence() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let first = initialize_space_presence(&state).await.unwrap();
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let state = state.clone();
        tasks.push(tokio::spawn(async move {
            initialize_space_presence(&state).await.unwrap()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), first);
    }
    let mut updates = Vec::new();
    for index in 0..12 {
        let state = state.clone();
        let token = token.clone();
        updates.push(tokio::spawn(async move {
            authed_post_json(
                app(state),
                &token,
                "/v1/wattetheria/space/activity",
                json!({"activity":format!("activity-{index}")}),
            )
            .await
        }));
    }
    let mut sequences = Vec::new();
    for task in updates {
        let result = task.await.unwrap();
        assert_eq!(result["ok"], true);
        sequences.push(result["presence"]["record"]["seq"].as_u64().unwrap());
    }
    sequences.sort_unstable();
    assert_eq!(sequences, (2..=13).collect::<Vec<_>>());
    let latest = initialize_space_presence(&state).await.unwrap();
    assert_eq!(latest.record.seq, 13);
    assert!(latest.record.activity.starts_with("activity-"));
    let persisted = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        persisted.get(&test_network_id(&state), current_package(&state)),
        Some(&latest)
    );
}

#[tokio::test]
async fn space_concurrent_first_join_and_movement_preserve_monotonic_persisted_position() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let state = disk_state(state, &dir.path().join("concurrent-first-join.db"));
    initialize_space_presence(&state).await.unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(33));
    let mut initializers = Vec::new();
    let mut movers = Vec::new();
    for _ in 0..20 {
        let state = state.clone();
        let barrier = barrier.clone();
        initializers.push(tokio::spawn(async move {
            barrier.wait().await;
            initialize_space_presence(&state).await.unwrap()
        }));
    }
    for _ in 0..12 {
        let state = state.clone();
        let token = token.clone();
        let barrier = barrier.clone();
        movers.push(tokio::spawn(async move {
            barrier.wait().await;
            authed_post_json(
                app(state),
                &token,
                "/v1/wattetheria/space/move",
                json!({"place_id":"hobbs-cafe/cafe","activity":"coffee"}),
            )
            .await
        }));
    }
    barrier.wait().await;
    for task in initializers {
        let observed = task.await.unwrap();
        current_package(&state)
            .validate_record(&observed.record)
            .unwrap();
        assert!((1..=13).contains(&observed.record.seq));
    }
    let mut sequences = Vec::new();
    for task in movers {
        let moved = task.await.unwrap();
        assert_eq!(moved["ok"], true);
        sequences.push(moved["presence"]["record"]["seq"].as_u64().unwrap());
    }
    sequences.sort_unstable();
    assert_eq!(sequences, (2..=13).collect::<Vec<_>>());
    let latest = initialize_space_presence(&state).await.unwrap();
    assert_eq!(latest.record.seq, 13);
    assert_eq!(latest.record.destination_place_id, "hobbs-cafe/cafe");
    assert_eq!(latest.record.activity, "coffee");
    let persisted = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        persisted.get(&test_network_id(&state), current_package(&state)),
        Some(&latest)
    );
}

#[tokio::test]
async fn space_real_sqlite_write_failure_does_not_replace_state_or_emit() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let path = dir.path().join("space-write-failure.db");
    let state = disk_state(state, &path);
    let before = initialize_space_presence(&state).await.unwrap();
    let table = "own_space_presence";
    rusqlite::Connection::open(&path).unwrap().execute_batch(&format!("CREATE TRIGGER reject_space_write BEFORE UPDATE ON {table} BEGIN SELECT RAISE(ABORT,'space write rejected'); END;")).unwrap();
    let mut events = state.stream_tx.subscribe();
    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/activity",
        json!({"activity":"must not commit"}),
    )
    .await;
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("upsert domain table")
    );
    assert_eq!(initialize_space_presence(&state).await.unwrap(), before);
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let persisted = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        persisted.get(&test_network_id(&state), current_package(&state)),
        Some(&before)
    );
}

#[tokio::test]
async fn space_rejects_unregistered_agent_and_invalid_movement_or_object_without_mutation() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let unregistered = ControlPlaneState {
        local_db: Arc::new(LocalDb::open_in_memory().unwrap()),
        ..state.clone()
    };
    assert!(
        initialize_space_presence(&unregistered)
            .await
            .unwrap_err()
            .to_string()
            .contains("active network agent permission")
    );
    assert_eq!(
        unregistered
            .own_space_presence
            .lock()
            .await
            .get(&test_network_id(&state), current_package(&state)),
        None
    );
    let before = initialize_space_presence(&state).await.unwrap();
    for (url, body) in [
        ("/v1/wattetheria/space/move", json!({"place_id":"missing"})),
        (
            "/v1/wattetheria/space/move",
            json!({"place_id":"johnson-park","target_agent_id":"someone"}),
        ),
        (
            "/v1/wattetheria/space/activity",
            json!({"activity":"piano","object_place_id":"hobbs-cafe/cafe/piano"}),
        ),
    ] {
        let result = authed_post_json(app(state.clone()), &token, url, body).await;
        assert!(result["error"].is_string());
        assert_eq!(initialize_space_presence(&state).await.unwrap(), before);
    }
    let own = authed_get_json(app(state.clone()), &token, "/v1/wattetheria/space/presence").await;
    assert_eq!(
        own["position"],
        json!(before.record.position_at(
            current_package(&state).walk_speed,
            own["server_time_ms"].as_i64().unwrap()
        ))
    );
    let places = authed_get_json(
        app(state),
        &token,
        "/v1/wattetheria/space/places?kind=sector",
    )
    .await;
    assert_eq!(places["places"].as_array().unwrap().len(), 19);
    assert!(
        places["places"]
            .as_array()
            .unwrap()
            .iter()
            .all(|place| place["service_bindings"] == json!([]))
    );
}
