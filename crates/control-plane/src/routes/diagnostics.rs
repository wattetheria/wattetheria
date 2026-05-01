use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error};
use crate::diagnostics::{DiagnosticFilter, list_diagnostics};
use crate::state::ControlPlaneState;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::swarm_bridge::SwarmDiagnosticsQuery;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DiagnosticsQuery {
    pub limit: Option<usize>,
    pub level: Option<String>,
    pub component: Option<String>,
    pub category: Option<String>,
    pub phase: Option<String>,
    pub trace_id: Option<String>,
    pub event_id: Option<String>,
    pub object_id: Option<String>,
    pub source_node_id: Option<String>,
    pub search: Option<String>,
}

impl From<DiagnosticsQuery> for DiagnosticFilter {
    fn from(query: DiagnosticsQuery) -> Self {
        Self {
            limit: query.limit,
            level: query.level,
            component: query.component,
            category: query.category,
            phase: query.phase,
            trace_id: query.trace_id,
            event_id: query.event_id,
            object_id: query.object_id,
            source_node_id: query.source_node_id,
            search: query.search,
        }
    }
}

impl From<DiagnosticsQuery> for SwarmDiagnosticsQuery {
    fn from(query: DiagnosticsQuery) -> Self {
        Self {
            limit: query.limit,
            level: query.level,
            component: query.component,
            category: query.category,
            phase: query.phase,
            event_id: query.event_id,
            object_id: query.object_id,
            source_node_id: query.source_node_id,
            search: query.search,
        }
    }
}

pub(crate) async fn client_diagnostics(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<DiagnosticsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let filter = DiagnosticFilter::from(query);
    let entries = match list_diagnostics(&state.data_dir, &filter) {
        Ok(entries) => entries,
        Err(error) => return internal_error(&error),
    };
    let payload = json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "entries": entries,
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.diagnostics.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "count": payload["entries"].as_array().map_or(0, Vec::len),
        })),
    });

    Json::<Value>(payload).into_response()
}

pub(crate) async fn client_wattswarm_diagnostics(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<DiagnosticsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let swarm_query = SwarmDiagnosticsQuery::from(query);
    let payload = match state.swarm_bridge.diagnostics(swarm_query).await {
        Ok(payload) => match serde_json::to_value(payload) {
            Ok(value) => value,
            Err(error) => return internal_error(&error.into()),
        },
        Err(error) => {
            json!({
                "ok": false,
                "generated_at": chrono::Utc::now().to_rfc3339(),
                "network_service_started": false,
                "snapshot": null,
                "diagnostics": [],
                "error": error.to_string(),
            })
        }
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.wattswarm_diagnostics.query".to_string(),
        status: if payload["ok"].as_bool().unwrap_or(false) {
            "ok".to_string()
        } else {
            "error".to_string()
        },
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "count": payload["diagnostics"].as_array().map_or(0, Vec::len),
            "error": payload.get("error").cloned(),
        })),
    });

    Json::<Value>(payload).into_response()
}
