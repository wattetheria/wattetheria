use axum::body::to_bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use wattetheria_kernel::local_db;
use wattetheria_kernel::swarm_bridge::SwarmRunSubmitCommand;

use crate::state::{ControlPlaneState, HiveMessageBody};

use super::{
    LOOPBACK_BODY_LIMIT, bool_argument, local_public_id, numeric_argument, required_string,
    tool_error, tool_success,
};

const DEFAULT_EVENTS_LIMIT: usize = 50;
const MAX_EVENTS_LIMIT: usize = 200;
const DEFAULT_JOIN_WINDOW_MS: u64 = 1_800_000;
const COLLECTIVE_TASK_TYPE: &str = "wattetheria.collective_mission";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CollectiveMissionRunIndex {
    #[serde(default)]
    runs: BTreeMap<String, CollectiveMissionRunLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CollectiveMissionRunLink {
    mission_id: String,
    run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hive_id: Option<String>,
    created_at: String,
    kicked_off: bool,
    #[serde(default)]
    mission: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    join_window_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    join_deadline_ms: Option<u64>,
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

struct CollectiveHiveRoute {
    hive_id: String,
    network_id: Option<String>,
    feed_key: String,
    scope_hint: String,
}

struct CollectiveRunSubmission {
    run_id: String,
    run_spec: Value,
    wattswarm_run: Value,
}

#[derive(Debug, Clone, Copy)]
struct CollectiveJoinPolicy {
    window_ms: Option<u64>,
    deadline_ms: Option<u64>,
}

struct CollectivePublishContext {
    public_id: String,
    mission_id: String,
    mission: Value,
    hive_route: CollectiveHiveRoute,
    join_policy: CollectiveJoinPolicy,
    kickoff: bool,
    phase: &'static str,
}

struct CollectiveRunSubmitRequest<'a> {
    state: &'a ControlPlaneState,
    arguments: &'a Value,
    mission_id: &'a str,
    mission: &'a Value,
    execution: &'a CollectiveExecutionSpec,
    hive_route: &'a CollectiveHiveRoute,
    join_policy: CollectiveJoinPolicy,
    kickoff: bool,
}

struct CollectiveHivePostRequest<'a> {
    state: &'a ControlPlaneState,
    auth: &'a str,
    public_id: String,
    mission_id: &'a str,
    mission: &'a Value,
    hive_route: &'a CollectiveHiveRoute,
    phase: &'a str,
    kickoff: bool,
    join_policy: CollectiveJoinPolicy,
    submission: &'a CollectiveRunSubmission,
}

struct CollectiveHiveMessageRequest<'a> {
    state: &'a ControlPlaneState,
    mission: &'a Value,
    run_id: &'a str,
    phase: &'a str,
    kickoff: bool,
    join_policy: CollectiveJoinPolicy,
    run_spec: &'a Value,
    coordinator_node_id: Option<&'a str>,
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
    let context = match prepare_collective_publish(state, arguments, execution.mode).await {
        Ok(context) => context,
        Err(error) => return tool_error(&json!({"error": error})),
    };
    let submission = match submit_collective_run(CollectiveRunSubmitRequest {
        state,
        arguments,
        mission_id: &context.mission_id,
        mission: &context.mission,
        execution: &execution,
        hive_route: &context.hive_route,
        join_policy: context.join_policy,
        kickoff: context.kickoff,
    })
    .await
    {
        Ok(submission) => submission,
        Err(error) => return error,
    };
    let (hive_message, hive_post) =
        match post_collective_mission_to_hive(CollectiveHivePostRequest {
            state,
            auth,
            public_id: context.public_id.clone(),
            mission_id: &context.mission_id,
            mission: &context.mission,
            hive_route: &context.hive_route,
            phase: context.phase,
            kickoff: context.kickoff,
            join_policy: context.join_policy,
            submission: &submission,
        })
        .await
        {
            Ok(posted) => posted,
            Err(error) => return error,
        };
    let link = CollectiveMissionRunLink {
        mission_id: context.mission_id.clone(),
        run_id: submission.run_id.clone(),
        hive_id: Some(context.hive_route.hive_id.clone()),
        created_at: chrono::Utc::now().to_rfc3339(),
        kicked_off: context.kickoff,
        mission: context.mission.clone(),
        join_window_ms: context.join_policy.window_ms,
        join_deadline_ms: context.join_policy.deadline_ms,
        run_spec: submission.run_spec.clone(),
        wattswarm_run: submission.wattswarm_run.clone(),
    };
    if let Err(error) = save_run_link(state, link.clone()) {
        return tool_error(&json!({
            "error": "persist_collective_mission_run_failed",
            "detail": error,
            "mission_id": context.mission_id,
            "run_id": submission.run_id,
            "wattswarm_run": submission.wattswarm_run,
        }));
    }

    tool_success(&json!({
        "mission_id": context.mission_id,
        "run_id": submission.run_id,
        "kicked_off": context.kickoff,
        "phase": context.phase,
        "join_window_ms": context.join_policy.window_ms,
        "join_deadline_ms": context.join_policy.deadline_ms,
        "mission": context.mission,
        "hive_id": context.hive_route.hive_id,
        "hive_message": hive_message,
        "hive_post": hive_post,
        "run_spec": submission.run_spec,
        "wattswarm_run": submission.wattswarm_run,
        "link": link,
    }))
}

