use axum::body::to_bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use wattetheria_kernel::local_db;
use wattetheria_kernel::swarm_bridge::{SwarmPeerDmMessageView, SwarmRunSubmitCommand};
use wattetheria_kernel::swarm_sync::SwarmRunResultSnapshot;

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
    #[serde(default)]
    task_prompt: String,
    #[serde(default)]
    participants: BTreeMap<String, CollectiveMissionParticipant>,
    run_spec: Value,
    wattswarm_run: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finalized_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finalized_hive_message: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finalized_hive_post: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CollectiveMissionParticipant {
    agent_id: String,
    executor: String,
    prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    participant_agent_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    participant_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision_id: Option<String>,
    joined_at: String,
    #[serde(default)]
    payload: Value,
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
    mission_id: &'a str,
    mission: &'a Value,
    run_spec: Value,
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
    coordinator_contact_material: Option<&'a Value>,
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
    let task_prompt = collective_task_prompt(arguments, &context.mission);
    let run_spec = build_run_spec(
        arguments,
        &context.mission_id,
        &context.mission,
        &execution,
        &context.hive_route,
        context.join_policy,
    );
    let run_id = required_string(&run_spec, "run_id")
        .unwrap_or_else(|| format!("collective-{}", context.mission_id));
    let submission = pending_collective_submission(&run_id, run_spec);
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
        task_prompt,
        participants: BTreeMap::new(),
        run_spec: submission.run_spec.clone(),
        wattswarm_run: submission.wattswarm_run.clone(),
        finalized_at: None,
        finalized_hive_message: None,
        finalized_hive_post: None,
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
    let join_policy = collective_join_policy(arguments, mode);
    Ok(CollectivePublishContext {
        public_id,
        mission_id,
        mission,
        hive_route,
        join_policy,
        kickoff: false,
        phase: "joining",
    })
}

fn pending_collective_submission(run_id: &str, run_spec: Value) -> CollectiveRunSubmission {
    CollectiveRunSubmission {
        run_id: run_id.to_owned(),
        run_spec,
        wattswarm_run: json!({
            "ok": true,
            "run_id": run_id,
            "submitted": false,
            "kicked_off": false,
        }),
    }
}

async fn submit_collective_run(
    request: CollectiveRunSubmitRequest<'_>,
) -> Result<CollectiveRunSubmission, Value> {
    let command = SwarmRunSubmitCommand {
        spec: request.run_spec.clone(),
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
                "run_spec": request.run_spec,
            }))
        })?;
    let run_id = wattswarm_run
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| required_string(&request.run_spec, "run_id"))
        .unwrap_or_else(|| format!("collective-{}", request.mission_id));
    Ok(CollectiveRunSubmission {
        run_id,
        run_spec: request.run_spec,
        wattswarm_run,
    })
}

