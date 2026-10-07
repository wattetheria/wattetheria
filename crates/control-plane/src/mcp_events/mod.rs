pub(crate) mod catalog;
pub(crate) mod crypto;
pub mod env_webhook;
mod service;
pub(crate) mod sqlite;
pub(crate) mod store;
pub(crate) mod subscriptions;
pub(crate) mod webhook;

pub use env_webhook::EnvWebhookConfig;
pub use service::McpEvents;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AgentEventMode {
    #[default]
    ApiRuntime,
    McpEvents,
}

impl std::str::FromStr for AgentEventMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "api_runtime" => Ok(Self::ApiRuntime),
            "mcp_events" => Ok(Self::McpEvents),
            _ => Err("agent event mode must be api_runtime or mcp_events".to_owned()),
        }
    }
}

pub(crate) async fn receive_agent_event(
    state: &crate::state::ControlPlaneState,
    event: &crate::routes::agent_events::AgentEventEnvelope,
    acked_at: u64,
) -> axum::response::Response {
    use crate::routes::agent_events::AgentEventCallbackResponse;
    use axum::response::IntoResponse;

    let result = match &state.mcp_events {
        Some(events) => {
            events
                .publish(
                    event,
                    &catalog::ProjectOutcome {
                        decision_status: "pending_external",
                        commit_status: "not_requested",
                        chosen_action: None,
                        route: None,
                        requires_action: true,
                    },
                )
                .await
        }
        None => Err(anyhow::anyhow!("MCP Events is unavailable")),
    };
    let (status, detail) = match result {
        Ok(()) => (
            axum::http::StatusCode::OK,
            "accepted for MCP Events".to_owned(),
        ),
        Err(error) => {
            tracing::warn!(event_id = %event.event_id, "MCP Events reception failed: {error:#}");
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "MCP Events reception failed".to_owned(),
            )
        }
    };
    (
        status,
        axum::Json(AgentEventCallbackResponse {
            ok: status.is_success(),
            acked_at: status.is_success().then_some(acked_at),
            detail: Some(detail),
            decision: None,
        }),
    )
        .into_response()
}
