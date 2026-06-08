use anyhow::{Context, bail};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::{authorize, internal_error};
use crate::routes::mcp::{
    fetch_gateway_tasks, normalized_gateway_tasks_url, resolve_gateway_query_url,
};
use crate::routes::reward_events::{ContributionEventArgs, record_contribution_event};
use crate::routes::reward_view::refresh_known_wallet_balances;
use crate::routes::settlement_delegation::{
    normalize_publish_delegation, payload_with_settlement_delegation,
    settlement_delegation_from_payload,
};
use crate::social_host::{SignedAgentEnvelopeArgs, build_signed_agent_envelope_for_nodes};
use crate::state::{
    ControlPlaneState, MissionClaimBody, MissionPublishBody, MissionSettleBody, MissionsQuery,
    StreamEvent, agent_commit_context_from_headers,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::missions::{
    CivilMission, MissionStatus, NetworkMissionClaimRegistry,
};
use wattetheria_kernel::identities::{ControllerBinding, PublicIdentity};
use wattetheria_kernel::local_db;
use wattetheria_kernel::swarm_bridge::{
    SwarmAgentEnvelope, SwarmTaskAnnounceCommand, SwarmTaskClaimCommand,
    SwarmTaskProposeCandidateCommand,
};
use wattetheria_kernel::tasks::system_puzzle::{
    SystemPuzzleSettlement, system_puzzle_settlement_from_mission,
};
use wattswarm_protocol::types::{ClaimRole, TaskContract};

const MISSION_FEED_KEY: &str = "wattetheria.missions";
const GATEWAY_CONTRACT_FETCH_LIMIT: usize = 200;
pub(crate) const MISSION_TASK_NO_EXPIRY_MS: u64 = u64::MAX;

struct CommitResponseArgs<'a> {
    action_type: &'a str,
    target_id: Option<String>,
    actor_agent_did: Option<String>,
    request_json: &'a Value,
    response_json: &'a Value,
}

struct NetworkMissionClaimRoute {
    task_id: String,
    mission_feed_key: String,
    mission_scope_hint: String,
    publisher_wattswarm_node_id: Option<String>,
    status: Option<String>,
}

fn mission_stream_kind(action: &str) -> &'static str {
    match action {
        "claim" => "mission.claimed",
        "complete" => "mission.completed",
        _ => "mission.updated",
    }
}

fn mission_signed_event_kind(action: &str) -> &'static str {
    match action {
        "claim" => "MISSION_CLAIMED",
        "complete" => "MISSION_COMPLETED",
        _ => "MISSION_UPDATED",
    }
}

fn mission_execution_id(mission_id: &str, agent_did: &str) -> String {
    format!("wattetheria:{mission_id}:{agent_did}")
}

fn mission_already_claimed_response(
    mission_id: &str,
    task_id: &str,
    agent_did: &str,
    execution_id: &str,
    detail: impl Into<String>,
) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "ok": false,
            "error": "mission already claimed",
            "code": "mission_already_claimed",
            "mission_id": mission_id,
            "task_id": task_id,
            "agent_did": agent_did,
            "execution_id": execution_id,
            "claim_status": "already_claimed",
            "detail": detail.into(),
        })),
    )
        .into_response()
}

fn mission_not_claimable_response(
    mission_id: &str,
    task_id: &str,
    agent_did: &str,
    execution_id: &str,
    status: &str,
) -> Response {
    let normalized = status.trim().to_ascii_lowercase();
    if normalized == "claimed" {
        return mission_already_claimed_response(
            mission_id,
            task_id,
            agent_did,
            execution_id,
            "Gateway reports this mission is already claimed.",
        );
    }
    (
        StatusCode::CONFLICT,
        Json(json!({
            "ok": false,
            "error": "mission is not claimable",
            "code": "mission_not_claimable",
            "mission_id": mission_id,
            "task_id": task_id,
            "agent_did": agent_did,
            "execution_id": execution_id,
            "claim_status": normalized,
            "detail": "Gateway reports this mission is not open for claims.",
        })),
    )
        .into_response()
}

fn network_claim_status_allows_claim(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "published" | "open"
    )
}

fn network_claim_already_recorded(
    state: &ControlPlaneState,
    mission_id: &str,
    task_id: &str,
    agent_did: &str,
) -> anyhow::Result<bool> {
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(local_db::domain::NETWORK_MISSION_CLAIMS)?;
    Ok(registry.contains(mission_id, task_id, agent_did))
}

fn record_network_claim_submission(
    state: &ControlPlaneState,
    mission_id: &str,
    task_id: &str,
    agent_did: &str,
    execution_id: &str,
) -> anyhow::Result<()> {
    let mut registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(local_db::domain::NETWORK_MISSION_CLAIMS)?;
    registry.record(mission_id, task_id, agent_did, execution_id);
    state
        .local_db
        .save_domain(local_db::domain::NETWORK_MISSION_CLAIMS, &registry)
}

fn network_claim_error_response(
    body: &MissionClaimBody,
    route: &NetworkMissionClaimRoute,
    execution_id: &str,
    error: &anyhow::Error,
) -> Response {
    let message = error.to_string();
    if message.contains("lease conflict") {
        return mission_already_claimed_response(
            &body.mission_id,
            &route.task_id,
            &body.agent_did,
            execution_id,
            "Wattswarm rejected the claim because another active lease already exists for this task and role.",
        );
    }
    internal_error(error)
}

fn mission_candidate_id(mission_id: &str, agent_did: &str) -> String {
    format!("wattetheria-candidate-{mission_id}-{agent_did}")
}

fn non_empty_string(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

struct TaskAgentEnvelopeArgs {
    source_agent_id: String,
    source_display_name: Option<String>,
    target_agent_id: Option<String>,
    source_node_id: Option<String>,
    target_node_id: Option<String>,
    capability: String,
    message: Value,
}

fn task_agent_envelope(
    state: &ControlPlaneState,
    args: TaskAgentEnvelopeArgs,
) -> anyhow::Result<SwarmAgentEnvelope> {
    build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: args.source_agent_id,
            source_display_name: args.source_display_name,
            target_agent_id: args.target_agent_id,
            source_node_id: args.source_node_id,
            target_node_id: args.target_node_id,
            capability: args.capability,
            message: args.message,
            extensions: None,
        },
    )
}

async fn agent_display_name_for_did(state: &ControlPlaneState, agent_did: &str) -> Option<String> {
    let agent_did = agent_did.trim();
    if agent_did.is_empty() {
        return None;
    }
    state
        .public_identity_registry
        .lock()
        .await
        .list()
        .into_iter()
        .find(|identity| identity.active && identity.agent_did.as_deref() == Some(agent_did))
        .map(|identity| identity.display_name)
}

