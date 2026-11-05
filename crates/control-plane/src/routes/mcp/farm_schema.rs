use super::{AgentTool, enum_field, string_field, tool_schema};
use serde_json::{Value, json};
use wattetheria_gateway_contract::farm::Crop;

pub(super) fn schema(tool: &AgentTool) -> Option<Value> {
    let crops: Vec<&str> = Crop::ALL.iter().map(|crop| crop.as_str()).collect();
    let (fields, required) = match tool.name {
        "list_games" | "farm_status" => (vec![], vec![]),
        "game_info" => (
            vec![string_field(
                "game_id",
                "Game id from list_games, e.g. farm.",
            )],
            vec!["game_id"],
        ),
        "farm_season" => (
            vec![(
                "season_no",
                json!({
                    "type": "integer",
                    "minimum": 1,
                    "description": "Season number; farm_status's season.season_no minus 1 is the last one that ended."
                }),
            )],
            vec!["season_no"],
        ),
        "farm_claim_plots" => (
            vec![
                action_id_field(),
                (
                    "parcel",
                    json!({
                        "type": "integer",
                        "minimum": 1,
                        "description": "Id of a free parcel: farm_status's next step names one, and game_info's places.parcels lists them all with their tiles."
                    }),
                ),
            ],
            vec!["action_id", "parcel"],
        ),
        "farm_plant" => (
            vec![
                action_id_field(),
                (
                    "tile",
                    tile_schema("Your empty plot, the tile you stand on."),
                ),
                enum_field("crop", "Crop to plant.", &crops),
            ],
            vec!["action_id", "tile", "crop"],
        ),
        "farm_water" | "farm_harvest" => (
            vec![
                action_id_field(),
                ("tile", tile_schema("Your plot, the tile you stand on.")),
            ],
            vec!["action_id", "tile"],
        ),
        "farm_sell" => (
            vec![
                action_id_field(),
                enum_field("crop", "Crop to sell from your inventory.", &crops),
                (
                    "units",
                    json!({
                        "type": "integer",
                        "minimum": 1,
                        "description": "How many units to sell; at most what you hold."
                    }),
                ),
            ],
            vec!["action_id", "crop", "units"],
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

fn tile_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "items": {"type": "integer", "minimum": 0},
        "minItems": 2,
        "maxItems": 2,
        "description": description
    })
}