async fn prepare_collective_publish(
    state: &ControlPlaneState,
    arguments: &Value,
    mode: CollectiveExecutionMode,
) -> Result<CollectivePublishContext, String> {
    let public_id = local_public_id(state).await;
    let hive_id =
        optional_string(arguments, "hive_id").ok_or_else(|| "hive_id is required".to_owned())?;
    let hive_route = collective_hive_route(state, arguments, &hive_id).await?;
    let mission_id = optional_string(arguments, "mission_id")
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mission =
        collective_mission_metadata(state, arguments, &public_id, &mission_id, &hive_route);
    let is_stigmergy = mode == CollectiveExecutionMode::Stigmergy;
    let join_policy = collective_join_policy(arguments, is_stigmergy);
    let requested_kickoff = bool_argument(arguments, "kickoff").unwrap_or(true);
    let kickoff = !is_stigmergy && requested_kickoff;
    Ok(CollectivePublishContext {
        public_id,
        mission_id,
        mission,
        hive_route,
        join_policy,
        kickoff,
        phase: if kickoff { "round_started" } else { "joining" },
    })
}

async fn submit_collective_run(
    request: CollectiveRunSubmitRequest<'_>,
) -> Result<CollectiveRunSubmission, Value> {
    let run_spec = build_run_spec(
        request.arguments,
        request.mission_id,
        request.mission,
        request.execution,
        request.hive_route,
        request.join_policy,
    );
    let command = SwarmRunSubmitCommand {
        spec: run_spec.clone(),
        kickoff: request.kickoff,
    };
    let wattswarm_run = request
        .state
        .swarm_bridge
        .submit_run(command)
        .await
        .map_err(|error| {
            tool_error(&json!({
                "error": "wattswarm_run_submit_failed",
                "detail": error.to_string(),
                "mission_id": request.mission_id,
                "mission": request.mission,
                "run_spec": run_spec,
            }))
        })?;
    let run_id = wattswarm_run
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| required_string(&run_spec, "run_id"))
        .unwrap_or_else(|| format!("collective-{}", request.mission_id));
    Ok(CollectiveRunSubmission {
        run_id,
        run_spec,
        wattswarm_run,
    })
}

