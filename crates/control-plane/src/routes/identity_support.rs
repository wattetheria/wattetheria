use axum::Json;
use axum::http::HeaderValue;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Serialize;

use crate::state::ControlPlaneState;
use wattetheria_kernel::audit::AuditEntry;

pub(crate) const IDENTITY_EXPORT_FORMAT: &str = "wattetheria-did-key-backup";
pub(crate) const IDENTITY_NETWORK_ID: &str = "mainnet.watt-etheria";

pub(crate) fn private_export_response(value: impl Serialize) -> Response {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(crate) fn append_identity_audit(
    state: &ControlPlaneState,
    actor: String,
    action: &str,
    subject: &str,
) {
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: Utc::now().timestamp(),
        category: "identity".to_owned(),
        action: action.to_owned(),
        status: "ok".to_owned(),
        actor: Some(actor),
        subject: Some(subject.to_owned()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: None,
    });
}
