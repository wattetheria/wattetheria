use axum::body::to_bytes;
use axum::http::{Method, StatusCode};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use wattetheria_kernel::local_db;
use wattetheria_kernel::swarm_bridge::SwarmRunSubmitCommand;

use crate::state::ControlPlaneState;

use super::{
    AgentTool, Availability, LOOPBACK_BODY_LIMIT, bool_argument, dispatch_loopback_tool,
    local_public_id, numeric_argument, required_string, response_to_tool_result, tool_error,
    tool_success,
};

const DEFAULT_EVENTS_LIMIT: usize = 50;
const MAX_EVENTS_LIMIT: usize = 200;
const COLLECTIVE_TASK_TYPE: &str = "wattetheria.collective_mission";
const MISSION_FEED_KEY: &str = "wattetheria.missions";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CollectiveMissionRunIndex {
    #[serde(default)]
    runs: BTreeMap<String, CollectiveMissionRunLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CollectiveMissionRunLink {
    mission_id: String,
    run_id: String,
    created_at: String,
    kicked_off: bool,
    run_spec: Value,
    wattswarm_run: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollectiveExecutionMode {
    Committee,
    Stigmergy,
}

struct CollectiveExecutionSpec {
    mode: CollectiveExecutionMode,
    agents: Value,
    round_policy: Option<Value>,
}

pub(super) async fn publish_collective_mission_result(
    state: &ControlPlaneState,
    auth: &str,
    arguments: &Value,
) -> Value {
    let execution = match collective_execution_spec(arguments) {
        Ok(execution) => execution,
        Err(error) => return tool_error(&json!({"error": error})),
    };
    let public_id = local_public_id(state).await;
    let mission_body = mission_publish_body(arguments, &public_id);
    let mission_response = match dispatch_loopback_tool(
        state.clone(),
        auth,
        &publish_mission_tool(),
        &json!({ "body": mission_body }),
    )
    .await
    {
        Ok(response) => response,
        Err(response) => {
            return response_to_tool_result("publish_collective_mission", response).await;
        }
    };
    let (status, mission) = match response_json(mission_response).await {
        Ok(parsed) => parsed,
        Err(result) => return result,
    };
    if !status.is_success() {
        return tool_error(&json!({
            "error": "mission_publish_failed",
            "status": status.as_u16(),
            "detail": mission,
        }));
    }
    let Some(mission_id) = required_string(&mission, "mission_id") else {
        return tool_error(&json!({
            "error": "mission_publish_response_missing_mission_id",
            "mission": mission,
        }));
    };

    let kickoff = bool_argument(arguments, "kickoff").unwrap_or(true);
    let run_spec = build_run_spec(arguments, &mission_id, &mission, &execution);
    let command = SwarmRunSubmitCommand {
        spec: run_spec.clone(),
        kickoff,
    };
    let wattswarm_run = match state.swarm_bridge.submit_run(command).await {
        Ok(payload) => payload,
        Err(error) => {
            return tool_error(&json!({
                "error": "wattswarm_run_submit_failed",
                "detail": error.to_string(),
                "mission_id": mission_id,
                "mission": mission,
                "run_spec": run_spec,
            }));
        }
    };
    let run_id = wattswarm_run
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| required_string(&run_spec, "run_id"))
        .unwrap_or_else(|| format!("collective-{mission_id}"));
    let link = CollectiveMissionRunLink {
        mission_id: mission_id.clone(),
        run_id: run_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        kicked_off: kickoff,
        run_spec: run_spec.clone(),
        wattswarm_run: wattswarm_run.clone(),
    };
    if let Err(error) = save_run_link(state, link.clone()) {
        return tool_error(&json!({
            "error": "persist_collective_mission_run_failed",
            "detail": error,
            "mission_id": mission_id,
            "run_id": run_id,
            "wattswarm_run": wattswarm_run,
        }));
    }

    tool_success(&json!({
        "mission_id": mission_id,
        "run_id": run_id,
        "kicked_off": kickoff,
        "mission": mission,
        "run_spec": run_spec,
        "wattswarm_run": wattswarm_run,
        "link": link,
    }))
}

pub(super) async fn get_collective_mission_result(
    state: &ControlPlaneState,
    arguments: &Value,
) -> Value {
    let index = match load_run_index(state) {
        Ok(index) => index,
        Err(error) => {
            return tool_error(&json!({
                "error": "load_collective_mission_runs_failed",
                "detail": error,
            }));
        }
    };
    let mission_id = optional_string(arguments, "mission_id");
    let resolved = match resolve_run_link(&index, mission_id.as_deref(), arguments) {
        Ok(resolved) => resolved,
        Err(error) => return tool_error(&json!({"error": error})),
    };
    let result = match state
        .swarm_bridge
        .run_result_snapshot(&resolved.run_id)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            return tool_error(&json!({
                "error": "wattswarm_run_result_failed",
                "detail": error.to_string(),
                "mission_id": resolved.mission_id,
                "run_id": resolved.run_id,
            }));
        }
    };
    let include_events = bool_argument(arguments, "include_events").unwrap_or(false);
    let mut payload = json!({
        "mission_id": resolved.mission_id,
        "run_id": resolved.run_id,
        "link": resolved.link,
        "result": result,
    });
    if include_events {
        let limit = numeric_argument(arguments, "events_limit")
            .unwrap_or(DEFAULT_EVENTS_LIMIT)
            .min(MAX_EVENTS_LIMIT);
        let events = match state
            .swarm_bridge
            .run_events_snapshot(&resolved.run_id, limit)
            .await
        {
            Ok(events) => events,
            Err(error) => {
                return tool_error(&json!({
                    "error": "wattswarm_run_events_failed",
                    "detail": error.to_string(),
                    "mission_id": resolved.mission_id,
                    "run_id": resolved.run_id,
                }));
            }
        };
        payload["events"] = json!(events);
    }
    tool_success(&payload)
}

