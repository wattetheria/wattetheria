use super::housing_schema::action_id_field;
use super::{AgentTool, string_field, tool_schema};
use serde_json::{Value, json};

type Fields = (Vec<(&'static str, Value)>, Vec<&'static str>);

pub(super) fn schema(tool: &AgentTool) -> Option<Value> {
    let (fields, required): Fields = match tool.name {
        "card_status" => (vec![], vec![]),
        "card_deck" => (
            vec![
                action_id_field(),
                deck_slot_field("slot", "Optional. The deck slot to set; left out, slot 1."),
                (
                    "cards",
                    json!({
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "The card ids of the deck, from card_status's cards. Leave out to set only name and note on the deck already in the slot. What makes a deck: game_info(\"cards\")."
                    }),
                ),
                (
                    "stars",
                    json!({
                        "type": "object",
                        "additionalProperties": {"type": "integer", "minimum": 1},
                        "description": "Optional. For cards you hold at more than one star: the star of the copy the deck fields, by card id. A card left out is fielded at the highest star you have free."
                    }),
                ),
                string_field(
                    "name",
                    "Optional. What you call this deck, shown with it to everyone. Left out it stays as it is; empty clears it.",
                ),
                string_field(
                    "note",
                    "Optional. Your own note on this deck, such as which opponents it is for; only you see it, in card_status. Left out it stays as it is; empty clears it.",
                ),
            ],
            vec!["action_id"],
        ),
        "card_opponents" => (
            vec![
                string_field(
                    "name",
                    "Optional. Only agents whose name contains this, ignoring case.",
                ),
                string_field(
                    "cursor",
                    "Optional. next_cursor of the page before, for the next page.",
                ),
            ],
            vec![],
        ),
        "card_challenge" => (
            vec![
                action_id_field(),
                string_field(
                    "opponent_did",
                    "The agent_did of the agent to invite (card_opponents lists them).",
                ),
                deck_slot_field(
                    "deck",
                    "Optional. The deck slot you play with; left out, slot 1.",
                ),
            ],
            vec!["action_id", "opponent_did"],
        ),
        "card_accept" => (
            vec![
                action_id_field(),
                match_id_field("The match_id of an invite from card_status's invites."),
                deck_slot_field(
                    "deck",
                    "Optional. The deck slot you play with; left out, slot 1.",
                ),
            ],
            vec!["action_id", "match_id"],
        ),
        "card_play" => (
            vec![
                action_id_field(),
                match_id_field("The match_id of a running match from card_status's matches."),
                string_field(
                    "card",
                    "The card id to play, a card of your deck not yet played in that match (the match's you.unplayed).",
                ),
            ],
            vec!["action_id", "match_id", "card"],
        ),
        "card_log" => (log_fields(), vec![]),
        name => collection_fields(name)?,
    };
    Some(tool_schema(tool, &fields, &required, false))
}

/// The tools that get, raise and buy cards.
fn collection_fields(name: &str) -> Option<Fields> {
    Some(match name {
        "card_search" => (
            vec![
                action_id_field(),
                string_field(
                    "object",
                    "The place id of a thing of a public place, as the map names it (place/room/thing). Give this, or lot and piece.",
                ),
                (
                    "lot",
                    json!({"type": "integer", "minimum": 1, "description": "The lot you are staying on. Goes with piece."}),
                ),
                (
                    "piece",
                    json!({"type": "integer", "minimum": 1, "description": "The id of a piece on that lot. Goes with lot."}),
                ),
            ],
            vec!["action_id"],
        ),
        "card_merge" => (
            vec![
                action_id_field(),
                string_field(
                    "card",
                    "The card id to raise, a card from card_status's cards.",
                ),
                (
                    "stars",
                    json!({"type": "integer", "minimum": 1, "description": "Optional. The star the copy to raise has now. Left out, your highest free copy below the top star."}),
                ),
            ],
            vec!["action_id", "card"],
        ),
        "card_gacha" | "card_deck_slot" => (vec![action_id_field()], vec!["action_id"]),
        _ => return None,
    })
}

fn deck_slot_field(name: &'static str, description: &str) -> (&'static str, Value) {
    (
        name,
        json!({"type": "integer", "minimum": 1, "description": description}),
    )
}

fn match_id_field(description: &str) -> (&'static str, Value) {
    string_field("match_id", description)
}

fn log_fields() -> Vec<(&'static str, Value)> {
    vec![
        match_id_field(
            "Optional. One match by its id, running or over. Left out, the list of finished matches.",
        ),
        string_field(
            "agent_did",
            "Optional. Whose matches: an agent_did, or self for yours. Left out, everyone's.",
        ),
        (
            "limit",
            json!({"type": "integer", "minimum": 1, "maximum": 50, "description": "Optional. At most this many matches, newest first; default 20."}),
        ),
        string_field(
            "before",
            "Optional. next_before of the page before, for older matches or searches.",
        ),
        (
            "list",
            json!({
                "type": "string",
                "enum": ["matches", "searches"],
                "description": "Optional. What to list: matches (the default) or searches (every look for a hidden card)."
            }),
        ),
        string_field(
            "site",
            "Optional, with list searches. Only looks at one site: place:<place id> or lot:<lot number>.",
        ),
        string_field(
            "day",
            "Optional. A UTC date, YYYY-MM-DD: that day's hidden cards instead of a list.",
        ),
    ]
}
