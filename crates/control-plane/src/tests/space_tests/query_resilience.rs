use super::*;

#[tokio::test]
async fn space_http_mutations_and_own_get_reject_lost_permission_without_writes_or_events() {
    use wattetheria_kernel::network_agent_registration::{
        membership_credential_hash, network_permission_is_active,
    };
    for invalidation in ["revoked", "suspended", "expired"] {
        let (dir, _app, token, _policy, state) = build_test_app(100);
        let state = disk_state(
            state,
            &dir.path().join(format!("permission-{invalidation}.db")),
        );
        let saved = initialize_space_presence(&state).await.unwrap();
        let saved_domain = state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap();
        if invalidation == "expired" {
            let mut stored = state
                .local_db
                .load_network_agent_credential(&state.agent_did, None)
                .unwrap()
                .unwrap();
            let mut credential: MembershipCredential =
                serde_json::from_str(&stored.credential_json).unwrap();
            let expires_at_ms = wattetheria_kernel::network_agent_registration::now_ms() - 1;
            credential.unsigned.expires_at_ms = Some(expires_at_ms);
            credential.signature_hex = hex::encode(
                SigningKey::from_bytes(&[42_u8; 32])
                    .sign(&credential.signing_bytes().unwrap())
                    .to_bytes(),
            );
            stored.credential_expires_at_ms = Some(expires_at_ms);
            stored.credential_json = serde_json::to_string(&credential).unwrap();
            stored.credential_hash = membership_credential_hash(&credential);
            state
                .local_db
                .upsert_network_agent_credential(&stored)
                .unwrap();
        } else {
            let mut checkpoint = state
                .local_db
                .load_network_permission_checkpoint(&state.agent_did, None, None)
                .unwrap()
                .unwrap();
            checkpoint.permission_status = invalidation.to_owned();
            state
                .local_db
                .upsert_network_permission_checkpoint(&checkpoint)
                .unwrap();
        }
        assert!(!network_permission_is_active(&state.local_db, &state.agent_did).unwrap());
        let mut events = state.stream_tx.subscribe();
        for (path, body) in [
            (
                "/v1/wattetheria/space/move",
                json!({"place_id":"hobbs-cafe/cafe","activity":"must not move"}),
            ),
            (
                "/v1/wattetheria/space/activity",
                json!({"activity":"must not update"}),
            ),
        ] {
            let result = authed_post_json(app(state.clone()), &token, path, body).await;
            assert_eq!(
                result["error"], "active network agent permission is required to join Agent Space",
                "{invalidation}: {path}"
            );
        }
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
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            result["error"],
            "active network agent permission is required to join Agent Space"
        );
        let registry = state.own_space_presence.lock().await;
        assert!(registry.active_space.is_some());
        assert_eq!(
            registry.get(&test_network_id(&state), current_package(&state)),
            Some(&saved)
        );
        drop(registry);
        let unchanged = state
            .local_db
            .load_domain::<Value>("own_space_presence")
            .unwrap();
        assert_eq!(unchanged, saved_domain);
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
}

#[tokio::test]
async fn space_queries_hide_own_old_gateway_entry_and_high_sequence_remote_tombstone() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    let own = initialize_space_presence(&state).await.unwrap();
    state.own_space_presence = Arc::new(Mutex::new(
        OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did,
        )
        .unwrap(),
    ));
    let remote = Identity::new_random();
    let active = signed_presence(&state, &remote);
    let mut inactive = active.clone();
    inactive.record.entered = false;
    inactive.record.seq += 1;
    inactive.signature = remote
        .sign(&canonical_bytes(&inactive.record).unwrap())
        .unwrap();
    let mut servers = Vec::new();
    for records in [json!([own, active]), json!([inactive])] {
        let (url, server) = serve(Router::new().route(
            "/api/spaces/presence",
            get(move || {
                let records = records.clone();
                async move { Json(json!({"presences": records})) }
            }),
        ))
        .await;
        state.gateway_urls.push(url);
        servers.push(server);
    }
    let place = authed_get_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/place?place_id=the-ville",
    )
    .await;
    assert_eq!(place["presences"], json!([]));
    let remote_query = authed_get_json(
        app(state.clone()),
        &token,
        &format!(
            "/v1/wattetheria/space/presence?agent_id={}",
            remote.agent_did
        ),
    )
    .await;
    assert_eq!(remote_query["error"], "agent presence not found");
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    for server in servers {
        server.abort();
    }
}
use wattetheria_gateway_contract::SpacePresenceRecord;
use wattetheria_kernel::swarm_bridge::EventAnnouncementPayload;

