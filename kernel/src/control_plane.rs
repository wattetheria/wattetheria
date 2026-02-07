//! Local control plane API (HTTP + WebSocket) with token auth, rate limits, and audit logs.

use anyhow::{Context, Result, bail};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

use crate::audit::{AuditEntry, AuditLog};
use crate::capabilities::TrustLevel;
use crate::event_log::EventLog;
use crate::governance::GovernanceEngine;
use crate::identity::Identity;
use crate::mailbox::CrossSubnetMailbox;
use crate::night_shift::generate_night_shift_report;
use crate::policy_engine::{CapabilityRequest, DecisionKind, GrantScope, PolicyEngine};
use crate::task_engine::TaskEngine;
use crate::types::{Reward, Sla, VerificationMode, VerificationSpec};

#[derive(Debug)]
pub struct RateLimiter {
    max_requests: usize,
    window_sec: i64,
    buckets: Mutex<BTreeMap<String, Vec<i64>>>,
}

impl RateLimiter {
    #[must_use]
    pub fn new(max_requests: usize, window_sec: i64) -> Self {
        Self {
            max_requests,
            window_sec,
            buckets: Mutex::new(BTreeMap::new()),
        }
    }

    pub async fn allow(&self, key: &str) -> bool {
        let mut buckets = self.buckets.lock().await;
        let now = Utc::now().timestamp();
        let window_start = now - self.window_sec;
        let entries = buckets.entry(key.to_string()).or_default();
        entries.retain(|timestamp| *timestamp >= window_start);
        if entries.len() >= self.max_requests {
            return false;
        }
        entries.push(now);
        true
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamEvent {
    pub kind: String,
    pub timestamp: i64,
    pub payload: Value,
}

#[derive(Clone)]
pub struct ControlPlaneState {
    pub agent_id: String,
    pub identity: Identity,
    pub started_at: i64,
    pub auth_token: String,
    pub event_log: EventLog,
    pub task_engine: Arc<Mutex<TaskEngine>>,
    pub task_ledger_path: PathBuf,
    pub governance_engine: Arc<Mutex<GovernanceEngine>>,
    pub governance_state_path: PathBuf,
    pub policy_engine: Arc<Mutex<PolicyEngine>>,
    pub mailbox: Arc<Mutex<CrossSubnetMailbox>>,
    pub mailbox_state_path: PathBuf,
    pub audit_log: AuditLog,
    pub rate_limiter: Arc<RateLimiter>,
    pub stream_tx: broadcast::Sender<StreamEvent>,
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    since: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct EventsExportQuery {
    since: Option<i64>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct NightShiftQuery {
    hours: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct AuthQuery {
    token: String,
}

#[derive(Debug, Deserialize)]
pub struct ActionRequest {
    pub action: String,
}

#[derive(Debug, Deserialize)]
pub struct PolicyCheckBody {
    pub subject: String,
    pub trust: TrustLevel,
    pub capability: String,
    pub reason: Option<String>,
    pub input_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PolicyApproveBody {
    pub request_id: String,
    pub approved_by: String,
    pub scope: GrantScope,
}

#[derive(Debug, Deserialize)]
pub struct PolicyRevokeBody {
    pub grant_id: String,
}

#[derive(Debug, Deserialize)]
struct GovernanceProposalsQuery {
    subnet_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProposalCreateBody {
    pub subnet_id: String,
    pub kind: String,
    pub payload: Value,
    pub created_by: String,
}

#[derive(Debug, Deserialize)]
pub struct ProposalVoteBody {
    pub proposal_id: String,
    pub voter: String,
    pub approve: bool,
}

#[derive(Debug, Deserialize)]
pub struct ProposalFinalizeBody {
    pub proposal_id: String,
    pub min_votes_for: usize,
}

#[derive(Debug, Deserialize)]
pub struct MailboxSendBody {
    pub to_agent: String,
    pub from_subnet: String,
    pub to_subnet: String,
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
pub struct MailboxFetchQuery {
    pub subnet_id: String,
}

#[derive(Debug, Deserialize)]
pub struct MailboxAckBody {
    pub subnet_id: String,
    pub message_id: String,
}

pub fn app(state: ControlPlaneState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/state", get(state_view))
        .route("/v1/events", get(events))
        .route("/v1/events/export", get(events_export))
        .route("/v1/night-shift", get(night_shift))
        .route("/v1/actions", post(actions))
        .route("/v1/governance/planets", get(governance_planets))
        .route(
            "/v1/governance/proposals",
            get(governance_proposals).post(governance_create_proposal),
        )
        .route(
            "/v1/governance/proposals/vote",
            post(governance_vote_proposal),
        )
        .route(
            "/v1/governance/proposals/finalize",
            post(governance_finalize_proposal),
        )
        .route(
            "/v1/mailbox/messages",
            get(mailbox_fetch).post(mailbox_send),
        )
        .route("/v1/mailbox/ack", post(mailbox_ack))
        .route("/v1/policy/check", post(policy_check))
        .route("/v1/policy/pending", get(policy_pending))
        .route("/v1/policy/approve", post(policy_approve))
        .route("/v1/policy/revoke", post(policy_revoke))
        .route("/v1/policy/grants", get(policy_grants))
        .route("/v1/audit", get(audit_recent))
        .route("/v1/stream", get(stream))
        .with_state(state)
}

pub async fn serve_control_plane(state: ControlPlaneState, bind: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind control plane on {bind}"))?;
    axum::serve(listener, app(state))
        .await
        .context("serve control plane")
}

async fn health(State(state): State<ControlPlaneState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "timestamp": Utc::now().timestamp(),
        "agent_id": state.agent_id,
        "uptime_sec": Utc::now().timestamp() - state.started_at,
    }))
}

async fn state_view(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    let auth = match authorize(&state, &headers, "state.query").await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let events = match state.event_log.get_all() {
        Ok(rows) => rows,
        Err(error) => return internal_error(&error),
    };
    let pending_count = state.policy_engine.lock().await.list_pending().len();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "state.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"events": events.len(), "pending": pending_count})),
    });

    Json(json!({
        "agent_id": state.agent_id,
        "events": events.len(),
        "pending_policy_requests": pending_count,
        "uptime_sec": Utc::now().timestamp() - state.started_at,
    }))
    .into_response()
}

async fn events(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers, "events.query").await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let rows = if let Some(since) = query.since {
        match state.event_log.since(since) {
            Ok(rows) => rows,
            Err(error) => return internal_error(&error),
        }
    } else {
        match state.event_log.get_all() {
            Ok(rows) => rows,
            Err(error) => return internal_error(&error),
        }
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "events.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": rows.len()})),
    });

    Json(rows).into_response()
}

