use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    ServiceNetClient, ServiceNetClientError, ServiceNetGetAgentTaskRequest,
    ServiceNetInvokeRequest, ServiceNetInvokeResponse, direct, direct_tasks,
    verify_service_agent_invoke_response,
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceNetListAgentTasksRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_timestamp_after: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_artifacts: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_context_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_envelope: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceNetCancelAgentTaskRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_context_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_envelope: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceNetSubscribeAgentTaskRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_context_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_envelope: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_events: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAgentMessageDispatch {
    WattetheriaRuntimeSync,
    WattetheriaRuntimeAsync,
    CustomizedAgent,
}

#[derive(Debug, Clone)]
pub struct DispatchedServiceAgentMessage {
    pub dispatch: ServiceAgentMessageDispatch,
    pub response: ServiceNetInvokeResponse,
}

#[must_use]
pub fn get_agent_task_envelope_message(
    task_id: &str,
    request: &ServiceNetGetAgentTaskRequest,
) -> Value {
    json!({
        "operation": "GetTask",
        "task_id": task_id,
        "history_length": request.history_length,
    })
}

#[must_use]
pub fn list_agent_tasks_envelope_message(request: &ServiceNetListAgentTasksRequest) -> Value {
    json!({
        "operation": "ListTasks",
        "context_id": request.context_id,
        "status": request.status,
        "page_size": request.page_size,
        "page_token": request.page_token,
        "history_length": request.history_length,
        "status_timestamp_after": request.status_timestamp_after,
        "include_artifacts": request.include_artifacts,
    })
}

#[must_use]
pub fn cancel_agent_task_envelope_message(task_id: &str) -> Value {
    json!({"operation": "CancelTask", "task_id": task_id})
}

#[must_use]
pub fn subscribe_agent_task_envelope_message(task_id: &str) -> Value {
    json!({"operation": "SubscribeToTask", "task_id": task_id})
}

impl ServiceNetClient {
    pub async fn send_service_agent_message(
        &self,
        agent_id: &str,
        request: &ServiceNetInvokeRequest,
    ) -> Result<DispatchedServiceAgentMessage, ServiceNetClientError> {
        let identity = self.resolve_service_identity(agent_id).await?;
        if published_agent_uses_wattetheria_runtime(&identity)
            .map_err(|error| Self::client_error(&error))?
        {
            if request.return_immediately.unwrap_or(false) {
                let response = self.invoke_agent_async(agent_id, request).await?;
                return Ok(DispatchedServiceAgentMessage {
                    dispatch: ServiceAgentMessageDispatch::WattetheriaRuntimeAsync,
                    response,
                });
            }
            let response = self
                .invoke_agent_with_identity(agent_id, request, &identity)
                .await?;
            return Ok(DispatchedServiceAgentMessage {
                dispatch: ServiceAgentMessageDispatch::WattetheriaRuntimeSync,
                response,
            });
        }
        ensure_customized_agent(&identity).map_err(|error| Self::client_error(&error))?;
        let response = self
            .invoke_agent_with_identity(agent_id, request, &identity)
            .await?;
        Ok(DispatchedServiceAgentMessage {
            dispatch: ServiceAgentMessageDispatch::CustomizedAgent,
            response,
        })
    }

    pub async fn get_service_agent_task(
        &self,
        agent_id: &str,
        task_id: &str,
        request: &ServiceNetGetAgentTaskRequest,
    ) -> Result<ServiceNetInvokeResponse, ServiceNetClientError> {
        let identity = self.resolve_service_identity(agent_id).await?;
        ensure_customized_agent(&identity).map_err(|error| Self::client_error(&error))?;
        if direct::uses_wattetheria_direct(&identity) {
            return direct_tasks::get_agent_task(self, agent_id, task_id, request, &identity).await;
        }
        let response = self
            .request_json(
                Method::POST,
                self.endpoint(&["v1", "agents", agent_id, "tasks", task_id, "get"])
                    .map_err(|error| Self::client_error(&error))?,
                Some(request),
            )
            .await?;
        self.verify_a2a_task_response(&identity, request.agent_envelope.as_ref(), &response)?;
        Ok(response)
    }