fn publisher_agent_did_from_claim(body: &MissionClaimBody) -> Option<String> {
    claim_route_string(body, "publisher_agent_did").or_else(|| {
        claim_route_value(body, "task_inputs")
            .and_then(|inputs| inputs.get("publisher_agent_did"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn claim_route_string(body: &MissionClaimBody, field: &str) -> Option<String> {
    body.claim_route
        .as_ref()
        .and_then(|route| route.get(field))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn claim_route_value<'a>(body: &'a MissionClaimBody, field: &str) -> Option<&'a Value> {
    body.claim_route.as_ref().and_then(|route| route.get(field))
}

fn network_claim_route(body: &MissionClaimBody) -> Result<NetworkMissionClaimRoute, String> {
    let task_id = non_empty_string(body.task_id.as_ref())
        .or_else(|| claim_route_string(body, "task_id"))
        .unwrap_or_else(|| body.mission_id.clone());
    let publisher_wattswarm_node_id = non_empty_string(body.publisher_wattswarm_node_id.as_ref())
        .or_else(|| claim_route_string(body, "publisher_wattswarm_node_id"));
    let mission_feed_key = non_empty_string(body.mission_feed_key.as_ref())
        .or_else(|| claim_route_string(body, "mission_feed_key"))
        .unwrap_or_else(|| MISSION_FEED_KEY.to_owned());
    let mission_scope_hint = non_empty_string(body.mission_scope_hint.as_ref())
        .or_else(|| claim_route_string(body, "mission_scope_hint"))
        .or_else(|| {
            publisher_wattswarm_node_id
                .as_ref()
                .map(|node_id| publisher_node_scope_hint(node_id))
        })
        .ok_or_else(|| {
            "network mission transition requires mission_scope_hint or publisher_wattswarm_node_id from list_missions claim_route".to_owned()
        })?;

    Ok(NetworkMissionClaimRoute {
        task_id,
        mission_feed_key,
        mission_scope_hint,
        publisher_wattswarm_node_id,
        status: claim_route_string(body, "status"),
    })
}

fn task_id_from_gateway_task(task: &Value) -> Option<&str> {
    task.get("task_id")
        .or_else(|| task.get("id"))
        .or_else(|| task.get("mission_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn gateway_task_string(task: &Value, field: &str) -> Option<String> {
    task.get(field)
        .or_else(|| task.get("claim_route").and_then(|value| value.get(field)))
        .or_else(|| task.get("summary").and_then(|value| value.get(field)))
        .or_else(|| task.get("inputs").and_then(|value| value.get(field)))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn network_claim_route_from_gateway_task(
    body: &MissionClaimBody,
    task: &Value,
) -> Result<NetworkMissionClaimRoute, String> {
    let task_id =
        task_id_from_gateway_task(task).map_or_else(|| body.mission_id.clone(), ToOwned::to_owned);
    let publisher_wattswarm_node_id = gateway_task_string(task, "publisher_wattswarm_node_id");
    let mission_feed_key = gateway_task_string(task, "mission_feed_key")
        .unwrap_or_else(|| MISSION_FEED_KEY.to_owned());
    let status = gateway_task_string(task, "status").or_else(|| gateway_task_string(task, "state"));
    let mission_scope_hint = gateway_task_string(task, "mission_scope_hint")
        .or_else(|| {
            publisher_wattswarm_node_id
                .as_ref()
                .map(|node_id| publisher_node_scope_hint(node_id))
        })
        .ok_or_else(|| {
            "gateway mission is missing mission_scope_hint or publisher_wattswarm_node_id"
                .to_owned()
        })?;

    Ok(NetworkMissionClaimRoute {
        task_id,
        mission_feed_key,
        mission_scope_hint,
        publisher_wattswarm_node_id,
        status,
    })
}

fn contract_value_from_gateway_task(task: &Value) -> Option<&Value> {
    task.get("task_contract").or_else(|| task.get("contract"))
}

fn parse_network_task_contract(value: &Value) -> Result<TaskContract, String> {
    serde_json::from_value::<TaskContract>(value.clone())
        .map_err(|error| format!("gateway task_contract is invalid: {error}"))
}

fn validate_network_task_contract(
    contract: &TaskContract,
    route: &NetworkMissionClaimRoute,
    body: &MissionClaimBody,
) -> Result<(), String> {
    if contract.task_id != route.task_id {
        return Err(format!(
            "gateway task_contract task_id `{}` does not match request task_id `{}`",
            contract.task_id, route.task_id
        ));
    }
    if contract.task_type != "wattetheria.mission" {
        return Err(format!(
            "gateway task_contract task_type `{}` is not wattetheria.mission",
            contract.task_type
        ));
    }
    let contract_mission_id = contract
        .inputs
        .get("mission_id")
        .and_then(Value::as_str)
        .unwrap_or(contract.task_id.as_str());
    if contract_mission_id != body.mission_id {
        return Err(format!(
            "gateway task_contract mission_id `{contract_mission_id}` does not match request mission_id `{}`",
            body.mission_id
        ));
    }
    Ok(())
}

async fn gateway_network_task_contract(
    state: &ControlPlaneState,
    task_id: &str,
) -> anyhow::Result<Option<TaskContract>> {
    let Some(task) = gateway_network_task(state, task_id).await? else {
        return Ok(None);
    };
    let Some(contract_value) = contract_value_from_gateway_task(&task) else {
        return Ok(None);
    };
    parse_network_task_contract(contract_value)
        .map(Some)
        .map_err(anyhow::Error::msg)
}

async fn gateway_network_task(
    state: &ControlPlaneState,
    task_id: &str,
) -> anyhow::Result<Option<Value>> {
    let gateway_url = resolve_gateway_query_url(state)?;
    let gateway_endpoint = normalized_gateway_tasks_url(&gateway_url);
    let tasks = fetch_gateway_tasks(&gateway_endpoint, GATEWAY_CONTRACT_FETCH_LIMIT).await?;
    let Some(task) = tasks
        .iter()
        .find(|task| task_id_from_gateway_task(task) == Some(task_id))
    else {
        return Ok(None);
    };
    Ok(Some(task.clone()))
}

async fn network_claim_route_for_action(
    state: &ControlPlaneState,
    body: &MissionClaimBody,
) -> Result<NetworkMissionClaimRoute, Response> {
    match network_claim_route(body) {
        Ok(route) => Ok(route),
        Err(route_error) => match gateway_network_task(state, &body.mission_id).await {
            Ok(Some(task)) => network_claim_route_from_gateway_task(body, &task).map_err(|error| {
                (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response()
            }),
            Ok(None) => Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": route_error,
                    "mission_id": body.mission_id.clone(),
                    "detail": "refresh list_missions or gateway snapshot before retrying",
                })),
            )
                .into_response()),
            Err(error) => Err(internal_error(&error)),
        },
    }
}

async fn network_task_contract_for_action(
    state: &ControlPlaneState,
    route: &NetworkMissionClaimRoute,
    body: &MissionClaimBody,
    action: &str,
) -> Result<TaskContract, Response> {
    let inline_contract = claim_route_value(body, "task_contract")
        .or_else(|| claim_route_value(body, "contract"))
        .map(parse_network_task_contract)
        .transpose();
    let contract = match inline_contract {
        Ok(Some(contract)) => contract,
        Ok(None) => match gateway_network_task_contract(state, &route.task_id).await {
            Ok(Some(contract)) => contract,
            Ok(None) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": format!("network mission {action} requires task_contract from gateway; refresh publisher snapshot before retrying"),
                        "action": action,
                        "task_id": route.task_id,
                    })),
                )
                    .into_response());
            }
            Err(error) => return Err(internal_error(&error)),
        },
        Err(error) => {
            return Err((StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response());
        }
    };
    if let Err(error) = validate_network_task_contract(&contract, route, body) {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response());
    }
    Ok(contract)
}

async fn import_network_task_contract(
    state: &ControlPlaneState,
    contract: TaskContract,
) -> Result<(String, Value), Response> {
    let subscriber_node_id = state
        .swarm_bridge
        .local_node_id()
        .await
        .map_err(|error| internal_error(&error))?;
    let task_contract_sync = state
        .swarm_bridge
        .import_task_contract(contract)
        .await
        .map_err(|error| internal_error(&error))?;
    Ok((subscriber_node_id, task_contract_sync))
}

fn mission_task_output_schema() -> Value {
    json!({
        "type": "object",
        "required": ["mission_id", "agent_did", "result"],
        "properties": {
            "mission_id": {"type": "string"},
            "agent_did": {"type": "string"},
            "result": {}
        }
    })
}

