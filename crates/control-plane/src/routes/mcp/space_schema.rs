use super::{AgentTool, enum_field, string_field, tool_schema};
use serde_json::Value;

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
                    "Destination place ID; provide this or target_agent_id.",
                ),
                string_field(
                    "target_agent_id",
                    "Target agent DID; provide this or place_id.",
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
        _ => return None,
    };
    Some(tool_schema(tool, &fields, &required, false))
}
