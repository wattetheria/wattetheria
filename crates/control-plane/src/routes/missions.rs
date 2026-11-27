//! Mission routes. The gateway keeps every mission and its status; this node turns each of its
//! agent's actions into a signed gateway command and carries the wattswarm task messages
//! between the two agents of a mission.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::{authorize, internal_error};
use crate::ledger_client::MissionAction;
use crate::routes::gateway_missions::{
    self, KIND_DELEGATED, KIND_STANDARD, PublishTerms, civil_mission, is_own,
};
use crate::routes::message_validation::{validate_mission_inputs, validation_error_response};
use crate::routes::mission_trade;
use crate::routes::settlement_delegation::{
    normalize_publish_delegation, payload_with_settlement_delegation,
    settlement_delegation_from_payload,
};
use crate::social_host::{
    SignedAgentEnvelopeArgs, build_signed_agent_envelope_for_nodes, public_agent_id,
};
use crate::state::{
    ControlPlaneState, MissionClaimBody, MissionPublishBody, MissionSettleBody, MissionsQuery,
    StreamEvent, agent_commit_context_from_headers,
};
use anyhow::bail;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::missions::{
    CivilMission, MissionStatus, NetworkMissionClaimMetadata, NetworkMissionClaimRecord,
    NetworkMissionClaimRegistry,
};
use wattetheria_kernel::identities::PublicIdentity;
use wattetheria_kernel::local_db;
use wattetheria_kernel::swarm_bridge::{
    SwarmAgentEnvelope, SwarmTaskAnnounceCommand, SwarmTaskClaimCommand,
    SwarmTaskClaimDecisionCommand, SwarmTaskCompleteCommand, SwarmTaskSettleCommand,
};
use wattswarm_protocol::types::{ClaimRole, TaskContract};

const MISSION_FEED_KEY: &str = "wattetheria.missions";
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

fn recorded_network_claim(
    state: &ControlPlaneState,
    mission_id: &str,
    task_id: &str,
    agent_did: &str,
) -> anyhow::Result<Option<NetworkMissionClaimRecord>> {
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(local_db::domain::NETWORK_MISSION_CLAIMS)?;
    Ok(registry.get(mission_id, task_id, agent_did))
}

fn network_claim_already_recorded_response(
    state: &ControlPlaneState,
    body: &MissionClaimBody,
    route: &NetworkMissionClaimRoute,
    execution_id: &str,
) -> Option<Response> {
    match network_claim_already_recorded(state, &body.mission_id, &route.task_id, &body.agent_did) {
        Ok(true) => Some(mission_already_claimed_response(
            &body.mission_id,
            &route.task_id,
            &body.agent_did,
            execution_id,
            "This Wattetheria node already submitted a network claim for this mission and agent.",
        )),
        Ok(false) => None,
        Err(error) => Some(internal_error(&error)),
    }
}

fn record_network_claim_submission(
    state: &ControlPlaneState,
    mission_id: &str,
    task_id: &str,
    agent_did: &str,
    execution_id: &str,
    metadata: NetworkMissionClaimMetadata,
) -> anyhow::Result<()> {
    let mut registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(local_db::domain::NETWORK_MISSION_CLAIMS)?;
    registry.record(mission_id, task_id, agent_did, execution_id, metadata);
    state
        .local_db
        .save_domain(local_db::domain::NETWORK_MISSION_CLAIMS, &registry)
}

fn network_claim_metadata(
    body: &MissionClaimBody,
    route: &NetworkMissionClaimRoute,
    contract: &TaskContract,
) -> NetworkMissionClaimMetadata {
    let reward = network_claim_value(body, contract, "reward").cloned();
    let reward_watt = reward
        .as_ref()
        .and_then(reward_agent_watt)
        .or_else(|| network_claim_i64(body, contract, "reward_watt"));
    NetworkMissionClaimMetadata {
        title: network_claim_string(body, contract, "title"),
        publisher_id: network_claim_string(body, contract, "publisher"),
        publisher_agent_did: network_claim_string(body, contract, "publisher_agent_did"),
        publisher_display_name: network_claim_string(body, contract, "publisher_display_name"),
        publisher_wattswarm_node_id: route
            .publisher_wattswarm_node_id
            .clone()
            .or_else(|| network_claim_string(body, contract, "publisher_wattswarm_node_id")),
        domain: network_claim_string(body, contract, "domain"),
        scope: network_claim_string(body, contract, "scope"),
        mission_feed_key: Some(route.mission_feed_key.clone()),
        mission_scope_hint: Some(route.mission_scope_hint.clone()),
        reward,
        reward_watt,
        executor_bounty_watt: network_claim_i64(body, contract, "executor_bounty_watt")
            .or(reward_watt),
        publisher_network_reward_watt: network_claim_i64(
            body,
            contract,
            "publisher_network_reward_watt",
        )
        .or_else(|| network_claim_i64(body, contract, "network_publish_reward_watt")),
    }
}

fn network_claim_string(
    body: &MissionClaimBody,
    contract: &TaskContract,
    field: &str,
) -> Option<String> {
    claim_route_string(body, field)
        .or_else(|| {
            claim_route_value(body, "task_inputs").and_then(|inputs| value_string(inputs, field))
        })
        .or_else(|| value_string(&contract.inputs, field))
}

fn network_claim_value<'a>(
    body: &'a MissionClaimBody,
    contract: &'a TaskContract,
    field: &str,
) -> Option<&'a Value> {
    claim_route_value(body, field)
        .or_else(|| claim_route_value(body, "task_inputs").and_then(|inputs| inputs.get(field)))
        .or_else(|| contract.inputs.get(field))
}

fn network_claim_i64(body: &MissionClaimBody, contract: &TaskContract, field: &str) -> Option<i64> {
    network_claim_value(body, contract, field).and_then(value_i64)
}

fn value_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn value_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|number| i64::try_from(number).ok()))
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn reward_agent_watt(reward: &Value) -> Option<i64> {
    reward
        .get("agent_watt")
        .or_else(|| reward.get("executor_bounty_watt"))
        .or_else(|| reward.get("reward_watt"))
        .and_then(value_i64)
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
    claim_notice_failed(&body.mission_id, error)
}

/// The answer when the gateway has recorded the request to take a mission but the claim did not
/// reach the publisher. It is an error status, so the caller knows to try again, and it says the
/// request stands, so nobody takes it for a claim that was never made. Claiming again only sends
/// the notice.
fn claim_notice_failed(mission_id: &str, error: &anyhow::Error) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": format!(
                "the request to take the mission is recorded, but the notice to the publisher \
                 was not delivered: {error}. Claim again to retry the notice; the request is \
                 not recorded twice."
            ),
            "code": "claim_notice_not_delivered",
            "mission_id": mission_id,
            "mission_lifecycle_notice": {"kind": "mission_claim", "sent": false},
        })),
    )
        .into_response()
}