    pub async fn list_agent_tasks(
        &self,
        agent_id: &str,
        request: &ServiceNetListAgentTasksRequest,
    ) -> Result<ServiceNetInvokeResponse, ServiceNetClientError> {
        let identity = self.resolve_service_identity(agent_id).await?;
        ensure_customized_agent(&identity).map_err(|error| Self::client_error(&error))?;
        if direct::uses_wattetheria_direct(&identity) {
            return direct_tasks::list_agent_tasks(self, agent_id, request, &identity).await;
        }
        let response = self
            .request_json(
                Method::POST,
                self.endpoint(&["v1", "agents", agent_id, "tasks", "list"])
                    .map_err(|error| Self::client_error(&error))?,
                Some(request),
            )
            .await?;
        self.verify_a2a_task_response(&identity, request.agent_envelope.as_ref(), &response)?;
        Ok(response)
    }

    fn verify_a2a_subscription_response(
        &self,
        identity: &Value,
        envelope: Option<&Value>,
        response: &ServiceNetInvokeResponse,
    ) -> Result<(), ServiceNetClientError> {
        let events = response
            .raw
            .pointer("/result/events")
            .and_then(Value::as_array)
            .context("A2A subscription response is missing result.events")
            .map_err(|error| Self::client_error(&error))?;
        for event in events {
            let signature = super::service_signature_from_raw(event)
                .context("A2A subscription event is missing Service Agent signature")
                .map_err(|error| Self::client_error(&error))?;
            let event_response = ServiceNetInvokeResponse {
                agent_id: response.agent_id.clone(),
                status: "event".to_owned(),
                receipt_id: None,
                task_id: response.task_id.clone(),
                context_id: None,
                message: None,
                output: None,
                settlement: None,
                payment_receipt: None,
                service_signature: Some(signature),
                raw: event.clone(),
            };
            self.verify_a2a_task_response(identity, envelope, &event_response)?;
        }
        Ok(())
    }

    pub async fn cancel_agent_task(
        &self,
        agent_id: &str,
        task_id: &str,
        request: &ServiceNetCancelAgentTaskRequest,
    ) -> Result<ServiceNetInvokeResponse, ServiceNetClientError> {
        let identity = self.resolve_service_identity(agent_id).await?;
        ensure_customized_agent(&identity).map_err(|error| Self::client_error(&error))?;
        if direct::uses_wattetheria_direct(&identity) {
            return direct_tasks::cancel_agent_task(self, agent_id, task_id, request, &identity)
                .await;
        }
        let response = self
            .request_json(
                Method::POST,
                self.endpoint(&["v1", "agents", agent_id, "tasks", task_id, "cancel"])
                    .map_err(|error| Self::client_error(&error))?,
                Some(request),
            )
            .await?;
        self.verify_a2a_task_response(&identity, request.agent_envelope.as_ref(), &response)?;
        Ok(response)
    }

    pub async fn subscribe_agent_task(
        &self,
        agent_id: &str,
        task_id: &str,
        request: &ServiceNetSubscribeAgentTaskRequest,
    ) -> Result<ServiceNetInvokeResponse, ServiceNetClientError> {
        let identity = self.resolve_service_identity(agent_id).await?;
        ensure_customized_agent(&identity).map_err(|error| Self::client_error(&error))?;
        if direct::uses_wattetheria_direct(&identity) {
            return direct_tasks::subscribe_agent_task(self, agent_id, task_id, request, &identity)
                .await;
        }
        let response = self
            .request_json(
                Method::POST,
                self.endpoint(&["v1", "agents", agent_id, "tasks", task_id, "subscribe"])
                    .map_err(|error| Self::client_error(&error))?,
                Some(request),
            )
            .await?;
        self.verify_a2a_subscription_response(
            &identity,
            request.agent_envelope.as_ref(),
            &response,
        )?;
        Ok(response)
    }

    pub(super) fn verify_a2a_task_response(
        &self,
        identity: &Value,
        envelope: Option<&Value>,
        response: &ServiceNetInvokeResponse,
    ) -> Result<(), ServiceNetClientError> {
        let envelope = envelope
            .context("A2A task request is missing agent_envelope")
            .map_err(|error| Self::client_error(&error))?;
        let expected_digest = envelope
            .pointer("/extensions/request_digest")
            .and_then(Value::as_str);
        let expected_nonce = envelope
            .pointer("/extensions/nonce")
            .and_then(Value::as_str);
        verify_service_agent_invoke_response(
            identity,
            response,
            expected_digest,
            expected_nonce,
            true,
        )
        .map_err(|error| Self::client_error(&error))?;
        self.record_service_response_nonce(response)
            .map_err(|error| Self::client_error(&error))
    }
}