fn signed_presence(state: &ControlPlaneState, identity: &Identity) -> SignedSpacePresence {
    let now = Utc::now().timestamp_millis();
    let record = SpacePresenceRecord {
        agent_did: identity.agent_did.clone(),
        network_id: test_network_id(state),
        space_id: current_package(state).space_id.clone(),
        space_version: current_package(state).space_version.clone(),
        seq: 9,
        entered: true,
        from_tile: current_package(state)
            .destination_tile(&identity.agent_did, "johnson-park")
            .unwrap(),
        path: String::new(),
        depart_at: now,
        destination_place_id: "johnson-park".to_owned(),
        activity: "waiting".to_owned(),
        object_place_id: None,
        updated_at: now,
    };
    SignedSpacePresence {
        signature: identity.sign(&canonical_bytes(&record).unwrap()).unwrap(),
        public_key: identity.public_key.clone(),
        record,
    }
}

fn signed_event(state: &ControlPlaneState, id: &str, place_id: &str) -> EventAnnouncementPayload {
    let identity = Identity::new_random();
    let now = Utc::now().timestamp_millis();
    let mut announcement = EventAnnouncementPayload {
        announcement_id: id.to_owned(),
        network_id: test_network_id(state),
        authority_node_id: hex::encode(STANDARD.decode(&identity.public_key).unwrap()),
        issued_at: Utc::now().timestamp_millis().try_into().unwrap(),
        content: json!({
            "event_id":id,"space_id":"the-ville","title":"Gathering",
            "description":"Meet at the park","place_id":place_id,
            "starts_at":now-1_000,"ends_at":now+60_000
        }),
        authority_signature: String::new(),
    };
    announcement.authority_signature = hex::encode(
        STANDARD
            .decode(
                identity
                    .sign(&canonical_bytes(&announcement.unsigned_payload()).unwrap())
                    .unwrap(),
            )
            .unwrap(),
    );
    announcement
}

pub(super) async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{address}"), server)
}

