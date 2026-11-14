//! The housing game's wire contract: what a node signs and sends to the gateway, and how the
//! gateway answers. An agent buys a lot in the residential district and builds on it freely,
//! one thing per command, standing at the lot's gate. Any agent may walk into a bought lot from
//! its gate and stay there. The gateway is the only authority on lots, what stands on them, who
//! is inside and the element catalog; a node only signs its agent's commands and relays the
//! answers.

use crate::space::SignedSpacePresence;
use serde::{Deserialize, Serialize};

pub const GAME_ID: &str = "housing";
/// `event_kind` prefix of the housing `game_activity` UI events, as in `housing.placed`.
pub const EVENT_KIND_PREFIX: &str = "housing.";

/// The four layers a lot cell can hold, bottom to top. A cell holds at most one thing per layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Floor, paths, soil: laid one cell at a time.
    Ground,
    /// Walls and windows: raised one cell at a time.
    Wall,
    /// Indoor pieces, which may cover several cells; wall-mounted pieces hang on a wall cell.
    Furniture,
    /// Outdoor pieces: trees, flowers, fences, wells.
    Garden,
}

impl Layer {
    pub const ALL: [Self; 4] = [Self::Ground, Self::Wall, Self::Furniture, Self::Garden];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ground => "ground",
            Self::Wall => "wall",
            Self::Furniture => "furniture",
            Self::Garden => "garden",
        }
    }
}

/// The way a piece's front looks on the map. Only some elements can be turned; the element
/// catalog lists the ways each one can face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Facing {
    /// Toward the bottom of the map: its front is seen.
    Down,
    Left,
    Right,
    /// Toward the top of the map: its back is seen.
    Up,
}

impl Facing {
    pub const ALL: [Self; 4] = [Self::Down, Self::Left, Self::Right, Self::Up];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
        }
    }
}

/// One housing command. Every command carries the caller's `action_id`: retrying with the same
/// id returns the first result instead of acting twice. `cell` is counted inside the lot, from
/// its top-left corner `[0, 0]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HousingAction {
    /// Buy a free lot at its fixed price, standing at its gate.
    BuyLot { lot: u32 },
    /// Put one element on the caller's lot with its top-left cell at `cell`, standing at the
    /// gate. Uses one from the caller's store when it has one, otherwise buys one. `facing`
    /// turns an element that can be turned; left out, it faces its usual way.
    Place {
        element: String,
        cell: [u32; 2],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        facing: Option<Facing>,
    },
    /// Take the topmost thing at `cell` off the caller's lot back into its store, standing at
    /// the gate.
    Remove { cell: [u32; 2] },
    /// Write the name on the caller's lot sign, standing at the gate.
    Name { name: String },
    /// Walk into a bought lot, the caller's own or anyone's, standing at its gate, and stay on
    /// `cell`; sent again from inside, walk on to another cell. Without `cell` the gateway picks
    /// the nearest free cell, indoors when the lot has floor.
    Enter {
        lot: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cell: Option<[u32; 2]>,
    },
}

impl HousingAction {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::BuyLot { .. } => "buy_lot",
            Self::Place { .. } => "place",
            Self::Remove { .. } => "remove",
            Self::Name { .. } => "name",
            Self::Enter { .. } => "enter",
        }
    }
}

/// What the agent's `did:key` signs (JCS) for `POST /api/housing/actions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HousingActionPayload {
    pub action_id: String,
    pub agent_did: String,
    #[serde(flatten)]
    pub action: HousingAction,
    /// The agent's newest signed presence, so the gateway judges where it stands now rather
    /// than from a presence push that may still be on its way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<SignedSpacePresence>,
    /// Display name shown on the agent's lot and ledger entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// Why the gateway refused a housing command. Serialized as the `error` code of the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HousingRejection {
    Invalid,
    NotInSpace,
    /// The caller is not standing still on the lot's gate.
    NotAtGate,
    Busy,
    /// The caller's Space level is below what the lot or element needs.
    LevelTooLow,
    /// The caller already owns as many lots as one agent may.
    AlreadyOwnsLot,
    LotTaken,
    /// The caller owns no lot.
    NoLot,
    /// Nobody owns the lot, so there is nothing to walk into.
    LotFree,
    UnknownElement,
    /// The element cannot face that way.
    UnknownFacing,
    /// Part of the element would stand outside the lot.
    OutsideLot,
    /// Something already takes that layer on one of the cells, or stands where the caller wants
    /// to stay.
    Occupied,
    /// Indoor pieces stand on floor.
    NeedsFloor,
    /// Outdoor pieces do not stand on floor.
    OnFloor,
    /// Wall-mounted pieces hang on a wall.
    NeedsWall,
    /// No free cell beside the spot, or the cell the caller wants to stay on, can be walked to
    /// from the gate.
    Unreachable,
    /// Nothing stands at the cell.
    Empty,
    InsufficientBalance,
}

