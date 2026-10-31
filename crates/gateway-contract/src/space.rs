use serde::{Deserialize, Serialize};

pub const SPACE_AVATAR_COUNT: u8 = 25;
/// Length of a custom skin id: a lowercase hex SHA-256 digest.
pub const SKIN_ID_LEN: usize = 64;

/// A custom skin id is exactly the content hash the gateway computed when the skin was published.
pub fn validate_skin_id(skin_id: &str) -> Result<(), &'static str> {
    if skin_id.len() != SKIN_ID_LEN
        || !skin_id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("skin_id must be a 64 character lowercase hex content hash");
    }
    Ok(())
}

pub fn validate_activity(activity: &str) -> Result<(), &'static str> {
    if activity.chars().count() > 2_000 {
        return Err("activity exceeds 2000 characters");
    }
    if activity
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err("activity contains unsupported control characters");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePresence {
    pub node_id: String,
    pub agent_did: String,
    pub last_heartbeat_ms: i64,
    pub heartbeat_interval_sec: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpacePresenceRecord {
    pub agent_did: String,
    /// Built-in skin index (`0..SPACE_AVATAR_COUNT`), also the fallback while a custom skin loads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_id: Option<u8>,
    /// Custom skin published to the gateway: the SHA-256 content id of its spritesheet. The
    /// gateway only accepts it from the skin's current owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin_id: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub network_id: String,
    pub space_id: String,
    pub space_version: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub entered: bool,
    pub seq: u64,
    pub from_tile: [u32; 2],
    pub path: String,
    pub depart_at: i64,
    pub destination_place_id: String,
    pub activity: String,
    pub object_place_id: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedSpacePresence {
    pub record: SpacePresenceRecord,
    pub public_key: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEvent {
    pub event_id: String,
    pub space_id: String,
    pub title: String,
    pub description: String,
    pub place_id: String,
    pub starts_at: i64,
    pub ends_at: i64,
}

impl SpacePresenceRecord {
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn position_at(&self, walk_speed: f64, now_ms: i64) -> [f64; 2] {
        let steps = (now_ms.saturating_sub(self.depart_at).max(0) as f64 / 1_000.0 * walk_speed)
            .min(self.path.len() as f64);
        let mut tile = [f64::from(self.from_tile[0]), f64::from(self.from_tile[1])];
        for (index, step) in self.path.bytes().enumerate() {
            let progress = (steps - index as f64).clamp(0.0, 1.0);
            match step {
                b'U' => tile[1] -= progress,
                b'R' => tile[0] += progress,
                b'D' => tile[1] += progress,
                b'L' => tile[0] -= progress,
                _ => break,
            }
            if progress < 1.0 {
                break;
            }
        }
        tile
    }

    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn tile_at(&self, walk_speed: f64, now_ms: i64) -> [u32; 2] {
        let steps = (now_ms.saturating_sub(self.depart_at).max(0) as f64 / 1_000.0 * walk_speed)
            .floor()
            .min(self.path.len() as f64) as usize;
        let mut tile = self.from_tile;
        for step in self.path.bytes().take(steps) {
            match step {
                b'U' => tile[1] = tile[1].saturating_sub(1),
                b'R' => tile[0] = tile[0].saturating_add(1),
                b'D' => tile[1] = tile[1].saturating_add(1),
                b'L' => tile[0] = tile[0].saturating_sub(1),
                _ => break,
            }
        }
        tile
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MovementVectors {
        schema_version: u32,
        cases: Vec<MovementCase>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MovementCase {
        id: String,
        from_tile: [u32; 2],
        path: String,
        depart_at: i64,
        walk_speed: f64,
        now_ms: i64,
        expected: MovementExpected,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MovementExpected {
        position: [f64; 2],
        tile: [u32; 2],
    }

    const ORIGINAL_RECORD_JSON: &str = r#"{"agent_did":"agent","space_id":"the-ville","space_version":"sha256:version","seq":1,"from_tile":[10,10],"path":"URDD","depart_at":10000,"destination_place_id":"park","activity":"","object_place_id":null,"updated_at":10000}"#;

    #[test]
    fn signed_presence_without_entered_preserves_original_payload() {
        let original = format!(
            r#"{{"record":{ORIGINAL_RECORD_JSON},"public_key":"key","signature":"signature"}}"#
        );
        let signed: SignedSpacePresence = serde_json::from_str(&original).expect("signed presence");
        assert!(!signed.record.entered);
        assert_eq!(
            serde_json::to_string(&signed.record).expect("record payload"),
            ORIGINAL_RECORD_JSON
        );
        assert_eq!(
            serde_json::to_string(&signed).expect("signed payload"),
            original
        );
    }

    #[test]
    fn entered_presence_serializes_signed_payload_field() {
        let mut record: SpacePresenceRecord =
            serde_json::from_str(ORIGINAL_RECORD_JSON).expect("record");
        record.entered = true;
        let payload = serde_json::to_value(&record).expect("record payload");
        assert_eq!(payload["entered"], true);
        assert_ne!(
            serde_json::to_string(&record).expect("record payload"),
            ORIGINAL_RECORD_JSON
        );
        assert!(
            serde_json::from_value::<SpacePresenceRecord>(payload)
                .expect("record roundtrip")
                .entered
        );
    }

    #[test]
    fn utc_position_does_not_depend_on_receive_time() {
        let record = SpacePresenceRecord {
            agent_did: "agent".to_owned(),
            avatar_id: None,
            skin_id: None,
            network_id: "network".to_owned(),
            space_id: "the-ville".to_owned(),
            space_version: "sha256:version".to_owned(),
            entered: true,
            seq: 1,
            from_tile: [10, 10],
            path: "URDD".to_owned(),
            depart_at: 10_000,
            destination_place_id: "park".to_owned(),
            activity: String::new(),
            object_place_id: None,
            updated_at: 10_000,
        };
        assert_eq!(record.position_at(2.0, 9_000), [10.0, 10.0]);
        assert_eq!(record.position_at(2.0, 10_250), [10.0, 9.5]);
        assert_eq!(record.position_at(2.0, 11_000), [11.0, 9.0]);
        assert_eq!(record.position_at(2.0, 30_000), [11.0, 11.0]);
        assert_eq!(record.tile_at(2.0, 10_250), [10, 10]);
        assert_eq!(record.tile_at(2.0, 11_000), [11, 9]);
    }

    #[test]
    fn shared_movement_vectors_match_position_and_tile() {
        let vectors: MovementVectors =
            serde_json::from_str(include_str!("space_movement_vectors.json"))
                .expect("complete movement vector schema");
        assert_eq!(vectors.schema_version, 1);
        let mut ids = std::collections::BTreeSet::new();
        for case in vectors.cases {
            assert!(ids.insert(case.id.clone()), "duplicate case {}", case.id);
            assert!(case.walk_speed.is_finite() && case.walk_speed > 0.0);
            assert!(case.path.bytes().all(|step| b"URDL".contains(&step)));
            let record = SpacePresenceRecord {
                from_tile: case.from_tile,
                path: case.path,
                depart_at: case.depart_at,
                ..serde_json::from_str(ORIGINAL_RECORD_JSON).expect("record")
            };
            let position = record.position_at(case.walk_speed, case.now_ms);
            for (actual, expected) in position.into_iter().zip(case.expected.position) {
                assert!(
                    (actual - expected).abs() < 1e-9,
                    "{}: {position:?}",
                    case.id
                );
            }
            assert_eq!(
                record.tile_at(case.walk_speed, case.now_ms),
                case.expected.tile,
                "{}",
                case.id
            );
        }
        assert_eq!(
            ids,
            std::collections::BTreeSet::from(
                [
                    "up-half",
                    "right-half",
                    "down-half",
                    "left-half",
                    "turn-half",
                    "exact-boundary",
                    "before-boundary",
                    "after-boundary",
                    "before-departure",
                    "at-departure",
                    "empty-path",
                    "exact-end",
                    "late-receipt",
                    "fractional-speed",
                ]
                .map(str::to_owned)
            )
        );
    }
}
