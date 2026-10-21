use super::*;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

fn test_package() -> SpacePackage {
    let package: SpacePackage =
        serde_json::from_str(include_str!("fixtures/the-ville.json")).unwrap();
    package.validate().unwrap();
    package
}

fn agent_did() -> String {
    let key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    watt_did::DidKey::from_ed25519_public_key(key.verifying_key().to_bytes())
        .unwrap()
        .did
        .to_string()
}

fn movement(package: &SpacePackage) -> SpacePresenceRecord {
    let did = agent_did();
    let from = package
        .destination_tile(&did, &package.spawn_place_id)
        .unwrap();
    let to = package
        .destination_tile(&did, "hobbs-cafe/cafe/piano")
        .unwrap();
    SpacePresenceRecord {
        agent_did: did,
        network_id: String::new(),
        space_id: package.space_id.clone(),
        space_version: package.space_version.clone(),
        entered: true,
        seq: 1,
        from_tile: from,
        path: package.path(from, to).unwrap(),
        depart_at: 1_760_000_000_000,
        destination_place_id: "hobbs-cafe/cafe/piano".to_owned(),
        activity: "play piano".to_owned(),
        object_place_id: Some("hobbs-cafe/cafe/piano".to_owned()),
        updated_at: 1_760_000_000_000,
    }
}

#[test]
fn previous_signed_presence_roundtrips_and_entered_is_signature_bound() {
    use crate::identity::Identity;
    use crate::signing::{canonical_bytes, sign_payload, verify_payload};
    use wattetheria_gateway_contract::SignedSpacePresence;

    let package = test_package();
    let record = movement(&package);
    let identity = Identity::from_ed25519_seed(&record.agent_did, [42; 32]).unwrap();
    let original = serde_json::json!({
        "agent_did": record.agent_did,
        "space_id": record.space_id,
        "space_version": record.space_version,
        "seq": record.seq,
        "from_tile": record.from_tile,
        "path": record.path,
        "depart_at": record.depart_at,
        "destination_place_id": record.destination_place_id,
        "activity": record.activity,
        "object_place_id": record.object_place_id,
        "updated_at": record.updated_at,
    });
    assert!(original.get("entered").is_none());
    let signature = sign_payload(&original, &identity).unwrap();
    let signed_json = serde_json::json!({
        "record": original,
        "public_key": identity.public_key,
        "signature": signature,
    });
    let signed: SignedSpacePresence = serde_json::from_value(signed_json.clone()).unwrap();
    assert!(!signed.record.entered);
    assert_eq!(serde_json::to_value(&signed).unwrap(), signed_json);
    assert_eq!(
        canonical_bytes(&signed.record).unwrap(),
        canonical_bytes(&original).unwrap()
    );
    assert!(verify_payload(&signed.record, &signed.signature, &signed.public_key).unwrap());

    let mut entered = signed.record;
    entered.entered = true;
    assert!(!verify_payload(&entered, &signed.signature, &signed.public_key).unwrap());
    let entered_signature = sign_payload(&entered, &identity).unwrap();
    assert!(verify_payload(&entered, &entered_signature, &signed.public_key).unwrap());
    entered.entered = false;
    assert!(!verify_payload(&entered, &entered_signature, &signed.public_key).unwrap());
}

#[test]
fn presence_activity_validates_unicode_length_and_control_characters() {
    use wattetheria_gateway_contract::space::validate_activity;

    let mut activity = "\u{754c}".repeat(2_000);
    validate_activity(&activity).unwrap();
    activity.push('\u{754c}');
    assert_eq!(
        validate_activity(&activity),
        Err("activity exceeds 2000 characters")
    );
    for control in ['\0', '\u{1b}', '\u{7f}', '\u{85}'] {
        assert_eq!(
            validate_activity(&format!("activity{control}")),
            Err("activity contains unsupported control characters")
        );
    }
    validate_activity("\n\r\t").unwrap();
}

#[test]
fn structural_presence_validation_preserves_legacy_activity_and_geometry_checks() {
    let package = test_package();
    let mut record = movement(&package);
    for activity in ["\u{754c}".repeat(2_001), "legacy\0activity".to_owned()] {
        record.activity.clone_from(&activity);
        assert!(wattetheria_gateway_contract::space::validate_activity(&record.activity).is_err());
        package.validate_record(&record).unwrap();
        let decoded: SpacePresenceRecord =
            serde_json::from_value(serde_json::to_value(&record).unwrap()).unwrap();
        assert_eq!(decoded, record);
        assert_eq!(decoded.activity, activity);
    }
    record.from_tile = [u32::MAX, u32::MAX];
    assert_eq!(
        package.validate_record(&record).unwrap_err().to_string(),
        "presence starts on blocked or out-of-bounds tile"
    );
}

