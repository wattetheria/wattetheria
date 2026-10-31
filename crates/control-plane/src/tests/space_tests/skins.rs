use super::*;
use wattetheria_gateway_contract::space::SPACE_AVATAR_COUNT;

fn reopen(state: &mut ControlPlaneState, path: &std::path::Path) {
    state.local_db = Arc::new(LocalDb::open(path).unwrap());
    state.own_space_presence = Arc::new(Mutex::new(
        OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did,
        )
        .unwrap(),
    ));
}

#[tokio::test]
async fn skin_assignment_survives_sqlite_reopen_movement_leave_and_portal() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let path = dir.path().join("appearance.db");
    let mut state = disk_state(state, &path);
    let (_, target) = reentry_last::two_spaces(&mut state);
    let entered = initialize_space_presence(&state).await.unwrap();
    let avatar = entered.record.avatar_id.unwrap();
    assert!(avatar < SPACE_AVATAR_COUNT);
    assert_eq!(initialize_space_presence(&state).await.unwrap(), entered);
    let network = test_network_id(&state);
    let stored: Value = state
        .local_db
        .load_domain("own_space_presence")
        .unwrap()
        .unwrap();
    assert_eq!(
        stored["networks"][&network]["the-ville"]["record"]["avatar_id"],
        avatar
    );
    reopen(&mut state, &path);
    let restored = initialize_space_presence(&state).await.unwrap();
    assert_eq!(restored.record.avatar_id, Some(avatar));
    assert_eq!(restored.record.from_tile, entered.record.from_tile);
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"tile":restored.record.from_tile,"activity":"waiting at portal"}),
    )
    .await;
    assert_eq!(moved["ok"], true);
    assert_eq!(moved["presence"]["record"]["avatar_id"], avatar);
    let crossed = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({"space_id":target.space_id,"portal_id":"next"}),
    )
    .await;
    assert_eq!(crossed["ok"], true);
    assert_eq!(crossed["presence"]["record"]["avatar_id"], avatar);
    let left = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/leave",
        json!({}),
    )
    .await;
    assert_eq!(left["ok"], true);
    reopen(&mut state, &path);
    let reentered = initialize_space_presence(&state).await.unwrap();
    assert_eq!(reentered.record.avatar_id, Some(avatar));
    assert_eq!(reentered.record.space_id, target.space_id);
    assert!(
        verify_payload(
            &reentered.record,
            &reentered.signature,
            &reentered.public_key
        )
        .unwrap()
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
async fn skinless_signed_record_is_preserved_until_explicit_entry_then_upgraded_once() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("upgrade-appearance.db");
    let mut state = disk_state(state, &path);
    let mut legacy = initialize_space_presence(&state).await.unwrap();
    legacy.record.avatar_id = None;
    legacy.record.activity = "existing activity".to_owned();
    legacy.record.entered = false;
    legacy.signature = state.sign_payload(&legacy.record).unwrap();
    let network = test_network_id(&state);
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([(
            network.clone(),
            BTreeMap::from([("the-ville".to_owned(), legacy.clone())]),
        )]),
        active_space: None,
    };
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    let before: Value = state
        .local_db
        .load_domain("own_space_presence")
        .unwrap()
        .unwrap();
    assert!(
        before["networks"][&network]["the-ville"]["record"]
            .get("avatar_id")
            .is_none()
    );
    reopen(&mut state, &path);
    assert_eq!(
        state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap(),
        Some(before)
    );
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .stored(&network, "the-ville"),
        Some(&legacy)
    );
    let upgraded = initialize_space_presence(&state).await.unwrap();
    let avatar = upgraded.record.avatar_id.unwrap();
    assert!(avatar < SPACE_AVATAR_COUNT);
    let mut without_assignment = upgraded.record.clone();
    without_assignment.avatar_id = None;
    without_assignment.seq = legacy.record.seq;
    without_assignment.updated_at = legacy.record.updated_at;
    without_assignment.entered = false;
    assert_eq!(without_assignment, legacy.record);
    assert_eq!(initialize_space_presence(&state).await.unwrap(), upgraded);
    reopen(&mut state, &path);
    assert_eq!(
        initialize_space_presence(&state)
            .await
            .unwrap()
            .record
            .avatar_id,
        Some(avatar)
    );
}

