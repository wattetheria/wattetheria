use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error};
use crate::state::{
    ControlPlaneState, GovernanceProposalsQuery, ProposalCreateBody, ProposalFinalizeBody,
    ProposalVoteBody, StreamEvent,
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