#[test]
fn generated_package_loads_with_original_tiles_and_version() {
    let package = test_package();
    assert_eq!(package.size, [140, 100]);
    assert_eq!(package.places.len(), 305);
    assert_eq!(package.service_bindings, []);
    assert_eq!(package.spawn_place_id, "johnson-park");
    assert_eq!(package.tile_size, 32);
    assert_eq!(package.walk_speed, 2.0);
    assert_eq!(package.movement_search_radius, Some(2));
    assert_eq!(
        package.space_version,
        "sha256:9fbcfdf85be3ae410be47a3f8349df50e58c15c75ef593bbb716fbe809347ab5"
    );
    assert_eq!(
        package
            .walkable_grid
            .iter()
            .flatten()
            .filter(|cell| **cell == 1)
            .count(),
        11_936
    );
    for (kind, expected) in [
        (PlaceKind::World, 1),
        (PlaceKind::Sector, 19),
        (PlaceKind::Arena, 63),
        (PlaceKind::GameObject, 222),
    ] {
        assert_eq!(
            package
                .places
                .iter()
                .filter(|place| place.kind == kind)
                .count(),
            expected
        );
    }
    let json = serde_json::to_string(&package).unwrap();
    let decoded: SpacePackage = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, package);
    decoded.validate().unwrap();
}

#[test]
fn original_grid_keeps_all_seventeen_components_and_park_candidates() {
    let package = test_package();
    let mut seen = vec![false; 14_000];
    let mut sizes = Vec::new();
    for y in 0..package.size[1] {
        for x in 0..package.size[0] {
            let tile = [x, y];
            if !package.is_walkable(tile) || seen[package.index(tile)] {
                continue;
            }
            let mut queue = VecDeque::from([tile]);
            seen[package.index(tile)] = true;
            let mut count = 0;
            while let Some(tile) = queue.pop_front() {
                count += 1;
                for direction in *b"URDL" {
                    let Some(next) = package.step(tile, direction) else {
                        continue;
                    };
                    if !seen[package.index(next)] {
                        seen[package.index(next)] = true;
                        queue.push_back(next);
                    }
                }
            }
            sizes.push(count);
        }
    }
    assert_eq!(
        sizes,
        [7519, 4158, 1, 1, 3, 6, 6, 4, 76, 70, 64, 1, 12, 6, 3, 3, 3]
    );
    let reachable = package.spawn_reachable().unwrap();
    assert_eq!(reachable.iter().filter(|cell| **cell).count(), 4158);
    let did = agent_did();
    let spawn = package.destination_tile(&did, "johnson-park").unwrap();
    for place in &package.places {
        let destination = package.destination_tile(&did, &place.place_id).unwrap();
        assert!(reachable[package.index(destination)], "{}", place.place_id);
        assert!(place.tiles.contains(&destination));
        let route = package.path(spawn, destination).unwrap();
        let mut tile = spawn;
        for direction in route.bytes() {
            tile = package.step(tile, direction).unwrap();
        }
        assert_eq!(tile, destination);
    }
    assert!(package.is_walkable([0, 0]));
    assert!(!reachable[package.index([0, 0])]);
    assert!(package.path(spawn, [0, 0]).is_err());
}

#[test]
fn destination_hash_uses_big_endian_first_eight_bytes_and_sorted_tiles() {
    let mut package = test_package();
    let did = agent_did();
    let place_id = "johnson-park";
    let reachable = package.spawn_reachable().unwrap();
    let mut tiles: Vec<_> = package
        .place(place_id)
        .unwrap()
        .tiles
        .iter()
        .copied()
        .filter(|tile| reachable[package.index(*tile)])
        .collect();
    tiles.sort_by_key(|tile| (tile[1], tile[0]));
    let bytes = Sha256::digest(format!("{did}\n{place_id}"));
    let index = u64::from_be_bytes(bytes[..8].try_into().unwrap()) % tiles.len() as u64;
    let expected = tiles[usize::try_from(index).unwrap()];
    assert_eq!(package.destination_tile(&did, place_id).unwrap(), expected);
    package
        .places
        .iter_mut()
        .find(|place| place.place_id == place_id)
        .unwrap()
        .tiles
        .reverse();
    assert_eq!(package.destination_tile(&did, place_id).unwrap(), expected);
    assert!(package.destination_tile(&did, "not-a-place").is_err());
}