fn non_empty_string(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

struct TaskAgentEnvelopeArgs {
    source_agent_id: String,
    source_public_id: Option<String>,
    source_display_name: Option<String>,
    target_agent_id: Option<String>,
    source_node_id: Option<String>,
    target_node_id: Option<String>,
    capability: String,
    message: Value,
}

struct MissionLifecycleEnvelopeArgs {
    kind: &'static str,
    task_id: String,
    mission_id: String,
    mission_feed_key: String,
    mission_scope_hint: String,
    source_agent_id: String,
    source_public_id: Option<String>,
    source_display_name: Option<String>,
    target_agent_id: Option<String>,
    target_node_id: Option<String>,
    capability: &'static str,
    content: Value,
}

struct MissionLifecycleEnvelope {
    content: Value,
    agent_envelope: SwarmAgentEnvelope,
    source_node_id: String,
    has_source_agent_card: bool,
}

fn task_agent_envelope(
    state: &ControlPlaneState,
    args: TaskAgentEnvelopeArgs,
) -> anyhow::Result<SwarmAgentEnvelope> {
    build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: args.source_agent_id,
            source_public_id: args.source_public_id,
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

async fn build_mission_lifecycle_envelope(
    state: &ControlPlaneState,
    args: MissionLifecycleEnvelopeArgs,
) -> anyhow::Result<MissionLifecycleEnvelope> {
    let source_node_id = state.swarm_bridge.local_node_id().await?;
    let mut content = args.content;
    if let Some(object) = content.as_object_mut() {
        object
            .entry("kind".to_owned())
            .or_insert_with(|| Value::String(args.kind.to_owned()));
        object
            .entry("mission_id".to_owned())
            .or_insert_with(|| Value::String(args.mission_id.clone()));
        object
            .entry("task_id".to_owned())
            .or_insert_with(|| Value::String(args.task_id.clone()));
        object
            .entry("mission_feed_key".to_owned())
            .or_insert_with(|| Value::String(args.mission_feed_key.clone()));
        object
            .entry("mission_scope_hint".to_owned())
            .or_insert_with(|| Value::String(args.mission_scope_hint.clone()));
    }
    let agent_envelope = task_agent_envelope(
        state,
        TaskAgentEnvelopeArgs {
            source_agent_id: args.source_agent_id.clone(),
            source_public_id: args.source_public_id,
            source_display_name: args.source_display_name,
            target_agent_id: args.target_agent_id.clone(),
            source_node_id: Some(source_node_id.clone()),
            target_node_id: args.target_node_id.clone(),
            capability: args.capability.to_owned(),
            message: content.clone(),
        },
    )?;
    let has_source_agent_card = agent_envelope.source_agent_card.is_some();
    Ok(MissionLifecycleEnvelope {
        content,
        agent_envelope,
        source_node_id,
        has_source_agent_card,
    })
}

async fn post_mission_lifecycle_topic_notice(
    state: &ControlPlaneState,
    mission_feed_key: &str,
    mission_scope_hint: &str,
    envelope: &MissionLifecycleEnvelope,
) -> anyhow::Result<()> {
    state
        .swarm_bridge
        .post_topic_message(
            None,
            mission_feed_key,
            mission_scope_hint,
            envelope.content.clone(),
            None,
            Some(envelope.agent_envelope.clone()),
        )
        .await
        .map(|_| ())
}

/// Display name of an agent this node knows. For the local agent the identity bound to its
/// controller wins (the same one `get_agent_identity` reports); otherwise any active identity
/// registered under that DID.
pub(crate) async fn agent_display_name_for_did(
    state: &ControlPlaneState,
    agent_did: &str,
) -> Option<String> {
    let agent_did = agent_did.trim();
    if agent_did.is_empty() {
        return None;
    }
    let bound_public_id = if agent_did == state.agent_did {
        state
            .controller_binding_registry
            .lock()
            .await
            .active_for_controller(agent_did)
            .map(|binding| binding.public_id)
    } else {
        None
    };
    let registry = state.public_identity_registry.lock().await;
    bound_public_id
        .and_then(|public_id| registry.get(&public_id))
        .filter(|identity| identity.active)
        .or_else(|| registry.active_for_agent_did(agent_did))
        .map(|identity| identity.display_name)
}

async fn agent_public_id_for_did(state: &ControlPlaneState, agent_did: &str) -> Option<String> {
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
        .and_then(|identity| public_agent_id(&identity.public_id))
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

/// The route to another node's mission: what the request carries, else what the publisher put
/// in the gateway record's payload.
fn network_claim_route(
    body: &MissionClaimBody,
    record: &Value,
) -> Result<NetworkMissionClaimRoute, Response> {
    let payload = record.get("payload").cloned().unwrap_or(Value::Null);
    let task_id = non_empty_string(body.task_id.as_ref())
        .or_else(|| claim_route_string(body, "task_id"))
        .or_else(|| value_string(&payload, "task_id"))
        .unwrap_or_else(|| body.mission_id.clone());
    let publisher_wattswarm_node_id = non_empty_string(body.publisher_wattswarm_node_id.as_ref())
        .or_else(|| claim_route_string(body, "publisher_wattswarm_node_id"))
        .or_else(|| value_string(&payload, "publisher_wattswarm_node_id"));
    let mission_feed_key = non_empty_string(body.mission_feed_key.as_ref())
        .or_else(|| claim_route_string(body, "mission_feed_key"))
        .or_else(|| value_string(&payload, "mission_feed_key"))
        .unwrap_or_else(|| MISSION_FEED_KEY.to_owned());
    let mission_scope_hint = non_empty_string(body.mission_scope_hint.as_ref())
        .or_else(|| claim_route_string(body, "mission_scope_hint"))
        .or_else(|| value_string(&payload, "mission_scope_hint"))
        .or_else(|| {
            publisher_wattswarm_node_id
                .as_ref()
                .map(|node_id| publisher_node_scope_hint(node_id))
        })
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "this mission is not on wattswarm: its gateway record names no mission_scope_hint or publisher_wattswarm_node_id",
                    "mission_id": body.mission_id,
                })),
            )
                .into_response()
        })?;
    Ok(NetworkMissionClaimRoute {
        task_id,
        mission_feed_key,
        mission_scope_hint,
        publisher_wattswarm_node_id,
    })
}

fn parse_network_task_contract(value: &Value) -> Result<TaskContract, String> {
    serde_json::from_value::<TaskContract>(value.clone())
        .map_err(|error| format!("gateway task_contract is invalid: {error}"))
}

/// Adds to the contract's inputs what the gateway record says of the mission and the contract
/// left out.
fn enrich_task_contract_inputs(mut contract: TaskContract, record: &Value) -> TaskContract {
    let Some(inputs) = contract.inputs.as_object_mut() else {
        return contract;
    };
    let fields = [
        ("title", record.get("title")),
        ("description", record.get("description")),
        ("domain", record.get("domain")),
        ("publisher_agent_did", record.get("publisher")),
        ("publisher_display_name", record.get("publisher_label")),
        (
            "publisher_wattswarm_node_id",
            record.pointer("/payload/publisher_wattswarm_node_id"),
        ),
        ("publisher", record.pointer("/payload/civil/publisher")),
    ];
    for (field, value) in fields {
        if !inputs.contains_key(field)
            && let Some(value) = value.filter(|value| !value.is_null())
        {
            inputs.insert(field.to_owned(), value.clone());
        }
    }
    contract
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

/// The wattswarm task contract of another node's mission: the one the request carries, else
/// the one its publisher put in the gateway record.
fn network_task_contract_for_action(
    record: &Value,
    route: &NetworkMissionClaimRoute,
    body: &MissionClaimBody,
    action: &str,
) -> Result<TaskContract, Response> {
    let bad_request =
        |error: String| (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
    let contract_value = claim_route_value(body, "task_contract")
        .or_else(|| claim_route_value(body, "contract"))
        .or_else(|| record.pointer("/payload/task_contract"))
        .filter(|value| !value.is_null());
    let Some(contract_value) = contract_value else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("network mission {action} needs the mission's task_contract, and its gateway record has none"),
                "action": action,
                "task_id": route.task_id,
            })),
        )
            .into_response());
    };
    let contract = parse_network_task_contract(contract_value).map_err(bad_request)?;
    let contract = enrich_task_contract_inputs(contract, record);
    validate_network_task_contract(&contract, route, body).map_err(bad_request)?;
    Ok(contract)
}

async fn import_network_task_contract(
    state: &ControlPlaneState,
    contract: TaskContract,
) -> anyhow::Result<(String, Value)> {
    let subscriber_node_id = state.swarm_bridge.local_node_id().await?;
    let task_contract_sync = state.swarm_bridge.import_task_contract(contract).await?;
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
        "title": mission.title,
        "description": mission.description,
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
        "scope": mission.scope,
        "required_role": mission.required_role,
        "required_faction": mission.required_faction,
        "subnet_id": mission.subnet_id,
        "zone_id": mission.zone_id,
        "lat": mission.lat,
        "lng": mission.lng,
        "coordinate_source": mission.coordinate_source.clone(),
        "city": mission.city.clone(),
        "country": mission.country.clone(),
        "payload": mission.payload,
    });
    if let Some(delegation) = settlement_delegation_from_payload(&mission.payload)
        && let Some(object) = inputs.as_object_mut()
    {
        object.insert("settlement_delegation".to_owned(), delegation.clone());
    }
    if let Some(reward) = mission.reward.as_ref()
        && let Some(object) = inputs.as_object_mut()
    {
        object.insert("reward".to_owned(), json!(reward));
    }
    insert_mission_assignee(&mut inputs, mission);
    inputs
}

/// Says, in what a mission announces to the network, which agent it is reserved for, so that
/// agent's node lets the claim through and every other node refuses it at once.
fn insert_mission_assignee(target: &mut Value, mission: &CivilMission) {
    let (Some(object), Some(assignee)) = (target.as_object_mut(), mission.assignee_did.as_ref())
    else {
        return;
    };
    object.insert("assignee_did".to_owned(), json!(assignee));
    object.insert(
        "assignee_display_name".to_owned(),
        json!(mission.assignee_display_name),
    );
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
        "scope": mission.scope,
        "publisher": mission.publisher,
        "publisher_agent_did": publisher_agent_did,
        "publisher_display_name": publisher_display_name,
        "publisher_wattswarm_node_id": publisher_wattswarm_node_id,
        "lat": mission.lat,
        "lng": mission.lng,
        "coordinate_source": mission.coordinate_source.clone(),
        "city": mission.city.clone(),
        "country": mission.country.clone(),
        "mission_feed_key": MISSION_FEED_KEY,
        "mission_scope_hint": mission_scope_hint,
    });
    if let Some(delegation) = settlement_delegation_from_payload(&mission.payload)
        && let Some(object) = summary.as_object_mut()
    {
        object.insert("settlement_delegation".to_owned(), delegation.clone());
    }
    if let Some(reward) = mission.reward.as_ref()
        && let Some(object) = summary.as_object_mut()
    {
        object.insert("reward".to_owned(), json!(reward));
    }
    insert_mission_assignee(&mut summary, mission);
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

