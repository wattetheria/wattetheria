use super::*;

async fn invalid_activity_fixture(
    activity: &str,
    catalog_present: bool,
    entered: bool,
    active: bool,
) -> (
    tempfile::TempDir,
    ControlPlaneState,
    SignedSpacePresence,
    String,
) {
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let path = dir.path().join("invalid-activity-background.db");
    let mut state = disk_state(state, &path);
    let public_id = bootstrap_broker_identity(app(state.clone()), &token, &state.agent_did).await;
    initialize_space_presence(&state).await.unwrap();
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"hobbs-cafe/cafe","activity":"coffee"}),
    )
    .await;
    let mut signed: SignedSpacePresence =
        serde_json::from_value(moved["presence"].clone()).unwrap();
    assert_ne!(signed.record.path, "");
    signed.record.entered = entered;
    let original =
        super::sequence_recovery::store_legacy_activity(&mut state, signed, activity.to_owned());
    let checkpoint = state
        .local_db
        .load_network_permission_checkpoint(
            &state.agent_did,
            Some(&original.record.network_id),
            None,
        )
        .unwrap()
        .unwrap();
    if !active {
        update_network_permission_checkpoint(
            &state.local_db,
            &checkpoint.network_id,
            &checkpoint.node_id,
            &state.agent_did,
            "pending",
            None,
            checkpoint.updated_at_ms + 1,
        )
        .unwrap();
    }
    if !catalog_present {
        state.space_catalog = None;
    }
    (dir, state, original, public_id)
}

async fn seed_other_network_permission_fixture(
    state: &ControlPlaneState,
    current_network: &str,
    current_status: &str,
    other_status: &str,
) -> String {
    let current = state
        .local_db
        .load_network_permission_checkpoint(&state.agent_did, Some(current_network), None)
        .unwrap()
        .unwrap();
    let other_network = MockSwarmBridge::default_for("other-permission-node".to_owned())
        .current_network_id()
        .await
        .unwrap();
    let stored = state
        .local_db
        .load_network_agent_credential(&state.agent_did, Some(&current.network_id))
        .unwrap()
        .unwrap();
    let mut credential: MembershipCredential =
        serde_json::from_str(&stored.credential_json).unwrap();
    credential.unsigned.network_id.clone_from(&other_network);
    credential.unsigned.credential_id = "other-network-credential".to_owned();
    let certificate = credential.unsigned.issuer_key_certificate.as_mut().unwrap();
    certificate.unsigned.network_id.clone_from(&other_network);
    let authority = SigningKey::from_bytes(&[42_u8; 32]);
    certificate.trust_anchor_signature = hex::encode(
        authority
            .sign(&certificate.unsigned.signing_bytes().unwrap())
            .to_bytes(),
    );
    credential.signature_hex = hex::encode(
        authority
            .sign(&credential.signing_bytes().unwrap())
            .to_bytes(),
    );
    let mut anchor: NetworkCredentialTrustAnchor =
        serde_json::from_str(stored.trust_anchor_json.as_ref().unwrap()).unwrap();
    anchor.network_id.clone_from(&other_network);
    let mut request = crate::build_registration_request(state).await.unwrap();
    request.network_id.clone_from(&other_network);
    request.signature_b64 = state
        .signer
        .sign_bytes(&request.signing_bytes().unwrap())
        .unwrap();
    store_membership_credential(
        &state.local_db,
        &request,
        &credential,
        &anchor,
        current.updated_at_ms + 1,
    )
    .unwrap();
    update_network_permission_checkpoint(
        &state.local_db,
        &current.network_id,
        &current.node_id,
        &state.agent_did,
        current_status,
        None,
        current.updated_at_ms + 1,
    )
    .unwrap();
    update_network_permission_checkpoint(
        &state.local_db,
        &other_network,
        "other-permission-node",
        &state.agent_did,
        other_status,
        None,
        current.updated_at_ms + 2,
    )
    .unwrap();
    other_network
}