#[test]
fn routing_follows_shortest_cardinal_path_and_rejects_invalid_endpoints() {
    let package = test_package();
    assert_eq!(package.path([1, 1], [1, 0]).unwrap(), "U");
    assert_eq!(package.path([1, 1], [2, 1]).unwrap(), "R");
    assert_eq!(package.path([1, 1], [1, 2]).unwrap(), "D");
    assert_eq!(package.path([1, 1], [0, 1]).unwrap(), "L");
    assert_eq!(package.path([1, 1], [2, 0]).unwrap(), "UR");
    assert_eq!(package.path([79, 12], [81, 12]).unwrap(), "URRD");
    assert_eq!(package.path([1, 1], [1, 1]).unwrap(), "");
    assert!(package.path([140, 1], [1, 1]).is_err());
    assert!(package.path([1, 1], [1, 100]).is_err());
    assert!(package.path([80, 12], [1, 1]).is_err());
    assert!(package.path([1, 1], [80, 12]).is_err());
}

#[test]
fn package_validation_rejects_content_version_and_grid_hierarchy_errors() {
    let package = test_package();
    let mut changed = package.clone();
    changed.space_version = "sha256:wrong".to_owned();
    assert!(
        changed
            .validate()
            .unwrap_err()
            .to_string()
            .contains("version")
    );
    let mut changed = package.clone();
    changed.walk_speed = 3.0;
    assert!(
        changed
            .validate()
            .unwrap_err()
            .to_string()
            .contains("version")
    );
    let mut changed = package.clone();
    changed.walkable_grid.pop();
    assert!(changed.validate().unwrap_err().to_string().contains("grid"));
    let mut changed = package.clone();
    changed.places[0].tiles.push([140, 0]);
    assert!(
        changed
            .validate()
            .unwrap_err()
            .to_string()
            .contains("out-of-bounds")
    );
    let mut changed = package.clone();
    changed.places[0].tiles.push([80, 12]);
    assert!(
        changed
            .validate()
            .unwrap_err()
            .to_string()
            .contains("blocked")
    );
    let mut changed = package;
    changed
        .places
        .iter_mut()
        .find(|place| place.kind == PlaceKind::Arena)
        .unwrap()
        .parent_place_id = Some("missing".to_owned());
    assert!(
        changed
            .validate()
            .unwrap_err()
            .to_string()
            .contains("parent")
    );
}

#[test]
fn presence_route_validation_and_utc_projection_use_real_package() {
    let package = test_package();
    let record = movement(&package);
    package.validate_record(&record).unwrap();
    assert!(record.path.len() > 2);
    assert_eq!(
        record.tile_at(package.walk_speed, record.depart_at - 1),
        record.from_tile
    );
    assert_eq!(
        record.position_at(package.walk_speed, record.depart_at - 1),
        record.from_tile.map(f64::from)
    );
    let first = package
        .step(record.from_tile, record.path.as_bytes()[0])
        .unwrap();
    let midpoint = [
        f64::from(record.from_tile[0] + first[0]) / 2.0,
        f64::from(record.from_tile[1] + first[1]) / 2.0,
    ];
    assert_eq!(
        record.position_at(package.walk_speed, record.depart_at + 250),
        midpoint
    );
    assert_eq!(
        record.tile_at(package.walk_speed, record.depart_at + 500),
        first
    );
    let arrival = record.depart_at + i64::try_from(record.path.len()).unwrap() * 500;
    let expected = package
        .destination_tile(&record.agent_did, &record.destination_place_id)
        .unwrap();
    assert_eq!(record.tile_at(package.walk_speed, arrival), expected);
    assert_eq!(
        record.position_at(package.walk_speed, arrival + 1_000_000),
        expected.map(f64::from)
    );
}