pub fn published_agent_uses_wattetheria_runtime(record: &Value) -> anyhow::Result<bool> {
    match record
        .pointer("/deployment/execution_mode")
        .and_then(Value::as_str)
    {
        Some("wattetheria_runtime") => Ok(true),
        Some("customized_agent") => Ok(false),
        Some(mode) => Err(anyhow!("unsupported Service Agent execution_mode `{mode}`")),
        None => Err(anyhow!(
            "Service Agent record is missing deployment.execution_mode"
        )),
    }
}

pub(super) fn ensure_customized_agent(record: &Value) -> anyhow::Result<()> {
    if published_agent_uses_wattetheria_runtime(record)? {
        Err(anyhow!(
            "A2A Task tools require a Customized Agent; Wattetheria Runtime agents use the internal invocation tools"
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};

    #[test]
    fn customized_agent_gate_is_independent_from_connection_mode() {
        for connection_mode in ["servicenet_relay", "wattetheria_direct"] {
            let record = json!({
                "deployment": {
                    "execution_mode": "customized_agent",
                    "connection_mode": connection_mode,
                }
            });
            assert!(ensure_customized_agent(&record).is_ok());
        }
    }

    #[test]
    fn wattetheria_runtime_does_not_expose_a2a_task_tools() {
        let record = json!({
            "deployment": {
                "execution_mode": "wattetheria_runtime",
                "connection_mode": "servicenet_relay",
            }
        });
        let error = ensure_customized_agent(&record).expect_err("runtime must use internal tools");
        assert!(error.to_string().contains("Customized Agent"));
    }

    #[test]
    fn published_execution_mode_rejects_missing_or_unknown_values() {
        let missing = json!({"deployment": {}});
        assert!(
            published_agent_uses_wattetheria_runtime(&missing)
                .expect_err("missing execution mode must fail")
                .to_string()
                .contains("missing")
        );

        let unknown = json!({"deployment": {"execution_mode": "future_runtime"}});
        assert!(
            published_agent_uses_wattetheria_runtime(&unknown)
                .expect_err("unknown execution mode must fail")
                .to_string()
                .contains("unsupported")
        );
    }

    #[tokio::test]
    async fn runtime_direct_rejects_async_receipt_mode() {
        let registry = Router::new().route(
            "/v1/agents/runtime-direct",
            get(|| async {
                Json(json!({
                    "agent_id": "runtime-direct",
                    "deployment": {
                        "execution_mode": "wattetheria_runtime",
                        "connection_mode": "wattetheria_direct",
                    },
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("registry listener should bind");
        let address = listener.local_addr().expect("registry address");
        tokio::spawn(async move {
            axum::serve(listener, registry)
                .await
                .expect("mock registry should run");
        });
        let client = ServiceNetClient::new(format!("http://{address}"))
            .expect("ServiceNet client should build");
        let request = ServiceNetInvokeRequest {
            return_immediately: Some(true),
            ..ServiceNetInvokeRequest::default()
        };

        let error = client
            .send_service_agent_message("runtime-direct", &request)
            .await
            .expect_err("Runtime Direct cannot create ServiceNet async receipts");

        assert!(error.to_string().contains("return_immediately=false"));
    }

    #[test]
    fn list_task_envelope_excludes_transport_and_auth_fields() {
        let message = list_agent_tasks_envelope_message(&ServiceNetListAgentTasksRequest {
            context_id: Some("ctx-1".to_owned()),
            status: Some("TASK_STATE_WORKING".to_owned()),
            page_size: Some(25),
            auth_token: Some("secret".to_owned()),
            ..ServiceNetListAgentTasksRequest::default()
        });

        assert_eq!(message["operation"], "ListTasks");
        assert_eq!(message["context_id"], "ctx-1");
        assert_eq!(message["page_size"], 25);
        assert!(message.get("auth_token").is_none());
        assert!(message.get("agent_envelope").is_none());
    }
}
