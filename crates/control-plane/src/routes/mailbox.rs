use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::json;

use crate::auth::{authorize, internal_error};
use crate::state::{
    ControlPlaneState, MailboxAckBody, MailboxFetchQuery, MailboxSendBody, StreamEvent,
};
use wattetheria_kernel::audit::AuditEntry;

pub(crate) async fn mailbox_send(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MailboxSendBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let mut mailbox = state.mailbox.lock().await;
    let message = match mailbox.enqueue_signed_with_signer(
        &state.identity,
        state.signer.as_ref(),
        &body.to_agent,
        &body.from_subnet,
        &body.to_subnet,
        body.payload,
    ) {
        Ok(message) => message,
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = state
        .local_db
        .save_domain(wattetheria_kernel::local_db::domain::MAILBOX, &*mailbox)
    {
        return internal_error(&error);
    }
    drop(mailbox);

    let payload = json!({
        "message_id": message.message_id,
        "from_subnet": message.from_subnet,
        "to_subnet": message.to_subnet,
        "to_agent": message.to_agent,
    });
    if let Err(error) = state.append_signed_event("MAILBOX_MESSAGE_ENQUEUED", payload.clone()) {
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

pub(crate) async fn mailbox_fetch(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<MailboxFetchQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
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

pub(crate) async fn mailbox_ack(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MailboxAckBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
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
    if let Err(error) = state
        .local_db
        .save_domain(wattetheria_kernel::local_db::domain::MAILBOX, &*mailbox)
    {
        return internal_error(&error);
    }
    drop(mailbox);

    let payload = json!({"subnet_id": body.subnet_id, "message_id": body.message_id});
    if let Err(error) = state.append_signed_event("MAILBOX_MESSAGE_ACKED", payload.clone()) {
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