/// What a route answers for a mission: the gateway record as this node shows it, with the
/// names this node knows for its publisher, its claimer and this node's own agent.
async fn mission_payload(state: &ControlPlaneState, record: &Value) -> Value {
    let mut payload = gateway_missions::mission_json_or_record(record);
    let participant = |field: &str| {
        payload
            .get(field)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    };
    let publisher = participant("publisher");
    let claimer = participant("claimed_by");
    let completer = participant("completed_by");
    let identities = state.public_identity_registry.lock().await.list();
    let Some(object) = payload.as_object_mut() else {
        return payload;
    };
    for (prefix, participant) in [
        ("created_by", publisher.as_deref()),
        ("claimer", claimer.as_deref()),
        ("completer", completer.as_deref()),
        ("source", Some(state.agent_did.as_str())),
    ] {
        if let Some(identity) = mission_identity_for_participant(&identities, participant) {
            insert_mission_identity_projection(object, prefix, identity);
        }
    }
    payload
}

/// The gateway record of a mission, or `404` when the gateway does not know it.
async fn mission_record(state: &ControlPlaneState, mission_id: &str) -> Result<Value, Response> {
    match gateway_missions::fetch(state, mission_id).await {
        Ok(Some(record)) => Ok(record),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "mission not found", "mission_id": mission_id})),
        )
            .into_response()),
        Err(error) => Err(error.into_response()),
    }
}

/// The gateway record read as a mission; an error when the gateway sent one this node cannot
/// read.
fn readable_mission(record: &Value) -> Result<CivilMission, Response> {
    civil_mission(record).ok_or_else(|| {
        internal_error(&anyhow::anyhow!(
            "the gateway sent a mission this node cannot read: {record}"
        ))
    })
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
            insert_mission_actor_projection_fallback(object, "claimer", agent_did, body);
            insert_mission_actor_projection_fallback(object, "completer", agent_did, body);
        }
        _ => {}
    }
}

fn insert_mission_settle_actor_projection_fallbacks(
    payload: &mut Value,
    mission: &CivilMission,
    body: &MissionSettleBody,
) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    let claimer_agent_did = mission.claimed_by.as_deref().or(body.agent_did.as_deref());
    if let Some(agent_did) = claimer_agent_did {
        let mut actor_body = MissionClaimBody::local(body.mission_id.clone(), agent_did.to_owned());
        actor_body.claim_route.clone_from(&body.claim_route);
        insert_mission_actor_projection_fallback(object, "claimer", agent_did, &actor_body);
    }
    let completer_agent_did = mission
        .completed_by
        .as_deref()
        .or(body.agent_did.as_deref());
    if let Some(agent_did) = completer_agent_did {
        let mut actor_body = MissionClaimBody::local(body.mission_id.clone(), agent_did.to_owned());
        actor_body.claim_route.clone_from(&body.claim_route);
        insert_mission_actor_projection_fallback(object, "completer", agent_did, &actor_body);
    }
}

