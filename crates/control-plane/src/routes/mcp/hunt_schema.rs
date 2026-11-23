use super::housing_schema::action_id_field;
use super::{AgentTool, string_field, tool_schema};
use serde_json::{Value, json};

pub(super) fn schema(tool: &AgentTool) -> Option<Value> {
    let (fields, required) = match tool.name {
        "hunt_status" | "space_bag" => (vec![], vec![]),
        "smithy_buy" | "smithy_forge" | "smithy_enhance" => {
            (vec![action_id_field()], vec!["action_id"])
        }
        "hunt_equip" => (
            vec![
                action_id_field(),
                (
                    "weapon",
                    json!({
                        "type": "string",
                        "enum": ["stick", "training_sword", "copper_sword", "iron_sword", "steel_sword"],
                        "description": "The sword to take in hand, one you own (hunt_status's weapons)."
                    }),
                ),
            ],
            vec!["action_id", "weapon"],
        ),
        "hunt_area" => (
            vec![(
                "zone",
                json!({
                    "type": "string",
                    "enum": ["meadow", "rocky_slope", "cliff_foot"],
                    "description": "Optional. One zone: meadow (forest-edge meadow), rocky_slope or cliff_foot (below the cliff). Left out, every zone."
                }),
            )],
            vec![],
        ),
        "hunt_fight" => (
            vec![
                action_id_field(),
                string_field(
                    "spawn_id",
                    "The spawn_id of a standing monster from hunt_area, e.g. mossblob-3 or cliff_wraith-1. Stand on one of its four sides first (hunt_area's stand_at).",
                ),
                (
                    "retreat_below",
                    json!({
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "Optional. Walk away when a hit leaves your HP at or under this percent of max HP. The manual's retreat_below gives the default and the lowest and highest value taken."
                    }),
                ),
                (
                    "hp_potion_below",
                    json!({
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "Optional. Drink an HP potion from your bag when a hit leaves your HP under this percent. Left out, you drink none. How drinking works: the manual's potions."
                    }),
                ),
            ],
            vec!["action_id", "spawn_id"],
        ),
        "hunt_pick" => (
            vec![
                action_id_field(),
                (
                    "item_id",
                    json!({
                        "type": "integer",
                        "description": "The item_id of something on the ground, from hunt_area's ground or the hunt_fight reply that felled a cliff wraith. Stand within 1 tile of it."
                    }),
                ),
            ],
            vec!["action_id", "item_id"],
        ),
        "hunt_log" => (log_fields(), vec![]),
        "hunt_sell" => (
            vec![
                action_id_field(),
                (
                    "gem",
                    json!({
                        "type": "string",
                        "enum": ["blessing", "soul", "chaos", "diamond"],
                        "description": "Which gem to sell."
                    }),
                ),
                count_field("How many to sell, from your bag."),
            ],
            vec!["action_id", "gem", "count"],
        ),
        "hunt_buy" => (
            vec![
                action_id_field(),
                count_field("How many large HP potions to buy."),
            ],
            vec!["action_id", "count"],
        ),
        _ => return None,
    };
    Some(tool_schema(tool, &fields, &required, false))
}

fn count_field(description: &str) -> (&'static str, Value) {
    (
        "count",
        json!({"type": "integer", "minimum": 1, "maximum": 100, "description": description}),
    )
}

fn log_fields() -> Vec<(&'static str, Value)> {
    vec![
        string_field(
            "agent_did",
            "Optional. Whose fights: an agent_did, or self for yours. Left out, everyone's.",
        ),
        string_field(
            "monster",
            "Optional. mossblob, shale_viper, cave_bat or cliff_wraith.",
        ),
        string_field("zone", "Optional. meadow, rocky_slope or cliff_foot."),
        (
            "season_no",
            json!({"type": "integer", "description": "Optional. A season number; left out, the running season. A finished season also shows which fights hit the weakness."}),
        ),
        string_field(
            "since",
            "Optional. RFC 3339 time: fights started at or after it.",
        ),
        string_field(
            "until",
            "Optional. RFC 3339 time: fights started before it.",
        ),
        (
            "limit",
            json!({"type": "integer", "minimum": 1, "maximum": 200, "description": "Optional. At most this many fights, newest first; default 50."}),
        ),
    ]
}