fn publisher_node_scope_hint(publisher_wattswarm_node_id: &str) -> String {
    format!("node:{publisher_wattswarm_node_id}")
}

fn mission_task_scope_hint(task_id: &str) -> String {
    format!("group:{task_id}")
}

fn mission_task_inputs(
    mission: &CivilMission,
    publisher_agent_did: &str,
    publisher_display_name: Option<&str>,
    publisher_wattswarm_node_id: &str,
) -> Value {
    let mission_scope_hint = mission_task_scope_hint(&mission.mission_id);
    let mut inputs = json!({
        "kind": "wattetheria_mission",
        "mission_id": mission.mission_id,
        "publisher": mission.publisher,
        "publisher_kind": mission.publisher_kind,
        "publisher_agent_did": publisher_agent_did,
        "publisher_display_name": publisher_display_name,
        "publisher_wattswarm_node_id": publisher_wattswarm_node_id,
        "swarm_scope": {
            "kind": "group",
            "id": mission.mission_id,
        },
        "mission_feed_key": MISSION_FEED_KEY,
        "mission_scope_hint": mission_scope_hint,
        "domain": mission.domain,
        "reward": mission.reward,
        "required_role": mission.required_role,
        "required_faction": mission.required_faction,
        "subnet_id": mission.subnet_id,
        "zone_id": mission.zone_id,
        "payload": mission.payload,
    });
    if let Some(delegation) = settlement_delegation_from_payload(&mission.payload)
        && let Some(object) = inputs.as_object_mut()
    {
        object.insert("settlement_delegation".to_owned(), delegation.clone());
    }
    inputs
}

pub(crate) fn mission_task_contract(
    mut contract: TaskContract,
    mission: &CivilMission,
    publisher_agent_did: &str,
    publisher_display_name: Option<&str>,
    publisher_wattswarm_node_id: &str,
) -> TaskContract {
    contract.task_id.clone_from(&mission.mission_id);
    "wattetheria.mission".clone_into(&mut contract.task_type);
    contract.inputs = mission_task_inputs(
        mission,
        publisher_agent_did,
        publisher_display_name,
        publisher_wattswarm_node_id,
    );
    contract.output_schema = mission_task_output_schema();
    contract.expiry_ms = MISSION_TASK_NO_EXPIRY_MS;
    contract
}

fn mission_announce_command(
    mission: &CivilMission,
    publisher_agent_did: &str,
    publisher_display_name: Option<&str>,
    publisher_wattswarm_node_id: &str,
    agent_envelope: Option<SwarmAgentEnvelope>,
) -> SwarmTaskAnnounceCommand {
    let mission_scope_hint = mission_task_scope_hint(&mission.mission_id);
    let mut summary = json!({
        "kind": "wattetheria_mission",
        "mission_id": mission.mission_id,
        "title": mission.title,
        "description": mission.description,
        "domain": mission.domain,
        "reward": mission.reward,
        "publisher": mission.publisher,
        "publisher_agent_did": publisher_agent_did,
        "publisher_display_name": publisher_display_name,
        "publisher_wattswarm_node_id": publisher_wattswarm_node_id,
        "mission_feed_key": MISSION_FEED_KEY,
        "mission_scope_hint": mission_scope_hint,
    });
    if let Some(delegation) = settlement_delegation_from_payload(&mission.payload)
        && let Some(object) = summary.as_object_mut()
    {
        object.insert("settlement_delegation".to_owned(), delegation.clone());
    }
    SwarmTaskAnnounceCommand {
        task_id: mission.mission_id.clone(),
        announcement_id: None,
        feed_key: MISSION_FEED_KEY.to_owned(),
        scope_hint: mission_scope_hint.clone(),
        summary,
        detail_ref: None,
        agent_envelope,
    }
}

fn mission_gateway_payload(mission: &CivilMission, task_contract: Option<&TaskContract>) -> Value {
    let mut payload = serde_json::to_value(mission).unwrap_or(Value::Null);
    let Some(object) = payload.as_object_mut() else {
        return payload;
    };
    object
        .entry("task_id".to_string())
        .or_insert_with(|| Value::String(mission.mission_id.clone()));
    object
        .entry("task_type".to_string())
        .or_insert_with(|| Value::String("wattetheria.mission".to_string()));
    if let Some(delegation) = settlement_delegation_from_payload(&mission.payload) {
        object.insert("settlement_delegation".to_owned(), delegation.clone());
    }
    let Some(contract) = task_contract else {
        return payload;
    };
    object.insert(
        "task_id".to_string(),
        Value::String(contract.task_id.clone()),
    );
    object.insert(
        "task_type".to_string(),
        Value::String(contract.task_type.clone()),
    );
    object.insert(
        "task_contract".to_string(),
        serde_json::to_value(contract).unwrap_or(Value::Null),
    );
    for key in [
        "publisher_wattswarm_node_id",
        "mission_feed_key",
        "mission_scope_hint",
        "settlement_delegation",
        "swarm_scope",
    ] {
        if let Some(value) = contract.inputs.get(key) {
            object.insert(key.to_string(), value.clone());
        }
    }
    payload
}

async fn mission_gateway_payload_with_identities(
    state: &ControlPlaneState,
    mission: &CivilMission,
    task_contract: Option<&TaskContract>,
) -> Value {
    let mut payload = mission_gateway_payload(mission, task_contract);
    let identities = state.public_identity_registry.lock().await.list();
    let Some(object) = payload.as_object_mut() else {
        return payload;
    };
    if let Some(identity) = mission_identity_for_participant(&identities, Some(&mission.publisher))
    {
        insert_mission_identity_projection(object, "created_by", identity);
    }
    if let Some(identity) =
        mission_identity_for_participant(&identities, mission.claimed_by.as_deref())
    {
        insert_mission_identity_projection(object, "claimer", identity);
    }
    if let Some(identity) =
        mission_identity_for_participant(&identities, mission.completed_by.as_deref())
    {
        insert_mission_identity_projection(object, "completer", identity);
    }
    if let Some(identity) = mission_identity_for_participant(&identities, Some(&state.agent_did)) {
        insert_mission_identity_projection(object, "source", identity);
    }
    payload
}

fn mission_identity_for_participant<'a>(
    identities: &'a [PublicIdentity],
    participant_id: Option<&str>,
) -> Option<&'a PublicIdentity> {
    let participant_id = participant_id?.trim();
    if participant_id.is_empty() {
        return None;
    }
    identities.iter().find(|identity| {
        identity.public_id == participant_id
            || identity.agent_did.as_deref() == Some(participant_id)
    })
}

fn insert_mission_identity_projection(
    object: &mut serde_json::Map<String, Value>,
    prefix: &str,
    identity: &PublicIdentity,
) {
    object
        .entry(format!("{prefix}_agent_identity"))
        .or_insert_with(|| Value::String(identity.display_name.clone()));
    object
        .entry(format!("{prefix}_display_name"))
        .or_insert_with(|| Value::String(identity.display_name.clone()));
    object
        .entry(format!("{prefix}_public_id"))
        .or_insert_with(|| Value::String(identity.public_id.clone()));
    if let Some(agent_did) = identity.agent_did.as_deref() {
        object
            .entry(format!("{prefix}_agent_did"))
            .or_insert_with(|| Value::String(agent_did.to_string()));
    }
}

