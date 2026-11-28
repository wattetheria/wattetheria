//! The card game's wire contract: what a node signs and sends to the gateway, and how the
//! gateway answers. Agents keep cards, set a deck and play each other one round at a time; the
//! gateway holds every card, keeps what each side played hidden until both have played, works
//! out every round and, when the match is over, moves one of the cards the loser played to the
//! winner. A node only signs its agent's commands, relays the answers and tells the other agent
//! in the battle channel.

use crate::space::SignedSpacePresence;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const GAME_ID: &str = "cards";
/// Prefix of the UI events the gateway streams for card commands shown on the map.
pub const EVENT_KIND_PREFIX: &str = "cards.";

/// `kind` of the fixed-format message a node posts in the battle channel after a command the
/// gateway accepted. The message is the JSON of [`BattleNotice`].
pub const BATTLE_NOTICE_KIND: &str = "card_battle";

/// One card command. Every command carries the caller's `action_id`: retrying with the same id
/// returns the first result instead of acting twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CardAction {
    /// Put `cards` in deck slot `slot` (1 when left out). `stars` names, by card id, the star
    /// of the copy to field; a card it leaves out is fielded at the highest star the caller has
    /// free when the match starts. `name` and `note` are the caller's own words for the deck:
    /// left out they stay as they are, empty they are cleared. Without `cards`, only the name
    /// and the note of the deck already in the slot are set.
    Deck {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot: Option<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cards: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        stars: BTreeMap<String, u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Invite `opponent_did` to a match with the deck in `deck` (slot 1 when left out).
    Challenge {
        opponent_did: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deck: Option<u32>,
    },
    /// Take up the invite `match_id` with the deck in `deck` (slot 1 when left out).
    Accept {
        match_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deck: Option<u32>,
    },
    /// Play `card` of the caller's deck in the running round of `match_id`.
    Play { match_id: String, card: String },
    /// Look for a hidden card in one thing: `object`, a map object the caller stands at, or
    /// `piece`, a piece on the lot `lot` the caller stays on.
    Search {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        object: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lot: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        piece: Option<i64>,
    },
    /// Raise one copy of `card` by a star, using up one-star copies of it, standing in the
    /// market. `stars` is the star of the copy to raise (the highest the caller has free when
    /// left out).
    Merge {
        card: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stars: Option<u32>,
    },
    /// Buy one pack of random cards in the market and open it.
    Gacha,
    /// Buy one more deck slot in the market.
    DeckSlot,
}

impl CardAction {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Deck { .. } => "deck",
            Self::Challenge { .. } => "challenge",
            Self::Accept { .. } => "accept",
            Self::Play { .. } => "play",
            Self::Search { .. } => "search",
            Self::Merge { .. } => "merge",
            Self::Gacha => "gacha",
            Self::DeckSlot => "deck_slot",
        }
    }
}

/// What the agent's `did:key` signs (JCS) for `POST /api/cards/actions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardActionPayload {
    pub action_id: String,
    pub agent_did: String,
    #[serde(flatten)]
    pub action: CardAction,
    /// Display name shown on the agent's matches and ledger entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// The agent's newest signed presence, for the commands that depend on where it stands
    /// (searching a map object, buying a pack).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<SignedSpacePresence>,
}

/// Why the gateway refused a card command. Serialized as the `error` code of the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardRejection {
    Invalid,
    /// No such card, match or deck slot.
    Unknown,
    /// The caller does not have a free copy of a card the command needs.
    Missing,
    /// The cards do not make a deck: wrong count, a card twice, too few of a family or too many
    /// of a rarity.
    BadDeck,
    /// The caller has no usable deck in that slot: none set, or a card of it has no copy that
    /// is not in a running match.
    NoDeck,
    /// The caller is already in as many matches as it may play at once.
    TooManyMatches,
    /// The two agents have started as many matches with each other today as a pair may.
    PairLimit,
    /// The other agent cannot be played now; `details.reason` says why.
    OpponentUnavailable,
    /// The caller is not the agent this step of the match waits for.
    NotYours,
    /// The match is not waiting for this command: not invited any more, not running, or over.
    NotOpen,
    /// The caller already played its card this round.
    AlreadyPlayed,
    /// The card is not in the caller's deck of this match, or was already played in it.
    NotInDeck,
    InsufficientBalance,
    /// The caller is still busy with its last search.
    Busy,
    /// The caller looked in this same thing too short a while ago.
    TooSoon,
    /// The caller does not stand where the command must be given.
    NotThere,
    /// The card is already at the highest star, or the caller has every deck slot.
    AtMost,
}

impl CardRejection {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Unknown => "unknown",
            Self::Missing => "missing",
            Self::BadDeck => "bad_deck",
            Self::NoDeck => "no_deck",
            Self::TooManyMatches => "too_many_matches",
            Self::PairLimit => "pair_limit",
            Self::OpponentUnavailable => "opponent_unavailable",
            Self::NotYours => "not_yours",
            Self::NotOpen => "not_open",
            Self::AlreadyPlayed => "already_played",
            Self::NotInDeck => "not_in_deck",
            Self::InsufficientBalance => "insufficient_balance",
            Self::Busy => "busy",
            Self::TooSoon => "too_soon",
            Self::NotThere => "not_there",
            Self::AtMost => "at_most",
        }
    }
}

/// The step of a match a battle notice tells the other agent about, and the step a match waits
/// at. As text: `challenge`, `accept`, `round:3`, `final`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleStep {
    /// An invite was sent: the invited agent may accept.
    Challenge,
    /// The invite was accepted and the match started: round 1 is open.
    Accept,
    /// This round was worked out: the next one is open.
    Round(u32),
    /// The match is over.
    Final,
}

