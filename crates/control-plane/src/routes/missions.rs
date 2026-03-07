use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error};
use crate::state::{
    ControlPlaneState, MissionClaimBody, MissionPublishBody, MissionSettleBody, MissionsQuery,
    StreamEvent,
};
use wattetheria_kernel::audit::AuditEntry;

pub(crate) async fn mission_list(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<MissionsQuery>,
) -> Response {
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
    if let Err(error) = board.persist(&state.mission_board_state_path) {
        return internal_error(&error);
    }
    drop(board);

    let payload = serde_json::to_value(&mission).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: "mission.published".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state
        .event_log
        .append_signed("MISSION_PUBLISHED", payload.clone(), &state.identity);

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

    (StatusCode::CREATED, Json(mission)).into_response()
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
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let mut board = state.mission_board.lock().await;
    let mission = match action {
        "claim" => board.claim(&body.mission_id, &body.agent_id),
        "complete" => board.complete(&body.mission_id, &body.agent_id),
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
    if let Err(error) = board.persist(&state.mission_board_state_path) {
        return internal_error(&error);
    }
    drop(board);

    let payload = serde_json::to_value(&mission).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: format!("mission.{action}ed"),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.event_log.append_signed(
        format!("MISSION_{}", action.to_uppercase()),
        payload.clone(),
        &state.identity,
    );

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mission".to_string(),
        action: format!("mission.{action}"),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.mission_id),
        capability: Some(format!("mission.{action}")),
        reason: Some(body.agent_id),
        duration_ms: None,
        details: Some(payload.clone()),
    });

    Json(mission).into_response()
}

pub(crate) async fn mission_settle(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MissionSettleBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

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
        if let Err(error) = board.persist(&state.mission_board_state_path) {
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
        if let Err(error) = governance.persist(&state.governance_state_path) {
            return internal_error(&error);
        }
    }

    let payload = serde_json::to_value(&mission).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: "mission.settled".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state
        .event_log
        .append_signed("MISSION_SETTLED", payload.clone(), &state.identity);

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

    Json(mission).into_response()
}
