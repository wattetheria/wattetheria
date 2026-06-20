use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};

use super::publish::{
    ServiceNetPublisherRegistration, load_publisher_state, registration_matches_identity,
};
use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;
use wattetheria_kernel::audit::AuditEntry;

fn published_registration_json(registration: &ServiceNetPublisherRegistration) -> Value {
    json!({
        "agent_id": registration.agent_id,
        "provider_id": registration.provider_id,
        "provider_did": registration.provider_did,
        "version": registration.version,
        "card_hash": registration.card_hash,
        "updated_at": registration.updated_at,
        "agent_card": registration.agent_card,
        "deployment": registration.deployment,
        "review": registration.review,
        "source": "local",
    })
}

pub(crate) async fn published_agents(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let publisher_state = match load_publisher_state(&state.data_dir) {
        Ok(state) => state,
        Err(error) => return internal_error(&error),
    };
    let items: Vec<Value> = publisher_state
        .registrations
        .iter()
        .filter(|registration| registration_matches_identity(registration, &state.agent_did))
        .map(published_registration_json)
        .collect();
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: Utc::now().timestamp(),
        category: "servicenet".to_string(),
        action: "servicenet.agents.published.list".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_did.clone()),
        capability: Some("net.outbound".to_string()),
        reason: Some("servicenet.publish".to_string()),
        duration_ms: None,
        details: Some(json!({"count": items.len()})),
    });
    let count = items.len();
    Json(json!({
        "items": items,
        "count": count,
        "provider_did": state.agent_did,
    }))
    .into_response()
}
