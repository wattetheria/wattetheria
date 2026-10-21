use super::namespace_portals::{configured_catalog, rehash_package};
use super::*;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use std::collections::BTreeMap;
use wattetheria_gateway_contract::SpacePresenceRecord;
use wattetheria_kernel::space::SpacePackage;

fn corridor_package(target_x: u32) -> SpacePackage {
    let tiles = json!([[0, 0], [1, 0], [2, 0], [3, 0], [4, 0]]);
    let mut package: SpacePackage = serde_json::from_value(json!({
        "space_id":"the-ville", "space_version":"pending", "size":[5,1],
        "tile_size":32, "walkable_grid":[[1,1,1,1,1]], "walk_speed":2.0,
        "movement_search_radius":2, "spawn_place_id":"spawn", "service_bindings":[],
        "places":[
            {"place_id":"the-ville","parent_place_id":null,"kind":"world",
             "name":{"en":"World","zh":"World"},"tiles":tiles},
            {"place_id":"corridor","parent_place_id":"the-ville","kind":"sector",
             "name":{"en":"Corridor","zh":"Corridor"},"tiles":tiles},
            {"place_id":"spawn","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Spawn","zh":"Spawn"},"tiles":[[0,0]]},
            {"place_id":"target","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Target","zh":"Target"},"tiles":[[target_x,0]]}
        ]
    }))
    .unwrap();
    rehash_package(&mut package);
    package
}

fn place_package() -> SpacePackage {
    let tiles = json!([[0, 0], [1, 0], [2, 0], [3, 0], [4, 0]]);
    let mut package: SpacePackage = serde_json::from_value(json!({
        "space_id":"the-ville", "space_version":"pending", "size":[5,1],
        "tile_size":32, "walkable_grid":[[1,1,1,1,1]], "walk_speed":2.0,
        "movement_search_radius":2, "spawn_place_id":"spawn", "service_bindings":[],
        "places":[
            {"place_id":"the-ville","parent_place_id":null,"kind":"world",
             "name":{"en":"World","zh":"World"},"tiles":tiles},
            {"place_id":"corridor","parent_place_id":"the-ville","kind":"sector",
             "name":{"en":"Corridor","zh":"Corridor"},"tiles":tiles},
            {"place_id":"spawn","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Spawn","zh":"Spawn"},"tiles":[[0,0]]},
            {"place_id":"target","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Target","zh":"Target"},"tiles":[[3,0],[4,0]]}
        ]
    }))
    .unwrap();
    rehash_package(&mut package);
    package
}

fn disconnected_package() -> SpacePackage {
    let tiles = json!([[0, 0], [1, 0], [3, 0], [4, 0]]);
    let mut package: SpacePackage = serde_json::from_value(json!({
        "space_id":"the-ville", "space_version":"pending", "size":[5,1],
        "tile_size":32, "walkable_grid":[[1,1,0,1,1]], "walk_speed":2.0,
        "movement_search_radius":2, "spawn_place_id":"spawn", "service_bindings":[],
        "places":[
            {"place_id":"the-ville","parent_place_id":null,"kind":"world",
             "name":{"en":"World","zh":"World"},"tiles":tiles},
            {"place_id":"corridor","parent_place_id":"the-ville","kind":"sector",
             "name":{"en":"Corridor","zh":"Corridor"},"tiles":tiles},
            {"place_id":"spawn","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Spawn","zh":"Spawn"},"tiles":[[0,0]]},
            {"place_id":"target","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Target","zh":"Target"},"tiles":[[4,0]]}
        ]
    }))
    .unwrap();
    rehash_package(&mut package);
    package
}