/// Name the claimer goes by: a local identity's display name first, otherwise the name the
/// claim carried in its agent card. Recorded on the mission so the bounty payout can name the
/// executor in the ledger.
async fn claimer_display_name(
    state: &ControlPlaneState,
    body: &MissionClaimBody,
) -> Option<String> {
    match agent_display_name_for_did(state, &body.agent_did).await {
        Some(name) => Some(name),
        None => mission_claim_actor_display_name(body),
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
    value_path_string(body.claim_route.as_ref()?, path)
}

fn value_path_string(value: &Value, path: &[&str]) -> Option<String> {
    path.iter()
        .try_fold(value, |value, key| value.get(*key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn claim_route_candidate_id(body: &MissionSettleBody) -> Option<String> {
    body.claim_route
        .as_ref()
        .and_then(|route| value_path_string(route, &["candidate_id"]))
}

fn claim_route_execution_id(body: &MissionClaimBody) -> Option<String> {
    claim_route_string(body, "execution_id").or_else(|| {
        [
            &["decision_payload", "execution_id"][..],
            &["agent_event_payload", "execution_id"][..],
            &["agent_event_payload", "output", "execution_id"][..],
            &["agent_event_payload", "content", "execution_id"][..],
            &["agent_event_payload", "topic_content", "execution_id"][..],
        ]
        .into_iter()
        .find_map(|path| claim_route_path_string(body, path))
    })
}

fn settle_route_execution_id(body: &MissionSettleBody) -> Option<String> {
    let route = body.claim_route.as_ref()?;
    [
        &["execution_id"][..],
        &["decision_payload", "execution_id"][..],
        &["agent_event_payload", "execution_id"][..],
        &["agent_event_payload", "output", "execution_id"][..],
        &["agent_event_payload", "content", "execution_id"][..],
        &["agent_event_payload", "topic_content", "execution_id"][..],
    ]
    .into_iter()
    .find_map(|path| value_path_string(route, path))
}

fn mission_lifecycle_target_node_id_from_claim(body: &MissionClaimBody) -> Option<String> {
    [
        &["agent_envelope", "source_node_id"][..],
        &["agent_event_payload", "source_node_id"][..],
        &["agent_event_payload", "claimer_node_id"][..],
        &["agent_event_payload", "agent_envelope", "source_node_id"][..],
        &["decision_payload", "claimer_node_id"][..],
    ]
    .into_iter()
    .find_map(|path| claim_route_path_string(body, path))
}

fn mission_lifecycle_target_node_id_from_settle(body: &MissionSettleBody) -> Option<String> {
    let route = body.claim_route.as_ref()?;
    [
        &["agent_envelope", "source_node_id"][..],
        &["agent_event_payload", "source_node_id"][..],
        &["agent_event_payload", "claimer_node_id"][..],
        &["agent_event_payload", "agent_envelope", "source_node_id"][..],
        &["decision_payload", "claimer_node_id"][..],
    ]
    .into_iter()
    .find_map(|path| value_path_string(route, path))
}

/// The wattswarm task contract of a mission this node publishes. It is built before the
/// gateway Publish so the gateway record carries it to the nodes that claim the mission. The
/// mission has no id yet, so the contract names none: the gateway writes it in.
async fn mission_contract(
    state: &ControlPlaneState,
    mission: &CivilMission,
) -> Result<TaskContract, Response> {
    let publisher_display_name = agent_display_name_for_did(state, &state.agent_did).await;
    let publisher_wattswarm_node_id = state
        .swarm_bridge
        .local_node_id()
        .await
        .map_err(|error| internal_error(&error))?;
    state
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
        .map_err(|error| internal_error(&error))
}

/// Puts the mission's task on wattswarm and announces it to the mission feed.
async fn announce_mission_on_swarm(
    state: &ControlPlaneState,
    mission: &CivilMission,
    contract: &TaskContract,
) -> anyhow::Result<()> {
    let publisher_display_name = agent_display_name_for_did(state, &state.agent_did).await;
    let publisher_public_id = agent_public_id_for_did(state, &state.agent_did).await;
    let publisher_wattswarm_node_id = state.swarm_bridge.local_node_id().await?;
    state.swarm_bridge.submit_task(contract.clone()).await?;
    let agent_envelope = task_agent_envelope(
        state,
        TaskAgentEnvelopeArgs {
            source_agent_id: state.agent_did.clone(),
            source_public_id: publisher_public_id,
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
    )?;
    state
        .swarm_bridge
        .announce_task(mission_announce_command(
            mission,
            &state.agent_did,
            publisher_display_name.as_deref(),
            &publisher_wattswarm_node_id,
            Some(agent_envelope),
        ))
        .await?;
    Ok(())
}

/// What a publish asks for, apart from what this node adds to each draft (its times and where
/// the node is), as one fingerprint: two publishes with the same one ask for the same mission.
/// It goes to the gateway as the publish's request id. The gateway keeps one mission per
/// publisher and request id, so the same mission asked for again, by a caller that lost the
/// answer or by this node after a restart, is answered with the one already there and locks no
/// second reward. Only a cancelled mission lets the same one be published anew.
fn publish_request_id(mission: &CivilMission, terms: &PublishTerms<'_>) -> anyhow::Result<String> {
    let mut asked = serde_json::to_value(mission)?;
    if let Some(object) = asked.as_object_mut() {
        for added in [
            "created_at",
            "updated_at",
            "lat",
            "lng",
            "coordinate_source",
            "city",
            "country",
        ] {
            object.remove(added);
        }
        object.insert("kind".to_owned(), json!(terms.kind));
        object.insert("organization_id".to_owned(), json!(terms.organization_id));
    }
    Ok(hex::encode(Sha256::digest(asked.to_string().as_bytes())))
}

/// What publishing a drafted mission came to.
pub(crate) struct MissionPublication {
    /// The mission as the gateway keeps it.
    pub(crate) record: Value,
    /// The mission was there already, on the gateway and where it belongs on wattswarm:
    /// nothing was published now.
    pub(crate) already_published: bool,
}

/// The task contract as the gateway keeps it for `record`, with the mission's id written in.
fn recorded_task_contract(record: &Value) -> anyhow::Result<TaskContract> {
    let contract = record
        .pointer("/payload/task_contract")
        .ok_or_else(|| anyhow::anyhow!("the gateway kept no task contract for the mission"))?;
    parse_network_task_contract(contract).map_err(anyhow::Error::msg)
}

/// Publishes a drafted mission on the gateway and returns the record it keeps. The gateway
/// makes the mission's id, which `mission` carries from then on, and answers a mission this
/// agent already published with that one (see [`publish_request_id`]).
///
/// With `on_swarm`, the mission's task contract travels in the record, where the gateway
/// writes the id into it, and the task is put on this node's wattswarm and announced with that
/// contract, unless wattswarm has the task already. A mission that was there before is put on
/// wattswarm all the same when it is missing there: an earlier publish got as far as the
/// gateway and no further. If announcing fails, the publish fails; the publish that made the
/// mission also cancels it, which returns its reward and lets it be published again, while a
/// mission that was there before is left as it is, somebody may be working on it. Its task
/// may be on wattswarm by then with only the announcement missing, and then stays so: the
/// announcement goes to the mission's own topic, which nobody but this node follows yet.
pub(crate) async fn publish_drafted_mission(
    state: &ControlPlaneState,
    mission: &mut CivilMission,
    terms: &PublishTerms<'_>,
    on_swarm: bool,
) -> Result<MissionPublication, Response> {
    if mission
        .expires_at
        .is_some_and(|at| at <= Utc::now().timestamp())
    {
        return Err(bad_publish("expires_at must be in the future", "invalid"));
    }
    let request_id = publish_request_id(mission, terms).map_err(|error| internal_error(&error))?;
    let contract = if on_swarm {
        Some(mission_contract(state, mission).await?)
    } else {
        None
    };
    let record = gateway_missions::publish(state, &request_id, mission, contract.as_ref(), terms)
        .await
        .map_err(IntoResponse::into_response)?;
    let Some(mission_id) = record.get("mission_id").and_then(Value::as_str) else {
        return Err(internal_error(&anyhow::anyhow!(
            "the gateway answered a publish without a mission id"
        )));
    };
    mission_id.clone_into(&mut mission.mission_id);
    let mission = &*mission;
    let already_published = record
        .get("already_published")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // wattswarm itself says whether the task is there: this node keeps no list of its own.
    if !on_swarm
        || (already_published
            && state
                .swarm_bridge
                .task_decision_snapshot(&mission.mission_id)
                .await
                .is_ok())
    {
        return Ok(MissionPublication {
            record,
            already_published,
        });
    }
    let announced = match recorded_task_contract(&record) {
        Ok(contract) => announce_mission_on_swarm(state, mission, &contract).await,
        Err(error) => Err(error),
    };
    if let Err(error) = announced {
        if !already_published
            && let Err(cancel_error) = gateway_missions::command(
                state,
                &mission.mission_id,
                MissionAction::Cancel,
                json!({"reason": "the mission could not be announced on wattswarm"}),
            )
            .await
        {
            // The mission stays on the gateway and is found again by the next publish.
            tracing::warn!(
                mission_id = %mission.mission_id,
                "mission not cancelled after its wattswarm announce failed: {cancel_error}"
            );
        }
        return Err(internal_error(&error));
    }
    Ok(MissionPublication {
        record,
        already_published: false,
    })
}

fn network_claim_agent_envelope(
    state: &ControlPlaneState,
    route: &NetworkMissionClaimRoute,
    body: &MissionClaimBody,
    execution_id: &str,
    source_public_id: Option<String>,
    source_display_name: Option<String>,
    source_node_id: Option<String>,
) -> anyhow::Result<SwarmAgentEnvelope> {
    task_agent_envelope(
        state,
        TaskAgentEnvelopeArgs {
            source_agent_id: body.agent_did.clone(),
            source_public_id,
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

async fn publish_claim_approved_lifecycle_notice(
    state: &ControlPlaneState,
    mission: &CivilMission,
    body: &MissionClaimBody,
    agent_did: &str,
    publish_topic_notice: bool,
) -> anyhow::Result<Value> {
    let task_id = body
        .task_id
        .clone()
        .or_else(|| claim_route_string(body, "task_id"))
        .unwrap_or_else(|| mission.mission_id.clone());
    let mission_feed_key = body
        .mission_feed_key
        .clone()
        .or_else(|| claim_route_string(body, "mission_feed_key"))
        .unwrap_or_else(|| MISSION_FEED_KEY.to_owned());
    let mission_scope_hint = body
        .mission_scope_hint
        .clone()
        .or_else(|| claim_route_string(body, "mission_scope_hint"))
        .unwrap_or_else(|| mission_task_scope_hint(&task_id));
    let target_node_id = match mission_lifecycle_target_node_id_from_claim(body) {
        Some(node_id) => Some(node_id),
        None => state.swarm_bridge.local_node_id().await.ok(),
    };
    let Some(claimer_node_id) = target_node_id.clone() else {
        bail!("mission claim approval requires claimer_node_id");
    };
    let execution_id = claim_route_execution_id(body)
        .unwrap_or_else(|| mission_execution_id(&mission.mission_id, agent_did));
    let source_public_id = agent_public_id_for_did(state, &state.agent_did).await;
    let source_display_name = agent_display_name_for_did(state, &state.agent_did).await;
    let envelope = build_mission_lifecycle_envelope(
        state,
        MissionLifecycleEnvelopeArgs {
            kind: "mission_claim_approved",
            task_id: task_id.clone(),
            mission_id: mission.mission_id.clone(),
            mission_feed_key: mission_feed_key.clone(),
            mission_scope_hint: mission_scope_hint.clone(),
            source_agent_id: state.agent_did.clone(),
            source_public_id,
            source_display_name,
            target_agent_id: Some(agent_did.to_owned()),
            target_node_id: target_node_id.clone(),
            capability: "mission.claim.approve",
            content: json!({
                "kind": "mission_claim_approved",
                "mission_id": mission.mission_id,
                "task_id": task_id,
                "mission_feed_key": mission_feed_key,
                "mission_scope_hint": mission_scope_hint,
                "publisher_agent_did": state.agent_did,
                "claimer_agent_did": agent_did,
                "claimer_node_id": claimer_node_id,
                "execution_id": execution_id.clone(),
                "status": "approved",
                "mission_status": "claimed",
                "next_action": "complete_mission",
            }),
        },
    )
    .await?;
    state
        .swarm_bridge
        .decide_task_claim(SwarmTaskClaimDecisionCommand {
            task_id: task_id.clone(),
            execution_id: execution_id.clone(),
            claimer_node_id: claimer_node_id.clone(),
            approved: true,
            reason: None,
            agent_envelope: envelope.agent_envelope.clone(),
        })
        .await?;
    if publish_topic_notice {
        post_mission_lifecycle_topic_notice(
            state,
            &mission_feed_key,
            &mission_scope_hint,
            &envelope,
        )
        .await?;
    }
    Ok(json!({
        "kind": "mission_claim_approved",
        "event_kind": "task_claim_decided",
        "mission_id": mission.mission_id,
        "task_id": task_id,
        "mission_feed_key": mission_feed_key,
        "mission_scope_hint": mission_scope_hint,
        "source_agent_id": state.agent_did,
        "source_node_id": envelope.source_node_id,
        "target_agent_id": agent_did,
        "target_node_id": target_node_id,
        "execution_id": execution_id,
        "approved": true,
        "has_agent_envelope": true,
        "has_source_agent_card": envelope.has_source_agent_card,
    }))
}

struct MissionCompletedLifecycleArgs {
    task_id: String,
    mission_id: String,
    mission_feed_key: String,
    mission_scope_hint: String,
    agent_did: String,
    result: Value,
    publisher_agent_did: Option<String>,
    publisher_wattswarm_node_id: Option<String>,
    execution_id: Option<String>,
    publish_topic_notice: bool,
}

async fn publish_mission_completed_lifecycle_notice(
    state: &ControlPlaneState,
    args: MissionCompletedLifecycleArgs,
) -> anyhow::Result<Value> {
    let source_public_id = agent_public_id_for_did(state, &args.agent_did).await;
    let source_display_name = agent_display_name_for_did(state, &args.agent_did).await;
    let execution_id = args
        .execution_id
        .clone()
        .unwrap_or_else(|| mission_execution_id(&args.mission_id, &args.agent_did));
    let envelope = build_mission_lifecycle_envelope(
        state,
        MissionLifecycleEnvelopeArgs {
            kind: "mission_completed",
            task_id: args.task_id.clone(),
            mission_id: args.mission_id.clone(),
            mission_feed_key: args.mission_feed_key.clone(),
            mission_scope_hint: args.mission_scope_hint.clone(),
            source_agent_id: args.agent_did.clone(),
            source_public_id,
            source_display_name,
            target_agent_id: args.publisher_agent_did.clone(),
            target_node_id: args.publisher_wattswarm_node_id.clone(),
            capability: "mission.complete",
            content: json!({
                "kind": "mission_completed",
                "mission_id": args.mission_id,
                "task_id": args.task_id,
                "mission_feed_key": args.mission_feed_key,
                "mission_scope_hint": args.mission_scope_hint,
                "publisher_agent_did": args.publisher_agent_did,
                "publisher_wattswarm_node_id": args.publisher_wattswarm_node_id,
                "claimer_agent_did": args.agent_did,
                "agent_did": args.agent_did,
                "execution_id": execution_id,
                "result": args.result,
                "status": "completed",
                "mission_status": "completed",
                "next_action": "settle_mission",
            }),
        },
    )
    .await?;
    state
        .swarm_bridge
        .complete_task(SwarmTaskCompleteCommand {
            task_id: args.task_id.clone(),
            execution_id: execution_id.clone(),
            output: envelope.content.clone(),
            agent_envelope: envelope.agent_envelope.clone(),
        })
        .await?;
    if args.publish_topic_notice {
        post_mission_lifecycle_topic_notice(
            state,
            &args.mission_feed_key,
            &args.mission_scope_hint,
            &envelope,
        )
        .await?;
    }
    Ok(json!({
        "kind": "mission_completed",
        "event_kind": "task_completed",
        "mission_id": args.mission_id,
        "task_id": args.task_id,
        "mission_feed_key": args.mission_feed_key,
        "mission_scope_hint": args.mission_scope_hint,
        "source_agent_id": args.agent_did,
        "source_node_id": envelope.source_node_id,
        "target_agent_id": args.publisher_agent_did,
        "target_node_id": args.publisher_wattswarm_node_id,
        "execution_id": execution_id,
        "has_agent_envelope": true,
        "has_source_agent_card": envelope.has_source_agent_card,
    }))
}

async fn publish_settled_lifecycle_notice(
    state: &ControlPlaneState,
    mission: &CivilMission,
    body: &MissionSettleBody,
    publish_topic_notice: bool,
) -> anyhow::Result<Value> {
    let task_id = body
        .task_id
        .clone()
        .unwrap_or_else(|| mission.mission_id.clone());
    let mission_feed_key = body
        .claim_route
        .as_ref()
        .and_then(|route| value_path_string(route, &["mission_feed_key"]))
        .unwrap_or_else(|| MISSION_FEED_KEY.to_owned());
    let mission_scope_hint = body
        .claim_route
        .as_ref()
        .and_then(|route| value_path_string(route, &["mission_scope_hint"]))
        .unwrap_or_else(|| mission_task_scope_hint(&task_id));
    // Who is paid is the gateway's to say: the request names a claimer only when the record
    // does not.
    let target_agent_id = mission
        .completed_by
        .clone()
        .or_else(|| mission.claimed_by.clone())
        .or_else(|| body.agent_did.clone());
    let execution_id = settle_route_execution_id(body)
        .or_else(|| {
            target_agent_id
                .as_ref()
                .map(|agent_did| mission_execution_id(&mission.mission_id, agent_did))
        })
        .unwrap_or_else(|| mission_execution_id(&mission.mission_id, "unknown"));
    let target_node_id = mission_lifecycle_target_node_id_from_settle(body);
    let source_public_id = agent_public_id_for_did(state, &state.agent_did).await;
    let source_display_name = agent_display_name_for_did(state, &state.agent_did).await;
    let envelope = build_mission_lifecycle_envelope(
        state,
        MissionLifecycleEnvelopeArgs {
            kind: "mission_settled",
            task_id: task_id.clone(),
            mission_id: mission.mission_id.clone(),
            mission_feed_key: mission_feed_key.clone(),
            mission_scope_hint: mission_scope_hint.clone(),
            source_agent_id: state.agent_did.clone(),
            source_public_id,
            source_display_name,
            target_agent_id: target_agent_id.clone(),
            target_node_id: target_node_id.clone(),
            capability: "mission.settle",
            content: json!({
                "kind": "mission_settled",
                "mission_id": mission.mission_id,
                "task_id": task_id,
                "mission_feed_key": mission_feed_key,
                "mission_scope_hint": mission_scope_hint,
                "publisher_agent_did": state.agent_did,
                "claimer_agent_did": target_agent_id,
                "claimer_node_id": target_node_id,
                "execution_id": execution_id,
                "status": "settled",
                "mission_status": "settled",
                "result": mission.completion_result,
            }),
        },
    )
    .await?;
    state
        .swarm_bridge
        .settle_task(SwarmTaskSettleCommand {
            task_id: task_id.clone(),
            execution_id: execution_id.clone(),
            receipt: Some(envelope.content.clone()),
            agent_envelope: envelope.agent_envelope.clone(),
        })
        .await?;
    if publish_topic_notice {
        post_mission_lifecycle_topic_notice(
            state,
            &mission_feed_key,
            &mission_scope_hint,
            &envelope,
        )
        .await?;
    }
    Ok(json!({
        "kind": "mission_settled",
        "event_kind": "task_settled",
        "mission_id": mission.mission_id,
        "task_id": task_id,
        "mission_feed_key": mission_feed_key,
        "mission_scope_hint": mission_scope_hint,
        "source_agent_id": state.agent_did,
        "source_node_id": envelope.source_node_id,
        "target_agent_id": target_agent_id,
        "target_node_id": target_node_id,
        "execution_id": execution_id,
        "has_agent_envelope": true,
        "has_source_agent_card": envelope.has_source_agent_card,
    }))
}

fn mission_settle_candidate(
    body: &MissionSettleBody,
    mission: &CivilMission,
) -> Option<(String, String)> {
    let task_id = body
        .task_id
        .clone()
        .unwrap_or_else(|| mission.mission_id.clone());
    let candidate_id = body
        .candidate_id
        .clone()
        .or_else(|| claim_route_candidate_id(body))?;
    Some((task_id, candidate_id))
}

async fn finalize_mission_task_before_settle(
    state: &ControlPlaneState,
    body: &MissionSettleBody,
    headers: &HeaderMap,
    mission: &CivilMission,
) -> Result<Option<Value>, Response> {
    if agent_commit_context_from_headers(headers).is_some()
        || mission.status != MissionStatus::Completed
    {
        return Ok(None);
    }
    let Some((task_id, candidate_id)) = mission_settle_candidate(body, mission) else {
        return Ok(None);
    };
    let target_agent_did = body
        .agent_did
        .as_deref()
        .or(mission.completed_by.as_deref())
        .map(ToOwned::to_owned);
    let local_node_id = state.swarm_bridge.local_node_id().await.ok();
    let source_public_id = agent_public_id_for_did(state, &state.agent_did).await;
    let source_display_name = agent_display_name_for_did(state, &state.agent_did).await;
    let agent_envelope = task_agent_envelope(
        state,
        TaskAgentEnvelopeArgs {
            source_agent_id: state.agent_did.clone(),
            source_public_id,
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
    )
    .map_err(|error| internal_error(&error))?;
    state
        .swarm_bridge
        .accept_and_finalize_task(&task_id, &candidate_id, Some(agent_envelope))
        .await
        .map(Some)
        .map_err(|error| internal_error(&error))
}

/// Publishes a cancellation to the local stream, the signed event log and the audit log.
fn announce_mission_cancelled(
    state: &ControlPlaneState,
    mission_id: &str,
    payload: &Value,
    reason: Option<String>,
    actor: Option<String>,
) {
    let mut payload = payload.clone();
    if let (Some(object), Some(reason)) = (payload.as_object_mut(), reason.as_deref()) {
        object.insert("cancel_reason".to_string(), json!(reason));
    }
    let _ = state.stream_tx.send(StreamEvent {
        kind: "mission.cancelled".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event("MISSION_CANCELLED", payload.clone());
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: "mission.cancel".to_string(),
        status: "ok".to_string(),
        actor,
        subject: Some(mission_id.to_owned()),
        capability: Some("mission.cancel".to_string()),
        reason,
        duration_ms: None,
        details: Some(payload),
    });
}

/// `POST /v1/wattetheria/missions/{mission_id}/cancel`: the publisher cancels its mission on
/// the gateway, which returns a held reward.
pub(crate) async fn mission_cancel_by_id(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(mission_id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let reason = body
        .as_ref()
        .and_then(|Json(body)| body.get("reason").and_then(Value::as_str))
        .map(str::to_owned);
    let record = match gateway_missions::command(
        &state,
        &mission_id,
        MissionAction::Cancel,
        json!({"reason": reason}),
    )
    .await
    {
        Ok(record) => record,
        Err(error) => return error.into_response(),
    };
    let payload = mission_payload(&state, &record).await;
    announce_mission_cancelled(&state, &mission_id, &payload, reason, Some(auth));
    Json(payload).into_response()
}

/// Drops what this node kept about its agent's claim on a mission it gave up, so that asking
/// for the mission again is answered by the gateway.
fn forget_network_claim(state: &ControlPlaneState, mission_id: &str) -> anyhow::Result<()> {
    let mut registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(local_db::domain::NETWORK_MISSION_CLAIMS)?;
    registry.forget(mission_id, &state.agent_did);
    state
        .local_db
        .save_domain(local_db::domain::NETWORK_MISSION_CLAIMS, &registry)
}

/// Tells a mission's publisher, on the mission's own topic, that this node's agent gave the
/// mission up. `record` is the mission as the gateway keeps it after the change; the notice
/// only wakes the publisher's agent.
async fn publish_claim_withdrawn_lifecycle_notice(
    state: &ControlPlaneState,
    record: &Value,
    mission_id: &str,
    reason: Option<&str>,
) -> anyhow::Result<Value> {
    let body = MissionClaimBody::local(mission_id.to_owned(), state.agent_did.clone());
    let Ok(route) = network_claim_route(&body, record) else {
        bail!("the gateway record names no wattswarm topic for this mission");
    };
    let publisher_agent_did = value_string(record, "publisher");
    let source_public_id = agent_public_id_for_did(state, &state.agent_did).await;
    let source_display_name = agent_display_name_for_did(state, &state.agent_did).await;
    let envelope = build_mission_lifecycle_envelope(
        state,
        MissionLifecycleEnvelopeArgs {
            kind: "mission_claim_withdrawn",
            task_id: route.task_id.clone(),
            mission_id: mission_id.to_owned(),
            mission_feed_key: route.mission_feed_key.clone(),
            mission_scope_hint: route.mission_scope_hint.clone(),
            source_agent_id: state.agent_did.clone(),
            source_public_id,
            source_display_name,
            target_agent_id: publisher_agent_did.clone(),
            target_node_id: route.publisher_wattswarm_node_id.clone(),
            capability: "mission.claim.withdraw",
            content: json!({
                "kind": "mission_claim_withdrawn",
                "mission_id": mission_id,
                "task_id": route.task_id,
                "mission_feed_key": route.mission_feed_key,
                "mission_scope_hint": route.mission_scope_hint,
                "publisher_agent_did": publisher_agent_did,
                "publisher_wattswarm_node_id": route.publisher_wattswarm_node_id,
                "claimer_agent_did": state.agent_did,
                "agent_did": state.agent_did,
                "reason": reason,
                "status": "withdrawn",
                "mission_status": gateway_missions::status(record),
            }),
        },
    )
    .await?;
    post_mission_lifecycle_topic_notice(
        state,
        &route.mission_feed_key,
        &route.mission_scope_hint,
        &envelope,
    )
    .await?;
    Ok(json!({
        "kind": "mission_claim_withdrawn",
        "sent": true,
        "mission_id": mission_id,
        "task_id": route.task_id,
        "mission_feed_key": route.mission_feed_key,
        "mission_scope_hint": route.mission_scope_hint,
        "source_agent_id": state.agent_did,
        "source_node_id": envelope.source_node_id,
        "target_agent_id": publisher_agent_did,
        "target_node_id": route.publisher_wattswarm_node_id,
    }))
}

/// `POST /v1/wattetheria/missions/{mission_id}/claims/withdraw` (`withdraw_mission_claim`):
/// this node's agent gives up a mission it asked for or holds. The gateway records it, then
/// the mission's publisher is told on wattswarm. The gateway's refusal comes back as it is.
pub(crate) async fn mission_claim_withdraw_by_id(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(mission_id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let reason = body
        .as_ref()
        .and_then(|Json(body)| body.get("reason").and_then(Value::as_str))
        .map(str::to_owned);
    let record = match gateway_missions::command(
        &state,
        &mission_id,
        MissionAction::WithdrawClaim,
        json!({"reason": reason}),
    )
    .await
    {
        Ok(record) => record,
        Err(error) => return error.into_response(),
    };
    // From here the gateway holds the change: whatever fails next, the answer says so.
    if let Err(error) = forget_network_claim(&state, &mission_id) {
        return internal_error(&error);
    }
    let mission = gateway_missions::mission_json_or_record(&record);
    let mission_lifecycle_notice = if gateway_missions::is_own(&state, &record) {
        Value::Null
    } else {
        match publish_claim_withdrawn_lifecycle_notice(
            &state,
            &record,
            &mission_id,
            reason.as_deref(),
        )
        .await
        {
            Ok(notice) => notice,
            Err(error) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "error": format!(
                            "the mission is given up, but the notice to its publisher was not \
                             delivered: {error}. Send the same request again to retry the \
                             notice; nothing else changes."
                        ),
                        "code": "withdraw_notice_not_delivered",
                        "mission_id": mission_id,
                        "mission": mission,
                        "mission_lifecycle_notice":
                            {"kind": "mission_claim_withdrawn", "sent": false},
                    })),
                )
                    .into_response();
            }
        }
    };
    let payload = json!({
        "ok": true,
        "status": "mission_claim_withdrawn",
        "mission_id": mission_id,
        "agent_did": state.agent_did,
        "mission": mission,
        "mission_lifecycle_notice": mission_lifecycle_notice,
    });
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: "mission.claim.withdraw".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(mission_id),
        capability: Some("mission.claim".to_string()),
        reason,
        duration_ms: None,
        details: Some(payload.clone()),
    });
    Json(payload).into_response()
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
    let records = match gateway_missions::view_records(&state).await {
        Ok(records) => records,
        Err(error) => return error.into_response(),
    };
    let mut missions = Vec::new();
    for record in &records {
        let Some(mission) = civil_mission(record) else {
            continue;
        };
        if query
            .status
            .as_ref()
            .is_none_or(|status| &mission.status == status)
        {
            missions.push(mission_payload(&state, record).await);
        }
    }
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
    let record = match mission_record(&state, &mission_id).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: "mission.get".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(mission_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: None,
    });
    Json(mission_payload(&state, &record).await).into_response()
}

/// The mission a publish request describes, before the gateway records it.
fn draft_mission(
    state: &ControlPlaneState,
    body: MissionPublishBody,
    settlement_delegation: Option<&Value>,
    assignee: Option<mission_trade::Assignee>,
) -> CivilMission {
    let mut mission = CivilMission::draft(
        &body.title,
        &body.description,
        &body.publisher,
        body.publisher_kind,
        body.domain,
        body.scope,
        body.subnet_id,
        body.zone_id,
        body.required_role,
        body.required_faction,
        body.reward,
        payload_with_settlement_delegation(body.payload, settlement_delegation),
        Some(state.public_geo_payload()),
    );
    mission.expires_at = body.expires_at;
    if let Some(assignee) = assignee {
        mission.assignee_did = Some(assignee.did);
        mission.assignee_display_name = assignee.display_name;
    }
    mission
}

fn bad_publish(error: impl Into<String>, code: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": error.into(), "code": code})),
    )
        .into_response()
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn mission_publish(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(mut body): Json<MissionPublishBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let in_commit = agent_commit_context_from_headers(&headers).is_some();
    if !in_commit
        && let Err(error) = validate_mission_inputs(&body.title, &body.description, &body.payload)
    {
        return validation_error_response(error);
    }
    let settlement_delegation =
        match normalize_publish_delegation(body.settlement_delegation.take()) {
            Ok(delegation) => delegation,
            Err(error) => return bad_publish(error.to_string(), "invalid"),
        };
    // The named agent is looked up before anything is published: an unknown name publishes
    // nothing.
    let assignee = match body.assignee.take() {
        Some(name) => match mission_trade::resolve_assignee(&state, &name).await {
            Ok(assignee) => Some(assignee),
            Err(response) => return response,
        },
        None => None,
    };
    let request_json = json!({"title": body.title, "publisher": body.publisher});
    let mut mission = draft_mission(&state, body, settlement_delegation.as_ref(), assignee);
    let publisher_label = agent_display_name_for_did(&state, &state.agent_did).await;
    let terms = PublishTerms {
        publisher_label: publisher_label.as_deref(),
        ..PublishTerms::kind(if settlement_delegation.is_some() {
            KIND_DELEGATED
        } else {
            KIND_STANDARD
        })
    };
    let publication = match publish_drafted_mission(&state, &mut mission, &terms, !in_commit).await
    {
        Ok(publication) => publication,
        Err(response) => return response,
    };
    let mut payload = mission_payload(&state, &publication.record).await;
    if publication.already_published {
        payload["already_published"] = json!(true);
        return Json(payload).into_response();
    }
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
            request_json: &request_json,
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

/// The task contract of another node's mission, once the ledger's rules let this agent ask for
/// This node's agent asks to take another node's mission: the gateway records the request
/// (and refuses it for a reserved mission, a banned claimer or a mission already taken), then
/// the claim goes to the publisher on wattswarm.
async fn claim_network_mission(
    state: ControlPlaneState,
    headers: HeaderMap,
    auth: String,
    body: MissionClaimBody,
    record: Value,
) -> Response {
    if agent_commit_context_from_headers(&headers).is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "network mission claim requires a direct request"})),
        )
            .into_response();
    }
    let route = match network_claim_route(&body, &record) {
        Ok(route) => route,
        Err(response) => return response,
    };
    let execution_id = mission_execution_id(&route.task_id, &body.agent_did);
    if let Some(response) =
        network_claim_already_recorded_response(&state, &body, &route, &execution_id)
    {
        return response;
    }
    let contract = match network_task_contract_for_action(&record, &route, &body, "claim") {
        Ok(contract) => contract,
        Err(response) => return response,
    };
    let claimer_label = claimer_display_name(&state, &body).await;
    let requested = match gateway_missions::command(
        &state,
        &body.mission_id,
        MissionAction::RequestClaim,
        json!({"claimer_label": claimer_label}),
    )
    .await
    {
        Ok(requested) => requested,
        Err(error) => return error.into_response(),
    };
    // From here the gateway holds the request: whatever fails next, the answer says so.
    let claim_metadata = network_claim_metadata(&body, &route, &contract);
    let (subscriber_node_id, task_contract_sync) =
        match import_network_task_contract(&state, contract).await {
            Ok(value) => value,
            Err(error) => return claim_notice_failed(&body.mission_id, &error),
        };
    let source_public_id = agent_public_id_for_did(&state, &body.agent_did).await;
    let source_display_name = agent_display_name_for_did(&state, &body.agent_did).await;
    let agent_envelope = match network_claim_agent_envelope(
        &state,
        &route,
        &body,
        &execution_id,
        source_public_id,
        source_display_name,
        Some(subscriber_node_id.clone()),
    ) {
        Ok(envelope) => envelope,
        Err(error) => return claim_notice_failed(&body.mission_id, &error),
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
        "mission": gateway_missions::mission_json_or_record(&requested),
    });

    if let Err(error) =
        record_network_claim_success(&state, auth, &execution_id, &response_json, claim_metadata)
    {
        return internal_error(&error);
    }
    Json(response_json).into_response()
}

