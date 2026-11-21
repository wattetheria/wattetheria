//! The hunting game's wire contract: what a node signs and sends to the gateway, and how the
//! gateway answers. An agent walks into the wilds south of the Space, stands beside a monster
//! and fights it in one command; the gateway works out the whole fight, pays the drops and keeps
//! the record clients replay. The gateway is the only authority on monsters, fights, bags,
//! weapons and the smithy; a node only signs its agent's commands and relays the answers.

use crate::space::SignedSpacePresence;
use serde::{Deserialize, Serialize};

pub const GAME_ID: &str = "hunt";
/// `event_kind` prefix of the hunt's `game_activity` UI events, as in `hunt.fight`.
pub const EVENT_KIND_PREFIX: &str = "hunt.";

/// The four gems. Blessing and soul gems enhance a weapon, a chaos gem goes into the steel
/// sword, a diamond is only sold; the system buys any of them back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gem {
    Blessing,
    Soul,
    Chaos,
    Diamond,
}

impl Gem {
    pub const ALL: [Self; 4] = [Self::Blessing, Self::Soul, Self::Chaos, Self::Diamond];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blessing => "blessing",
            Self::Soul => "soul",
            Self::Chaos => "chaos",
            Self::Diamond => "diamond",
        }
    }
}

/// One hunt command. Every command carries the caller's `action_id`: retrying with the same id
/// returns the first result instead of acting twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HuntAction {
    /// Fight the monster at spawn point `monster_id`, standing on a tile beside it. The fight is
    /// worked out at once. `retreat_below` is the HP percentage at or under which the agent
    /// walks away; `hp_potion_below` the HP percentage under which it drinks an HP potion.
    Fight {
        monster_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retreat_below: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hp_potion_below: Option<u32>,
    },
    /// Pick up one thing a cliff wraith dropped on the ground, standing within one tile of it.
    Pick { item_id: i64 },
    /// Buy the training sword, standing in the smithy.
    SmithyBuy,
    /// Forge the next sword of the line, standing in the smithy.
    SmithyForge,
    /// Enhance the sword in use by one level with a gem, standing in the smithy.
    SmithyEnhance,
    /// Take `weapon`, one the agent owns, in hand for the fights to come, anywhere in the Space.
    /// A newly bought or forged sword is taken in hand by itself.
    Equip { weapon: String },
    /// Sell gems to the system, standing in the market.
    Sell { gem: Gem, count: u32 },
    /// Buy large HP potions, standing in the market.
    Buy { count: u32 },
}

impl HuntAction {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Fight { .. } => "fight",
            Self::Pick { .. } => "pick",
            Self::SmithyBuy => "smithy_buy",
            Self::SmithyForge => "smithy_forge",
            Self::SmithyEnhance => "smithy_enhance",
            Self::Equip { .. } => "equip",
            Self::Sell { .. } => "sell",
            Self::Buy { .. } => "buy",
        }
    }
}

/// What the agent's `did:key` signs (JCS) for `POST /api/hunt/actions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HuntActionPayload {
    pub action_id: String,
    pub agent_did: String,
    #[serde(flatten)]
    pub action: HuntAction,
    /// The agent's newest signed presence, so the gateway judges where it stands now rather
    /// than from a presence push that may still be on its way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<SignedSpacePresence>,
    /// Display name shown on the agent's fights and ledger entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// Why the gateway refused a hunt command. Serialized as the `error` code of the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HuntRejection {
    Invalid,
    NotInSpace,
    /// The caller is not standing still where the command needs it.
    NotAtPlace,
    Busy,
    /// No such monster, item, gem or weapon.
    Unknown,
    /// The caller is not on a tile beside the monster.
    NotBeside,
    /// The caller's Space level is below what the monster's zone needs.
    LevelTooLow,
    /// The caller has fought as many fights as it may today.
    NoStamina,
    /// Another agent is fighting this monster.
    Fighting,
    /// The monster is not back yet.
    Respawning,
    /// The cliff wraith was already defeated before this fight came to it.
    AlreadyDefeated,
    /// The thing on the ground was picked up by someone else first.
    Taken,
    /// The thing on the ground is gone.
    Vanished,
    /// The thing on the ground is still kept for the agents who fought for it.
    Protected,
    /// The caller is too far from the thing on the ground.
    TooFar,
    /// The caller picked or looked a moment ago; one a second.
    TooSoon,
    /// The caller cannot take Watt or gems from the hunt (not registered).
    NotEligible,
    /// The caller already has that weapon.
    AlreadyOwned,
    /// The caller lacks a weapon, material or gem the command needs.
    Missing,
    /// The weapon is at its highest enhancement.
    MaxLevel,
    /// The caller carries fewer than it wants to sell.
    InsufficientUnits,
    InsufficientBalance,
}

impl HuntRejection {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::NotInSpace => "not_in_space",
            Self::NotAtPlace => "not_at_place",
            Self::Busy => "busy",
            Self::Unknown => "unknown",
            Self::NotBeside => "not_beside",
            Self::LevelTooLow => "level_too_low",
            Self::NoStamina => "no_stamina",
            Self::Fighting => "fighting",
            Self::Respawning => "respawning",
            Self::AlreadyDefeated => "already_defeated",
            Self::Taken => "taken",
            Self::Vanished => "vanished",
            Self::Protected => "protected",
            Self::TooFar => "too_far",
            Self::TooSoon => "too_soon",
            Self::NotEligible => "not_eligible",
            Self::AlreadyOwned => "already_owned",
            Self::Missing => "missing",
            Self::MaxLevel => "max_level",
            Self::InsufficientUnits => "insufficient_units",
            Self::InsufficientBalance => "insufficient_balance",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_fight_flattens_its_tag_and_leaves_out_unset_lines() {
        let payload = HuntActionPayload {
            action_id: "a1".to_owned(),
            agent_did: "did:key:z".to_owned(),
            action: HuntAction::Fight {
                monster_id: "mossblob-1".to_owned(),
                retreat_below: Some(40),
                hp_potion_below: None,
            },
            presence: None,
            agent_label: None,
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            value,
            json!({"action_id": "a1", "agent_did": "did:key:z", "action": "fight",
                   "monster_id": "mossblob-1", "retreat_below": 40})
        );
        assert_eq!(
            serde_json::from_value::<HuntActionPayload>(value).unwrap(),
            payload
        );
    }

    #[test]
    fn names_and_codes_match_their_serde_names() {
        for (action, name) in [
            (HuntAction::Pick { item_id: 7 }, "pick"),
            (HuntAction::SmithyBuy, "smithy_buy"),
            (HuntAction::SmithyForge, "smithy_forge"),
            (HuntAction::SmithyEnhance, "smithy_enhance"),
            (
                HuntAction::Equip {
                    weapon: "copper_sword".to_owned(),
                },
                "equip",
            ),
            (
                HuntAction::Sell {
                    gem: Gem::Diamond,
                    count: 1,
                },
                "sell",
            ),
            (HuntAction::Buy { count: 2 }, "buy"),
        ] {
            assert_eq!(action.name(), name);
            assert_eq!(
                serde_json::to_value(&action).unwrap()["action"],
                json!(name)
            );
        }
        for gem in Gem::ALL {
            assert_eq!(serde_json::to_value(gem).unwrap(), json!(gem.as_str()));
        }
        for code in [
            HuntRejection::NotBeside,
            HuntRejection::NoStamina,
            HuntRejection::AlreadyDefeated,
            HuntRejection::TooSoon,
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), json!(code.as_str()));
        }
    }
}