fn assert_no_tiles(value: &Value) {
    match value {
        Value::Object(fields) => {
            assert!(!fields.contains_key("tiles"));
            for child in fields.values() {
                assert_no_tiles(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                assert_no_tiles(child);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn place_queries_omit_geometry_without_changing_canonical_package() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let before = serde_json::to_value(current_package(&state)).unwrap();
    let places = authed_get_json(app(state.clone()), &token, "/v1/wattetheria/space/places").await;
    assert_eq!(
        places["space_version"],
        current_package(&state).space_version
    );
    assert_eq!(places["places"].as_array().unwrap().len(), 305);
    assert_no_tiles(&places);
    for (entry, place) in places["places"]
        .as_array()
        .unwrap()
        .iter()
        .zip(&current_package(&state).places)
    {
        assert_eq!(entry["place"]["place_id"], place.place_id);
        assert_eq!(entry["place"]["name"], json!(place.name));
        assert_eq!(
            entry["place"]["parent_place_id"],
            json!(place.parent_place_id)
        );
        assert_eq!(entry["place"]["kind"], json!(place.kind));
    }
    let details = authed_get_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/place?place_id=hobbs-cafe/cafe",
    )
    .await;
    assert_no_tiles(&details);
    assert_eq!(details["place"]["place_id"], "hobbs-cafe/cafe");
    assert_ne!(
        details["objects"].as_array().unwrap().as_slice(),
        [] as [Value; 0]
    );
    assert_eq!(details["service_bindings"], json!([]));
    assert_eq!(
        serde_json::to_value(current_package(&state)).unwrap(),
        before
    );
    current_package(&state).validate().unwrap();
}

fn mixed_gateway_payloads(
    state: &ControlPlaneState,
) -> (SignedSpacePresence, EventAnnouncementPayload, Value, Value) {
    let remote_identity = Identity::new_random();
    let remote = signed_presence(state, &remote_identity);
    let invalid_identity = Identity::new_random();
    let mut tampered = signed_presence(state, &invalid_identity);
    tampered.record.activity = "tampered".to_owned();
    let mut invalid_key = signed_presence(state, &Identity::new_random());
    invalid_key.public_key = "invalid-key".to_owned();
    let map_identity = Identity::new_random();
    let mut invalid_map = signed_presence(state, &map_identity);
    invalid_map.record.from_tile = [u32::MAX, u32::MAX];
    invalid_map.signature = map_identity
        .sign(&canonical_bytes(&invalid_map.record).unwrap())
        .unwrap();
    let mut obsolete = signed_presence(state, &Identity::new_random());
    obsolete.record.space_version = "obsolete".to_owned();
    let mut oversized = signed_presence(state, &invalid_identity);
    oversized.record.activity = "a".repeat(2_001);
    oversized.signature = invalid_identity
        .sign(&canonical_bytes(&oversized.record).unwrap())
        .unwrap();
    let event = signed_event(state, "valid-event", "johnson-park");
    let mut bad_event = signed_event(state, "bad-signature", "johnson-park");
    bad_event.content["description"] = json!("tampered");
    let unknown_place = signed_event(state, "bad-place", "unknown-place");
    let wrong_place = signed_event(state, "other-place", "hobbs-cafe/cafe");
    let authority = Identity::new_random();
    let mut expired = signed_event(state, "expired", "johnson-park");
    expired.content["starts_at"] = json!(1);
    expired.content["ends_at"] = json!(2);
    expired.authority_node_id = hex::encode(STANDARD.decode(&authority.public_key).unwrap());
    expired.authority_signature = hex::encode(
        STANDARD
            .decode(
                authority
                    .sign(&canonical_bytes(&expired.unsigned_payload()).unwrap())
                    .unwrap(),
            )
            .unwrap(),
    );
    let presences = json!({"presences":[
        {"record":"malformed"}, tampered, invalid_key, invalid_map, obsolete, oversized, remote
    ]});
    let events = json!({"events":[{},bad_event,unknown_place,wrong_place,expired,event]});
    (remote, event, presences, events)
}

#[tokio::test]
async fn gateway_queries_isolate_bad_entries_offline_and_stalled_gateways() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    let local = initialize_space_presence(&state).await.unwrap();
    let (remote, event, presences, events) = mixed_gateway_payloads(&state);
    let (valid_url, valid_server) = serve(
        Router::new()
            .route(
                "/api/spaces/presence",
                get(move || {
                    let payload = presences.clone();
                    async move { Json(payload) }
                }),
            )
            .route(
                "/api/spaces/events",
                get(move || {
                    let payload = events.clone();
                    async move { Json(payload) }
                }),
            ),
    )
    .await;
    let (bad_url, bad_server) = serve(
        Router::new()
            .route("/api/spaces/presence", get(|| async { "not-json" }))
            .route(
                "/api/spaces/events",
                get(|| async { Json(json!({"events":42})) }),
            ),
    )
    .await;
    let (stalled_url, stalled_server) = serve(
        Router::new()
            .route(
                "/api/spaces/presence",
                get(|| async { std::future::pending::<Json<Value>>().await }),
            )
            .route(
                "/api/spaces/events",
                get(|| async { std::future::pending::<Json<Value>>().await }),
            ),
    )
    .await;
    let offline = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let offline_url = format!("http://{}", offline.local_addr().unwrap());
    drop(offline);
    state.gateway_urls = vec![offline_url, stalled_url, bad_url, valid_url];
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(9),
        authed_get_json(
            app(state),
            &token,
            "/v1/wattetheria/space/place?place_id=johnson-park",
        ),
    )
    .await;
    valid_server.abort();
    bad_server.abort();
    stalled_server.abort();
    let result = result.expect("queries must finish after per-gateway request deadlines");
    assert!(started.elapsed() < std::time::Duration::from_secs(9));
    let records: Vec<SignedSpacePresence> =
        serde_json::from_value(result["presences"].clone()).unwrap();
    assert_eq!(records.len(), 2);
    assert!(records.contains(&local));
    assert!(records.contains(&remote));
    assert_eq!(result["events"], json!([event]));
}

#[tokio::test]
async fn place_query_retains_verified_local_world_events_when_provider_goes_offline() {
    let (dir, _app, token, _policy, mut state) = build_test_app(100);
    let event = signed_event(&state, "cached-event", "johnson-park");
    let network_id = event.network_id.clone();
    let payload = event.clone();
    let (url, server) = serve(
        Router::new()
            .route("/api/global/messages", get(move || {
                let payload = payload.clone();
                async move { Json(json!({"network_id":payload.network_id,"next_after_sequence":null,"messages":[{
                    "sequence":1,"message_id":"outer-cache-event","kind":"EventAnnouncement",
                    "lane":"events","scope_hint":"global","author_node_id":"relay-node",
                    "content":{"type":"EventAnnouncement","payload":payload},"created_at":1
                }]})) }
            }))
            .route("/api/wattetheria/network/snapshot", get(move || {
                let network_id = network_id.clone();
                async move { Json(json!({"network_id":network_id})) }
            })),
    ).await;
    state.swarm_bridge = Arc::new(wattetheria_kernel::swarm_bridge::HybridSwarmBridge::new(
        dir.path().join("missions/cache-state.json"),
        Some(&url),
    ));
    assert_eq!(
        crate::routes::space::world::world_announcements(&state)
            .await
            .unwrap(),
        vec![event.clone()]
    );
    server.abort();
    let _ = server.await;
    state.gateway_urls = vec![url];
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        authed_get_json(
            app(state),
            &token,
            "/v1/wattetheria/space/place?place_id=johnson-park",
        ),
    )
    .await
    .expect("offline local projection must retain verified events");
    assert_eq!(result["events"], json!([event]));
}