#[tokio::test]
async fn invalid_legacy_activity_background_never_rewrites_or_projects() {
    for activity in ["\u{754c}".repeat(2001), "old\u{0007}activity".to_owned()] {
        for (catalog_present, entered, active) in [
            (true, true, true),
            (true, true, false),
            (true, false, true),
            (true, false, false),
            (false, true, true),
            (false, true, false),
            (false, false, true),
            (false, false, false),
        ] {
            let (dir, state, original, public_id) =
                invalid_activity_fixture(&activity, catalog_present, entered, active).await;
            let path = dir.path().join("invalid-activity-background.db");
            let before = super::sequence_recovery::registry_bytes(&path);
            rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TRIGGER reject_background_activity_repair BEFORE UPDATE ON own_space_presence BEGIN SELECT RAISE(ABORT,'background activity repair is unauthorized'); END;").unwrap();
            let mut events = state.stream_tx.subscribe();
            let reset = crate::reset_space_presence(&state).await;
            assert!(
                reset.is_ok(),
                "catalog={catalog_present} entered={entered} active={active}: {reset:?}"
            );
            crate::reconcile_space_presence(&state).await.unwrap();
            crate::emit_current_space_presence(&state).await.unwrap();
            assert_eq!(super::sequence_recovery::registry_bytes(&path), before);
            let registry = state.own_space_presence.lock().await;
            assert_eq!(
                registry.stored(&original.record.network_id, &original.record.space_id),
                Some(&original)
            );
            assert_eq!(
                registry.projection(&original.record.network_id, state.space_catalog.as_deref()),
                Vec::<SignedSpacePresence>::new()
            );
            assert!(registry.active_space.is_none());
            drop(registry);
            assert_eq!(
                crate::routes::space::snapshot::export(&state)
                    .await
                    .unwrap()
                    .space_presences,
                Vec::<SignedSpacePresence>::new()
            );
            let exported = crate::routes::client_api::build_signed_public_client_snapshot(
                &state,
                &crate::ClientExportQuery {
                    public_id: Some(public_id),
                    ..crate::ClientExportQuery::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(
                exported.payload.space_presences,
                Vec::<SignedSpacePresence>::new()
            );
            assert!(
                serde_json::to_value(&exported.payload)
                    .unwrap()
                    .get("space_presences")
                    .is_none()
            );
            assert!(
                verify_payload(
                    &exported.payload,
                    &exported.signature,
                    &state.identity.public_key
                )
                .unwrap()
            );
            assert!(matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
            assert_eq!(super::sequence_recovery::registry_bytes(&path), before);
        }
    }
}

#[tokio::test]
async fn reset_only_retires_old_geometry_before_explicit_current_map_entry() {
    let (dir, _app, _token, _policy, state) = build_test_app(100);
    let path = dir.path().join("inactive-old-geometry.db");
    let mut state = disk_state(state, &path);
    let identity = wattetheria_kernel::agent_identity::load_agent_identity(dir.path()).unwrap();
    let old_package = current_package(&state).clone();
    let old = super::map_upgrade::signed_spawn(&state, &identity, &old_package, 10);
    let current = super::map_upgrade::upgraded_package(&old_package, old.record.from_tile);
    assert!(!current.is_walkable(old.record.from_tile));
    state.space_catalog = Some(Arc::new(super::namespace_portals::configured_catalog(
        &old_package.space_id,
        &[old_package.clone(), current.clone()],
    )));
    let registry = OwnSpacePresence {
        networks: BTreeMap::from([(
            old.record.network_id.clone(),
            BTreeMap::from([(old.record.space_id.clone(), old.clone())]),
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
    let mut events = state.stream_tx.subscribe();
    crate::reset_space_presence(&state).await.unwrap();
    let inactive: SignedSpacePresence =
        serde_json::from_value(events.try_recv().unwrap().payload).unwrap();
    let mut expected = old.record.clone();
    expected.entered = false;
    expected.seq = 11;
    expected.updated_at = inactive.record.updated_at;
    assert_eq!(inactive.record, expected);
    old_package.validate_record(&inactive.record).unwrap();
    assert!(verify_payload(&inactive.record, &inactive.signature, &inactive.public_key).unwrap());
    let reopened = OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert_eq!(
        reopened.stored(&old.record.network_id, &old.record.space_id),
        Some(&inactive)
    );
    assert!(reopened.active_space.is_none());
    let entered = initialize_space_presence(&state).await.unwrap();
    assert_eq!(entered.record.seq, 12);
    assert_eq!(entered.record.space_version, current.space_version);
    assert_ne!(entered.record.from_tile, old.record.from_tile);
    assert_eq!(entered.record.path, "");
    assert_eq!(entered.record.destination_place_id, current.spawn_place_id);
    current.validate_record(&entered.record).unwrap();
    let mut invalid = old.clone();
    invalid.record.from_tile = [u32::MAX, u32::MAX];
    invalid.signature = state.sign_payload(&invalid.record).unwrap();
    let mut registry = state.own_space_presence.lock().await.clone();
    registry
        .networks
        .get_mut(&old.record.network_id)
        .unwrap()
        .insert(old.record.space_id.clone(), invalid);
    state
        .local_db
        .save_domain("own_space_presence", &registry)
        .unwrap();
    assert_eq!(
        OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did
        )
        .unwrap_err()
        .to_string(),
        "presence starts on blocked or out-of-bounds tile"
    );
}

#[tokio::test]
async fn own_presence_get_uses_current_network_permission_not_latest_other_network() {
    let mut observations = Vec::new();
    for (current_status, other_status) in [("active", "pending"), ("revoked", "active")] {
        let (dir, _app, token, _policy, state) = build_test_app(100);
        let state = disk_state(state, &dir.path().join("own-get-permission.db"));
        let original = initialize_space_presence(&state).await.unwrap();
        let other_network = seed_other_network_permission_fixture(
            &state,
            &original.record.network_id,
            current_status,
            other_status,
        )
        .await;
        assert_eq!(
            state
                .local_db
                .load_network_permission_checkpoint(&state.agent_did, None, None)
                .unwrap()
                .unwrap()
                .network_id,
            other_network
        );
        assert_eq!(
            wattetheria_kernel::network_agent_registration::network_permission_is_active(
                &state.local_db,
                &state.agent_did
            )
            .unwrap(),
            other_status == "active"
        );
        let before = state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap();
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
        let status = response.status();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        observations.push((
            current_status,
            status,
            body["presence"] == json!(original),
            body["error"].clone(),
        ));
        assert_eq!(
            state
                .local_db
                .load_domain::<Value>("own_space_presence")
                .unwrap(),
            before
        );
    }
    assert_eq!(
        observations,
        vec![
            ("active", StatusCode::OK, true, Value::Null),
            (
                "revoked",
                StatusCode::INTERNAL_SERVER_ERROR,
                false,
                json!("active network agent permission is required to join Agent Space")
            )
        ]
    );
}
