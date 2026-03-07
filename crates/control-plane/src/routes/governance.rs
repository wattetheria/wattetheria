use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error};
use crate::state::{
    ControlPlaneState, GovernanceCustodyBody, GovernanceCustodyReleaseBody,
    GovernanceProposalsQuery, GovernanceRecallBody, GovernanceStabilityBody,
    GovernanceSuccessorBody, GovernanceTakeoverBody, GovernanceTreasuryBody, ProposalCreateBody,
    ProposalFinalizeBody, ProposalVoteBody, StreamEvent,
};
use wattetheria_kernel::audit::AuditEntry;

pub(crate) async fn governance_planets(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let planets = state.governance_engine.lock().await.list_planets();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "governance".to_string(),
        action: "governance.planets".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": planets.len()})),
    });

    Json(planets).into_response()
}

pub(crate) async fn governance_proposals(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<GovernanceProposalsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let proposals = state
        .governance_engine
        .lock()
        .await
        .list_proposals(query.subnet_id.as_deref());

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "governance".to_string(),
        action: "governance.proposals".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: query.subnet_id,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": proposals.len()})),
    });

    Json(proposals).into_response()
}

pub(crate) async fn governance_create_proposal(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ProposalCreateBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let mut engine = state.governance_engine.lock().await;
    let proposal = match engine.create_proposal(
        &body.subnet_id,
        &body.kind,
        body.payload.clone(),
        &body.created_by,
    ) {
        Ok(proposal) => proposal,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    if let Err(error) = engine.persist(&state.governance_state_path) {
        return internal_error(&error);
    }
    drop(engine);

    let payload = serde_json::to_value(&proposal).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: "governance.proposal.created".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "governance".to_string(),
        action: "governance.proposal.create".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.subnet_id),
        capability: Some(body.kind),
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    (StatusCode::CREATED, Json(proposal)).into_response()
}

pub(crate) async fn governance_vote_proposal(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ProposalVoteBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let mut engine = state.governance_engine.lock().await;
    if let Err(error) = engine.vote_proposal(&body.proposal_id, &body.voter, body.approve) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": error.to_string()})),
        )
            .into_response();
    }

    let proposal = engine
        .list_proposals(None)
        .into_iter()
        .find(|proposal| proposal.proposal_id == body.proposal_id);
    let Some(proposal) = proposal else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "proposal disappeared after vote"})),
        )
            .into_response();
    };
    if let Err(error) = engine.persist(&state.governance_state_path) {
        return internal_error(&error);
    }
    drop(engine);

    let payload = serde_json::to_value(&proposal).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: "governance.proposal.voted".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "governance".to_string(),
        action: "governance.proposal.vote".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.proposal_id),
        capability: None,
        reason: Some(format!("approve={}", body.approve)),
        duration_ms: None,
        details: Some(payload),
    });

    Json(proposal).into_response()
}

pub(crate) async fn governance_finalize_proposal(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ProposalFinalizeBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let mut engine = state.governance_engine.lock().await;
    let proposal = match engine.finalize_proposal(&body.proposal_id, body.min_votes_for) {
        Ok(proposal) => proposal,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    if let Err(error) = engine.persist(&state.governance_state_path) {
        return internal_error(&error);
    }
    drop(engine);

    let payload = serde_json::to_value(&proposal).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: "governance.proposal.finalized".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "governance".to_string(),
        action: "governance.proposal.finalize".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.proposal_id),
        capability: None,
        reason: Some(format!("min_votes_for={}", body.min_votes_for)),
        duration_ms: None,
        details: Some(payload.clone()),
    });

    Json(proposal).into_response()
}

pub(crate) async fn governance_fund_treasury(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GovernanceTreasuryBody>,
) -> Response {
    mutate_planet(
        state,
        headers,
        "governance.treasury.fund",
        Some("governance.treasury.fund".to_string()),
        Some(body.subnet_id.clone()),
        move |engine| engine.fund_treasury(&body.subnet_id, body.amount_watt),
    )
    .await
}