fn insert_mission_actor_projection_fallback(
    object: &mut serde_json::Map<String, Value>,
    prefix: &str,
    agent_did: &str,
    body: &MissionClaimBody,
) {
    if !agent_did.trim().is_empty() {
        object
            .entry(format!("{prefix}_agent_did"))
            .or_insert_with(|| Value::String(agent_did.to_string()));
    }
    if let Some(display_name) = mission_claim_actor_display_name(body) {
        object
            .entry(format!("{prefix}_agent_identity"))
            .or_insert_with(|| Value::String(display_name.clone()));
        object
            .entry(format!("{prefix}_display_name"))
            .or_insert_with(|| Value::String(display_name));
    }
    if let Some(public_id) = mission_claim_actor_public_id(body) {
        object
            .entry(format!("{prefix}_public_id"))
            .or_insert_with(|| Value::String(public_id));
    }
}

fn insert_mission_transition_actor_projection(
    payload: &mut Value,
    action: &str,
    agent_did: &str,
    body: &MissionClaimBody,
) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    match action {
        "claim" => insert_mission_actor_projection_fallback(object, "claimer", agent_did, body),
        "complete" => {
            insert_mission_actor_projection_fallback(object, "completer", agent_did, body);
        }
        _ => {}
    }
}

fn mission_claim_actor_display_name(body: &MissionClaimBody) -> Option<String> {
    [
        &["decision_payload", "display_name"][..],
        &["decision_payload", "agent_identity"][..],
        &[
            "agent_envelope",
            "source_agent_card",
            "card",
            "metadata",
            "display_name",
        ][..],
        &["agent_envelope", "source_agent_card", "card", "name"][..],
        &[
            "agent_event_payload",
            "agent_envelope",
            "source_agent_card",
            "card",
            "metadata",
            "display_name",
        ][..],
        &[
            "agent_event_payload",
            "agent_envelope",
            "source_agent_card",
            "card",
            "name",
        ][..],
    ]
    .into_iter()
    .find_map(|path| claim_route_path_string(body, path))
}

fn mission_claim_actor_public_id(body: &MissionClaimBody) -> Option<String> {
    [
        &["decision_payload", "public_id"][..],
        &["decision_payload", "agent_public_id"][..],
        &[
            "agent_envelope",
            "source_agent_card",
            "card",
            "metadata",
            "public_id",
        ][..],
        &[
            "agent_event_payload",
            "agent_envelope",
            "source_agent_card",
            "card",
            "metadata",
            "public_id",
        ][..],
    ]
    .into_iter()
    .find_map(|path| claim_route_path_string(body, path))
}