async fn events_export(
    State(state): State<ControlPlaneState>,
    Query(query): Query<EventsExportQuery>,
) -> Response {
    let mut rows = if let Some(since) = query.since {
        match state.event_log.since(since) {
            Ok(rows) => rows,
            Err(error) => return internal_error(&error),
        }
    } else {
        match state.event_log.get_all() {
            Ok(rows) => rows,
            Err(error) => return internal_error(&error),
        }
    };

    if let Some(limit) = query.limit {
        let cap = limit.max(1);
        if rows.len() > cap {
            rows = rows.split_off(rows.len() - cap);
        }
    }

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "recovery".to_string(),
        action: "events.export".to_string(),
        status: "ok".to_string(),
        actor: Some("public".to_string()),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": rows.len()})),
    });

    Json(json!({
        "events": rows,
        "count": rows.len(),
        "generated_at": Utc::now().timestamp(),
    }))
    .into_response()
}

async fn night_shift(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<NightShiftQuery>,
) -> Response {
    let auth = match authorize(&state, &headers, "night_shift.query").await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let hours = query.hours.unwrap_or(12).max(1);
    let now = Utc::now().timestamp();
    let events = match state.event_log.get_all() {
        Ok(rows) => rows,
        Err(error) => return internal_error(&error),
    };
    let report = generate_night_shift_report(&events, now - hours * 3600, now);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "night_shift.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"hours": hours})),
    });

    Json(report).into_response()
}