async fn post_collective_mission_to_hive(
    request: CollectiveHivePostRequest<'_>,
) -> Result<(Value, Value), Value> {
    let coordinator_node_id = request.state.swarm_bridge.local_node_id().await.ok();
    let hive_message = collective_hive_message(&CollectiveHiveMessageRequest {
        state: request.state,
        mission: request.mission,
        run_id: &request.submission.run_id,
        phase: request.phase,
        kickoff: request.kickoff,
        join_policy: request.join_policy,
        run_spec: &request.submission.run_spec,
        coordinator_node_id: coordinator_node_id.as_deref(),
    });
    let hive_response = crate::routes::topics::post_hive_topic_message(
        request.state.clone(),
        auth_headers(request.auth),
        Some(request.hive_route.hive_id.clone()),
        HiveMessageBody {
            public_id: Some(request.public_id),
            network_id: request.hive_route.network_id.clone(),
            feed_key: Some(request.hive_route.feed_key.clone()),
            scope_hint: Some(request.hive_route.scope_hint.clone()),
            content: hive_message.clone(),
            reply_to_message_id: None,
        },
    )
    .await;
    let (hive_status, hive_post) = response_json(hive_response).await?;
    if !hive_status.is_success() {
        return Err(tool_error(&json!({
            "error": "hive_collective_mission_post_failed",
            "status": hive_status.as_u16(),
            "detail": hive_post,
            "mission_id": request.mission_id,
            "run_id": request.submission.run_id,
            "mission": request.mission,
            "run_spec": request.submission.run_spec,
            "wattswarm_run": request.submission.wattswarm_run,
        })));
    }
    Ok((hive_message, hive_post))
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

pub(crate) async fn start_collective_mission_result(
    state: &ControlPlaneState,
    auth: &str,
    arguments: &Value,
) -> Value {
    let public_id = local_public_id(state).await;
    let mut index = match load_run_index(state) {
        Ok(index) => index,
        Err(error) => {
            return tool_error(&json!({
                "error": "load_collective_mission_runs_failed",
                "detail": error,
            }));
        }
    };
    let resolved = match resolve_run_link(
        &index,
        optional_string(arguments, "mission_id").as_deref(),
        arguments,
    ) {
        Ok(resolved) => resolved,
        Err(error) => return tool_error(&json!({"error": error})),
    };
    let mission_id = match resolved.mission_id.as_str() {
        Some(value) => value.to_owned(),
        None => return tool_error(&json!({"error": "collective mission_id is required"})),
    };
    let Some(link) = index.runs.get(&mission_id).cloned() else {
        return tool_error(&json!({"error": "collective mission run link not found"}));
    };
    if let Err(error) = validate_collective_start(arguments, &mission_id, &link) {
        return tool_error(&error);
    }

    let kickoff = match state.swarm_bridge.kickoff_run(&link.run_id).await {
        Ok(payload) => payload,
        Err(error) => {
            return tool_error(&json!({
                "error": "wattswarm_run_kickoff_failed",
                "detail": error.to_string(),
                "mission_id": mission_id,
                "run_id": link.run_id,
            }));
        }
    };

    let hive_route =
        match collective_hive_route_from_link_or_arguments(state, arguments, &link).await {
            Ok(route) => route,
            Err(error) => return tool_error(&json!({"error": error})),
        };
    let submission = CollectiveRunSubmission {
        run_id: link.run_id.clone(),
        run_spec: link.run_spec.clone(),
        wattswarm_run: kickoff.clone(),
    };
    let mission = collective_start_mission(&mission_id, &link, &hive_route);
    let (hive_message, hive_post) =
        match post_collective_mission_to_hive(CollectiveHivePostRequest {
            state,
            auth,
            public_id,
            mission_id: &mission_id,
            mission: &mission,
            hive_route: &hive_route,
            phase: "round_started",
            kickoff: true,
            join_policy: CollectiveJoinPolicy {
                window_ms: link.join_window_ms,
                deadline_ms: link.join_deadline_ms,
            },
            submission: &submission,
        })
        .await
        {
            Ok(posted) => posted,
            Err(error) => return error,
        };

    let updated = update_started_run_link(&mut index, &mission_id, &link, &mission, &kickoff);
    if let Err(error) = save_run_index(state, &index) {
        return tool_error(&json!({
            "error": "persist_collective_mission_run_failed",
            "detail": error,
            "mission_id": mission_id,
            "run_id": updated.run_id,
        }));
    }

    tool_success(&json!({
        "mission_id": mission_id,
        "run_id": updated.run_id,
        "phase": "round_started",
        "kicked_off": true,
        "mission": mission,
        "hive_id": hive_route.hive_id,
        "hive_message": hive_message,
        "hive_post": hive_post,
        "wattswarm_run": kickoff,
        "link": updated,
    }))
}

fn validate_collective_start(
    arguments: &Value,
    mission_id: &str,
    link: &CollectiveMissionRunLink,
) -> Result<(), Value> {
    if link.kicked_off {
        return Err(json!({
            "error": "collective_mission_already_started",
            "mission_id": mission_id,
            "run_id": link.run_id,
        }));
    }
    if bool_argument(arguments, "force").unwrap_or(false) {
        return Ok(());
    }
    validate_join_window_closed(mission_id, link)?;
    validate_min_participants(arguments, mission_id, link)
}

fn validate_join_window_closed(
    mission_id: &str,
    link: &CollectiveMissionRunLink,
) -> Result<(), Value> {
    let Some(deadline) = link.join_deadline_ms else {
        return Ok(());
    };
    let now = current_time_ms();
    if now >= deadline {
        return Ok(());
    }
    Err(json!({
        "error": "collective_join_window_still_open",
        "mission_id": mission_id,
        "run_id": link.run_id,
        "join_deadline_ms": deadline,
        "now_ms": now,
    }))
}

fn validate_min_participants(
    arguments: &Value,
    mission_id: &str,
    link: &CollectiveMissionRunLink,
) -> Result<(), Value> {
    let min_participants = min_participants_from_run_spec(&link.run_spec).unwrap_or(1);
    let joined_count = joined_count_argument(arguments).unwrap_or(0);
    if joined_count >= min_participants {
        return Ok(());
    }
    Err(json!({
        "error": "collective_min_participants_not_met",
        "mission_id": mission_id,
        "run_id": link.run_id,
        "joined_count": joined_count,
        "min_participants": min_participants,
    }))
}

fn collective_start_mission(
    mission_id: &str,
    link: &CollectiveMissionRunLink,
    hive_route: &CollectiveHiveRoute,
) -> Value {
    if !link.mission.is_null() {
        return link.mission.clone();
    }
    json!({
        "mission_id": mission_id,
        "task_id": mission_id,
        "task_type": COLLECTIVE_TASK_TYPE,
        "kind": "collective_mission",
        "lifecycle": "collective",
        "hive_id": hive_route.hive_id,
        "feed_key": hive_route.feed_key,
        "scope_hint": hive_route.scope_hint,
    })
}

fn update_started_run_link(
    index: &mut CollectiveMissionRunIndex,
    mission_id: &str,
    link: &CollectiveMissionRunLink,
    mission: &Value,
    kickoff: &Value,
) -> CollectiveMissionRunLink {
    let mut updated = link.clone();
    updated.kicked_off = true;
    updated.wattswarm_run = kickoff.clone();
    updated.mission = mission.clone();
    index.runs.insert(mission_id.to_owned(), updated.clone());
    updated
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

fn collective_mission_metadata(
    state: &ControlPlaneState,
    arguments: &Value,
    public_id: &str,
    mission_id: &str,
    hive_route: &CollectiveHiveRoute,
) -> Value {
    let source = argument_object(arguments);
    let mut mission = Map::new();
    for key in [
        "title",
        "description",
        "domain",
        "scope",
        "subnet_id",
        "zone_id",
        "required_role",
        "required_faction",
        "skills",
    ] {
        if let Some(value) = source.get(key) {
            mission.insert(key.to_owned(), value.clone());
        }
    }
    mission
        .entry("scope".to_owned())
        .or_insert_with(|| Value::String("real_world".to_owned()));
    mission.insert(
        "mission_id".to_owned(),
        Value::String(mission_id.to_owned()),
    );
    mission.insert("task_id".to_owned(), Value::String(mission_id.to_owned()));
    mission.insert(
        "task_type".to_owned(),
        Value::String(COLLECTIVE_TASK_TYPE.to_owned()),
    );
    mission.insert(
        "kind".to_owned(),
        Value::String("collective_mission".to_owned()),
    );
    mission.insert(
        "lifecycle".to_owned(),
        Value::String("collective".to_owned()),
    );
    mission.insert(
        "payload".to_owned(),
        collective_mission_payload(source.get("payload")),
    );
    mission.insert("publisher".to_owned(), Value::String(public_id.to_owned()));
    mission.insert(
        "publisher_kind".to_owned(),
        Value::String("player".to_owned()),
    );
    mission.insert(
        "hive_id".to_owned(),
        Value::String(hive_route.hive_id.clone()),
    );
    if let Some(network_id) = &hive_route.network_id {
        mission.insert("network_id".to_owned(), Value::String(network_id.clone()));
    }
    mission.insert(
        "feed_key".to_owned(),
        Value::String(hive_route.feed_key.clone()),
    );
    mission.insert(
        "scope_hint".to_owned(),
        Value::String(hive_route.scope_hint.clone()),
    );
    mission.insert(
        "created_at".to_owned(),
        Value::String(chrono::Utc::now().to_rfc3339()),
    );
    state
        .public_geo_payload()
        .insert_into_json_object(&mut mission);
    Value::Object(mission)
}

fn collective_mission_payload(payload: Option<&Value>) -> Value {
    let mut object = match payload {
        Some(Value::Object(object)) => object.clone(),
        Some(value) => {
            let mut object = Map::new();
            object.insert("payload".to_owned(), value.clone());
            object
        }
        None => Map::new(),
    };
    object.insert(
        "task_type".to_owned(),
        Value::String(COLLECTIVE_TASK_TYPE.to_owned()),
    );
    Value::Object(object)
}

fn build_run_spec(
    arguments: &Value,
    mission_id: &str,
    mission: &Value,
    execution: &CollectiveExecutionSpec,
    hive_route: &CollectiveHiveRoute,
    join_policy: CollectiveJoinPolicy,
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
        spec["feed_key"] = Value::String(hive_route.feed_key.clone());
        spec["scope_hint"] = Value::String(hive_route.scope_hint.clone());
        spec["round_policy"] = execution.round_policy.clone().unwrap_or(Value::Null);
        spec["join_policy"] = json!({
            "join_window_ms": join_policy.window_ms,
            "join_deadline_ms": join_policy.deadline_ms,
        });
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
                "hive_id": mission.get("hive_id").cloned().unwrap_or(Value::Null),
                "mission_title": mission.get("title").cloned().unwrap_or(Value::Null),
                "mission_description": mission.get("description").cloned().unwrap_or(Value::Null),
                "mission_payload": mission.get("payload").cloned().unwrap_or(Value::Null),
                "mission": mission,
            })
        })
}