fn detour_package() -> SpacePackage {
    let tiles = json!([[2, 0], [0, 2], [4, 0], [2, 2]]);
    let mut package: SpacePackage = serde_json::from_value(json!({
        "space_id":"the-ville", "space_version":"pending", "size":[5,3],
        "tile_size":32,
        "walkable_grid":[[0,0,1,0,1],[1,1,1,0,1],[1,0,1,1,1]],
        "walk_speed":2.0, "movement_search_radius":2, "spawn_place_id":"spawn",
        "service_bindings":[],
        "places":[
            {"place_id":"the-ville","parent_place_id":null,"kind":"world",
             "name":{"en":"World","zh":"World"},"tiles":tiles},
            {"place_id":"corridor","parent_place_id":"the-ville","kind":"sector",
             "name":{"en":"Corridor","zh":"Corridor"},"tiles":tiles},
            {"place_id":"spawn","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Spawn","zh":"Spawn"},"tiles":[[4,0]]},
            {"place_id":"target","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Target","zh":"Target"},"tiles":[[2,2]]}
        ]
    }))
    .unwrap();
    rehash_package(&mut package);
    package
}

fn crowded_target_package() -> SpacePackage {
    let tiles = (0..7)
        .flat_map(|y| (0..7).map(move |x| [x, y]))
        .collect::<Vec<[u32; 2]>>();
    let mut package: SpacePackage = serde_json::from_value(json!({
        "space_id":"the-ville", "space_version":"pending", "size":[7,7],
        "tile_size":32, "walkable_grid":vec![vec![1; 7]; 7], "walk_speed":2.0,
        "movement_search_radius":2, "spawn_place_id":"spawn", "service_bindings":[],
        "places":[
            {"place_id":"the-ville","parent_place_id":null,"kind":"world",
             "name":{"en":"World","zh":"World"},"tiles":tiles.clone()},
            {"place_id":"corridor","parent_place_id":"the-ville","kind":"sector",
             "name":{"en":"Corridor","zh":"Corridor"},"tiles":tiles},
            {"place_id":"spawn","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Spawn","zh":"Spawn"},"tiles":[[0,0]]},
            {"place_id":"target","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Target","zh":"Target"},"tiles":[[3,3]]}
        ]
    }))
    .unwrap();
    rehash_package(&mut package);
    package
}

fn branch_package() -> SpacePackage {
    let tiles = json!([[2, 0], [3, 0], [4, 0], [3, 1], [2, 2], [3, 2], [2, 3]]);
    let mut package: SpacePackage = serde_json::from_value(json!({
        "space_id":"the-ville", "space_version":"pending", "size":[5,4],
        "tile_size":32,
        "walkable_grid":[[0,0,1,1,1],[0,0,0,1,0],[0,0,1,1,0],[0,0,1,0,0]],
        "walk_speed":2.0, "movement_search_radius":2, "spawn_place_id":"spawn",
        "service_bindings":[],
        "places":[
            {"place_id":"the-ville","parent_place_id":null,"kind":"world",
             "name":{"en":"World","zh":"World"},"tiles":tiles},
            {"place_id":"corridor","parent_place_id":"the-ville","kind":"sector",
             "name":{"en":"Corridor","zh":"Corridor"},"tiles":tiles},
            {"place_id":"spawn","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Spawn","zh":"Spawn"},"tiles":[[4,0]]},
            {"place_id":"target","parent_place_id":"corridor","kind":"arena",
             "name":{"en":"Target","zh":"Target"},"tiles":[[2,2]]}
        ]
    }))
    .unwrap();
    rehash_package(&mut package);
    package
}

fn signed_presence(
    identity: &Identity,
    package: &SpacePackage,
    network_id: String,
    seq: u64,
    from_tile: [u32; 2],
    path: &str,
    depart_at: i64,
) -> SignedSpacePresence {
    let final_tile = path.bytes().fold(from_tile, |mut tile, direction| {
        match direction {
            b'U' => tile[1] -= 1,
            b'R' => tile[0] += 1,
            b'D' => tile[1] += 1,
            b'L' => tile[0] -= 1,
            _ => unreachable!(),
        }
        tile
    });
    let record = SpacePresenceRecord {
        agent_did: identity.agent_did.clone(),
        network_id,
        space_id: package.space_id.clone(),
        space_version: package.space_version.clone(),
        seq,
        entered: true,
        from_tile,
        path: path.to_owned(),
        depart_at,
        destination_place_id: package.place_at_tile(final_tile).unwrap().place_id.clone(),
        activity: String::new(),
        object_place_id: None,
        updated_at: depart_at,
    };
    SignedSpacePresence {
        signature: identity.sign(&canonical_bytes(&record).unwrap()).unwrap(),
        public_key: identity.public_key.clone(),
        record,
    }
}