fn claim_route_path_string(body: &MissionClaimBody, path: &[&str]) -> Option<String> {
    path.iter()
        .try_fold(body.claim_route.as_ref()?, |value, key| value.get(*key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

struct MissionRewardActor {
    controller_id: String,
    public_id: Option<String>,
    agent_identity: Option<String>,
}

impl MissionRewardActor {
    fn matches_public_id(&self, public_id: &str) -> bool {
        self.public_id.as_deref() == Some(public_id) || self.controller_id == public_id
    }
}

async fn mission_reward_actor_for_participant(
    state: &ControlPlaneState,
    participant_id: &str,
    fallback_agent_identity: Option<&str>,
) -> MissionRewardActor {
    let participant_id = participant_id.trim();
    let identities = state.public_identity_registry.lock().await.list();
    let bindings = state.controller_binding_registry.lock().await.list();
    let identity = mission_identity_for_participant(&identities, Some(participant_id));
    if let Some(identity) = identity {
        let binding = bindings
            .iter()
            .find(|binding| binding.active && binding.public_id == identity.public_id);
        return MissionRewardActor {
            controller_id: controller_id_for_identity(identity, binding),
            public_id: Some(identity.public_id.clone()),
            agent_identity: Some(identity.display_name.clone()),
        };
    }

    MissionRewardActor {
        controller_id: participant_id.to_string(),
        public_id: Some(participant_id.to_string()),
        agent_identity: fallback_agent_identity
            .map(ToOwned::to_owned)
            .or_else(|| Some(participant_id.to_string())),
    }
}

fn controller_id_for_identity(
    identity: &PublicIdentity,
    binding: Option<&ControllerBinding>,
) -> String {
    binding
        .and_then(|binding| binding.controller_node_id.clone())
        .or_else(|| {
            binding
                .map(|binding| binding.controller_ref.clone())
                .filter(|controller_ref| {
                    !controller_ref.trim().is_empty() && controller_ref != "local-default"
                })
        })
        .unwrap_or_else(|| identity.public_id.clone())
}

async fn record_system_puzzle_settlement_rewards(
    state: &ControlPlaneState,
    mission: &CivilMission,
    settlement: &SystemPuzzleSettlement,
) -> anyhow::Result<()> {
    let mission_verifier_id = mission
        .completed_by
        .as_deref()
        .context("system puzzle settled mission is missing completed_by")?;
    let proposer = mission_reward_actor_for_participant(
        state,
        &settlement.proposer_public_id,
        settlement.proposer_agent_identity.as_deref(),
    )
    .await;
    let solver = mission_reward_actor_for_participant(
        state,
        &settlement.solver_public_id,
        settlement.solver_agent_identity.as_deref(),
    )
    .await;
    let verifier = mission_reward_actor_for_participant(
        state,
        mission_verifier_id,
        settlement.verifier_agent_identity.as_deref(),
    )
    .await;
    if !verifier.matches_public_id(&settlement.verifier_public_id)
        && mission_verifier_id != settlement.verifier_public_id
    {
        bail!("system puzzle verification receipt verifier does not match mission completer");
    }

    let receipt = json!({
        "mission_id": mission.mission_id,
        "challenge_id": settlement.challenge_id,
        "solution_id": settlement.solution_id,
        "reward_policy": settlement.reward_policy,
        "verification_receipt": settlement.receipt,
        "gateway_authoritative": false,
    });
    let propose_source_id = format!(
        "system-puzzle:{}:{}:propose",
        mission.mission_id, settlement.challenge_id
    );
    let solve_source_id = format!(
        "system-puzzle:{}:{}:solve",
        mission.mission_id, settlement.solution_id
    );
    let verify_source_id = format!(
        "system-puzzle:{}:{}:verify:{}",
        mission.mission_id, settlement.solution_id, settlement.verifier_public_id
    );
    record_contribution_event(
        state,
        ContributionEventArgs {
            action_type: "system_puzzle.propose.success",
            source_id: &propose_source_id,
            controller_id: &proposer.controller_id,
            public_id: proposer.public_id.as_deref(),
            agent_identity: proposer.agent_identity.as_deref(),
            receipt: receipt.clone(),
        },
    )
    .await?;
    record_contribution_event(
        state,
        ContributionEventArgs {
            action_type: "system_puzzle.solve",
            source_id: &solve_source_id,
            controller_id: &solver.controller_id,
            public_id: solver.public_id.as_deref(),
            agent_identity: solver.agent_identity.as_deref(),
            receipt: receipt.clone(),
        },
    )
    .await?;
    record_contribution_event(
        state,
        ContributionEventArgs {
            action_type: "system_puzzle.verify.success",
            source_id: &verify_source_id,
            controller_id: &verifier.controller_id,
            public_id: verifier.public_id.as_deref(),
            agent_identity: verifier.agent_identity.as_deref(),
            receipt,
        },
    )
    .await?;
    Ok(())
}

async fn mission_gateway_payload_with_current_contract(
    state: &ControlPlaneState,
    mission: &CivilMission,
) -> Value {
    let publisher_display_name = agent_display_name_for_did(state, &state.agent_did).await;
    let task_contract = match state.swarm_bridge.local_node_id().await {
        Ok(publisher_wattswarm_node_id) => state
            .swarm_bridge
            .sample_task_contract(&mission.mission_id)
            .await
            .ok()
            .map(|contract| {
                mission_task_contract(
                    contract,
                    mission,
                    &state.agent_did,
                    publisher_display_name.as_deref(),
                    &publisher_wattswarm_node_id,
                )
            }),
        Err(_) => None,
    };
    mission_gateway_payload_with_identities(state, mission, task_contract.as_ref()).await
}

fn mission_complete_command(
    mission: &CivilMission,
    agent_did: &str,
    agent_envelope: SwarmAgentEnvelope,
) -> SwarmTaskProposeCandidateCommand {
    let execution_id = mission_execution_id(&mission.mission_id, agent_did);
    let result = mission
        .completion_result
        .clone()
        .unwrap_or_else(|| mission.payload.clone());
    SwarmTaskProposeCandidateCommand {
        task_id: mission.mission_id.clone(),
        execution_id: execution_id.clone(),
        candidate_id: mission_candidate_id(&mission.mission_id, agent_did),
        output: json!({
            "kind": "wattetheria_mission_result",
            "mission_id": mission.mission_id,
            "agent_did": agent_did,
            "result": result,
        }),
        evidence_inline: Vec::new(),
        evidence_refs: Vec::new(),
        agent_envelope,
    }
}

fn mission_network_complete_command(
    route: &NetworkMissionClaimRoute,
    body: &MissionClaimBody,
    result: &Value,
    agent_envelope: SwarmAgentEnvelope,
) -> SwarmTaskProposeCandidateCommand {
    let execution_id = mission_execution_id(&route.task_id, &body.agent_did);
    SwarmTaskProposeCandidateCommand {
        task_id: route.task_id.clone(),
        execution_id: execution_id.clone(),
        candidate_id: mission_candidate_id(&route.task_id, &body.agent_did),
        output: json!({
            "kind": "wattetheria_mission_result",
            "mission_id": body.mission_id,
            "agent_did": body.agent_did,
            "result": result,
        }),
        evidence_inline: Vec::new(),
        evidence_refs: Vec::new(),
        agent_envelope,
    }
}

async fn publish_mission_task_to_swarm(
    state: &ControlPlaneState,
    mission: &CivilMission,
) -> Result<TaskContract, Response> {
    let publisher_display_name = agent_display_name_for_did(state, &state.agent_did).await;
    let publisher_wattswarm_node_id = state
        .swarm_bridge
        .local_node_id()
        .await
        .map_err(|error| internal_error(&error))?;
    let contract = state
        .swarm_bridge
        .sample_task_contract(&mission.mission_id)
        .await
        .map(|contract| {
            mission_task_contract(
                contract,
                mission,
                &state.agent_did,
                publisher_display_name.as_deref(),
                &publisher_wattswarm_node_id,
            )
        })
        .map_err(|error| internal_error(&error))?;
    state
        .swarm_bridge
        .submit_task(contract.clone())
        .await
        .map_err(|error| internal_error(&error))?;
    let agent_envelope = task_agent_envelope(
        state,
        TaskAgentEnvelopeArgs {
            source_agent_id: state.agent_did.clone(),
            source_display_name: publisher_display_name.clone(),
            target_agent_id: None,
            source_node_id: Some(publisher_wattswarm_node_id.clone()),
            target_node_id: None,
            capability: "task.announce".to_owned(),
            message: json!({
            "task_id": mission.mission_id,
            "mission_id": mission.mission_id,
            "publisher_agent_did": state.agent_did,
            "publisher_wattswarm_node_id": publisher_wattswarm_node_id,
            }),
        },
    )
    .map(Some)
    .map_err(|error| internal_error(&error))?;
    state
        .swarm_bridge
        .announce_task(mission_announce_command(
            mission,
            &state.agent_did,
            publisher_display_name.as_deref(),
            &publisher_wattswarm_node_id,
            agent_envelope,
        ))
        .await
        .map_err(|error| internal_error(&error))?;
    Ok(contract)
}

fn network_claim_agent_envelope(
    state: &ControlPlaneState,
    route: &NetworkMissionClaimRoute,
    body: &MissionClaimBody,
    execution_id: &str,
    source_display_name: Option<String>,
    source_node_id: Option<String>,
) -> anyhow::Result<SwarmAgentEnvelope> {
    task_agent_envelope(
        state,
        TaskAgentEnvelopeArgs {
            source_agent_id: body.agent_did.clone(),
            source_display_name,
            target_agent_id: publisher_agent_did_from_claim(body),
            source_node_id,
            target_node_id: route.publisher_wattswarm_node_id.clone(),
            capability: "task.claim".to_owned(),
            message: json!({
            "task_id": route.task_id,
            "mission_id": body.mission_id,
            "agent_did": body.agent_did,
            "role": "propose",
            "execution_id": execution_id,
            }),
        },
    )
}

fn mission_settle_candidate(
    body: &MissionSettleBody,
    mission: &CivilMission,
) -> Option<(String, String)> {
    let task_id = body
        .task_id
        .clone()
        .unwrap_or_else(|| mission.mission_id.clone());
    let candidate_id = body.candidate_id.clone().or_else(|| {
        body.agent_did
            .as_ref()
            .or(mission.completed_by.as_ref())
            .map(|agent_did| mission_candidate_id(&task_id, agent_did))
    })?;
    Some((task_id, candidate_id))
}

async fn finalize_mission_task_before_settle(
    state: &ControlPlaneState,
    body: &MissionSettleBody,
    headers: &HeaderMap,
) -> Result<Option<Value>, Response> {
    if agent_commit_context_from_headers(headers).is_some() {
        return Ok(None);
    }
    let mission = {
        let board = state.mission_board.lock().await;
        match board.get(&body.mission_id) {
            Some(mission) => mission.clone(),
            None => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "mission not found"})),
                )
                    .into_response());
            }
        }
    };
    if mission.status != MissionStatus::Completed {
        return Ok(None);
    }
    let Some((task_id, candidate_id)) = mission_settle_candidate(body, &mission) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "settle_mission requires candidate_id or completed mission agent_did"})),
        )
            .into_response());
    };
    let target_agent_did = body
        .agent_did
        .as_deref()
        .or(mission.completed_by.as_deref())
        .map(ToOwned::to_owned);
    let local_node_id = state.swarm_bridge.local_node_id().await.ok();
    let source_display_name = agent_display_name_for_did(state, &state.agent_did).await;
    let agent_envelope = match task_agent_envelope(
        state,
        TaskAgentEnvelopeArgs {
            source_agent_id: state.agent_did.clone(),
            source_display_name,
            target_agent_id: target_agent_did.clone(),
            source_node_id: local_node_id,
            target_node_id: None,
            capability: "task.result.finalize".to_owned(),
            message: json!({
            "task_id": task_id,
            "mission_id": body.mission_id,
            "candidate_id": candidate_id,
            "publisher_agent_did": state.agent_did,
            "target_agent_did": target_agent_did,
            }),
        },
    ) {
        Ok(envelope) => Some(envelope),
        Err(error) => return Err(internal_error(&error)),
    };
    state
        .swarm_bridge
        .accept_and_finalize_task(&task_id, &candidate_id, agent_envelope)
        .await
        .map(Some)
        .map_err(|error| internal_error(&error))
}

async fn system_puzzle_settlement_for_mission(
    state: &ControlPlaneState,
    mission_id: &str,
) -> Result<Option<SystemPuzzleSettlement>, Response> {
    let board = state.mission_board.lock().await;
    match board.get(mission_id) {
        Some(mission) => system_puzzle_settlement_from_mission(&mission).map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response()
        }),
        None => Ok(None),
    }
}