impl HousingRejection {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::NotInSpace => "not_in_space",
            Self::NotAtGate => "not_at_gate",
            Self::Busy => "busy",
            Self::LevelTooLow => "level_too_low",
            Self::AlreadyOwnsLot => "already_owns_lot",
            Self::LotTaken => "lot_taken",
            Self::NoLot => "no_lot",
            Self::LotFree => "lot_free",
            Self::UnknownElement => "unknown_element",
            Self::UnknownFacing => "unknown_facing",
            Self::OutsideLot => "outside_lot",
            Self::Occupied => "occupied",
            Self::NeedsFloor => "needs_floor",
            Self::OnFloor => "on_floor",
            Self::NeedsWall => "needs_wall",
            Self::Unreachable => "unreachable",
            Self::Empty => "empty",
            Self::InsufficientBalance => "insufficient_balance",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn action_payload_flattens_the_action_tag() {
        let payload = HousingActionPayload {
            action_id: "a1".to_owned(),
            agent_did: "did:key:z".to_owned(),
            action: HousingAction::Place {
                element: "sofa_blue".to_owned(),
                cell: [3, 4],
                facing: None,
            },
            presence: None,
            agent_label: None,
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            value,
            json!({"action_id": "a1", "agent_did": "did:key:z", "action": "place",
                   "element": "sofa_blue", "cell": [3, 4]})
        );
        assert_eq!(
            serde_json::from_value::<HousingActionPayload>(value).unwrap(),
            payload
        );
    }

    #[test]
    fn placing_may_turn_the_piece() {
        let turned: HousingAction = serde_json::from_value(
            json!({"action": "place", "element": "chair", "cell": [1, 2], "facing": "up"}),
        )
        .unwrap();
        assert_eq!(
            turned,
            HousingAction::Place {
                element: "chair".to_owned(),
                cell: [1, 2],
                facing: Some(Facing::Up),
            }
        );
        assert_eq!(
            serde_json::to_value(&turned).unwrap(),
            json!({"action": "place", "element": "chair", "cell": [1, 2], "facing": "up"})
        );
        assert!(
            serde_json::from_value::<HousingAction>(
                json!({"action": "place", "element": "chair", "cell": [1, 2], "facing": "north"})
            )
            .is_err()
        );
        for facing in Facing::ALL {
            assert_eq!(
                serde_json::to_value(facing).unwrap(),
                json!(facing.as_str())
            );
        }
    }

    #[test]
    fn entering_names_a_lot_and_may_leave_the_cell_to_the_gateway() {
        let free: HousingAction =
            serde_json::from_value(json!({"action": "enter", "lot": 7})).unwrap();
        assert_eq!(free, HousingAction::Enter { lot: 7, cell: None });
        assert_eq!(
            serde_json::to_value(&free).unwrap(),
            json!({"action": "enter", "lot": 7})
        );
        let chosen = HousingAction::Enter {
            lot: 7,
            cell: Some([3, 4]),
        };
        assert_eq!(
            serde_json::to_value(&chosen).unwrap(),
            json!({"action": "enter", "lot": 7, "cell": [3, 4]})
        );
    }

    #[test]
    fn names_and_codes_match_their_serde_names() {
        for (action, name) in [
            (HousingAction::BuyLot { lot: 1 }, "buy_lot"),
            (HousingAction::Remove { cell: [0, 0] }, "remove"),
            (
                HousingAction::Name {
                    name: "Home".to_owned(),
                },
                "name",
            ),
            (HousingAction::Enter { lot: 7, cell: None }, "enter"),
        ] {
            assert_eq!(action.name(), name);
            assert_eq!(
                serde_json::to_value(&action).unwrap()["action"],
                json!(name)
            );
        }
        for layer in Layer::ALL {
            assert_eq!(serde_json::to_value(layer).unwrap(), json!(layer.as_str()));
        }
        for code in [
            HousingRejection::NotAtGate,
            HousingRejection::LevelTooLow,
            HousingRejection::AlreadyOwnsLot,
            HousingRejection::LotFree,
            HousingRejection::UnknownFacing,
            HousingRejection::Unreachable,
            HousingRejection::Empty,
            HousingRejection::InsufficientBalance,
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), json!(code.as_str()));
        }
    }
}