async fn presence_gateway(
    records: Vec<SignedSpacePresence>,
    fail_unscoped: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new().route(
        "/api/spaces/presence",
        get(move |Query(query): Query<BTreeMap<String, String>>| {
            let records = records.clone();
            async move {
                if fail_unscoped && !query.contains_key("network_id") {
                    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({}))).into_response()
                } else {
                    Json(json!({ "presences": records })).into_response()
                }
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{address}"), server)
}

async fn app_with_package(
    package: &SpacePackage,
) -> (tempfile::TempDir, String, ControlPlaneState) {
    let (dir, _app, token, _policy, mut state) = build_test_app(100);
    state.space_catalog = Some(Arc::new(configured_catalog(
        "the-ville",
        std::slice::from_ref(package),
    )));
    initialize_space_presence(&state).await.unwrap();
    (dir, token, state)
}

#[tokio::test]
async fn place_movement_keeps_hash_tile_when_global_occupancy_is_unavailable() {
    let package = corridor_package(4);
    let (_dir, token, state) = app_with_package(&package).await;
    let anchor = package
        .destination_tile(&state.agent_did, "target")
        .unwrap();
    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"target"}),
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    let moved: SignedSpacePresence = serde_json::from_value(result["presence"].clone()).unwrap();
    package.validate_record(&moved.record).unwrap();
    assert_eq!(moved.record.tile_at(package.walk_speed, i64::MAX), anchor);
    assert_eq!(moved.record.destination_place_id, "target");
}

#[tokio::test]
async fn place_movement_prefers_an_unoccupied_tile_inside_the_requested_place() {
    let package = place_package();
    let (_dir, token, mut state) = app_with_package(&package).await;
    let anchor = package
        .destination_tile(&state.agent_did, "target")
        .unwrap();
    let occupied = signed_presence(
        &Identity::new_random(),
        &package,
        test_network_id(&state),
        1,
        anchor,
        "",
        Utc::now().timestamp_millis(),
    );
    let (gateway, server) = presence_gateway(vec![occupied], false).await;
    state.gateway_urls = vec![gateway];

    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"target"}),
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    let moved: SignedSpacePresence = serde_json::from_value(result["presence"].clone()).unwrap();
    package.validate_record(&moved.record).unwrap();
    let stopped = moved.record.tile_at(package.walk_speed, i64::MAX);
    assert_ne!(stopped, anchor);
    assert!(package.place("target").unwrap().tiles.contains(&stopped));
    assert_eq!(moved.record.destination_place_id, "target");
    server.abort();
}