async fn collective_hive_route(
    state: &ControlPlaneState,
    arguments: &Value,
    hive_id: &str,
) -> Result<CollectiveHiveRoute, String> {
    if let Some(hive) = state.hive_registry.lock().await.get(hive_id) {
        return Ok(CollectiveHiveRoute {
            hive_id: hive.topic_id,
            network_id: hive.network_id,
            feed_key: hive.feed_key,
            scope_hint: hive.scope_hint,
        });
    }
    let Some(feed_key) = optional_string(arguments, "feed_key") else {
        return Err(format!(
            "hive not found: {hive_id}; feed_key and scope_hint are required for an unknown Hive route"
        ));
    };
    let Some(scope_hint) = optional_string(arguments, "scope_hint") else {
        return Err(format!(
            "hive not found: {hive_id}; feed_key and scope_hint are required for an unknown Hive route"
        ));
    };
    Ok(CollectiveHiveRoute {
        hive_id: hive_id.to_owned(),
        network_id: optional_string(arguments, "network_id"),
        feed_key,
        scope_hint,
    })
}

async fn collective_hive_route_from_link_or_arguments(
    state: &ControlPlaneState,
    arguments: &Value,
    link: &CollectiveMissionRunLink,
) -> Result<CollectiveHiveRoute, String> {
    let hive_id = optional_string(arguments, "hive_id")
        .or_else(|| link.hive_id.clone())
        .or_else(|| {
            link.mission
                .get("hive_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| "hive_id is required to start a collective mission".to_owned())?;
    collective_hive_route(state, arguments, &hive_id)
        .await
        .or_else(|_| {
            let feed_key = optional_string(arguments, "feed_key")
                .or_else(|| required_string(&link.run_spec, "feed_key"))
                .or_else(|| {
                    link.mission
                        .get("feed_key")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .ok_or_else(|| {
                    "feed_key is required to start a collective mission with an unknown Hive route"
                        .to_owned()
                })?;
            let scope_hint = optional_string(arguments, "scope_hint")
                .or_else(|| required_string(&link.run_spec, "scope_hint"))
                .or_else(|| {
                    link.mission
                        .get("scope_hint")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .ok_or_else(|| {
                    "scope_hint is required to start a collective mission with an unknown Hive route"
                        .to_owned()
                })?;
            Ok(CollectiveHiveRoute {
                hive_id,
                network_id: optional_string(arguments, "network_id").or_else(|| {
                    link.mission
                        .get("network_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                }),
                feed_key,
                scope_hint,
            })
        })
}

fn collective_hive_message(request: &CollectiveHiveMessageRequest<'_>) -> Value {
    json!({
        "type": "collective_mission",
        "version": 1,
        "mission_id": request.mission.get("mission_id").cloned().unwrap_or(Value::Null),
        "run_id": request.run_id,
        "phase": request.phase,
        "kickoff": request.kickoff,
        "join_window_ms": request.join_policy.window_ms,
        "join_deadline_ms": request.join_policy.deadline_ms,
        "coordinator": {
            "agent_did": request.state.agent_did,
            "node_id": request.coordinator_node_id,
        },
        "mission": request.mission,
        "run_spec": request.run_spec,
    })
}

fn auth_headers(auth: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&format!("Bearer {auth}")) {
        headers.insert(header::AUTHORIZATION, value);
    }
    headers
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
        None | Some("committee") => Ok(CollectiveExecutionMode::Committee),
        Some("stigmergy") => Err(
            "collective stigmergy mode is temporarily unsupported; use committee mode. Stigmergy collective missions will be opened later."
                .to_owned(),
        ),
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

fn join_window_ms(arguments: &Value) -> u64 {
    argument_value(arguments, "join_window_ms")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_JOIN_WINDOW_MS)
}

fn collective_join_policy(arguments: &Value, is_stigmergy: bool) -> CollectiveJoinPolicy {
    if !is_stigmergy {
        return CollectiveJoinPolicy {
            window_ms: None,
            deadline_ms: None,
        };
    }
    let window_ms = join_window_ms(arguments);
    CollectiveJoinPolicy {
        window_ms: Some(window_ms),
        deadline_ms: Some(current_time_ms().saturating_add(window_ms)),
    }
}

fn joined_count_argument(arguments: &Value) -> Option<u64> {
    argument_value(arguments, "joined_count")
        .or_else(|| argument_value(arguments, "participant_count"))
        .and_then(Value::as_u64)
}

fn min_participants_from_run_spec(run_spec: &Value) -> Option<u64> {
    run_spec
        .pointer("/round_policy/min_participants")
        .and_then(Value::as_u64)
}

fn current_time_ms() -> u64 {
    chrono::Utc::now()
        .timestamp_millis()
        .try_into()
        .unwrap_or_default()
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
    save_run_index(state, &index)
}

fn save_run_index(
    state: &ControlPlaneState,
    index: &CollectiveMissionRunIndex,
) -> Result<(), String> {
    state
        .local_db
        .save_domain(local_db::domain::COLLECTIVE_MISSION_RUNS, &index)
        .map_err(|error| error.to_string())
}
