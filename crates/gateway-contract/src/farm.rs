//! The farm game's wire contract: what a node signs and sends to the gateway, and how the
//! gateway answers. The gateway is the only authority on farm state and holds its rules and the
//! field's place on the map; a node only signs its agent's commands and relays the answers.

use crate::space::SignedSpacePresence;
use serde::{Deserialize, Serialize};

pub const GAME_ID: &str = "farm";
/// `event_kind` prefix of the farm's `game_activity` UI events, as in `farm.planted`.
pub const EVENT_KIND_PREFIX: &str = "farm.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Crop {
    Barley,
    CoffeeBean,
}

impl Crop {
    pub const ALL: [Self; 2] = [Self::Barley, Self::CoffeeBean];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Barley => "barley",
            Self::CoffeeBean => "coffee_bean",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|crop| crop.as_str() == value)
    }
}

/// One farm command. Every command carries the caller's `action_id`: retrying with the same id
/// returns the first result instead of acting twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum FarmAction {
    /// Join the season by claiming one whole parcel of the field, standing on it or beside it.
    ClaimPlots { parcel: u32 },
    /// Buy seed and plant it, standing on the plot.
    Plant { tile: [u32; 2], crop: Crop },
    /// Water a dry plot, standing on it.
    Water { tile: [u32; 2] },
    /// Harvest a ripe plot into the inventory, standing on it.
    Harvest { tile: [u32; 2] },
    /// Sell crops to the system, standing inside the buyback place.
    Sell { crop: Crop, units: u32 },
}

impl FarmAction {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::ClaimPlots { .. } => "claim_plots",
            Self::Plant { .. } => "plant",
            Self::Water { .. } => "water",
            Self::Harvest { .. } => "harvest",
            Self::Sell { .. } => "sell",
        }
    }
}

/// What the agent's `did:key` signs (JCS) for `POST /api/farm/actions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FarmActionPayload {
    pub action_id: String,
    pub agent_did: String,
    #[serde(flatten)]
    pub action: FarmAction,
    /// The agent's newest signed presence, so the gateway judges where it stands now rather
    /// than from a presence push that may still be on its way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<SignedSpacePresence>,
    /// Display name shown on the agent's plots and ledger entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// Why the gateway refused a farm command. Serialized as the `error` code of the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FarmRejection {
    Invalid,
    NotInSpace,
    NotAtPlace,
    NotAtTile,
    Busy,
    AlreadyJoined,
    NotJoined,
    SeasonEnding,
    NoFreePlots,
    PlotsTaken,
    NotOwner,
    NotEmpty,
    NotGrowing,
    AlreadyWet,
    NotReady,
    InsufficientBalance,
    InsufficientUnits,
}

impl FarmRejection {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::NotInSpace => "not_in_space",
            Self::NotAtPlace => "not_at_place",
            Self::NotAtTile => "not_at_tile",
            Self::Busy => "busy",
            Self::AlreadyJoined => "already_joined",
            Self::NotJoined => "not_joined",
            Self::SeasonEnding => "season_ending",
            Self::NoFreePlots => "no_free_plots",
            Self::PlotsTaken => "plots_taken",
            Self::NotOwner => "not_owner",
            Self::NotEmpty => "not_empty",
            Self::NotGrowing => "not_growing",
            Self::AlreadyWet => "already_wet",
            Self::NotReady => "not_ready",
            Self::InsufficientBalance => "insufficient_balance",
            Self::InsufficientUnits => "insufficient_units",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn action_payload_flattens_the_action_tag() {
        let payload = FarmActionPayload {
            action_id: "a1".to_owned(),
            agent_did: "did:key:z".to_owned(),
            action: FarmAction::Plant {
                tile: [60, 90],
                crop: Crop::CoffeeBean,
            },
            presence: None,
            agent_label: None,
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            value,
            json!({"action_id": "a1", "agent_did": "did:key:z", "action": "plant",
                   "tile": [60, 90], "crop": "coffee_bean"})
        );
        assert_eq!(
            serde_json::from_value::<FarmActionPayload>(value).unwrap(),
            payload
        );
    }
}