#[tokio::test]
async fn target_movement_uses_current_and_final_tiles_as_soft_occupancy() {
    for (from_tile, path) in [([3, 2], ""), ([3, 1], "D")] {
        let package = branch_package();
        let (_dir, token, mut state) = app_with_package(&package).await;
        let network_id = test_network_id(&state);
        let target = Identity::new_random();
        let occupied = Identity::new_random();
        let target_presence = signed_presence(
            &target,
            &package,
            network_id.clone(),
            9,
            [2, 2],
            "",
            Utc::now().timestamp_millis(),
        );
        let occupied_presence = signed_presence(
            &occupied,
            &package,
            network_id,
            1,
            from_tile,
            path,
            Utc::now().timestamp_millis() + 60_000,
        );
        let (gateway, server) =
            presence_gateway(vec![target_presence, occupied_presence], false).await;
        state.gateway_urls = vec![gateway];

        let result = authed_post_json(
            app(state.clone()),
            &token,
            "/v1/wattetheria/space/move",
            json!({"target_agent_id":target.agent_did,"activity":"meeting"}),
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");
        let moved: SignedSpacePresence =
            serde_json::from_value(result["presence"].clone()).unwrap();
        package.validate_record(&moved.record).unwrap();
        assert_eq!(moved.record.tile_at(package.walk_speed, i64::MAX), [2, 3]);
        assert_eq!(moved.record.destination_place_id, "corridor");
        server.abort();
    }
}

#[tokio::test]
async fn target_movement_excludes_detours_beyond_walking_search_radius() {
    let package = detour_package();
    let target_tile = [2_u32, 2];
    let occupied_tile = [2, 0];
    let empty_tile = [0, 2];
    let manhattan =
        |tile: [u32; 2]| target_tile[0].abs_diff(tile[0]) + target_tile[1].abs_diff(tile[1]);
    assert_eq!(manhattan(occupied_tile), manhattan(empty_tile));
    assert_eq!(package.path(target_tile, occupied_tile).unwrap().len(), 2);
    assert_eq!(package.path(target_tile, empty_tile).unwrap().len(), 4);
    let (_dir, token, mut state) = app_with_package(&package).await;
    let network_id = test_network_id(&state);
    let now = Utc::now().timestamp_millis();
    let target = Identity::new_random();
    let mut presences = vec![signed_presence(
        &target,
        &package,
        network_id.clone(),
        1,
        [2, 2],
        "",
        now,
    )];
    presences.push(signed_presence(
        &Identity::new_random(),
        &package,
        network_id.clone(),
        1,
        occupied_tile,
        "",
        now,
    ));
    let (gateway, server) = presence_gateway(presences, false).await;
    state.gateway_urls = vec![gateway];

    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"target_agent_id":target.agent_did}),
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    let moved: SignedSpacePresence = serde_json::from_value(result["presence"].clone()).unwrap();
    package.validate_record(&moved.record).unwrap();
    assert_eq!(
        moved.record.tile_at(package.walk_speed, i64::MAX),
        occupied_tile
    );
    assert_eq!(moved.record.destination_place_id, "corridor");
    server.abort();
}

#[tokio::test]
async fn target_movement_uses_free_tile_two_steps_away_when_neighbors_are_occupied() {
    let package = crowded_target_package();
    let target_tile = [3_u32, 3];
    let neighbors = [[2, 3], [4, 3], [3, 2], [3, 4]];
    let (_dir, token, mut state) = app_with_package(&package).await;
    let network_id = test_network_id(&state);
    let now = Utc::now().timestamp_millis();
    let target = Identity::new_random();
    let mut presences = vec![signed_presence(
        &target,
        &package,
        network_id.clone(),
        1,
        target_tile,
        "",
        now,
    )];
    for tile in neighbors {
        presences.push(signed_presence(
            &Identity::new_random(),
            &package,
            network_id.clone(),
            1,
            tile,
            "",
            now,
        ));
    }
    let (gateway, server) = presence_gateway(presences, false).await;
    state.gateway_urls = vec![gateway];

    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"target_agent_id":target.agent_did}),
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    let moved: SignedSpacePresence = serde_json::from_value(result["presence"].clone()).unwrap();
    let stopped = moved.record.tile_at(package.walk_speed, i64::MAX);
    assert!(!neighbors.contains(&stopped));
    assert_eq!(package.path(target_tile, stopped).unwrap().len(), 2);
    assert_eq!(moved.record.destination_place_id, "corridor");
    server.abort();
}