#[test]
fn presence_rejects_wrong_space_version_sequence_places_and_routes() {
    let package = test_package();
    let record = movement(&package);
    let mut invalid = record.clone();
    invalid.space_id = "missing".to_owned();
    assert!(
        package
            .validate_record(&invalid)
            .unwrap_err()
            .to_string()
            .contains("space ID")
    );
    let mut invalid = record.clone();
    invalid.space_version = "sha256:wrong".to_owned();
    assert!(
        package
            .validate_record(&invalid)
            .unwrap_err()
            .to_string()
            .contains("version")
    );
    let mut invalid = record.clone();
    invalid.seq = 0;
    assert!(
        package
            .validate_record(&invalid)
            .unwrap_err()
            .to_string()
            .contains("sequence")
    );
    let mut invalid = record.clone();
    invalid.destination_place_id = "missing".to_owned();
    assert!(
        package
            .validate_record(&invalid)
            .unwrap_err()
            .to_string()
            .contains("destination")
    );
    let mut invalid = record.clone();
    invalid.from_tile = [140, 0];
    assert!(package.validate_record(&invalid).is_err());
    let mut invalid = record.clone();
    invalid.path = "X".to_owned();
    assert!(package.validate_record(&invalid).is_err());
    let mut invalid = record.clone();
    invalid.from_tile = [79, 12];
    invalid.path = "R".to_owned();
    assert!(package.validate_record(&invalid).is_err());
    let mut invalid = record.clone();
    invalid.from_tile = [0, 0];
    invalid.path = "U".to_owned();
    assert!(package.validate_record(&invalid).is_err());
    let mut invalid = record.clone();
    invalid.path.clear();
    assert!(
        package
            .validate_record(&invalid)
            .unwrap_err()
            .to_string()
            .contains("end at destination")
    );
    let mut invalid = record.clone();
    invalid.object_place_id = Some("missing".to_owned());
    assert!(
        package
            .validate_record(&invalid)
            .unwrap_err()
            .to_string()
            .contains("object")
    );
    let mut invalid = record.clone();
    invalid.object_place_id = Some("hobbs-cafe/cafe".to_owned());
    assert!(
        package
            .validate_record(&invalid)
            .unwrap_err()
            .to_string()
            .contains("game object")
    );
    let mut invalid = record;
    invalid.object_place_id = Some("johnson-park/park/park-garden".to_owned());
    assert!(
        package
            .validate_record(&invalid)
            .unwrap_err()
            .to_string()
            .contains("destination tile")
    );
}

#[test]
fn places_at_tile_returns_world_to_deepest_object() {
    let package = test_package();
    let tile = [82, 26];
    let places = package.places_at_tile(tile);
    assert_eq!(
        places
            .iter()
            .map(|place| place.place_id.as_str())
            .collect::<Vec<_>>(),
        [
            "the-ville",
            "hobbs-cafe",
            "hobbs-cafe/cafe",
            "hobbs-cafe/cafe/piano"
        ]
    );
    assert_eq!(
        package.place_at_tile(tile).unwrap().place_id,
        "hobbs-cafe/cafe/piano"
    );
    assert_eq!(package.places_at_tile([80, 12]), [] as [&Place; 0]);
    assert!(package.place_at_tile([140, 100]).is_none());
}

#[test]
fn world_event_requires_known_space_place_and_valid_time_interval() {
    let package = test_package();
    let event = WorldEvent {
        event_id: "park-meeting-2026-10-12".to_owned(),
        space_id: package.space_id.clone(),
        title: "Park meeting".to_owned(),
        description: "Meet at the garden".to_owned(),
        place_id: "johnson-park/park".to_owned(),
        starts_at: 1_760_000_000_000,
        ends_at: 1_760_000_060_000,
    };
    package.validate_world_event(&event).unwrap();
    let mut invalid = event.clone();
    invalid.space_id = "missing".to_owned();
    assert!(package.validate_world_event(&invalid).is_err());
    let mut invalid = event.clone();
    invalid.place_id = "missing".to_owned();
    assert!(package.validate_world_event(&invalid).is_err());
    let mut invalid = event.clone();
    invalid.ends_at = invalid.starts_at;
    assert!(package.validate_world_event(&invalid).is_err());
    invalid.ends_at -= 1;
    assert!(package.validate_world_event(&invalid).is_err());
    let mut invalid = event.clone();
    invalid.title.clear();
    assert!(package.validate_world_event(&invalid).is_err());
    let mut invalid = event;
    invalid.event_id.clear();
    assert!(package.validate_world_event(&invalid).is_err());
}
