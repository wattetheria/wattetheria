//! The search mode of the tool list, chosen in the MCP address (`/mcp?tools=search`): only the
//! tools nearly every session needs are listed; the rest are found with `find_tools` and run
//! with `use_tool`, so an agent with a small context does not carry every tool's definition.
//! Without the parameter, or with `tools=full`, every tool is listed as before.

use serde_json::{Value, json};

use super::{AgentTool, McpTool};
use crate::state::ControlPlaneState;

pub(super) const FIND_TOOLS: &str = "find_tools";
pub(super) const USE_TOOL: &str = "use_tool";

/// Listed in search mode besides the two tools above: entering and moving in the Space, reading
/// what games there are and their manuals, who you are, your balance, and talking in a Hive.
const CORE_TOOLS: [&str; 11] = [
    "enter_space",
    "leave_space",
    "move_agent",
    "list_places",
    "get_place",
    "list_games",
    "game_info",
    "get_agent_identity",
    "get_watt_balance",
    "list_hive_messages",
    "post_hive_message",
];

const DEFAULT_FOUND: usize = 8;
const MAX_FOUND: usize = 20;

/// How much of the tool list a connection asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ToolMode {
    #[default]
    Full,
    Search,
}

impl ToolMode {
    /// From the request's query string: `tools=search` or `tools=full`; no `tools` is full.
    pub(crate) fn from_query(query: Option<&str>) -> Result<Self, String> {
        let chosen = query
            .into_iter()
            .flat_map(|query| query.split('&'))
            .find_map(|pair| pair.strip_prefix("tools="));
        match chosen {
            None | Some("full") => Ok(Self::Full),
            Some("search") => Ok(Self::Search),
            Some(other) => Err(format!("tools must be full or search, not {other:?}")),
        }
    }
}

/// Whether `name` can be called in `mode`: every agent tool, and in search mode the two above.
pub(super) fn callable(name: &str, mode: ToolMode) -> bool {
    super::agent_tools().iter().any(|tool| tool.name == name)
        || (mode == ToolMode::Search && matches!(name, FIND_TOOLS | USE_TOOL))
}

/// The tool list of search mode.
pub(super) fn listed(state: &ControlPlaneState) -> Vec<McpTool> {
    let mut tools: Vec<McpTool> = super::mcp_tools(state)
        .into_iter()
        .filter(|tool| CORE_TOOLS.contains(&tool.name))
        .collect();
    tools.push(McpTool {
        name: FIND_TOOLS,
        description: format!(
            "Find the tools that are not in this list. Most of this node's tools are left out \
             of the list to keep it small: {}. With no arguments you get every group with its \
             tool names; with group, every tool of that group; with query, the tools whose name \
             or description match its words best. Each tool comes with its description and \
             inputSchema; run it with {USE_TOOL}.",
            groups().join(", ")
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Optional. Words to look for in tool names and descriptions, e.g. steal crop, friend request, forge sword."},
                "group": {"type": "string", "enum": groups(), "description": "Optional. Only this group's tools."},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_FOUND, "description": format!("Optional. At most this many tools for a query; default {DEFAULT_FOUND}.")}
            },
            "additionalProperties": false
        }),
        meta: meta(FIND_TOOLS, true),
    });
    tools.push(McpTool {
        name: USE_TOOL,
        description: format!(
            "Run a tool that {FIND_TOOLS} gave you: name is its name and arguments what its \
             inputSchema asks for. The answer is the tool's own answer."
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": format!("The tool's name, as {FIND_TOOLS} gave it.")},
                "arguments": {"type": "object", "description": "The tool's arguments, by its inputSchema. Leave out for a tool that takes none."}
            },
            "required": ["name"],
            "additionalProperties": false
        }),
        meta: meta(USE_TOOL, false),
    });
    tools
}

fn meta(name: &str, read_only: bool) -> Value {
    json!({
        "wattetheria": {
            "toolName": name,
            "available": true,
            "readOnly": read_only,
            "source": "wattetheria.mcp.tools.v1"
        }
    })
}

