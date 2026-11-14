use super::{AgentTool, string_field, tool_schema};
use serde_json::{Value, json};

pub(super) fn schema(tool: &AgentTool) -> Option<Value> {
    let (fields, required) = match tool.name {
        "housing_status" => (vec![], vec![]),
        "housing_buy_lot" => (
            vec![
                action_id_field(),
                (
                    "lot",
                    json!({
                        "type": "integer",
                        "minimum": 1,
                        "description": "Number of a free lot: housing_status's free_lots lists them with price, size and gate."
                    }),
                ),
            ],
            vec!["action_id", "lot"],
        ),
        "housing_place" => (
            vec![
                action_id_field(),
                string_field(
                    "element",
                    "Element id from game_info(\"housing\")'s elements, e.g. floor_wood, wall, bed, piano, tree.",
                ),
                (
                    "cell",
                    cell_schema(
                        "The lot cell for the element's top-left corner, counted from the lot's top-left [0, 0]; an element of size [w, h] covers w cells right and h cells down.",
                    ),
                ),
                (
                    "facing",
                    json!({
                        "type": "string",
                        "enum": ["down", "left", "right", "up"],
                        "description": "Optional. The way the piece's front looks on the map, for an element whose facings in game_info(\"housing\") is not empty; only the ways listed there are accepted. down shows its front, up its back. Left out, the piece faces the first way listed."
                    }),
                ),
            ],
            vec!["action_id", "element", "cell"],
        ),
        "housing_remove" => (
            vec![
                action_id_field(),
                (
                    "cell",
                    cell_schema(
                        "Any lot cell the piece covers; the topmost piece there comes off (furniture, then garden, wall, ground).",
                    ),
                ),
            ],
            vec!["action_id", "cell"],
        ),
        "housing_name" => (
            vec![
                action_id_field(),
                string_field(
                    "name",
                    "The name on your lot's sign, up to game_info(\"housing\")'s name limit; whitespace is tidied.",
                ),
            ],
            vec!["action_id", "name"],
        ),
        "housing_enter" => (
            vec![
                action_id_field(),
                (
                    "lot",
                    json!({
                        "type": "integer",
                        "minimum": 1,
                        "description": "Number of a bought lot, yours or another agent's; you must stand on its gate (place residential-district/lot-N-gate)."
                    }),
                ),
                (
                    "cell",
                    cell_schema(
                        "Optional. The lot cell to stay on, counted from the lot's top-left [0, 0]: free of blocking pieces and of other agents, and reachable on foot from the gate. Leave it out to get the nearest free cell, indoors when the lot has floor.",
                    ),
                ),
            ],
            vec!["action_id", "lot"],
        ),
        _ => return None,
    };
    Some(tool_schema(tool, &fields, &required, false))
}

fn action_id_field() -> (&'static str, Value) {
    string_field(
        "action_id",
        "Required key you choose for this one command (for example a UUID), up to 128 characters with no whitespace. On a network or 5xx error retry with the SAME action_id: the gateway returns the first result instead of acting twice. Use a new one for every new command.",
    )
}

fn cell_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "items": {"type": "integer", "minimum": 0},
        "minItems": 2,
        "maxItems": 2,
        "description": description
    })
}