async fn actions(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(request): Json<ActionRequest>,
) -> Response {
    let auth = match authorize(&state, &headers, "action.exec").await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let result = match request.action.as_str() {
        "task.run_demo_market" => match run_demo_market_task(&state).await {
            Ok(payload) => payload,
            Err(error) => return internal_error(&error),
        },
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("unsupported action: {}", request.action)})),
            )
                .into_response();
        }
    };

    let _ = state.stream_tx.send(StreamEvent {
        kind: "action.result".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: result.clone(),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "action.exec".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_id.clone()),
        capability: Some("task.run_demo_market".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(result.clone()),
    });

    Json(result).into_response()
}

async fn governance_planets(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers, "governance.planets").await {
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

async fn governance_proposals(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<GovernanceProposalsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers, "governance.proposals").await {
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

async fn governance_create_proposal(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ProposalCreateBody>,
) -> Response {
    let auth = match authorize(&state, &headers, "governance.proposal.create").await {
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

async fn governance_vote_proposal(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ProposalVoteBody>,
) -> Response {
    let auth = match authorize(&state, &headers, "governance.proposal.vote").await {
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

async fn governance_finalize_proposal(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ProposalFinalizeBody>,
) -> Response {
    let auth = match authorize(&state, &headers, "governance.proposal.finalize").await {
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

async fn mailbox_send(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MailboxSendBody>,
) -> Response {
    let auth = match authorize(&state, &headers, "mailbox.send").await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let mut mailbox = state.mailbox.lock().await;
    let message = match mailbox.enqueue_signed(
        &state.identity,
        &body.to_agent,
        &body.from_subnet,
        &body.to_subnet,
        body.payload,
    ) {
        Ok(message) => message,
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = mailbox.persist(&state.mailbox_state_path) {
        return internal_error(&error);
    }
    drop(mailbox);

    let payload = json!({
        "message_id": message.message_id,
        "from_subnet": message.from_subnet,
        "to_subnet": message.to_subnet,
        "to_agent": message.to_agent,
    });
    if let Err(error) =
        state
            .event_log
            .append_signed("MAILBOX_MESSAGE_ENQUEUED", payload.clone(), &state.identity)
    {
        return internal_error(&error);
    }

    let _ = state.stream_tx.send(StreamEvent {
        kind: "mailbox.sent".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mailbox".to_string(),
        action: "mailbox.send".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.to_agent),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    (StatusCode::CREATED, Json(message)).into_response()
}

async fn mailbox_fetch(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<MailboxFetchQuery>,
) -> Response {
    let auth = match authorize(&state, &headers, "mailbox.fetch").await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let messages = state
        .mailbox
        .lock()
        .await
        .fetch_for_subnet(&query.subnet_id);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mailbox".to_string(),
        action: "mailbox.fetch".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(query.subnet_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": messages.len()})),
    });

    Json(messages).into_response()
}

async fn mailbox_ack(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MailboxAckBody>,
) -> Response {
    let auth = match authorize(&state, &headers, "mailbox.ack").await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let mut mailbox = state.mailbox.lock().await;
    if let Err(error) = mailbox.ack(&body.subnet_id, &body.message_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": error.to_string()})),
        )
            .into_response();
    }
    if let Err(error) = mailbox.persist(&state.mailbox_state_path) {
        return internal_error(&error);
    }
    drop(mailbox);

    let payload = json!({"subnet_id": body.subnet_id, "message_id": body.message_id});
    if let Err(error) =
        state
            .event_log
            .append_signed("MAILBOX_MESSAGE_ACKED", payload.clone(), &state.identity)
    {
        return internal_error(&error);
    }

    let _ = state.stream_tx.send(StreamEvent {
        kind: "mailbox.acked".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "mailbox".to_string(),
        action: "mailbox.ack".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    Json(json!({"acked": true})).into_response()
}

async fn policy_check(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<PolicyCheckBody>,
) -> Response {
    let auth = match authorize(&state, &headers, "policy.check").await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let mut engine = state.policy_engine.lock().await;
    let decision = match engine.evaluate(CapabilityRequest {
        request_id: String::new(),
        timestamp: 0,
        subject: body.subject.clone(),
        trust: body.trust,
        capability: body.capability.clone(),
        reason: body.reason.clone(),
        input_digest: body.input_digest.clone(),
    }) {
        Ok(decision) => decision,
        Err(error) => return internal_error(&error),
    };
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

async fn policy_pending(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    let auth = match authorize(&state, &headers, "policy.pending").await {
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

async fn policy_approve(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<PolicyApproveBody>,
) -> Response {
    let auth = match authorize(&state, &headers, "policy.approve").await {
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

async fn policy_grants(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    let auth = match authorize(&state, &headers, "policy.grants").await {
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

async fn policy_revoke(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<PolicyRevokeBody>,
) -> Response {
    let auth = match authorize(&state, &headers, "policy.revoke").await {
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

async fn audit_recent(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Response {
    let auth = match authorize(&state, &headers, "audit.query").await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let limit = query.limit.unwrap_or(50).max(1);
    let rows = match state.audit_log.list_recent(limit) {
        Ok(rows) => rows,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "control".to_string(),
        action: "audit.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"limit": limit})),
    });

    Json(rows).into_response()
}

async fn stream(
    State(state): State<ControlPlaneState>,
    Query(query): Query<AuthQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    if query.token != state.auth_token {
        return unauthorized();
    }

    ws.on_upgrade(move |socket| handle_ws(socket, state))
        .into_response()
}

async fn handle_ws(mut socket: WebSocket, state: ControlPlaneState) {
    let mut receiver = state.stream_tx.subscribe();

    let hello = json!({
        "kind": "hello",
        "timestamp": Utc::now().timestamp(),
        "agent_id": state.agent_id,
    });

    if socket
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    while let Ok(event) = receiver.recv().await {
        let Ok(payload) = serde_json::to_string(&event) else {
            continue;
        };

        if socket.send(Message::Text(payload.into())).await.is_err() {
            break;
        }
    }
}

async fn run_demo_market_task(state: &ControlPlaneState) -> Result<Value> {
    let mut engine = state.task_engine.lock().await;
    let worker_id = state.agent_id.clone();

    let task = engine.publish_task(
        "market.match",
        "T0",
        json!({
            "buy_orders": [
                {"id":"c-buy-1", "price":120, "qty":5},
                {"id":"c-buy-2", "price":118, "qty":3}
            ],
            "sell_orders": [
                {"id":"c-sell-1", "price":110, "qty":2},
                {"id":"c-sell-2", "price":112, "qty":6}
            ]
        }),
        VerificationSpec {
            mode: VerificationMode::Deterministic,
            witnesses: None,
        },
        Reward {
            watt: 12,
            reputation: 3,
            capacity: 4,
        },
        Sla { timeout_sec: 120 },
    )?;

    engine.claim_task(&task.task_id, &worker_id)?;
    let result = engine.execute_task(&task.task_id)?;
    engine.submit_task_result(&task.task_id, &result, &worker_id)?;

    if !engine.verify_task(&task.task_id)? {
        bail!("demo task verification failed");
    }

    let ledger = engine.settle_task(&task.task_id)?;
    engine.persist_ledger(&state.task_ledger_path)?;
    Ok(json!({
        "task_id": task.task_id,
        "status": "SETTLED",
        "ledger": ledger,
        "result": result,
    }))
}

async fn authorize(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    _action: &str,
) -> std::result::Result<String, Response> {
    let token = match bearer_token(headers) {
        Some(token) if token == state.auth_token => token.to_string(),
        _ => return Err(unauthorized()),
    };

    if !state.rate_limiter.allow(&token).await {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"rate limit exceeded"})),
        )
            .into_response());
    }

    Ok(token)
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get("authorization")?.to_str().ok()?;
    value.strip_prefix("Bearer ")
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error":"unauthorized"})),
    )
        .into_response()
}

fn internal_error(error: &anyhow::Error) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": error.to_string()})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::capabilities::CapabilityPolicy;
    use crate::governance::{GovernanceEngine, PlanetCreationRequest};
    use crate::identity::Identity;
    use crate::mailbox::CrossSubnetMailbox;

    fn build_test_app(
        rate_limit: usize,
    ) -> (tempfile::TempDir, Router, String, Arc<Mutex<PolicyEngine>>) {
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::new_random();
        let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
        let task_engine = TaskEngine::new(event_log.clone(), identity.clone());
        let ledger_path = dir.path().join("ledger.json");
        let governance_state_path = dir.path().join("governance/state.json");
        let mailbox_state_path = dir.path().join("mailbox/state.json");

        let policy_engine = Arc::new(Mutex::new(
            PolicyEngine::load_or_new(
                dir.path().join("policy.json"),
                "test-session",
                CapabilityPolicy::default(),
            )
            .unwrap(),
        ));

        let mut governance = GovernanceEngine::default();
        governance.issue_license(&identity.agent_id, &identity.agent_id, "proof", 7);
        governance.lock_bond(&identity.agent_id, 100, 30);
        let signer = Identity::new_random();
        let created_at = Utc::now().timestamp();
        let approvals = vec![
            GovernanceEngine::sign_genesis(
                "planet-test",
                "Planet Test",
                &identity.agent_id,
                created_at,
                &identity,
            )
            .unwrap(),
            GovernanceEngine::sign_genesis(
                "planet-test",
                "Planet Test",
                &identity.agent_id,
                created_at,
                &signer,
            )
            .unwrap(),
        ];
        let planet_request = PlanetCreationRequest {
            subnet_id: "planet-test".to_string(),
            name: "Planet Test".to_string(),
            creator: identity.agent_id.clone(),
            created_at,
            tax_rate: 0.05,
            min_bond: 50,
            min_approvals: 2,
        };
        governance
            .create_planet(&planet_request, &approvals)
            .unwrap();
        governance.persist(&governance_state_path).unwrap();
        let governance_engine = Arc::new(Mutex::new(governance));

        let audit_log = AuditLog::new(dir.path().join("audit/control_plane.jsonl")).unwrap();
        let mailbox = Arc::new(Mutex::new(CrossSubnetMailbox::default()));
        let (stream_tx, _) = broadcast::channel(32);
        let token = "test-token".to_string();

        let state = ControlPlaneState {
            agent_id: identity.agent_id.clone(),
            identity,
            started_at: Utc::now().timestamp(),
            auth_token: token.clone(),
            event_log,
            task_engine: Arc::new(Mutex::new(task_engine)),
            task_ledger_path: ledger_path,
            governance_engine,
            governance_state_path,
            policy_engine: policy_engine.clone(),
            mailbox,
            mailbox_state_path,
            audit_log,
            rate_limiter: Arc::new(RateLimiter::new(rate_limit, 60)),
            stream_tx,
        };

        (dir, app(state), token, policy_engine)
    }

    #[tokio::test]
    async fn state_requires_auth() {
        let (_dir, app, _token, _) = build_test_app(10);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/state")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn policy_flow_pending_then_approve_once() {
        let (_dir, app, token, _policy) = build_test_app(20);

        let check_body = json!({
            "subject": "skill:test@0.1.0",
            "trust": "verified",
            "capability": "p2p.publish",
            "reason": "integration-test"
        });

        let first = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/policy/check")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(check_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);

        let first_json: Value =
            serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let request_id = first_json["request_id"].as_str().unwrap().to_string();

        let approve_body = json!({
            "request_id": request_id,
            "approved_by": "operator",
            "scope": "once"
        });

        let approve = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/policy/approve")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(approve_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(approve.status(), StatusCode::OK);

        let second = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/policy/check")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(check_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);

        let third = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/policy/check")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(check_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(third.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn governance_proposal_flow_works() {
        let (dir, app, token, _) = build_test_app(30);

        let state_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/state")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(state_resp.status(), StatusCode::OK);
        let state_json: Value =
            serde_json::from_slice(&state_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let agent_id = state_json["agent_id"].as_str().unwrap().to_string();

        let create_body = json!({
            "subnet_id": "planet-test",
            "kind": "update_tax_rate",
            "payload": {"tax_rate": 0.09},
            "created_by": agent_id,
        });
        let create_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/governance/proposals")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(create_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);
        let create_json: Value =
            serde_json::from_slice(&create_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let proposal_id = create_json["proposal_id"].as_str().unwrap().to_string();

        let vote_body = json!({
            "proposal_id": proposal_id,
            "voter": state_json["agent_id"],
            "approve": true,
        });
        let vote_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/governance/proposals/vote")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(vote_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(vote_resp.status(), StatusCode::OK);

        let finalize_body = json!({
            "proposal_id": create_json["proposal_id"],
            "min_votes_for": 1,
        });
        let finalize_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/governance/proposals/finalize")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(finalize_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finalize_resp.status(), StatusCode::OK);
        let finalize_json: Value = serde_json::from_slice(
            &finalize_resp
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap();
        assert_eq!(finalize_json["status"], "accepted");

        let list_resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/governance/proposals?subnet_id=planet-test")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_resp.status(), StatusCode::OK);
        let list_json: Value =
            serde_json::from_slice(&list_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(list_json.as_array().unwrap().len(), 1);
        let persisted =
            GovernanceEngine::load_or_new(dir.path().join("governance/state.json")).unwrap();
        assert_eq!(persisted.list_proposals(Some("planet-test")).len(), 1);
    }

    #[tokio::test]
    async fn demo_action_persists_ledger_to_disk() {
        let (dir, app, token, _) = build_test_app(20);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/actions")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"action": "task.run_demo_market"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let ledger =
            crate::task_engine::TaskEngine::load_ledger(dir.path().join("ledger.json")).unwrap();
        assert!(!ledger.is_empty());
        assert!(ledger.values().any(|stats| stats.watt > 0));
    }

    #[tokio::test]
    async fn mailbox_send_fetch_ack_persists() {
        let (dir, app, token, _) = build_test_app(30);

        let send_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/mailbox/messages")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({
                            "to_agent": "agent-receiver",
                            "from_subnet": "planet-a",
                            "to_subnet": "planet-b",
                            "payload": {"kind": "offer", "price": 42}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(send_resp.status(), StatusCode::CREATED);
        let send_json: Value =
            serde_json::from_slice(&send_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let message_id = send_json["message_id"].as_str().unwrap().to_string();

        let fetch_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/mailbox/messages?subnet_id=planet-b")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fetch_resp.status(), StatusCode::OK);
        let fetch_json: Value =
            serde_json::from_slice(&fetch_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(fetch_json.as_array().unwrap().len(), 1);

        let ack_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/mailbox/ack")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"subnet_id": "planet-b", "message_id": message_id}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ack_resp.status(), StatusCode::OK);

        let persisted =
            CrossSubnetMailbox::load_or_new(dir.path().join("mailbox/state.json")).unwrap();
        assert!(persisted.fetch_for_subnet("planet-b").is_empty());
    }

    #[tokio::test]
    async fn events_export_is_public_for_recovery() {
        let (_dir, app, _token, _) = build_test_app(10);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/events/export")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(body.get("events").is_some());
    }

    #[tokio::test]
    async fn rate_limit_returns_429() {
        let (_dir, app, token, _) = build_test_app(1);

        let first = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/state")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let second = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/state")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