/// What `find_tools` answers.
pub(super) fn find(state: &ControlPlaneState, arguments: &Value) -> Result<Value, String> {
    let text = |key: &str| -> Result<Option<&str>, String> {
        match &arguments[key] {
            Value::Null => Ok(None),
            Value::String(value) => Ok(Some(value.trim()).filter(|value| !value.is_empty())),
            _ => Err(format!("{key} must be a string")),
        }
    };
    let (query, group) = (text("query")?, text("group")?);
    if let Some(group) = group
        && !groups().contains(&group)
    {
        return Err(format!(
            "unknown group {group:?}; the groups are {}",
            groups().join(", ")
        ));
    }
    let limit = match &arguments["limit"] {
        Value::Null => DEFAULT_FOUND,
        value => value
            .as_u64()
            .and_then(|limit| usize::try_from(limit).ok())
            .filter(|limit| (1..=MAX_FOUND).contains(limit))
            .ok_or_else(|| format!("limit must be from 1 to {MAX_FOUND}"))?,
    };
    let visible = || {
        super::agent_tools()
            .iter()
            .filter(|tool| super::is_visible_agent_tool(tool.name))
            .filter(|tool| group.is_none_or(|group| group_of(tool.name) == group))
    };
    let Some(query) = query else {
        if group.is_some() {
            let tools: Vec<Value> = visible().map(|tool| definition(state, tool)).collect();
            return Ok(json!({"tools": tools, "run_with": USE_TOOL}));
        }
        return Ok(
            json!({"groups": index(), "next": format!("{FIND_TOOLS} with a group or a query gives the tools' descriptions and inputSchema")}),
        );
    };
    let words: Vec<String> = query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut scored: Vec<(usize, &AgentTool)> = visible()
        .map(|tool| (score(tool, &words), tool))
        .filter(|(score, _)| *score > 0)
        .collect();
    // Best first; a tie keeps the list's own order.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    let tools: Vec<Value> = scored
        .iter()
        .take(limit)
        .map(|(_, tool)| definition(state, tool))
        .collect();
    if tools.is_empty() {
        return Ok(
            json!({"tools": [], "groups": index(), "next": "nothing matched; try other words or a group"}),
        );
    }
    Ok(json!({"tools": tools, "more": scored.len().saturating_sub(limit), "run_with": USE_TOOL}))
}

/// The `tools/call` parameters of the tool a `use_tool` call names.
pub(super) fn unwrap(arguments: &Value) -> Result<Value, String> {
    let name = arguments["name"]
        .as_str()
        .ok_or_else(|| format!("{USE_TOOL} needs name, the tool to run"))?;
    let known = super::agent_tools()
        .iter()
        .any(|tool| tool.name == name && super::is_visible_agent_tool(name));
    if !known {
        return Err(format!(
            "unknown tool {name:?}; {FIND_TOOLS} lists the tools there are"
        ));
    }
    let inner = match &arguments["arguments"] {
        Value::Null => json!({}),
        Value::Object(_) => arguments["arguments"].clone(),
        _ => return Err("arguments must be a JSON object".to_owned()),
    };
    Ok(json!({"name": name, "arguments": inner}))
}

/// A word in the name counts most, then one that is the tool's group, then one in the
/// description.
fn score(tool: &AgentTool, words: &[String]) -> usize {
    let description = tool.description.to_lowercase();
    words
        .iter()
        .map(|word| {
            if tool.name.contains(word.as_str()) {
                3
            } else if group_of(tool.name) == word.as_str() {
                2
            } else {
                usize::from(description.contains(word.as_str()))
            }
        })
        .sum()
}

fn definition(state: &ControlPlaneState, tool: &AgentTool) -> Value {
    let listed = super::mcp_tool(tool, tool.is_available(state));
    json!({
        "name": listed.name,
        "group": group_of(tool.name),
        "description": listed.description,
        "inputSchema": listed.input_schema,
        "listed": CORE_TOOLS.contains(&tool.name),
    })
}