impl BattleStep {
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "challenge" => Some(Self::Challenge),
            "accept" => Some(Self::Accept),
            "final" => Some(Self::Final),
            _ => text
                .strip_prefix("round:")
                .and_then(|round| round.parse().ok())
                .filter(|round| *round > 0)
                .map(Self::Round),
        }
    }
}

impl std::fmt::Display for BattleStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Challenge => f.write_str("challenge"),
            Self::Accept => f.write_str("accept"),
            Self::Round(round) => write!(f, "round:{round}"),
            Self::Final => f.write_str("final"),
        }
    }
}

/// The message a node posts in the battle channel, mentioning the other agent, after the
/// gateway accepted a command. A node that receives one asks the gateway for `match_id` before
/// it tells its agent anything: anyone may post in the channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BattleNotice {
    /// Always [`BATTLE_NOTICE_KIND`].
    pub kind: String,
    pub match_id: String,
    /// A [`BattleStep`] as text.
    pub step: String,
    /// The same in plain words, for readers that do not know the format.
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_command_flattens_its_tag_and_leaves_out_unset_fields() {
        let payload = CardActionPayload {
            action_id: "a1".to_owned(),
            agent_did: "did:key:z".to_owned(),
            action: CardAction::Challenge {
                opponent_did: "did:key:y".to_owned(),
                deck: Some(2),
            },
            agent_label: None,
            presence: None,
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            value,
            json!({"action_id": "a1", "agent_did": "did:key:z", "action": "challenge",
                   "opponent_did": "did:key:y", "deck": 2})
        );
        assert_eq!(
            serde_json::from_value::<CardActionPayload>(value).unwrap(),
            payload
        );
    }

    #[test]
    fn a_deck_names_the_star_of_a_card_only_where_the_caller_chose_one() {
        let plain = CardAction::Deck {
            slot: None,
            cards: vec!["stick".to_owned()],
            stars: BTreeMap::new(),
            name: None,
            note: None,
        };
        let value = serde_json::to_value(&plain).unwrap();
        assert_eq!(value, json!({"action": "deck", "cards": ["stick"]}));
        assert_eq!(serde_json::from_value::<CardAction>(value).unwrap(), plain);
        let chosen: CardAction = serde_json::from_value(
            json!({"action": "deck", "slot": 2, "cards": ["stick"], "stars": {"stick": 1}}),
        )
        .unwrap();
        assert_eq!(
            chosen,
            CardAction::Deck {
                slot: Some(2),
                cards: vec!["stick".to_owned()],
                stars: BTreeMap::from([("stick".to_owned(), 1)]),
                name: None,
                note: None,
            }
        );
        // A deck is named, and noted on, without sending its cards again.
        let named = CardAction::Deck {
            slot: Some(2),
            cards: Vec::new(),
            stars: BTreeMap::new(),
            name: Some("anti-hunt".to_owned()),
            note: Some(String::new()),
        };
        let value = serde_json::to_value(&named).unwrap();
        assert_eq!(
            value,
            json!({"action": "deck", "slot": 2, "name": "anti-hunt", "note": ""})
        );
        assert_eq!(serde_json::from_value::<CardAction>(value).unwrap(), named);
        // The two things bought in the market take nothing but where the agent stands.
        assert_eq!(
            serde_json::to_value(CardAction::Gacha).unwrap(),
            json!({"action": "gacha"})
        );
        assert_eq!(
            serde_json::to_value(CardAction::DeckSlot).unwrap(),
            json!({"action": "deck_slot"})
        );
    }

    #[test]
    fn names_and_codes_match_their_serde_names() {
        for (action, name) in [
            (
                CardAction::Deck {
                    slot: None,
                    cards: Vec::new(),
                    stars: BTreeMap::new(),
                    name: None,
                    note: None,
                },
                "deck",
            ),
            (
                CardAction::Accept {
                    match_id: "cm_1".to_owned(),
                    deck: Some(1),
                },
                "accept",
            ),
            (
                CardAction::Play {
                    match_id: "cm_1".to_owned(),
                    card: "stick".to_owned(),
                },
                "play",
            ),
            (
                CardAction::Search {
                    object: Some("hobbs-cafe/cafe/piano".to_owned()),
                    lot: None,
                    piece: None,
                },
                "search",
            ),
            (
                CardAction::Merge {
                    card: "stick".to_owned(),
                    stars: None,
                },
                "merge",
            ),
            (CardAction::Gacha, "gacha"),
            (CardAction::DeckSlot, "deck_slot"),
        ] {
            assert_eq!(action.name(), name);
            assert_eq!(
                serde_json::to_value(&action).unwrap()["action"],
                json!(name)
            );
        }
        for code in [
            CardRejection::BadDeck,
            CardRejection::OpponentUnavailable,
            CardRejection::AlreadyPlayed,
            CardRejection::InsufficientBalance,
            CardRejection::TooSoon,
            CardRejection::NotThere,
            CardRejection::AtMost,
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), json!(code.as_str()));
        }
    }

    #[test]
    fn battle_steps_read_back_from_their_text() {
        for step in [
            BattleStep::Challenge,
            BattleStep::Accept,
            BattleStep::Round(3),
            BattleStep::Final,
        ] {
            assert_eq!(BattleStep::parse(&step.to_string()), Some(step));
        }
        assert_eq!(BattleStep::parse("round:0"), None);
        assert_eq!(BattleStep::parse("round:x"), None);
        assert_eq!(BattleStep::parse("invite"), None);
    }
}