async fn post_collective_mission_to_hive(
    request: CollectiveHivePostRequest<'_>,
) -> Result<(Value, Value), Value> {
    let coordinator_node_id = request.state.swarm_bridge.local_node_id().await.ok();
    let coordinator_contact_material = if collective_route_is_private_hive(request.hive_route) {
        None
    } else {
        match request.state.swarm_bridge.local_contact_material().await {
            Ok(material) => Some(material),
            Err(error) => {
                return Err(tool_error(&json!({
                    "error": "coordinator_contact_material_unavailable",
                    "detail": error.to_string(),
                    "mission_id": request.mission_id,
                    "run_id": request.submission.run_id,
                })));
            }
        }
    };
    let hive_message = collective_hive_message(&CollectiveHiveMessageRequest {
        state: request.state,
        mission: request.mission,
        run_id: &request.submission.run_id,
        phase: request.phase,
        kickoff: request.kickoff,
        join_policy: request.join_policy,
        run_spec: &request.submission.run_spec,
        coordinator_node_id: coordinator_node_id.as_deref(),
        coordinator_contact_material: coordinator_contact_material.as_ref(),
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
    match start_collective_mission_core(state, auth, arguments).await {
        Ok(started) => tool_success(&started),
        Err(error) => tool_error(&error),
    }
}

pub(crate) async fn start_due_collective_missions(
    state: &ControlPlaneState,
    limit: usize,
) -> anyhow::Result<usize> {
    let index = load_run_index(state).map_err(anyhow::Error::msg)?;
    let due_run_ids = due_collective_run_ids(&index, limit);
    let mut processed = 0;
    for run_id in due_run_ids {
        let result = start_collective_mission_core(
            state,
            &state.auth_token,
            &json!({
                "run_id": run_id,
            }),
        )
        .await;
        match result {
            Ok(_) => processed += 1,
            Err(error) => {
                if error.get("error").and_then(Value::as_str)
                    == Some("collective_mission_already_started")
                {
                    continue;
                }
                return Err(anyhow::anyhow!(error.to_string()));
            }
        }
    }
    Ok(processed)
}

pub(crate) async fn publish_finalized_collective_mission_results(
    state: &ControlPlaneState,
    auth: &str,
    limit: usize,
) -> anyhow::Result<usize> {
    if limit == 0 {
        return Ok(0);
    }
    let index = load_run_index(state).map_err(anyhow::Error::msg)?;
    let mission_ids = finalized_publish_candidate_mission_ids(&index, limit);
    let mut processed = 0;
    for mission_id in mission_ids {
        if publish_finalized_collective_mission_result(state, auth, &mission_id).await? {
            processed += 1;
        }
    }
    Ok(processed)
}

async fn publish_finalized_collective_mission_result(
    state: &ControlPlaneState,
    auth: &str,
    mission_id: &str,
) -> anyhow::Result<bool> {
    let mut index = load_run_index(state).map_err(anyhow::Error::msg)?;
    let Some(link) = index.runs.get(mission_id).cloned() else {
        return Ok(false);
    };
    if !link.kicked_off || link.finalized_hive_message.is_some() {
        return Ok(false);
    }
    let Ok(result) = state.swarm_bridge.run_result_snapshot(&link.run_id).await else {
        return Ok(false);
    };
    if !collective_run_result_is_finalized(&result.result) {
        return Ok(false);
    }
    let hive_route = collective_hive_route_from_link_or_arguments(state, &json!({}), &link)
        .await
        .map_err(anyhow::Error::msg)?;
    let public_id = local_public_id(state).await;
    let finalized_at = chrono::Utc::now().to_rfc3339();
    let hive_message = collective_finalized_hive_message(&link, &result, &public_id, &finalized_at);
    let hive_response = crate::routes::topics::post_hive_topic_message(
        state.clone(),
        auth_headers(auth),
        Some(hive_route.hive_id.clone()),
        HiveMessageBody {
            public_id: Some(public_id),
            network_id: hive_route.network_id.clone(),
            feed_key: Some(hive_route.feed_key.clone()),
            scope_hint: Some(hive_route.scope_hint.clone()),
            content: hive_message.clone(),
            reply_to_message_id: None,
        },
    )
    .await;
    let (hive_status, hive_post) = response_json(hive_response).await.map_err(|error| {
        anyhow::anyhow!(
            "decode finalized collective Hive post response for {}: {}",
            link.run_id,
            error
        )
    })?;
    if !hive_status.is_success() {
        return Err(anyhow::anyhow!(
            "finalized collective Hive post failed for {}: status={} detail={}",
            link.run_id,
            hive_status.as_u16(),
            hive_post
        ));
    }

    let Some(updated) = index.runs.get_mut(mission_id) else {
        return Ok(false);
    };
    updated.finalized_at = Some(finalized_at);
    updated.finalized_hive_message = Some(hive_message);
    updated.finalized_hive_post = Some(hive_post);
    save_run_index(state, &index).map_err(anyhow::Error::msg)?;
    Ok(true)
}

fn finalized_publish_candidate_mission_ids(
    index: &CollectiveMissionRunIndex,
    limit: usize,
) -> Vec<String> {
    index
        .runs
        .values()
        .filter(|link| link.kicked_off)
        .filter(|link| link.finalized_hive_message.is_none())
        .take(limit)
        .map(|link| link.mission_id.clone())
        .collect()
}

async fn start_collective_mission_core(
    state: &ControlPlaneState,
    auth: &str,
    arguments: &Value,
) -> Result<Value, Value> {
    let public_id = local_public_id(state).await;
    let mut index = match load_run_index(state) {
        Ok(index) => index,
        Err(error) => {
            return Err(json!({
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
        Err(error) => return Err(json!({"error": error})),
    };
    let mission_id = match resolved.mission_id.as_str() {
        Some(value) => value.to_owned(),
        None => return Err(json!({"error": "collective mission_id is required"})),
    };
    let Some(link) = index.runs.get(&mission_id).cloned() else {
        return Err(json!({"error": "collective mission run link not found"}));
    };
    validate_collective_start(arguments, &mission_id, &link)?;

    let participant_agents = match collective_participant_agents(&link) {
        Ok(agents) => agents,
        Err(error) => return Err(error),
    };

    let hive_route =
        match collective_hive_route_from_link_or_arguments(state, arguments, &link).await {
            Ok(route) => route,
            Err(error) => return Err(json!({"error": error})),
        };
    let mission = collective_start_mission(&mission_id, &link, &hive_route);
    let run_spec = build_started_run_spec(&link, participant_agents);
    let submission = match submit_collective_run(CollectiveRunSubmitRequest {
        state,
        mission_id: &mission_id,
        mission: &mission,
        run_spec,
        kickoff: true,
    })
    .await
    {
        Ok(submission) => submission,
        Err(error) => return Err(tool_error_payload(&error)),
    };
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
            Err(error) => return Err(tool_error_payload(&error)),
        };

    let updated = update_started_run_link(&mut index, &mission_id, &link, &mission, &submission);
    if let Err(error) = save_run_index(state, &index) {
        return Err(json!({
            "error": "persist_collective_mission_run_failed",
            "detail": error,
            "mission_id": mission_id,
            "run_id": updated.run_id,
        }));
    }

    Ok(json!({
        "mission_id": mission_id,
        "run_id": updated.run_id,
        "phase": "round_started",
        "kicked_off": true,
        "mission": mission,
        "hive_id": hive_route.hive_id,
        "hive_message": hive_message,
        "hive_post": hive_post,
        "run_spec": submission.run_spec,
        "wattswarm_run": submission.wattswarm_run,
        "link": updated,
    }))
}

fn tool_error_payload(value: &Value) -> Value {
    value.get("structuredContent").cloned().unwrap_or_else(|| {
        value
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_else(|| value.clone())
    })
}

fn due_collective_run_ids(index: &CollectiveMissionRunIndex, limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let now = current_time_ms();
    index
        .runs
        .values()
        .filter(|link| !link.kicked_off)
        .filter(|link| link.join_deadline_ms.is_none_or(|deadline| now >= deadline))
        .filter(|link| {
            let min_participants = min_participants_from_run_spec(&link.run_spec).unwrap_or(1);
            (link.participants.len() as u64) >= min_participants
        })
        .take(limit)
        .map(|link| link.run_id.clone())
        .collect()
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
    validate_min_participants(mission_id, link)
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
    mission_id: &str,
    link: &CollectiveMissionRunLink,
) -> Result<(), Value> {
    let min_participants = min_participants_from_run_spec(&link.run_spec).unwrap_or(1);
    let joined_count = link.participants.len() as u64;
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
    submission: &CollectiveRunSubmission,
) -> CollectiveMissionRunLink {
    let mut updated = link.clone();
    updated.kicked_off = true;
    updated.run_spec = submission.run_spec.clone();
    updated.wattswarm_run = submission.wattswarm_run.clone();
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
    spec["round_policy"] = execution.round_policy.clone().unwrap_or(Value::Null);
    if let Some(window_ms) = join_policy.window_ms {
        spec["join_policy"] = json!({
            "join_window_ms": window_ms,
            "join_deadline_ms": join_policy.deadline_ms,
        });
    }
    if execution.mode == CollectiveExecutionMode::Stigmergy {
        spec["market_task_id"] = Value::String(mission_id.to_owned());
        spec["feed_key"] = Value::String(hive_route.feed_key.clone());
        spec["scope_hint"] = Value::String(hive_route.scope_hint.clone());
    }
    spec
}

fn build_started_run_spec(link: &CollectiveMissionRunLink, agents: Value) -> Value {
    let mut spec = link.run_spec.clone();
    spec["agents"] = agents;
    if let Some(object) = spec.as_object_mut()
        && let Some(policy) = object.remove("round_policy")
    {
        object.insert("collective_policy".to_owned(), policy);
    }
    if let Some(object) = spec.as_object_mut() {
        object.remove("market_task_id");
        object.remove("feed_key");
        object.remove("scope_hint");
    }
    spec
}

fn collective_participant_agents(link: &CollectiveMissionRunLink) -> Result<Value, Value> {
    if link.participants.is_empty() {
        return Err(json!({
            "error": "collective_no_participants_joined",
            "mission_id": link.mission_id,
            "run_id": link.run_id,
        }));
    }
    let agents = link
        .participants
        .values()
        .map(collective_participant_agent_spec)
        .collect::<Vec<_>>();
    Ok(Value::Array(agents))
}

fn collective_participant_agent_spec(participant: &CollectiveMissionParticipant) -> Value {
    let mut spec = Map::new();
    spec.insert(
        "agent_id".to_owned(),
        Value::String(participant.agent_id.clone()),
    );
    spec.insert(
        "executor".to_owned(),
        Value::String(participant.executor.clone()),
    );
    spec.insert(
        "prompt".to_owned(),
        Value::String(participant.prompt.clone()),
    );
    if let Some(profile) = &participant.profile {
        spec.insert("profile".to_owned(), Value::String(profile.clone()));
    }
    if let Some(weight) = participant.weight {
        spec.insert("weight".to_owned(), json!(weight));
    }
    if let Some(priority) = participant.priority {
        spec.insert("priority".to_owned(), json!(priority));
    }
    Value::Object(spec)
}

fn collective_task_prompt(arguments: &Value, mission: &Value) -> String {
    optional_string(arguments, "task_prompt")
        .or_else(|| optional_string(arguments, "prompt"))
        .unwrap_or_else(|| default_collective_task_prompt(mission))
}

fn default_collective_task_prompt(mission: &Value) -> String {
    let title = mission
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Collective mission");
    let description = mission
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("Complete the assigned collective mission.");
    let domain = mission
        .get("domain")
        .and_then(Value::as_str)
        .unwrap_or("unspecified");
    let scope = mission
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("real_world");
    let skills = mission
        .get("skills")
        .filter(|value| !value.is_null())
        .map_or_else(|| "[]".to_owned(), Value::to_string);
    let payload = mission
        .get("payload")
        .filter(|value| !value.is_null())
        .map_or_else(|| "{}".to_owned(), Value::to_string);
    format!(
        "Collective mission: {title}\nDomain: {domain}\nScope: {scope}\nDescription: {description}\nRequired/visible skills: {skills}\nPayload: {payload}\n\nComplete this task independently from your own perspective. Apply your own available skills, expertise, role, and local context. If the mission specifies required skills, prioritize contributions matching those skills. Do not assume other participants share your skills, role, context, or conclusion. Treat the mission description and payload as the source of truth. Return a concise structured result with evidence, reasoning, and assumptions. Do not wait for other participants."
    )
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

fn collective_route_is_private_hive(route: &CollectiveHiveRoute) -> bool {
    route.feed_key != "wattswarm.dm" && route.scope_hint.starts_with("group:dm-")
}

fn collective_hive_message(request: &CollectiveHiveMessageRequest<'_>) -> Value {
    let mut message = json!({
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
    });
    if let Some(contact_material) = request.coordinator_contact_material {
        message["contact_material"] = contact_material.clone();
    }
    message
}

fn collective_finalized_hive_message(
    link: &CollectiveMissionRunLink,
    result: &SwarmRunResultSnapshot,
    coordinator_public_id: &str,
    finalized_at: &str,
) -> Value {
    let policy = collective_policy_from_run_spec(&link.run_spec);
    let aggregation = collective_finalized_aggregation(&result.result, policy.as_ref());
    let final_summary = collective_final_summary(&result.result, &aggregation);
    let joined_count = u64::try_from(link.participants.len()).unwrap_or(u64::MAX);
    let submitted_count = collective_submitted_count(&result.result);
    let missing_count = submitted_count.map(|submitted| joined_count.saturating_sub(submitted));
    json!({
        "type": "collective_mission_finalized",
        "kind": "collective_mission_finalized",
        "version": 1,
        "mission_id": link.mission_id,
        "run_id": link.run_id,
        "title": link.mission.get("title").cloned().unwrap_or(Value::Null),
        "domain": link.mission.get("domain").cloned().unwrap_or(Value::Null),
        "mode": "committee",
        "scope": link.mission.get("scope").cloned().unwrap_or(Value::Null),
        "status": "finalized",
        "mission": link.mission,
        "final": final_summary,
        "aggregation": aggregation,
        "participation": {
            "joined_count": joined_count,
            "submitted_count": submitted_count,
            "missing_count": missing_count,
            "missing_views": collective_missing_views(&result.result),
        },
        "rounds": {
            "round_count": collective_number_from_paths(&result.result, &[
                &["round_count"],
                &["aggregation", "round_count"],
                &["result", "round_count"],
            ]),
            "max_rounds": policy.as_ref().and_then(|value| value.get("max_rounds")).cloned().unwrap_or(Value::Null),
        },
        "evidence": {
            "key_takeaways": collective_array_from_paths(&result.result, &[
                &["key_takeaways"],
                &["final", "key_takeaways"],
                &["result", "key_takeaways"],
            ]),
        },
        "coordinator": {
            "agent_did": link.mission.get("publisher_agent_did").cloned().unwrap_or(Value::Null),
            "public_id": coordinator_public_id,
            "display_name": link.mission.get("source_display_name").cloned().unwrap_or_else(|| link.mission.get("publisher").cloned().unwrap_or(Value::Null)),
        },
        "finalized_at": finalized_at,
    })
}

fn collective_run_result_is_finalized(result: &Value) -> bool {
    collective_text_from_paths(result, &[&["status"], &["result", "status"]])
        .is_some_and(|status| status.eq_ignore_ascii_case("finalized"))
}

fn collective_policy_from_run_spec(run_spec: &Value) -> Option<Value> {
    run_spec
        .get("collective_policy")
        .or_else(|| run_spec.get("round_policy"))
        .filter(|value| value.is_object())
        .cloned()
}

fn collective_finalized_aggregation(result: &Value, policy: Option<&Value>) -> Value {
    let source = result
        .get("aggregation")
        .or_else(|| result.pointer("/result/aggregation"))
        .filter(|value| value.is_object());
    let mut object = Map::new();
    for key in [
        "mode",
        "source",
        "final_decision",
        "final_answer",
        "decision_votes",
        "answer_votes",
        "quorum_met",
        "resolution_paths",
        "null_resolution",
        "null_policy",
    ] {
        if let Some(value) = source.and_then(|source| source.get(key)).cloned() {
            object.insert(key.to_owned(), value);
        }
    }
    if !object.contains_key("final_decision")
        && let Some(value) = result.get("final_decision").cloned()
    {
        object.insert("final_decision".to_owned(), value);
    }
    if !object.contains_key("final_answer")
        && let Some(value) = result.get("final_answer").cloned()
    {
        object.insert("final_answer".to_owned(), value);
    }
    if let Some(policy) = policy {
        for key in ["threshold_percent", "fallback_decision", "min_participants"] {
            if !object.contains_key(key)
                && let Some(value) = policy.get(key).cloned()
            {
                object.insert(key.to_owned(), value);
            }
        }
    }
    Value::Object(object)
}

fn collective_final_summary(result: &Value, aggregation: &Value) -> Value {
    let summary = collective_text_from_paths(
        result,
        &[
            &["final", "summary"],
            &["result", "summary"],
            &["summary"],
            &["result_summary"],
        ],
    )
    .or_else(|| value_string(aggregation, "final_answer"))
    .or_else(|| value_string(aggregation, "final_decision"));
    json!({
        "summary": summary,
        "decision": value_string(aggregation, "final_decision")
            .or_else(|| collective_text_from_paths(result, &[&["final_decision"]])),
        "answer": value_string(aggregation, "final_answer")
            .or_else(|| collective_text_from_paths(result, &[&["final_answer"]])),
        "confidence": collective_number_from_paths(result, &[
            &["final", "confidence"],
            &["confidence"],
            &["aggregation", "confidence"],
        ]),
    })
}

fn collective_submitted_count(result: &Value) -> Option<u64> {
    collective_number_from_paths(
        result,
        &[
            &["submitted_count"],
            &["counts", "submitted"],
            &["counts", "succeeded"],
            &["result", "counts", "submitted"],
            &["result", "counts", "succeeded"],
        ],
    )
}

fn collective_missing_views(result: &Value) -> Value {
    collective_array_from_paths(
        result,
        &[
            &["missing_views"],
            &["result", "missing_views"],
            &["aggregation", "missing_views"],
        ],
    )
}

fn collective_text_from_paths(result: &Value, paths: &[&[&str]]) -> Option<String> {
    paths
        .iter()
        .find_map(|path| value_at_path(result, path))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn collective_number_from_paths(result: &Value, paths: &[&[&str]]) -> Option<u64> {
    paths
        .iter()
        .find_map(|path| value_at_path(result, path))
        .and_then(Value::as_u64)
}

fn collective_array_from_paths(result: &Value, paths: &[&[&str]]) -> Value {
    paths
        .iter()
        .find_map(|path| value_at_path(result, path))
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()))
}

fn value_at_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
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
    let round_policy = collective_round_policy(arguments)?;
    Ok(CollectiveExecutionSpec {
        mode,
        agents: json!([]),
        round_policy: Some(round_policy),
    })
}

fn collective_execution_mode(arguments: &Value) -> Result<CollectiveExecutionMode, String> {
    match optional_string(arguments, "mode").as_deref() {
        None => Err("mode is required for collective mission".to_owned()),
        Some("committee") => Ok(CollectiveExecutionMode::Committee),
        Some("stigmergy") => Err(
            "collective stigmergy mode is temporarily unsupported; use committee mode. Stigmergy collective missions will be opened later."
                .to_owned(),
        ),
        Some(mode) => Err(format!("mode must be committee or stigmergy, got {mode}")),
    }
}

fn collective_round_policy(arguments: &Value) -> Result<Value, String> {
    let min_participants = required_positive_integer(arguments, "min_participants")?;
    let mut policy = json!({
        "min_participants": min_participants,
    });
    if let Some(threshold_percent) = optional_positive_integer(arguments, "threshold_percent")? {
        if threshold_percent > 100 {
            return Err("threshold_percent must be between 1 and 100".to_owned());
        }
        policy["threshold_percent"] = Value::from(threshold_percent);
    }
    if let Some(round_timeout_ms) = optional_positive_integer(arguments, "round_timeout_ms")? {
        policy["round_timeout_ms"] = Value::from(round_timeout_ms);
    }
    if let Some(max_rounds) = optional_positive_integer(arguments, "max_rounds")? {
        policy["max_rounds"] = Value::from(max_rounds);
    }
    if let Some(fallback_decision) = optional_string(arguments, "fallback_decision") {
        policy["fallback_decision"] = Value::String(fallback_decision);
    }
    Ok(policy)
}

fn required_positive_integer(arguments: &Value, key: &str) -> Result<u64, String> {
    let Some(value) = argument_value(arguments, key).and_then(Value::as_u64) else {
        return Err(format!("{key} is required for collective mission"));
    };
    if value == 0 {
        return Err(format!("{key} must be greater than zero"));
    }
    Ok(value)
}

fn optional_positive_integer(arguments: &Value, key: &str) -> Result<Option<u64>, String> {
    let Some(value) = argument_value(arguments, key) else {
        return Ok(None);
    };
    let Some(value) = value.as_u64() else {
        return Err(format!("{key} must be a positive integer"));
    };
    if value == 0 {
        return Err(format!("{key} must be greater than zero"));
    }
    Ok(Some(value))
}

fn join_window_ms(arguments: &Value) -> u64 {
    argument_value(arguments, "join_window_ms")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_JOIN_WINDOW_MS)
}

fn collective_join_policy(
    arguments: &Value,
    mode: CollectiveExecutionMode,
) -> CollectiveJoinPolicy {
    if mode != CollectiveExecutionMode::Committee {
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

pub(crate) fn record_collective_participation_from_dm(
    state: &ControlPlaneState,
    view: &SwarmPeerDmMessageView,
) -> Result<Option<Value>, String> {
    if view.direction != "inbound" {
        return Ok(None);
    }
    let Some(message) = collective_participation_dm_message(view) else {
        return Ok(None);
    };
    if message.get("type").and_then(Value::as_str) != Some("collective_participation") {
        return Ok(None);
    }
    if message.get("status").and_then(Value::as_str) != Some("join") {
        return Ok(None);
    }
    let Some(mission_id) = value_string(message, "mission_id") else {
        return Ok(Some(json!({
            "recorded": false,
            "reason": "missing_mission_id",
            "message_id": view.message_id,
        })));
    };
    let Some(run_id) = value_string(message, "run_id") else {
        return Ok(Some(json!({
            "recorded": false,
            "reason": "missing_run_id",
            "mission_id": mission_id,
            "message_id": view.message_id,
        })));
    };
    let mut index = load_run_index(state)?;
    let Some(link) = index.runs.get_mut(&mission_id) else {
        return Ok(Some(json!({
            "recorded": false,
            "reason": "collective_mission_run_link_not_found",
            "mission_id": mission_id,
            "run_id": run_id,
            "message_id": view.message_id,
        })));
    };
    if link.run_id != run_id {
        return Ok(Some(json!({
            "recorded": false,
            "reason": "collective_mission_run_id_mismatch",
            "mission_id": mission_id,
            "expected_run_id": link.run_id,
            "run_id": run_id,
            "message_id": view.message_id,
        })));
    }
    if link.kicked_off {
        return Ok(Some(json!({
            "recorded": false,
            "reason": "collective_mission_already_started",
            "mission_id": mission_id,
            "run_id": run_id,
            "message_id": view.message_id,
        })));
    }
    let participant = collective_participant_from_dm(link, message, view);
    let key = collective_participant_key(&participant, view);
    let inserted = if link.participants.contains_key(&key) {
        false
    } else {
        link.participants.insert(key.clone(), participant);
        true
    };
    let joined_count = link.participants.len();
    if inserted {
        save_run_index(state, &index)?;
    }
    Ok(Some(json!({
        "recorded": true,
        "inserted": inserted,
        "mission_id": mission_id,
        "run_id": run_id,
        "participant_key": key,
        "joined_count": joined_count,
        "message_id": view.message_id,
    })))
}

fn collective_participation_dm_message(view: &SwarmPeerDmMessageView) -> Option<&Value> {
    view.agent_envelope
        .as_ref()
        .map(|envelope| &envelope.message)
        .filter(|message| {
            message.get("type").and_then(Value::as_str) == Some("collective_participation")
        })
        .or_else(|| {
            if view.content.get("type").and_then(Value::as_str) == Some("collective_participation")
            {
                Some(&view.content)
            } else {
                None
            }
        })
}

fn collective_participant_from_dm(
    link: &CollectiveMissionRunLink,
    message: &Value,
    view: &SwarmPeerDmMessageView,
) -> CollectiveMissionParticipant {
    let payload = message.get("payload").cloned().unwrap_or_else(|| json!({}));
    let participant_agent_did = value_string(message, "participant_agent_did")
        .or_else(|| value_string(&payload, "agent_did"))
        .or_else(|| {
            view.agent_envelope
                .as_ref()
                .and_then(|envelope| envelope.source_agent_id.clone())
        });
    let participant_node_id = value_string(message, "participant_node_id")
        .or_else(|| value_string(&payload, "node_id"))
        .or_else(|| {
            view.agent_envelope
                .as_ref()
                .and_then(|envelope| envelope.source_node_id.clone())
        })
        .or_else(|| Some(view.remote_node_id.clone()));
    let public_id = value_string(&payload, "public_id")
        .or_else(|| value_string(message, "participant_public_id"))
        .or_else(|| {
            view.agent_envelope
                .as_ref()
                .and_then(|envelope| envelope.source_agent_card.as_ref())
                .and_then(|card| {
                    value_string(&card.card, "public_id").or_else(|| value_string(&card.card, "id"))
                })
        });
    let agent_id = value_string(&payload, "agent_id")
        .or_else(|| public_id.clone())
        .or_else(|| participant_node_id.clone())
        .or_else(|| participant_agent_did.clone())
        .unwrap_or_else(|| view.remote_node_id.clone());
    let executor = value_string(&payload, "executor")
        .or_else(|| {
            participant_node_id
                .as_ref()
                .map(|node| format!("remote:{node}"))
        })
        .unwrap_or_else(|| format!("remote:{}", view.remote_node_id));
    let prompt = value_string(&payload, "prompt")
        .or_else(|| (!link.task_prompt.trim().is_empty()).then(|| link.task_prompt.clone()))
        .unwrap_or_else(|| default_collective_task_prompt(&link.mission));
    CollectiveMissionParticipant {
        agent_id,
        executor,
        prompt,
        profile: value_string(&payload, "profile"),
        weight: payload.get("weight").and_then(Value::as_f64),
        priority: payload.get("priority").and_then(Value::as_i64),
        participant_agent_did,
        participant_node_id,
        public_id,
        event_id: value_string(message, "event_id"),
        decision_id: value_string(message, "decision_id"),
        joined_at: chrono::Utc::now().to_rfc3339(),
        payload,
    }
}

fn collective_participant_key(
    participant: &CollectiveMissionParticipant,
    view: &SwarmPeerDmMessageView,
) -> String {
    participant
        .public_id
        .as_ref()
        .map(|value| format!("public:{value}"))
        .or_else(|| {
            participant
                .participant_node_id
                .as_ref()
                .map(|value| format!("node:{value}"))
        })
        .or_else(|| {
            participant
                .participant_agent_did
                .as_ref()
                .map(|value| format!("agent:{value}"))
        })
        .unwrap_or_else(|| format!("message:{}", view.message_id))
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
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