#[tokio::test]
async fn target_movement_keeps_same_did_network_records_separate() {
    let package = branch_package();
    let (_dir, token, mut state) = app_with_package(&package).await;
    let target = Identity::new_random();
    let network_id = test_network_id(&state);
    let target_tile = [2, 2];
    let repeated_did_tile = [3, 2];
    let other_occupants_tile = [2, 3];
    assert_eq!(
        package.path(target_tile, repeated_did_tile).unwrap().len(),
        1
    );
    assert_eq!(
        package
            .path(target_tile, other_occupants_tile)
            .unwrap()
            .len(),
        1
    );
    let now = Utc::now().timestamp_millis();
    let mut presences = vec![signed_presence(
        &target,
        &package,
        network_id.clone(),
        9,
        target_tile,
        "",
        now,
    )];
    for index in 0..3 {
        presences.push(signed_presence(
            &target,
            &package,
            format!("another-network-{index}"),
            1,
            repeated_did_tile,
            "",
            now,
        ));
    }
    for _ in 0..2 {
        presences.push(signed_presence(
            &Identity::new_random(),
            &package,
            network_id.clone(),
            1,
            other_occupants_tile,
            "",
            now,
        ));
    }
    for tile in [[2, 0], [3, 1]] {
        for _ in 0..3 {
            presences.push(signed_presence(
                &Identity::new_random(),
                &package,
                network_id.clone(),
                1,
                tile,
                "",
                now,
            ));
        }
    }
    let (gateway, server) = presence_gateway(presences, false).await;
    state.gateway_urls = vec![gateway];

    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"target_agent_id":target.agent_did}),
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    let moved: SignedSpacePresence = serde_json::from_value(result["presence"].clone()).unwrap();
    assert_eq!(
        moved.record.tile_at(package.walk_speed, i64::MAX),
        other_occupants_tile,
        "all three (network_id, DID) observations on the first equidistant tile must count"
    );
    server.abort();
}

#[tokio::test]
async fn target_movement_does_not_fail_when_all_nearby_tiles_are_occupied() {
    let package = corridor_package(4);
    let (_dir, token, mut state) = app_with_package(&package).await;
    let network_id = test_network_id(&state);
    let target = Identity::new_random();
    let mut presences = vec![signed_presence(
        &target,
        &package,
        network_id.clone(),
        9,
        [4, 0],
        "",
        Utc::now().timestamp_millis(),
    )];
    for tile in [[2, 0], [3, 0]] {
        presences.push(signed_presence(
            &Identity::new_random(),
            &package,
            network_id.clone(),
            1,
            tile,
            "",
            Utc::now().timestamp_millis(),
        ));
    }
    let (gateway, server) = presence_gateway(presences, false).await;
    state.gateway_urls = vec![gateway];

    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"target_agent_id":target.agent_did}),
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    let moved: SignedSpacePresence = serde_json::from_value(result["presence"].clone()).unwrap();
    assert!([[2, 0], [3, 0]].contains(&moved.record.tile_at(package.walk_speed, i64::MAX)));
    server.abort();
}

#[tokio::test]
async fn person_movement_uses_network_scoped_presence_when_global_read_fails() {
    let package = branch_package();
    let (_dir, token, mut state) = app_with_package(&package).await;
    let network_id = test_network_id(&state);
    let target = Identity::new_random();
    let occupied = Identity::new_random();
    let records = vec![
        signed_presence(
            &target,
            &package,
            network_id.clone(),
            9,
            [2, 2],
            "",
            Utc::now().timestamp_millis(),
        ),
        signed_presence(
            &occupied,
            &package,
            network_id,
            1,
            [3, 2],
            "",
            Utc::now().timestamp_millis(),
        ),
    ];
    let (gateway, server) = presence_gateway(records, true).await;
    state.gateway_urls = vec![gateway];

    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"target_agent_id":target.agent_did}),
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    let moved: SignedSpacePresence = serde_json::from_value(result["presence"].clone()).unwrap();
    assert_eq!(moved.record.tile_at(package.walk_speed, i64::MAX), [2, 3]);
    server.abort();
}