async fn settle_mission_board(
    state: &ControlPlaneState,
    mission_id: &str,
) -> Result<CivilMission, Response> {
    let mut board = state.mission_board.lock().await;
    let mission = board.settle(mission_id).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": error.to_string()})),
        )
            .into_response()
    })?;
    state
        .local_db
        .save_domain(wattetheria_kernel::local_db::domain::MISSION_BOARD, &*board)
        .map_err(|error| internal_error(&error))?;
    Ok(mission)
}

async fn fund_mission_treasury(
    state: &ControlPlaneState,
    mission: &CivilMission,
) -> Result<(), Response> {
    if let Some(subnet_id) = mission.subnet_id.clone()
        && mission.reward.treasury_share_watt > 0
    {
        let mut governance = state.governance_engine.lock().await;
        governance
            .fund_treasury(&subnet_id, mission.reward.treasury_share_watt)
            .map_err(|error| internal_error(&error))?;
        state
            .local_db
            .save_domain(
                wattetheria_kernel::local_db::domain::GOVERNANCE,
                &*governance,
            )
            .map_err(|error| internal_error(&error))?;
    }
    Ok(())
}

fn replay_commit_response(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    action_type: &str,
) -> anyhow::Result<Option<Response>> {
    let Some(context) = agent_commit_context_from_headers(headers) else {
        return Ok(None);
    };
    let Some(entry) = state.local_db.load_agent_action_commit(
        &context.event_id,
        &context.decision_id,
        action_type,
    )?
    else {
        return Ok(None);
    };
    let payload: Value = serde_json::from_str(&entry.result_json)?;
    Ok(Some(Json(payload).into_response()))
}

fn append_commit_response(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    args: CommitResponseArgs<'_>,
) -> anyhow::Result<()> {
    let Some(context) = agent_commit_context_from_headers(headers) else {
        return Ok(());
    };
    state.local_db.append_agent_action_commit(
        &wattetheria_kernel::local_db::AgentActionCommitLogEntry {
            commit_id: Uuid::new_v4().to_string(),
            event_id: context.event_id,
            decision_id: context.decision_id,
            action_type: args.action_type.to_owned(),
            domain: "mission".to_owned(),
            target_id: args.target_id,
            expected_state: None,
            result_state: None,
            request_json: serde_json::to_string(args.request_json)?,
            result_json: serde_json::to_string(args.response_json)?,
            status: "accepted".to_owned(),
            actor_public_id: None,
            actor_agent_did: args.actor_agent_did,
            created_at: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        },
    )
}

pub(crate) async fn mission_list(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<MissionsQuery>,
) -> Response {
    if let Ok(Some(response)) = replay_commit_response(&state, &headers, "missions.publish") {
        return response;
    }
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let missions = state.mission_board.lock().await.list(query.status.as_ref());
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: "mission.list".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": missions.len()})),
    });
    Json(missions).into_response()
}

pub(crate) async fn mission_get(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(mission_id): Path<String>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let mission = state.mission_board.lock().await.get(&mission_id);
    let Some(mission) = mission else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "mission not found", "mission_id": mission_id})),
        )
            .into_response();
    };
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: "mission.get".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(mission.mission_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: None,
    });
    Json(mission).into_response()
}

pub(crate) async fn mission_publish(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MissionPublishBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let settlement_delegation = match normalize_publish_delegation(body.settlement_delegation) {
        Ok(delegation) => delegation,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    let mission_payload =
        payload_with_settlement_delegation(body.payload, settlement_delegation.as_ref());
    let mut board = state.mission_board.lock().await;
    let mission = board.publish(
        &body.title,
        &body.description,
        &body.publisher,
        body.publisher_kind,
        body.domain,
        body.subnet_id,
        body.zone_id,
        body.required_role,
        body.required_faction,
        body.reward,
        mission_payload,
    );
    if let Err(error) = state
        .local_db
        .save_domain(wattetheria_kernel::local_db::domain::MISSION_BOARD, &*board)
    {
        return internal_error(&error);
    }
    drop(board);
    if let Err(error) = refresh_known_wallet_balances(&state).await {
        return internal_error(&error);
    }
    let mut published_task_contract = None;
    if agent_commit_context_from_headers(&headers).is_none() {
        let contract = match publish_mission_task_to_swarm(&state, &mission).await {
            Ok(contract) => contract,
            Err(response) => return response,
        };
        published_task_contract = Some(contract);
    }

    let payload =
        mission_gateway_payload_with_identities(&state, &mission, published_task_contract.as_ref())
            .await;
    let _ = state.stream_tx.send(StreamEvent {
        kind: "mission.published".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event("MISSION_PUBLISHED", payload.clone());

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: "mission.publish".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(mission.mission_id.clone()),
        capability: Some("mission.publish".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: "missions.publish",
            target_id: Some(mission.mission_id.clone()),
            actor_agent_did: None,
            request_json: &json!({"title": body.title, "publisher": body.publisher}),
            response_json: &payload,
        },
    ) {
        return internal_error(&error);
    }
    (StatusCode::CREATED, Json(payload)).into_response()
}

pub(crate) async fn mission_claim(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MissionClaimBody>,
) -> Response {
    transition_mission(state, headers, body, "claim").await
}

pub(crate) async fn mission_claim_by_id(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(mission_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let body = match mission_claim_body_with_path_id(body, mission_id) {
        Ok(body) => body,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
        }
    };
    mission_claim(State(state), headers, Json(body)).await
}

pub(crate) async fn mission_complete(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MissionClaimBody>,
) -> Response {
    transition_mission(state, headers, body, "complete").await
}

pub(crate) async fn mission_complete_by_id(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(mission_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let body = match mission_claim_body_with_path_id(body, mission_id) {
        Ok(body) => body,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
        }
    };
    mission_complete(State(state), headers, Json(body)).await
}

fn mission_claim_body_with_path_id(
    mut value: Value,
    mission_id: String,
) -> Result<MissionClaimBody, String> {
    let Some(object) = value.as_object_mut() else {
        return Err("mission request body must be a JSON object".to_string());
    };
    object.insert("mission_id".to_string(), Value::String(mission_id));
    serde_json::from_value(value).map_err(|error| format!("invalid mission request body: {error}"))
}

async fn claim_network_mission(
    state: ControlPlaneState,
    headers: HeaderMap,
    auth: String,
    body: MissionClaimBody,
) -> Response {
    if agent_commit_context_from_headers(&headers).is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "network mission claim requires a direct request"})),
        )
            .into_response();
    }
    let route = match network_claim_route_for_action(&state, &body).await {
        Ok(route) => route,
        Err(response) => return response,
    };
    let execution_id = mission_execution_id(&route.task_id, &body.agent_did);
    if let Some(status) = route.status.as_deref()
        && !network_claim_status_allows_claim(status)
    {
        return mission_not_claimable_response(
            &body.mission_id,
            &route.task_id,
            &body.agent_did,
            &execution_id,
            status,
        );
    }
    match network_claim_already_recorded(&state, &body.mission_id, &route.task_id, &body.agent_did)
    {
        Ok(true) => {
            return mission_already_claimed_response(
                &body.mission_id,
                &route.task_id,
                &body.agent_did,
                &execution_id,
                "This Wattetheria node already submitted a network claim for this mission and agent.",
            );
        }
        Ok(false) => {}
        Err(error) => return internal_error(&error),
    }
    let contract = match network_task_contract_for_action(&state, &route, &body, "claim").await {
        Ok(contract) => contract,
        Err(response) => return response,
    };
    let (subscriber_node_id, task_contract_sync) =
        match import_network_task_contract(&state, contract).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let source_display_name = agent_display_name_for_did(&state, &body.agent_did).await;
    let agent_envelope = match network_claim_agent_envelope(
        &state,
        &route,
        &body,
        &execution_id,
        source_display_name,
        Some(subscriber_node_id.clone()),
    ) {
        Ok(envelope) => envelope,
        Err(error) => return internal_error(&error),
    };
    let swarm_claim = match state
        .swarm_bridge
        .claim_task(SwarmTaskClaimCommand {
            task_id: route.task_id.clone(),
            role: ClaimRole::Propose,
            execution_id: execution_id.clone(),
            lease_ms: None,
            agent_envelope,
        })
        .await
    {
        Ok(value) => value,
        Err(error) => return network_claim_error_response(&body, &route, &execution_id, &error),
    };
    let response_json = json!({
        "ok": true,
        "status": "network_claim_submitted",
        "mission_id": body.mission_id,
        "task_id": route.task_id,
        "agent_did": body.agent_did,
        "mission_feed_key": route.mission_feed_key,
        "mission_scope_hint": route.mission_scope_hint,
        "publisher_wattswarm_node_id": route.publisher_wattswarm_node_id,
        "subscriber_node_id": subscriber_node_id,
        "task_contract_sync": task_contract_sync,
        "swarm_claim": swarm_claim,
    });

    if let Err(error) = record_network_claim_success(&state, auth, &execution_id, &response_json) {
        return internal_error(&error);
    }
    Json(response_json).into_response()
}