pub(crate) async fn governance_spend_treasury(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GovernanceTreasuryBody>,
) -> Response {
    let reason = body.reason.unwrap_or_else(|| "unspecified".to_string());
    mutate_planet(
        state,
        headers,
        "governance.treasury.spend",
        Some("governance.treasury.spend".to_string()),
        Some(body.subnet_id.clone()),
        move |engine| engine.spend_treasury(&body.subnet_id, body.amount_watt, &reason),
    )
    .await
}

pub(crate) async fn governance_adjust_stability(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GovernanceStabilityBody>,
) -> Response {
    mutate_planet(
        state,
        headers,
        "governance.stability.adjust",
        Some("governance.stability.adjust".to_string()),
        Some(body.subnet_id.clone()),
        move |engine| engine.adjust_stability(&body.subnet_id, body.delta),
    )
    .await
}

pub(crate) async fn governance_start_recall(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GovernanceRecallBody>,
) -> Response {
    mutate_planet(
        state,
        headers,
        "governance.recall.start",
        Some("governance.recall.start".to_string()),
        Some(body.subnet_id.clone()),
        move |engine| {
            engine.start_recall(
                &body.subnet_id,
                &body.initiated_by,
                &body.reason,
                body.threshold,
            )
        },
    )
    .await
}

pub(crate) async fn governance_resolve_recall(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GovernanceSuccessorBody>,
) -> Response {
    mutate_planet(
        state,
        headers,
        "governance.recall.resolve",
        Some("governance.recall.resolve".to_string()),
        Some(body.subnet_id.clone()),
        move |engine| engine.resolve_recall(&body.subnet_id, &body.successor, body.min_bond),
    )
    .await
}

pub(crate) async fn governance_enter_custody(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GovernanceCustodyBody>,
) -> Response {
    mutate_planet(
        state,
        headers,
        "governance.custody.enter",
        Some("governance.custody.enter".to_string()),
        Some(body.subnet_id.clone()),
        move |engine| engine.enter_custody(&body.subnet_id, &body.reason, body.managed_by),
    )
    .await
}

pub(crate) async fn governance_release_custody(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GovernanceCustodyReleaseBody>,
) -> Response {
    mutate_planet(
        state,
        headers,
        "governance.custody.release",
        Some("governance.custody.release".to_string()),
        Some(body.subnet_id.clone()),
        move |engine| {
            engine.release_custody(&body.subnet_id, body.successor.as_deref(), body.min_bond)
        },
    )
    .await
}

pub(crate) async fn governance_hostile_takeover(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GovernanceTakeoverBody>,
) -> Response {
    mutate_planet(
        state,
        headers,
        "governance.takeover.hostile",
        Some("governance.takeover.hostile".to_string()),
        Some(body.subnet_id.clone()),
        move |engine| {
            engine.hostile_takeover(
                &body.subnet_id,
                &body.challenger,
                body.min_bond,
                &body.reason,
            )
        },
    )
    .await
}

async fn mutate_planet<F>(
    state: ControlPlaneState,
    headers: HeaderMap,
    action: &str,
    capability: Option<String>,
    subject: Option<String>,
    mutator: F,
) -> Response
where
    F: FnOnce(
        &mut wattetheria_kernel::governance::GovernanceEngine,
    ) -> anyhow::Result<wattetheria_kernel::governance::SubnetPlanet>,
{
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let mut engine = state.governance_engine.lock().await;
    let planet = match mutator(&mut engine) {
        Ok(planet) => planet,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    if let Err(error) = engine.persist(&state.governance_state_path) {
        return internal_error(&error);
    }
    drop(engine);

    let payload = serde_json::to_value(&planet).unwrap_or(Value::Null);
    let _ = state.stream_tx.send(StreamEvent {
        kind: action.to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.event_log.append_signed(
        action.to_uppercase().replace('.', "_"),
        payload.clone(),
        &state.identity,
    );
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "governance".to_string(),
        action: action.to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject,
        capability,
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    Json(planet).into_response()
}