#[tokio::test]
async fn missing_target_never_falls_back_to_an_arbitrary_tile() {
    let package = corridor_package(4);
    let (_dir, token, mut state) = app_with_package(&package).await;
    let (gateway, server) = presence_gateway(Vec::new(), false).await;
    state.gateway_urls = vec![gateway];
    let (before_networks, before_active_space) = {
        let presence = state.own_space_presence.lock().await;
        (presence.networks.clone(), presence.active_space.clone())
    };

    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"target_agent_id":"did:key:missing"}),
    )
    .await;
    assert_eq!(result["error"], "target agent presence not found");
    let presence = state.own_space_presence.lock().await;
    assert_eq!(presence.networks, before_networks);
    assert_eq!(presence.active_space, before_active_space);
    server.abort();
}

#[tokio::test]
async fn already_adjacent_target_stays_put_and_same_tile_target_uses_nearby_tile() {
    for (target_x, expected_tile) in [(1, [0, 0]), (0, [1, 0])] {
        let package = corridor_package(target_x);
        let (_dir, token, mut state) = app_with_package(&package).await;
        let target = Identity::new_random();
        let presence = signed_presence(
            &target,
            &package,
            test_network_id(&state),
            1,
            [target_x, 0],
            "",
            Utc::now().timestamp_millis(),
        );
        let (gateway, server) = presence_gateway(vec![presence], false).await;
        state.gateway_urls = vec![gateway];

        let result = authed_post_json(
            app(state.clone()),
            &token,
            "/v1/wattetheria/space/move",
            json!({"target_agent_id":target.agent_did}),
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");
        let moved: SignedSpacePresence =
            serde_json::from_value(result["presence"].clone()).unwrap();
        assert_eq!(
            moved.record.tile_at(package.walk_speed, i64::MAX),
            expected_tile
        );
        if target_x == 1 {
            assert_eq!(moved.record.path, "");
        }
        server.abort();
    }
}

#[tokio::test]
async fn explicit_tile_is_exact_and_must_be_walkable_and_reachable() {
    let package = corridor_package(4);
    let (_dir, token, state) = app_with_package(&package).await;
    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"tile":[3,0]}),
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    let moved: SignedSpacePresence = serde_json::from_value(result["presence"].clone()).unwrap();
    package.validate_record(&moved.record).unwrap();
    assert_eq!(moved.record.tile_at(package.walk_speed, i64::MAX), [3, 0]);
    assert_eq!(moved.record.destination_place_id, "corridor");

    for (package, tile) in [
        (corridor_package(4), [5, 0]),
        (disconnected_package(), [2, 0]),
        (disconnected_package(), [4, 0]),
    ] {
        let (_dir, token, state) = app_with_package(&package).await;
        let (before_networks, before_active_space) = {
            let presence = state.own_space_presence.lock().await;
            (presence.networks.clone(), presence.active_space.clone())
        };
        let result = authed_post_json(
            app(state.clone()),
            &token,
            "/v1/wattetheria/space/move",
            json!({"tile":tile}),
        )
        .await;
        assert!(result.get("error").is_some(), "{result}");
        let presence = state.own_space_presence.lock().await;
        assert_eq!(presence.networks, before_networks);
        assert_eq!(presence.active_space, before_active_space);
    }
}

#[tokio::test]
async fn explicit_tile_stays_exact_when_occupied() {
    let package = corridor_package(4);
    let (_dir, token, mut state) = app_with_package(&package).await;
    let network_id = test_network_id(&state);
    let tile = [3, 0];
    let occupied = signed_presence(
        &Identity::new_random(),
        &package,
        network_id,
        1,
        tile,
        "",
        Utc::now().timestamp_millis(),
    );
    let (gateway, server) = presence_gateway(vec![occupied], false).await;
    state.gateway_urls = vec![gateway];

    let result = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"tile":tile}),
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    let moved: SignedSpacePresence = serde_json::from_value(result["presence"].clone()).unwrap();
    assert_eq!(moved.record.tile_at(package.walk_speed, i64::MAX), tile);
    server.abort();
}