#[tokio::test]
async fn failed_skin_assignment_does_not_change_saved_record_or_emit_presence() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let state = disk_state(state, &dir.path().join("failed-appearance.db"));
    let conn = rusqlite::Connection::open(dir.path().join("failed-appearance.db")).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_appearance BEFORE INSERT ON own_space_presence BEGIN SELECT RAISE(FAIL, 'disk write rejected'); END;").unwrap();
    let mut events = state.stream_tx.subscribe();
    assert!(initialize_space_presence(&state).await.is_err());
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
async fn skin_assignment_survives_network_change_and_signed_gateway_recovery() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("network-appearance.db");
    let mut state = disk_state(state, &path);
    let mut foreign = initialize_space_presence(&state).await.unwrap();
    foreign.record.network_id = "another-network".to_owned();
    foreign.record.avatar_id = Some(24);
    foreign.signature = state.sign_payload(&foreign.record).unwrap();
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([(
            "another-network".to_owned(),
            BTreeMap::from([(foreign.record.space_id.clone(), foreign.clone())]),
        )]),
        active_space: None,
    };
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    reopen(&mut state, &path);
    let entered = initialize_space_presence(&state).await.unwrap();
    assert_eq!(entered.record.avatar_id, Some(24));
    assert_eq!(entered.record.network_id, test_network_id(&state));
    assert_eq!(
        state
            .own_space_presence
            .lock()
            .await
            .stored("another-network", &foreign.record.space_id),
        Some(&foreign)
    );
    assert_eq!(
        crate::routes::space::snapshot::export(&state)
            .await
            .unwrap()
            .space_presences,
        vec![entered.clone()]
    );
    let mut tampered = entered.record.clone();
    tampered.avatar_id = Some(0);
    assert!(!verify_payload(&tampered, &entered.signature, &entered.public_key).unwrap());
    let (url, server) = sequence_recovery::floor_server(entered).await;
    let mut recovered = disk_state(state, &dir.path().join("recovered-appearance.db"));
    recovered.gateway_urls = vec![url];
    recovered.own_space_presence = Arc::new(Mutex::new(OwnSpacePresence::default()));
    let restored = initialize_space_presence(&recovered).await.unwrap();
    assert_eq!(restored.record.avatar_id, Some(24));
    assert!(verify_payload(&restored.record, &restored.signature, &restored.public_key).unwrap());
    server.abort();
}

#[tokio::test]
async fn concurrent_first_entries_share_one_persisted_skin() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let state = disk_state(state, &dir.path().join("concurrent-appearance.db"));
    let (first, second, third) = tokio::join!(
        initialize_space_presence(&state),
        initialize_space_presence(&state),
        initialize_space_presence(&state)
    );
    let first = first.unwrap();
    assert!(first.record.avatar_id.unwrap() < SPACE_AVATAR_COUNT);
    assert_eq!(second.unwrap(), first);
    assert_eq!(third.unwrap(), first);
    let saved = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        saved.stored(&test_network_id(&state), "the-ville"),
        Some(&first)
    );
}

#[tokio::test]
async fn reentry_wears_the_most_recent_choice_across_spaces() {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let mut state = disk_state(state, &dir.path().join("latest-skin.db"));
    let (default, other) = super::reentry_last::two_spaces(&mut state);
    let network = test_network_id(&state);
    let custom = "c".repeat(64);
    // An older choice in the default Space (which sorts first) and a newer one, with a custom
    // skin, made later in the other Space.
    let mut older = wattetheria_gateway_contract::SpacePresenceRecord {
        agent_did: state.agent_did.clone(),
        avatar_id: Some(3),
        skin_id: None,
        network_id: network.clone(),
        space_id: default.space_id.clone(),
        space_version: default.space_version.clone(),
        entered: false,
        seq: 5,
        from_tile: default
            .destination_tile(&state.agent_did, &default.spawn_place_id)
            .unwrap(),
        path: String::new(),
        depart_at: 1_000,
        destination_place_id: default.spawn_place_id.clone(),
        activity: String::new(),
        object_place_id: None,
        updated_at: 1_000,
    };
    let mut newer = older.clone();
    newer.space_id.clone_from(&other.space_id);
    newer.space_version.clone_from(&other.space_version);
    newer.from_tile = other
        .destination_tile(&state.agent_did, &other.spawn_place_id)
        .unwrap();
    newer.destination_place_id.clone_from(&other.spawn_place_id);
    newer.avatar_id = Some(9);
    newer.skin_id = Some(custom.clone());
    newer.seq = 6;
    newer.depart_at = 2_000;
    newer.updated_at = 2_000;
    older.seq = 7;
    let records: Vec<SignedSpacePresence> = [older, newer]
        .into_iter()
        .map(|record| SignedSpacePresence {
            signature: state.sign_payload(&record).unwrap(),
            public_key: state.identity.public_key.clone(),
            record,
        })
        .collect();
    super::reentry_last::save_inactive_registry(&mut state, &records);
    let entered = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({}),
    )
    .await;
    assert_eq!(entered["ok"], true, "{entered}");
    assert_eq!(entered["presence"]["record"]["avatar_id"], 9);
    assert_eq!(entered["presence"]["record"]["skin_id"], custom);
}