fn record_network_claim_success(
    state: &ControlPlaneState,
    auth: String,
    execution_id: &str,
    response_json: &Value,
    metadata: NetworkMissionClaimMetadata,
) -> anyhow::Result<()> {
    record_network_claim_submission(
        state,
        response_json["mission_id"].as_str().unwrap_or_default(),
        response_json["task_id"].as_str().unwrap_or_default(),
        response_json["agent_did"].as_str().unwrap_or_default(),
        execution_id,
        metadata,
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

/// Hands `result` in on the gateway. Returns the mission as it stands and the result the
/// gateway keeps, which is what the publisher is told: a mission handed in before keeps its
/// first result, whatever this request carries.
async fn deliver_on_gateway(
    state: &ControlPlaneState,
    mission_id: &str,
    result: Value,
) -> gateway_missions::MissionResult<(Value, Value)> {
    let delivered = gateway_missions::command(
        state,
        mission_id,
        MissionAction::Deliver,
        json!({"result": result}),
    )
    .await?;
    let kept = delivered
        .get("result")
        .filter(|saved| !saved.is_null())
        .cloned()
        .unwrap_or(result);
    Ok((delivered, kept))
}

/// This node's agent hands in another node's mission: the gateway records the result (and
/// refuses it when the agent does not hold the mission), then the publisher is told on
/// wattswarm.
async fn complete_network_mission(
    state: ControlPlaneState,
    auth: String,
    body: MissionClaimBody,
    record: Value,
) -> Response {
    let route = match network_claim_route(&body, &record) {
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
    let execution_id =
        match recorded_network_claim(&state, &body.mission_id, &route.task_id, &body.agent_did) {
            Ok(claim) => claim.map_or_else(
                || mission_execution_id(&route.task_id, &body.agent_did),
                |claim| claim.execution_id,
            ),
            Err(error) => return internal_error(&error),
        };
    let contract = match network_task_contract_for_action(&record, &route, &body, "complete") {
        Ok(contract) => contract,
        Err(response) => return response,
    };
    let (delivered, result) = match deliver_on_gateway(&state, &body.mission_id, result).await {
        Ok(delivered) => delivered,
        Err(error) => return error.into_response(),
    };
    let (subscriber_node_id, task_contract_sync) =
        match import_network_task_contract(&state, contract).await {
            Ok(value) => value,
            Err(error) => return internal_error(&error),
        };
    let mission_lifecycle_notice = match publish_mission_completed_lifecycle_notice(
        &state,
        MissionCompletedLifecycleArgs {
            task_id: route.task_id.clone(),
            mission_id: body.mission_id.clone(),
            mission_feed_key: route.mission_feed_key.clone(),
            mission_scope_hint: route.mission_scope_hint.clone(),
            agent_did: body.agent_did.clone(),
            result,
            publisher_agent_did: publisher_agent_did_from_claim(&body)
                .or_else(|| value_string(&record, "publisher")),
            publisher_wattswarm_node_id: route.publisher_wattswarm_node_id.clone(),
            execution_id: Some(execution_id.clone()),
            publish_topic_notice: true,
        },
    )
    .await
    {
        Ok(notice) => notice,
        Err(error) => return internal_error(&error),
    };
    let response_json = json!({
        "ok": true,
        "status": "network_complete_published",
        "mission_id": body.mission_id,
        "task_id": route.task_id,
        "agent_did": body.agent_did,
        "execution_id": execution_id,
        "mission_feed_key": route.mission_feed_key,
        "mission_scope_hint": route.mission_scope_hint,
        "publisher_wattswarm_node_id": route.publisher_wattswarm_node_id,
        "subscriber_node_id": subscriber_node_id,
        "task_contract_sync": task_contract_sync,
        "mission_lifecycle_notice": mission_lifecycle_notice,
        "mission": gateway_missions::mission_json_or_record(&delivered),
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

/// Whether `agent_did` is this node's own agent: the only agent this node can sign for.
fn is_this_nodes_agent(state: &ControlPlaneState, agent_did: &str) -> bool {
    agent_did == state.identity.agent_did
}

/// The wattswarm claim of a mission this node publishes, sent when the claimer is on this node.
async fn dispatch_local_mission_transition_to_swarm(
    state: &ControlPlaneState,
    mission: &CivilMission,
    agent_did: &str,
) -> anyhow::Result<Value> {
    let local_node_id = state.swarm_bridge.local_node_id().await.ok();
    let source_public_id = agent_public_id_for_did(state, agent_did).await;
    let source_display_name = agent_display_name_for_did(state, agent_did).await;
    let agent_envelope = task_agent_envelope(
        state,
        TaskAgentEnvelopeArgs {
            source_agent_id: agent_did.to_owned(),
            source_public_id,
            source_display_name,
            target_agent_id: Some(state.agent_did.clone()),
            source_node_id: local_node_id,
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

async fn publish_transition_lifecycle_notice(
    state: &ControlPlaneState,
    action: &str,
    mission: &CivilMission,
    body: &MissionClaimBody,
    request_agent_did: &str,
    publish_topic_notice: bool,
) -> anyhow::Result<Option<Value>> {
    match action {
        "claim" => publish_claim_approved_lifecycle_notice(
            state,
            mission,
            body,
            request_agent_did,
            publish_topic_notice,
        )
        .await
        .map(Some),
        "complete" => {
            // The publisher recorded this hand-in for a claimer on another node. The notice of
            // a hand-in is the claimer's to send and sign, and it is addressed to the publisher,
            // who already knows: none is sent.
            if !is_this_nodes_agent(state, request_agent_did) {
                return Ok(None);
            }
            let result = mission
                .completion_result
                .clone()
                .unwrap_or_else(|| mission.payload.clone());
            publish_mission_completed_lifecycle_notice(
                state,
                MissionCompletedLifecycleArgs {
                    task_id: body
                        .task_id
                        .clone()
                        .or_else(|| claim_route_string(body, "task_id"))
                        .unwrap_or_else(|| mission.mission_id.clone()),
                    mission_id: mission.mission_id.clone(),
                    mission_feed_key: body
                        .mission_feed_key
                        .clone()
                        .or_else(|| claim_route_string(body, "mission_feed_key"))
                        .unwrap_or_else(|| MISSION_FEED_KEY.to_owned()),
                    mission_scope_hint: body
                        .mission_scope_hint
                        .clone()
                        .or_else(|| claim_route_string(body, "mission_scope_hint"))
                        .unwrap_or_else(|| mission_task_scope_hint(&mission.mission_id)),
                    agent_did: request_agent_did.to_owned(),
                    result,
                    publisher_agent_did: Some(state.agent_did.clone()),
                    publisher_wattswarm_node_id: body
                        .publisher_wattswarm_node_id
                        .clone()
                        .or_else(|| claim_route_string(body, "publisher_wattswarm_node_id")),
                    execution_id: claim_route_string(body, "execution_id"),
                    publish_topic_notice,
                },
            )
            .await
            .map(Some)
        }
        _ => Ok(None),
    }
}

fn record_mission_transition_events(
    state: &ControlPlaneState,
    action: &str,
    auth: String,
    body: &MissionClaimBody,
    payload: &Value,
) {
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
        subject: Some(body.mission_id.clone()),
        capability: Some(format!("mission.{action}")),
        reason: Some(body.agent_did.clone()),
        duration_ms: None,
        details: Some(payload.clone()),
    });
}

/// A claim or a completion on a mission this node's agent published, sent to the gateway. A
/// claim approves the claimer (the agent claiming its own mission first asks, as any claimer
/// does); a completion records the result unless the claimer already handed it in.
async fn own_mission_transition(
    state: &ControlPlaneState,
    record: &Value,
    body: &MissionClaimBody,
    action: &str,
) -> Result<Value, Response> {
    let sent = if action == "claim" {
        if body.agent_did == state.identity.agent_did {
            let claimer_label = claimer_display_name(state, body).await;
            gateway_missions::command(
                state,
                &body.mission_id,
                MissionAction::RequestClaim,
                json!({"claimer_label": claimer_label}),
            )
            .await
            .map_err(IntoResponse::into_response)?;
        }
        gateway_missions::command(
            state,
            &body.mission_id,
            MissionAction::ApproveClaim,
            json!({"claimer": body.agent_did}),
        )
        .await
    } else {
        if gateway_missions::status(record) == "completed" {
            return Ok(record.clone());
        }
        gateway_missions::command(
            state,
            &body.mission_id,
            MissionAction::Deliver,
            json!({"result": body.result}),
        )
        .await
    };
    sent.map_err(IntoResponse::into_response)
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
    let record = match mission_record(&state, &body.mission_id).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    if !is_own(&state, &record) {
        return if action == "claim" {
            claim_network_mission(state, headers, auth, body, record).await
        } else {
            complete_network_mission(state, auth, body, record).await
        };
    }
    let record = match own_mission_transition(&state, &record, &body, action).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let mission = match readable_mission(&record) {
        Ok(mission) => mission,
        Err(response) => return response,
    };
    let in_commit = agent_commit_context_from_headers(&headers).is_some();
    // A claimer on another node sent its own claim; this node cannot sign one in its name.
    if action == "claim"
        && !in_commit
        && is_this_nodes_agent(&state, &request_agent_did)
        && let Err(error) =
            dispatch_local_mission_transition_to_swarm(&state, &mission, &request_agent_did).await
    {
        return internal_error(&error);
    }

    let mut payload = mission_payload(&state, &record).await;
    insert_mission_transition_actor_projection(&mut payload, action, &request_agent_did, &body);
    record_mission_transition_events(&state, action, auth, &body, &payload);

    let mission_lifecycle_notice = match publish_transition_lifecycle_notice(
        &state,
        action,
        &mission,
        &body,
        &request_agent_did,
        !in_commit,
    )
    .await
    {
        Ok(notice) => notice,
        Err(error) => return internal_error(&error),
    };

    let mut response_json = payload.clone();
    if let Some(notice) = mission_lifecycle_notice
        && let Some(object) = response_json.as_object_mut()
    {
        object.insert("mission_lifecycle_notice".to_string(), notice);
    }
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

#[allow(clippy::too_many_lines)]
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
    let record = match mission_record(&state, &body.mission_id).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    if gateway_missions::status(&record) == "settled" {
        return resend_settled_notice(&state, &headers, &body, &record).await;
    }
    let mission = match readable_mission(&record) {
        Ok(mission) => mission,
        Err(response) => return response,
    };
    let swarm_finalize =
        match finalize_mission_task_before_settle(&state, &body, &headers, &mission).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let settled =
        match gateway_missions::command(&state, &body.mission_id, MissionAction::Settle, json!({}))
            .await
        {
            Ok(settled) => settled,
            Err(error) => return error.into_response(),
        };
    let mission = match readable_mission(&settled) {
        Ok(mission) => mission,
        Err(response) => return response,
    };

    let mut payload = mission_payload(&state, &settled).await;
    insert_mission_settle_actor_projection_fallbacks(&mut payload, &mission, &body);
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
        subject: Some(body.mission_id.clone()),
        capability: Some("mission.settle".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    let mission_lifecycle_notice = match publish_settled_lifecycle_notice(
        &state,
        &mission,
        &body,
        agent_commit_context_from_headers(&headers).is_none(),
    )
    .await
    {
        Ok(notice) => notice,
        Err(error) => return settle_notice_failed(&mission.mission_id, &error),
    };
    let mut response_json = payload.clone();
    if let Some(swarm_finalize) = swarm_finalize
        && let Some(object) = response_json.as_object_mut()
    {
        object.insert("swarm_finalize".to_string(), swarm_finalize);
    }
    if let Some(object) = response_json.as_object_mut() {
        object.insert(
            "mission_lifecycle_notice".to_string(),
            mission_lifecycle_notice,
        );
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

/// The answer when a mission is settled in the gateway but its notice to the claimer did not go
/// out. It is an error status, so a caller that retries on failure (wattswarm's commit delivery)
/// tries again, and it says the payment is done, so nobody takes it for an unpaid mission. The
/// retry only sends the notice.
fn settle_notice_failed(mission_id: &str, error: &anyhow::Error) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": format!(
                "the mission is settled and its reward is paid, but the notice to the claimer \
                 was not delivered: {error}. Send settle again to retry the notice; nothing is \
                 paid twice."
            ),
            "code": "settle_notice_not_delivered",
            "mission_id": mission_id,
            "status": "settled",
            "mission_lifecycle_notice": {"kind": "mission_settled", "sent": false},
        })),
    )
        .into_response()
}

/// A settle sent again for a mission the gateway already settled pays nothing twice, but its
/// notice to the claimer may not have gone out the first time: the publisher sends it again.
async fn resend_settled_notice(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    body: &MissionSettleBody,
    record: &Value,
) -> Response {
    let mut payload = mission_payload(state, record).await;
    if !gateway_missions::is_own(state, record) {
        return Json(payload).into_response();
    }
    let mission = match readable_mission(record) {
        Ok(mission) => mission,
        Err(response) => return response,
    };
    let notice = match publish_settled_lifecycle_notice(
        state,
        &mission,
        body,
        agent_commit_context_from_headers(headers).is_none(),
    )
    .await
    {
        Ok(notice) => notice,
        Err(error) => return settle_notice_failed(&mission.mission_id, &error),
    };
    if let Some(object) = payload.as_object_mut() {
        object.insert("mission_lifecycle_notice".to_string(), notice);
    }
    Json(payload).into_response()
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
        MissionDomain, MissionPublisherKind, MissionReward, MissionScope,
    };

    fn sample_mission() -> CivilMission {
        CivilMission::draft(
            "Route cargo",
            "Move supplies to the outpost",
            "captain-alpha",
            MissionPublisherKind::Player,
            MissionDomain::Trade,
            MissionScope::RealWorld,
            Some("subnet-1".to_owned()),
            Some("zone-2".to_owned()),
            None,
            None,
            Some(MissionReward {
                agent_watt: 10,
                reputation: 2,
                capacity: 1,
            }),
            json!({"cargo": "water"}),
            None,
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
        assert_eq!(inputs["title"].as_str(), Some("Route cargo"));
        assert_eq!(
            inputs["description"].as_str(),
            Some("Move supplies to the outpost")
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
        assert_eq!(inputs["scope"].as_str(), Some("real_world"));
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
        assert_eq!(command.summary["scope"].as_str(), Some("real_world"));
    }
}