fn record_network_claim_success(
    state: &ControlPlaneState,
    auth: String,
    execution_id: &str,
    response_json: &Value,
) -> anyhow::Result<()> {
    record_network_claim_submission(
        state,
        response_json["mission_id"].as_str().unwrap_or_default(),
        response_json["task_id"].as_str().unwrap_or_default(),
        response_json["agent_did"].as_str().unwrap_or_default(),
        execution_id,
    )?;

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: "mission.claim.network".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: response_json["mission_id"].as_str().map(ToOwned::to_owned),
        capability: Some("mission.claim".to_string()),
        reason: response_json["agent_did"].as_str().map(ToOwned::to_owned),
        duration_ms: None,
        details: Some(response_json.clone()),
    });
    Ok(())
}

async fn complete_network_mission(
    state: ControlPlaneState,
    headers: HeaderMap,
    auth: String,
    body: MissionClaimBody,
) -> Response {
    if agent_commit_context_from_headers(&headers).is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "network mission complete requires a direct request"})),
        )
            .into_response();
    }
    let route = match network_claim_route_for_action(&state, &body).await {
        Ok(route) => route,
        Err(response) => return response,
    };
    let Some(result) = body
        .result
        .clone()
        .or_else(|| claim_route_value(&body, "result").cloned())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "network mission complete requires result",
                "mission_id": body.mission_id,
                "task_id": route.task_id,
            })),
        )
            .into_response();
    };
    let contract = match network_task_contract_for_action(&state, &route, &body, "complete").await {
        Ok(contract) => contract,
        Err(response) => return response,
    };
    let (subscriber_node_id, task_contract_sync) =
        match import_network_task_contract(&state, contract).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let source_display_name = agent_display_name_for_did(&state, &body.agent_did).await;
    let agent_envelope = match task_agent_envelope(
        &state,
        TaskAgentEnvelopeArgs {
            source_agent_id: body.agent_did.clone(),
            source_display_name,
            target_agent_id: publisher_agent_did_from_claim(&body),
            source_node_id: Some(subscriber_node_id.clone()),
            target_node_id: route.publisher_wattswarm_node_id.clone(),
            capability: "task.result.propose".to_owned(),
            message: json!({
            "task_id": route.task_id,
            "mission_id": body.mission_id,
            "agent_did": body.agent_did,
            "result": result,
            }),
        },
    ) {
        Ok(envelope) => envelope,
        Err(error) => return internal_error(&error),
    };
    let command = mission_network_complete_command(&route, &body, &result, agent_envelope);
    let candidate_id = command.candidate_id.clone();
    let swarm_candidate = match state.swarm_bridge.propose_task_candidate(command).await {
        Ok(value) => value,
        Err(error) => return internal_error(&error),
    };
    let response_json = json!({
        "ok": true,
        "status": "network_complete_submitted",
        "mission_id": body.mission_id,
        "task_id": route.task_id,
        "agent_did": body.agent_did,
        "candidate_id": candidate_id,
        "mission_feed_key": route.mission_feed_key,
        "mission_scope_hint": route.mission_scope_hint,
        "publisher_wattswarm_node_id": route.publisher_wattswarm_node_id,
        "subscriber_node_id": subscriber_node_id,
        "task_contract_sync": task_contract_sync,
        "swarm_candidate": swarm_candidate,
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: "mission.complete.network".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: response_json["mission_id"].as_str().map(ToOwned::to_owned),
        capability: Some("mission.complete".to_string()),
        reason: response_json["agent_did"].as_str().map(ToOwned::to_owned),
        duration_ms: None,
        details: Some(response_json.clone()),
    });
    Json(response_json).into_response()
}

async fn dispatch_local_mission_transition_to_swarm(
    state: &ControlPlaneState,
    action: &str,
    mission: &CivilMission,
    agent_did: &str,
) -> anyhow::Result<Value> {
    let local_node_id = state.swarm_bridge.local_node_id().await.ok();
    let source_display_name = agent_display_name_for_did(state, agent_did).await;
    match action {
        "claim" => {
            let agent_envelope = task_agent_envelope(
                state,
                TaskAgentEnvelopeArgs {
                    source_agent_id: agent_did.to_owned(),
                    source_display_name: source_display_name.clone(),
                    target_agent_id: Some(state.agent_did.clone()),
                    source_node_id: local_node_id.clone(),
                    target_node_id: None,
                    capability: "task.claim".to_owned(),
                    message: json!({
                    "task_id": mission.mission_id,
                    "mission_id": mission.mission_id,
                    "agent_did": agent_did,
                    "role": "propose",
                    }),
                },
            )?;
            state
                .swarm_bridge
                .claim_task(SwarmTaskClaimCommand {
                    task_id: mission.mission_id.clone(),
                    role: ClaimRole::Propose,
                    execution_id: mission_execution_id(&mission.mission_id, agent_did),
                    lease_ms: None,
                    agent_envelope,
                })
                .await
        }
        "complete" => {
            let result = mission
                .completion_result
                .clone()
                .unwrap_or_else(|| mission.payload.clone());
            let agent_envelope = task_agent_envelope(
                state,
                TaskAgentEnvelopeArgs {
                    source_agent_id: agent_did.to_owned(),
                    source_display_name,
                    target_agent_id: Some(state.agent_did.clone()),
                    source_node_id: local_node_id,
                    target_node_id: None,
                    capability: "task.result.propose".to_owned(),
                    message: json!({
                    "task_id": mission.mission_id,
                    "mission_id": mission.mission_id,
                    "agent_did": agent_did,
                    "result": result,
                    }),
                },
            )?;
            state
                .swarm_bridge
                .propose_task_candidate(mission_complete_command(
                    mission,
                    agent_did,
                    agent_envelope,
                ))
                .await
        }
        _ => unreachable!("unsupported mission transition"),
    }
}