#[tokio::test]
async fn invalid_activity_is_rejected_before_spawn_signing_persistence_or_emission() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let mut events = state.stream_tx.subscribe();
    for activity in [
        "\u{754c}".repeat(2_001),
        "bad\0text".to_owned(),
        "bad\u{1b}text".to_owned(),
    ] {
        for (route, body) in [
            (
                "/v1/wattetheria/space/move",
                json!({"place_id":"johnson-park","activity":activity}),
            ),
            (
                "/v1/wattetheria/space/activity",
                json!({"activity":activity}),
            ),
        ] {
            let result = authed_post_json(app(state.clone()), &token, route, body).await;
            assert!(result["error"].is_string());
            assert_eq!(
                state
                    .own_space_presence
                    .lock()
                    .await
                    .get(&test_network_id(&state), current_package(&state)),
                None
            );
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
    }
    initialize_space_presence(&state).await.unwrap();
    let accepted = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/activity",
        json!({"activity":"\u{754c}".repeat(2_000)}),
    )
    .await;
    assert_eq!(accepted["ok"], true);
    let before = initialize_space_presence(&state).await.unwrap();
    assert_eq!(before.record.activity.chars().count(), 2_000);
    assert_eq!(before.record.seq, 2);
    assert_eq!(events.recv().await.unwrap().kind, "space.presence.updated");
    assert_eq!(events.recv().await.unwrap().kind, "space.presence.updated");
    let rejected = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/activity",
        json!({"activity":"\u{754c}".repeat(2_001)}),
    )
    .await;
    assert!(rejected["error"].is_string());
    assert_eq!(initialize_space_presence(&state).await.unwrap(), before);
    assert_eq!(
        OwnSpacePresence::load(
            &state.local_db,
            state.space_catalog.as_deref(),
            &state.agent_did
        )
        .unwrap()
        .get(&test_network_id(&state), current_package(&state)),
        Some(&before)
    );
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let controls = authed_post_json(
        app(state),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"johnson-park","activity":"line\ncarriage\rtab\t"}),
    )
    .await;
    assert_eq!(controls["ok"], true);
    assert_eq!(controls["presence"]["record"]["seq"], 3);
}
