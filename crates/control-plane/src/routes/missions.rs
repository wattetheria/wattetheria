use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::{authorize, internal_error};
use crate::routes::reward_view::refresh_known_wallet_balances;
use crate::state::{
    ControlPlaneState, MissionClaimBody, MissionPublishBody, MissionSettleBody, MissionsQuery,
    StreamEvent, agent_commit_context_from_headers,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::missions::CivilMission;
use wattetheria_kernel::swarm_bridge::{
    SwarmTaskAnnounceCommand, SwarmTaskClaimCommand, SwarmTaskProposeCandidateCommand,
};
use wattswarm_protocol::types::{ClaimRole, TaskContract};

struct CommitResponseArgs<'a> {
    action_type: &'a str,
    target_id: Option<String>,
    actor_agent_did: Option<String>,
    request_json: &'a Value,
    response_json: &'a Value,
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

fn mission_candidate_id(mission_id: &str, agent_did: &str) -> String {
    format!("wattetheria-candidate-{mission_id}-{agent_did}")
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

fn mission_task_contract(mut contract: TaskContract, mission: &CivilMission) -> TaskContract {
    contract.task_id.clone_from(&mission.mission_id);
    "wattetheria.mission".clone_into(&mut contract.task_type);
    contract.inputs = json!({
        "kind": "wattetheria_mission",
        "mission_id": mission.mission_id,
        "publisher": mission.publisher,
        "publisher_kind": mission.publisher_kind,
        "domain": mission.domain,
        "reward": mission.reward,
        "required_role": mission.required_role,
        "required_faction": mission.required_faction,
        "subnet_id": mission.subnet_id,
        "zone_id": mission.zone_id,
        "payload": mission.payload,
    });
    contract.output_schema = mission_task_output_schema();
    contract
}

fn mission_announce_command(mission: &CivilMission) -> SwarmTaskAnnounceCommand {
    SwarmTaskAnnounceCommand {
        task_id: mission.mission_id.clone(),
        announcement_id: None,
        feed_key: "wattetheria.missions".to_owned(),
        scope_hint: "global".to_owned(),
        summary: json!({
            "kind": "wattetheria_mission",
            "mission_id": mission.mission_id,
            "title": mission.title,
            "description": mission.description,
            "domain": mission.domain,
            "reward": mission.reward,
            "publisher": mission.publisher,
        }),
        detail_ref: None,
    }
}

fn mission_complete_command(
    mission: &CivilMission,
    agent_did: &str,
) -> SwarmTaskProposeCandidateCommand {
    let execution_id = mission_execution_id(&mission.mission_id, agent_did);
    SwarmTaskProposeCandidateCommand {
        task_id: mission.mission_id.clone(),
        execution_id: execution_id.clone(),
        candidate_id: mission_candidate_id(&mission.mission_id, agent_did),
        output: json!({
            "kind": "wattetheria_mission_result",
            "mission_id": mission.mission_id,
            "agent_did": agent_did,
            "result": mission.payload,
        }),
        evidence_inline: Vec::new(),
        evidence_refs: Vec::new(),
    }
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

pub(crate) async fn mission_publish(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MissionPublishBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
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
        body.payload,
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
    if agent_commit_context_from_headers(&headers).is_none() {
        let contract = match state
            .swarm_bridge
            .sample_task_contract(&mission.mission_id)
            .await
        {
            Ok(contract) => mission_task_contract(contract, &mission),
            Err(error) => return internal_error(&error),
        };
        if let Err(error) = state.swarm_bridge.submit_task(contract).await {
            return internal_error(&error);
        }
        if let Err(error) = state
            .swarm_bridge
            .announce_task(mission_announce_command(&mission))
            .await
        {
            return internal_error(&error);
        }
    }

    let payload = serde_json::to_value(&mission).unwrap_or(Value::Null);
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

    let response_json = serde_json::to_value(&mission).unwrap_or(Value::Null);
    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: "missions.publish",
            target_id: Some(mission.mission_id.clone()),
            actor_agent_did: None,
            request_json: &json!({"title": body.title, "publisher": body.publisher}),
            response_json: &response_json,
        },
    ) {
        return internal_error(&error);
    }
    (StatusCode::CREATED, Json(response_json)).into_response()
}

pub(crate) async fn mission_claim(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MissionClaimBody>,
) -> Response {
    transition_mission(state, headers, body, "claim").await
}

pub(crate) async fn mission_complete(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MissionClaimBody>,
) -> Response {
    transition_mission(state, headers, body, "complete").await
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
    let mission = match action {
        "claim" => board.claim(&body.mission_id, &body.agent_did),
        "complete" => board.complete(&body.mission_id, &body.agent_did),
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
    if agent_commit_context_from_headers(&headers).is_none() {
        let bridge_result = match action {
            "claim" => {
                state
                    .swarm_bridge
                    .claim_task(SwarmTaskClaimCommand {
                        task_id: mission.mission_id.clone(),
                        role: ClaimRole::Propose,
                        execution_id: mission_execution_id(&mission.mission_id, &request_agent_did),
                        lease_ms: None,
                    })
                    .await
            }
            "complete" => {
                state
                    .swarm_bridge
                    .propose_task_candidate(mission_complete_command(&mission, &request_agent_did))
                    .await
            }
            _ => unreachable!("unsupported mission transition"),
        };
        if let Err(error) = bridge_result {
            return internal_error(&error);
        }
    }

    let payload = serde_json::to_value(&mission).unwrap_or(Value::Null);
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

    let response_json = serde_json::to_value(&mission).unwrap_or(Value::Null);
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

    let mission = {
        let mut board = state.mission_board.lock().await;
        let mission = match board.settle(&body.mission_id) {
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
        mission
    };

    if let Some(subnet_id) = mission.subnet_id.clone()
        && mission.reward.treasury_share_watt > 0
    {
        let mut governance = state.governance_engine.lock().await;
        if let Err(error) = governance.fund_treasury(&subnet_id, mission.reward.treasury_share_watt)
        {
            return internal_error(&error);
        }
        if let Err(error) = state.local_db.save_domain(
            wattetheria_kernel::local_db::domain::GOVERNANCE,
            &*governance,
        ) {
            return internal_error(&error);
        }
    }
    if let Err(error) = refresh_known_wallet_balances(&state).await {
        return internal_error(&error);
    }

    let payload = serde_json::to_value(&mission).unwrap_or(Value::Null);
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

    let response_json = serde_json::to_value(&mission).unwrap_or(Value::Null);
    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: "missions.settle",
            target_id: Some(mission.mission_id.clone()),
            actor_agent_did: None,
            request_json: &json!({"mission_id": request_mission_id}),
            response_json: &response_json,
        },
    ) {
        return internal_error(&error);
    }
    Json(response_json).into_response()
}
