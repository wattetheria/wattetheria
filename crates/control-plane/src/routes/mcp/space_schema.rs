use super::{AgentTool, enum_field, string_field, tool_schema};
use serde_json::{Value, json};
use wattetheria_gateway_contract::space::SPACE_AVATAR_COUNT;

pub(super) fn schema(tool: &AgentTool) -> Option<Value> {
    let (fields, required) = match tool.name {
        "enter_space" => (
            vec![
                string_field("space_id", "Configured destination Space ID."),
                string_field("portal_id", "Source portal ID for a cross-Space switch."),
            ],
            vec![],
        ),
        "leave_space" => (vec![], vec![]),
        "list_places" => (
            vec![
                string_field(
                    "space_id",
                    "Configured Space ID; defaults to your active Space.",
                ),
                enum_field(
                    "kind",
                    "Place kind.",
                    &["world", "sector", "arena", "game_object"],
                ),
                string_field("parent_place_id", "Optional immediate parent place ID."),
            ],
            vec![],
        ),
        "get_place" => (
            vec![
                string_field(
                    "space_id",
                    "Configured Space ID; defaults to your active Space.",
                ),
                string_field("place_id", "Place ID."),
            ],
            vec!["place_id"],
        ),
        "get_agent_presence" => (
            vec![
                string_field(
                    "space_id",
                    "Configured Space ID; defaults to your active Space.",
                ),
                string_field("agent_id", "Agent DID; defaults to yourself."),
            ],
            vec![],
        ),
        "move_agent" => (
            vec![
                string_field(
                    "place_id",
                    "Destination place ID; provide exactly one of this, target_agent_id or tile.",
                ),
                string_field(
                    "target_agent_id",
                    "Target agent DID; provide exactly one of this, place_id or tile.",
                ),
                (
                    "tile",
                    json!({
                        "type": "array",
                        "items": {"type": "integer", "minimum": 0},
                        "minItems": 2,
                        "maxItems": 2,
                        "description": "Exact destination tile [x, y]; provide exactly one of this, place_id or target_agent_id."
                    }),
                ),
                string_field("activity", "Activity at the destination."),
            ],
            vec![],
        ),
        "set_agent_activity" => (
            vec![
                string_field("activity", "Current activity."),
                string_field(
                    "object_place_id",
                    "Optional object at the current position.",
                ),
            ],
            vec!["activity"],
        ),
        name => skin_fields(name)?,
    };
    Some(tool_schema(tool, &fields, &required, false))
}

/// Argument schemas of the skin tools: what the agent wears and the custom skins it owns.
#[allow(clippy::type_complexity)]
fn skin_fields(name: &str) -> Option<(Vec<(&'static str, Value)>, Vec<&'static str>)> {
    let fields = match name {
        "set_agent_skin" => (
            vec![
                (
                    "avatar_id",
                    json!({
                        "type": "integer",
                        "minimum": 0,
                        "maximum": SPACE_AVATAR_COUNT - 1,
                        "description": "Built-in skin to wear, 0-24. Clears any custom skin."
                    }),
                ),
                string_field(
                    "skin_id",
                    "Custom skin to wear: a skin_id this agent published, from upload_agent_skin or list_agent_skins.",
                ),
            ],
            vec![],
        ),
        "upload_agent_skin" => (
            vec![
                string_field(
                    "png_base64",
                    "Standard base64 of the 96x128 PNG spritesheet (at most 256 KiB).",
                ),
                string_field("name", "Display name for the skin, up to 60 characters."),
            ],
            vec!["png_base64"],
        ),
        "list_agent_skins" => (vec![], vec![]),
        _ => return None,
    };
    Some(fields)
}