struct ResolvedRunLink {
    mission_id: Value,
    run_id: String,
    link: Value,
}

fn resolve_run_link(
    index: &CollectiveMissionRunIndex,
    mission_id: Option<&str>,
    arguments: &Value,
) -> Result<ResolvedRunLink, String> {
    if let Some(run_id) = optional_string(arguments, "run_id") {
        let link = match mission_id {
            Some(mission_id) => {
                let Some(link) = index.runs.get(mission_id) else {
                    return Err(format!(
                        "collective mission run link not found for mission_id: {mission_id}"
                    ));
                };
                if link.run_id != run_id {
                    return Err(format!(
                        "collective mission run link not found for mission_id: {mission_id} and run_id: {run_id}"
                    ));
                }
                link
            }
            None => index
                .runs
                .values()
                .find(|link| link.run_id == run_id)
                .ok_or_else(|| {
                    format!("collective mission run link not found for run_id: {run_id}")
                })?,
        };
        return Ok(ResolvedRunLink {
            mission_id: Value::String(link.mission_id.clone()),
            run_id,
            link: json!(link),
        });
    }
    let Some(mission_id) = mission_id else {
        return Err("mission_id or run_id is required".to_owned());
    };
    let Some(link) = index.runs.get(mission_id) else {
        return Err(format!(
            "collective mission run link not found for mission_id: {mission_id}"
        ));
    };
    Ok(ResolvedRunLink {
        mission_id: Value::String(mission_id.to_owned()),
        run_id: link.run_id.clone(),
        link: json!(link),
    })
}

async fn response_json(response: Response) -> Result<(StatusCode, Value), Value> {
    let status = response.status();
    let bytes = match to_bytes(response.into_body(), LOOPBACK_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(error) => return Err(tool_error(&json!({"error": error.to_string()}))),
    };
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };
    Ok((status, payload))
}

fn publish_mission_tool() -> AgentTool {
    AgentTool {
        name: "publish_mission",
        method: Method::POST,
        path: "/v1/wattetheria/missions",
        description: "Publish a new mission.",
        availability: Availability::Always,
    }
}

fn mission_publish_body(arguments: &Value, public_id: &str) -> Value {
    let source = argument_object(arguments);
    let mut body = Map::new();
    for key in [
        "title",
        "description",
        "domain",
        "subnet_id",
        "zone_id",
        "required_role",
        "required_faction",
        "reward",
        "payload",
    ] {
        if let Some(value) = source.get(key) {
            body.insert(key.to_owned(), value.clone());
        }
    }
    body.insert("publisher".to_owned(), Value::String(public_id.to_owned()));
    body.insert(
        "publisher_kind".to_owned(),
        Value::String("player".to_owned()),
    );
    Value::Object(body)
}

fn build_run_spec(
    arguments: &Value,
    mission_id: &str,
    mission: &Value,
    execution: &CollectiveExecutionSpec,
) -> Value {
    let run_id =
        optional_string(arguments, "run_id").unwrap_or_else(|| format!("collective-{mission_id}"));
    let mut spec = json!({
        "run_id": run_id,
        "task_type": optional_string(arguments, "task_type").unwrap_or_else(|| COLLECTIVE_TASK_TYPE.to_owned()),
        "shared_inputs": shared_inputs(arguments, mission_id, mission),
        "agents": execution.agents,
        "retry": argument_value(arguments, "retry").cloned().unwrap_or_else(|| json!({})),
        "aggregation": argument_value(arguments, "aggregation").cloned().unwrap_or_else(|| json!({})),
    });
    if execution.mode == CollectiveExecutionMode::Stigmergy {
        spec["market_task_id"] = Value::String(mission_id.to_owned());
        spec["feed_key"] = Value::String(MISSION_FEED_KEY.to_owned());
        spec["scope_hint"] = Value::String(format!("group:{mission_id}"));
        spec["round_policy"] = execution.round_policy.clone().unwrap_or(Value::Null);
    }
    spec
}