async fn transition_mission(
    state: ControlPlaneState,
    headers: HeaderMap,
    body: MissionClaimBody,
    action: &str,
) -> Response {
    if let Ok(Some(response)) =
        replay_commit_response(&state, &headers, &format!("missions.{action}"))
    {
        return response;
    }
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let request_mission_id = body.mission_id.clone();
    let request_agent_did = body.agent_did.clone();
    let mut board = state.mission_board.lock().await;
    if board.get(&body.mission_id).is_none() {
        if action == "claim" {
            drop(board);
            return claim_network_mission(state, headers, auth, body).await;
        }
        if action == "complete" {
            drop(board);
            return complete_network_mission(state, headers, auth, body).await;
        }
    }
    let mission = match action {
        "claim" => board.claim(&body.mission_id, &body.agent_did),
        "complete" => board.complete(&body.mission_id, &body.agent_did, body.result.clone()),
        _ => unreachable!("unsupported mission transition"),
    };
    let mission = match mission {
        Ok(mission) => mission,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    if let Err(error) = state
        .local_db
        .save_domain(wattetheria_kernel::local_db::domain::MISSION_BOARD, &*board)
    {
        return internal_error(&error);
    }
    drop(board);
    if let Err(error) = refresh_known_wallet_balances(&state).await {
        return internal_error(&error);
    }
    if agent_commit_context_from_headers(&headers).is_none()
        && let Err(error) =
            dispatch_local_mission_transition_to_swarm(&state, action, &mission, &request_agent_did)
                .await
    {
        return internal_error(&error);
    }

    let mut payload = mission_gateway_payload_with_current_contract(&state, &mission).await;
    insert_mission_transition_actor_projection(&mut payload, action, &request_agent_did, &body);
    let _ = state.stream_tx.send(StreamEvent {
        kind: mission_stream_kind(action).to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event(mission_signed_event_kind(action), payload.clone());

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: format!("mission.{action}"),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.mission_id),
        capability: Some(format!("mission.{action}")),
        reason: Some(body.agent_did),
        duration_ms: None,
        details: Some(payload.clone()),
    });

    let response_json = payload.clone();
    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: &format!("missions.{action}"),
            target_id: Some(mission.mission_id.clone()),
            actor_agent_did: Some(request_agent_did.clone()),
            request_json: &json!({"mission_id": request_mission_id, "agent_did": request_agent_did}),
            response_json: &response_json,
        },
    ) {
        return internal_error(&error);
    }
    Json(response_json).into_response()
}

pub(crate) async fn mission_settle(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MissionSettleBody>,
) -> Response {
    if let Ok(Some(response)) = replay_commit_response(&state, &headers, "missions.settle") {
        return response;
    }
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let request_mission_id = body.mission_id.clone();
    let swarm_finalize = match finalize_mission_task_before_settle(&state, &body, &headers).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let system_puzzle_settlement =
        match system_puzzle_settlement_for_mission(&state, &body.mission_id).await {
            Ok(settlement) => settlement,
            Err(response) => return response,
        };

    let mission = match settle_mission_board(&state, &body.mission_id).await {
        Ok(mission) => mission,
        Err(response) => return response,
    };
    if let Err(response) = fund_mission_treasury(&state, &mission).await {
        return response;
    }
    if let Some(settlement) = system_puzzle_settlement.as_ref()
        && let Err(error) =
            record_system_puzzle_settlement_rewards(&state, &mission, settlement).await
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": error.to_string()})),
        )
            .into_response();
    }
    if let Err(error) = refresh_known_wallet_balances(&state).await {
        return internal_error(&error);
    }

    let mut payload = mission_gateway_payload_with_current_contract(&state, &mission).await;
    if let (Some(agent_did), Some(object)) = (body.agent_did.as_deref(), payload.as_object_mut()) {
        let mut actor_body = MissionClaimBody::local(body.mission_id.clone(), agent_did.to_owned());
        actor_body.claim_route.clone_from(&body.claim_route);
        insert_mission_actor_projection_fallback(object, "completer", agent_did, &actor_body);
    }
    let _ = state.stream_tx.send(StreamEvent {
        kind: "mission.settled".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event("MISSION_SETTLED", payload.clone());

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: "mission.settle".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.mission_id),
        capability: Some("mission.settle".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    let mut response_json = payload.clone();
    if let Some(swarm_finalize) = swarm_finalize
        && let Some(object) = response_json.as_object_mut()
    {
        object.insert("swarm_finalize".to_string(), swarm_finalize);
    }
    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: "missions.settle",
            target_id: Some(mission.mission_id.clone()),
            actor_agent_did: body.agent_did.clone(),
            request_json: &json!({"mission_id": request_mission_id, "agent_did": body.agent_did}),
            response_json: &response_json,
        },
    ) {
        return internal_error(&error);
    }
    Json(response_json).into_response()
}

pub(crate) async fn mission_settle_by_id(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(mission_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let body = match mission_settle_body_with_path_id(body, mission_id) {
        Ok(body) => body,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
        }
    };
    mission_settle(State(state), headers, Json(body)).await
}

fn mission_settle_body_with_path_id(
    mut value: Value,
    mission_id: String,
) -> Result<MissionSettleBody, String> {
    let Some(object) = value.as_object_mut() else {
        return Err("mission request body must be a JSON object".to_string());
    };
    object.insert("mission_id".to_string(), Value::String(mission_id));
    serde_json::from_value(value).map_err(|error| format!("invalid mission request body: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wattetheria_kernel::civilization::missions::{
        MissionBoard, MissionDomain, MissionPublisherKind, MissionReward,
    };

    fn sample_mission() -> CivilMission {
        let mut board = MissionBoard::default();
        board.publish(
            "Route cargo",
            "Move supplies to the outpost",
            "captain-alpha",
            MissionPublisherKind::Player,
            MissionDomain::Trade,
            Some("subnet-1".to_owned()),
            Some("zone-2".to_owned()),
            None,
            None,
            MissionReward {
                agent_watt: 10,
                reputation: 2,
                capacity: 1,
                treasury_share_watt: 3,
            },
            json!({"cargo": "water"}),
        )
    }

    #[test]
    fn mission_task_inputs_are_group_scoped_to_mission_task() {
        let mission = sample_mission();
        let inputs = mission_task_inputs(
            &mission,
            "did:agent:publisher",
            Some("Publisher Name"),
            "node-publisher",
        );

        assert_eq!(inputs["kind"].as_str(), Some("wattetheria_mission"));
        assert_eq!(
            inputs["mission_id"].as_str(),
            Some(mission.mission_id.as_str())
        );
        assert_eq!(inputs["publisher"].as_str(), Some("captain-alpha"));
        assert_eq!(
            inputs["publisher_agent_did"].as_str(),
            Some("did:agent:publisher")
        );
        assert_eq!(
            inputs["publisher_display_name"].as_str(),
            Some("Publisher Name")
        );
        assert_eq!(
            inputs["publisher_wattswarm_node_id"].as_str(),
            Some("node-publisher")
        );
        assert_eq!(
            inputs["swarm_scope"],
            json!({"kind": "group", "id": mission.mission_id})
        );
        assert_eq!(inputs["mission_feed_key"].as_str(), Some(MISSION_FEED_KEY));
        assert_eq!(
            inputs["mission_scope_hint"].as_str(),
            Some(format!("group:{}", mission.mission_id).as_str())
        );
    }

    #[test]
    fn mission_announce_uses_same_group_scope_as_contract_inputs() {
        let mission = sample_mission();
        let command = mission_announce_command(
            &mission,
            "did:agent:publisher",
            Some("Publisher Name"),
            "node-publisher",
            None,
        );

        assert_eq!(command.feed_key, MISSION_FEED_KEY);
        assert_eq!(command.scope_hint, format!("group:{}", mission.mission_id));
        assert_eq!(
            command.summary["publisher_wattswarm_node_id"].as_str(),
            Some("node-publisher")
        );
        assert_eq!(
            command.summary["publisher_display_name"].as_str(),
            Some("Publisher Name")
        );
        assert_eq!(
            command.summary["mission_scope_hint"].as_str(),
            Some(command.scope_hint.as_str())
        );
    }
}
