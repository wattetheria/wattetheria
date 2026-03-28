use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};

use crate::auth::authorize;
use crate::state::{
    ControlPlaneState, PolicyApproveBody, PolicyCheckBody, PolicyRevokeBody, StreamEvent,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::policy_engine::{CapabilityRequest, DecisionKind};

pub(crate) async fn policy_check(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<PolicyCheckBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let mut engine = state.policy_engine.lock().await;
    let decision = engine.evaluate(CapabilityRequest {
        request_id: String::new(),
        timestamp: 0,
        subject: body.subject.clone(),
        trust: body.trust,
        capability: body.capability.clone(),
        reason: body.reason.clone(),
        input_digest: body.input_digest.clone(),
    });
    if let Err(error) = state
        .local_db
        .save_domain(wattetheria_kernel::local_db::domain::POLICY, engine.state())
    {
        tracing::warn!("persist policy state: {error:#}");
    }
    drop(engine);

    let status = if decision.decision == DecisionKind::Allowed {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "policy".to_string(),
        action: "policy.check".to_string(),
        status: match decision.decision {
            DecisionKind::Allowed => "allowed".to_string(),
            DecisionKind::DeniedPendingApproval => "pending".to_string(),
        },
        actor: Some(auth),
        subject: Some(body.subject),
        capability: Some(body.capability),
        reason: Some(decision.reason.clone()),
        duration_ms: None,
        details: Some(json!({"request_id": decision.request_id})),
    });

    let _ = state.stream_tx.send(StreamEvent {
        kind: "policy.decision".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: serde_json::to_value(&decision).unwrap_or(Value::Null),
    });

    (status, Json(decision)).into_response()
}

pub(crate) async fn policy_pending(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let pending = state.policy_engine.lock().await.list_pending();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "policy".to_string(),
        action: "policy.pending".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": pending.len()})),
    });

    Json(pending).into_response()
}

pub(crate) async fn policy_approve(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<PolicyApproveBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let mut engine = state.policy_engine.lock().await;
    let grant = match engine.approve_pending(&body.request_id, &body.approved_by, body.scope) {
        Ok(grant) => grant,
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
        .save_domain(wattetheria_kernel::local_db::domain::POLICY, engine.state())
    {
        tracing::warn!("persist policy state: {error:#}");
    }

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "policy".to_string(),
        action: "policy.approve".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(grant.subject_pattern.clone()),
        capability: Some(grant.capability_pattern.clone()),
        reason: None,
        duration_ms: None,
        details: Some(json!({"grant_id": grant.grant_id, "scope": grant.scope})),
    });

    Json(grant).into_response()
}

pub(crate) async fn policy_grants(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let grants = state.policy_engine.lock().await.list_grants();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "policy".to_string(),
        action: "policy.grants".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": grants.len()})),
    });

    Json(grants).into_response()
}

pub(crate) async fn policy_revoke(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<PolicyRevokeBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let mut engine = state.policy_engine.lock().await;
    if let Err(error) = engine.revoke_grant(&body.grant_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": error.to_string()})),
        )
            .into_response();
    }
    if let Err(error) = state
        .local_db
        .save_domain(wattetheria_kernel::local_db::domain::POLICY, engine.state())
    {
        tracing::warn!("persist policy state: {error:#}");
    }

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "policy".to_string(),
        action: "policy.revoke".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"grant_id": body.grant_id})),
    });

    Json(json!({"revoked": body.grant_id})).into_response()
}