fn shared_inputs(arguments: &Value, mission_id: &str, mission: &Value) -> Value {
    argument_value(arguments, "shared_inputs")
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "source": "wattetheria.mcp.publish_collective_mission",
                "mission_id": mission_id,
                "mission_title": mission.get("title").cloned().unwrap_or(Value::Null),
                "mission_description": mission.get("description").cloned().unwrap_or(Value::Null),
                "mission_payload": mission.get("payload").cloned().unwrap_or(Value::Null),
                "mission": mission,
            })
        })
}

fn collective_execution_spec(arguments: &Value) -> Result<CollectiveExecutionSpec, String> {
    let mode = collective_execution_mode(arguments)?;
    let agents = collective_agents(arguments, mode)?;
    let round_policy = match mode {
        CollectiveExecutionMode::Committee => None,
        CollectiveExecutionMode::Stigmergy => Some(stigmergy_round_policy(arguments)?),
    };
    Ok(CollectiveExecutionSpec {
        mode,
        agents,
        round_policy,
    })
}

fn collective_execution_mode(arguments: &Value) -> Result<CollectiveExecutionMode, String> {
    match optional_string(arguments, "mode").as_deref() {
        None | Some("stigmergy") => Ok(CollectiveExecutionMode::Stigmergy),
        Some("committee") => Ok(CollectiveExecutionMode::Committee),
        Some(mode) => Err(format!("mode must be committee or stigmergy, got {mode}")),
    }
}

fn collective_agents(arguments: &Value, mode: CollectiveExecutionMode) -> Result<Value, String> {
    if mode == CollectiveExecutionMode::Stigmergy {
        return Ok(json!([]));
    }
    let Some(agents) = argument_value(arguments, "agents") else {
        return Err("agents is required".to_owned());
    };
    let Some(items) = agents.as_array() else {
        return Err("agents must be a non-empty array".to_owned());
    };
    if items.is_empty() {
        return Err("agents must be a non-empty array".to_owned());
    }
    for (index, agent) in items.iter().enumerate() {
        for field in ["agent_id", "executor", "prompt"] {
            if agent
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .is_none_or(str::is_empty)
            {
                return Err(format!("agents[{index}].{field} is required"));
            }
        }
    }
    Ok(agents.clone())
}

fn stigmergy_round_policy(arguments: &Value) -> Result<Value, String> {
    let min_participants = required_positive_integer(arguments, "min_participants")?;
    let threshold_percent = required_positive_integer(arguments, "threshold_percent")?;
    if threshold_percent > 100 {
        return Err("threshold_percent must be between 1 and 100".to_owned());
    }
    let round_timeout_ms = required_positive_integer(arguments, "round_timeout_ms")?;
    let max_rounds = required_positive_integer(arguments, "max_rounds")?;
    let mut policy = json!({
        "min_participants": min_participants,
        "threshold_percent": threshold_percent,
        "round_timeout_ms": round_timeout_ms,
        "max_rounds": max_rounds,
    });
    if let Some(fallback_decision) = optional_string(arguments, "fallback_decision") {
        policy["fallback_decision"] = Value::String(fallback_decision);
    }
    Ok(policy)
}

fn required_positive_integer(arguments: &Value, key: &str) -> Result<u64, String> {
    let Some(value) = argument_value(arguments, key).and_then(Value::as_u64) else {
        return Err(format!("{key} is required for stigmergy mode"));
    };
    if value == 0 {
        return Err(format!("{key} must be greater than zero"));
    }
    Ok(value)
}

fn argument_object(arguments: &Value) -> &Map<String, Value> {
    arguments
        .get("body")
        .and_then(Value::as_object)
        .or_else(|| arguments.as_object())
        .expect("MCP arguments are validated as an object before direct dispatch")
}

fn argument_value<'a>(arguments: &'a Value, key: &str) -> Option<&'a Value> {
    argument_object(arguments).get(key)
}

fn optional_string(arguments: &Value, key: &str) -> Option<String> {
    argument_value(arguments, key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn load_run_index(state: &ControlPlaneState) -> Result<CollectiveMissionRunIndex, String> {
    state
        .local_db
        .load_domain_or_default(local_db::domain::COLLECTIVE_MISSION_RUNS)
        .map_err(|error| error.to_string())
}

fn save_run_link(state: &ControlPlaneState, link: CollectiveMissionRunLink) -> Result<(), String> {
    let mut index = load_run_index(state)?;
    index.runs.insert(link.mission_id.clone(), link);
    state
        .local_db
        .save_domain(local_db::domain::COLLECTIVE_MISSION_RUNS, &index)
        .map_err(|error| error.to_string())
}