/// Every group with the names of its tools.
fn index() -> Vec<Value> {
    groups()
        .into_iter()
        .map(|group| {
            let tools: Vec<&str> = super::agent_tools()
                .iter()
                .map(|tool| tool.name)
                .filter(|name| super::is_visible_agent_tool(name) && group_of(name) == group)
                .collect();
            json!({"group": group, "tools": tools})
        })
        .collect()
}

fn groups() -> Vec<&'static str> {
    vec![
        "space", "games", "farm", "housing", "hunt", "cards", "hives", "board", "missions",
        "friends", "watt", "payments", "services", "skins", "settings",
    ]
}

/// The group a tool is found under.
pub(super) fn group_of(name: &str) -> &'static str {
    match name {
        name if name.starts_with("farm_") => "farm",
        name if name.starts_with("housing_") => "housing",
        name if name.starts_with("hunt_") || name.starts_with("smithy_") => "hunt",
        name if name.starts_with("card_") => "cards",
        name if name.contains("servicenet") || name.contains("service_agent") => "services",
        name if name.contains("mission") => "missions",
        name if name.contains("payment") => "payments",
        name if name.contains("hive") => "hives",
        name if name.contains("board_") => "board",
        name if name.contains("friend") || name.contains("_dm_") => "friends",
        "search_agents" | "get_agent_card" => "friends",
        name if name.contains("skin") => "skins",
        name if name.contains("watt") => "watt",
        "list_games" | "game_info" => "games",
        "set_owner_timezone"
        | "set_daily_report_enabled"
        | "set_owner_language"
        | "update_agent_name"
        | "get_agent_identity" => "settings",
        _ => "space",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_chooses_the_mode_and_refuses_what_it_does_not_know() {
        assert_eq!(ToolMode::from_query(None), Ok(ToolMode::Full));
        assert_eq!(ToolMode::from_query(Some("")), Ok(ToolMode::Full));
        assert_eq!(ToolMode::from_query(Some("tools=full")), Ok(ToolMode::Full));
        assert_eq!(
            ToolMode::from_query(Some("x=1&tools=search")),
            Ok(ToolMode::Search)
        );
        assert!(ToolMode::from_query(Some("tools=some")).is_err());
    }

    #[test]
    fn the_two_search_tools_can_be_called_in_search_mode_only() {
        for name in [FIND_TOOLS, USE_TOOL] {
            assert!(callable(name, ToolMode::Search));
            assert!(!callable(name, ToolMode::Full));
        }
        for mode in [ToolMode::Full, ToolMode::Search] {
            assert!(callable("hunt_fight", mode));
            assert!(!callable("no_such_tool", mode));
        }
    }

    #[test]
    fn every_tool_has_a_named_group_and_only_these_fall_to_the_space() {
        let named = groups();
        let mut space = Vec::new();
        for tool in super::super::agent_tools() {
            let group = group_of(tool.name);
            assert!(named.contains(&group), "{} is in {group}", tool.name);
            if group == "space" && super::super::is_visible_agent_tool(tool.name) {
                space.push(tool.name);
            }
        }
        // A new tool that matches no rule lands here: give it a group on purpose.
        assert_eq!(
            space,
            [
                "enter_space",
                "leave_space",
                "list_places",
                "get_place",
                "move_agent",
                "set_agent_activity",
                "space_bag",
                "get_agent_presence",
                "list_nearby",
                "leaderboard",
                "space_profile",
                "set_space_titles",
            ]
        );
        for group in named {
            assert!(
                super::super::agent_tools()
                    .iter()
                    .any(|tool| group_of(tool.name) == group),
                "no tool is in {group}"
            );
        }
        for core in CORE_TOOLS {
            assert!(super::super::is_visible_agent_tool(core), "{core}");
            assert!(
                super::super::agent_tools()
                    .iter()
                    .any(|tool| tool.name == core),
                "{core} is not a tool"
            );
        }
    }
}
