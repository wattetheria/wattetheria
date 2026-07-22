use a2a::{
    CancelTaskRequest, GetTaskRequest, ListTasksRequest, Message, Part, Role,
    SendMessageConfiguration, SendMessageRequest, SubscribeToTaskRequest,
};
use a2a_client::{
    A2AClient, auth::AuthInterceptor, jsonrpc::JsonRpcTransport, middleware::CallInterceptor,
};
use async_trait::async_trait;
use futures_util::{StreamExt, stream::BoxStream};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::time::timeout;
use uuid::Uuid;

use super::wire;
use crate::state::ControlPlaneState;
use wattetheria_kernel::brain::RuntimeSessionContext;
use wattetheria_kernel::servicenet::{
    CustomizedAgentProtocol, ServiceAgentExecution, ServiceNetPublisherRegistration,
};
use wattetheria_kernel::swarm_bridge::SwarmAgentEnvelope;

static A2A_HTTP_CLIENT: OnceLock<reqwest_a2a::Client> = OnceLock::new();

pub(super) async fn execute_service_agent(
    state: &ControlPlaneState,
    registration: &ServiceNetPublisherRegistration,
    params: &Value,
    message_text: &str,
    envelope: &SwarmAgentEnvelope,
    authorization: Option<&str>,
) -> Result<Value, String> {
    match &registration.execution {
        ServiceAgentExecution::WattetheriaRuntime => {
            execute_wattetheria_runtime(
                state,
                &registration.agent_id,
                params,
                message_text,
                envelope,
            )
            .await
        }
        ServiceAgentExecution::CustomizedAgent {
            protocol,
            customized_agent_url,
        } => {
            customized_agent_executor(*protocol)
                .send_message(customized_agent_url, params, authorization)
                .await
        }
    }
}

pub(super) async fn get_customized_agent_task(
    registration: &ServiceNetPublisherRegistration,
    params: &Value,
    authorization: Option<&str>,
) -> Result<Value, String> {
    let (protocol, endpoint) = customized_target(registration)?;
    customized_agent_executor(protocol)
        .get_task(endpoint, params, authorization)
        .await
}

pub(super) async fn list_customized_agent_tasks(
    registration: &ServiceNetPublisherRegistration,
    params: &Value,
    authorization: Option<&str>,
) -> Result<Value, String> {
    let (protocol, endpoint) = customized_target(registration)?;
    customized_agent_executor(protocol)
        .list_tasks(endpoint, params, authorization)
        .await
}

pub(super) async fn cancel_customized_agent_task(
    registration: &ServiceNetPublisherRegistration,
    params: &Value,
    authorization: Option<&str>,
) -> Result<Value, String> {
    let (protocol, endpoint) = customized_target(registration)?;
    customized_agent_executor(protocol)
        .cancel_task(endpoint, params, authorization)
        .await
}

pub(super) async fn subscribe_customized_agent_task(
    registration: &ServiceNetPublisherRegistration,
    params: &Value,
    authorization: Option<&str>,
) -> Result<BoxStream<'static, Result<Value, String>>, String> {
    let (protocol, endpoint) = customized_target(registration)?;
    customized_agent_executor(protocol)
        .subscribe_to_task(endpoint, params, authorization)
        .await
}

fn customized_target(
    registration: &ServiceNetPublisherRegistration,
) -> Result<(CustomizedAgentProtocol, &str), String> {
    match &registration.execution {
        ServiceAgentExecution::CustomizedAgent {
            protocol,
            customized_agent_url,
        } => Ok((*protocol, customized_agent_url)),
        ServiceAgentExecution::WattetheriaRuntime => Err(
            "A2A Task operations require Customized Agent execution; Wattetheria Runtime uses the internal invocation flow"
                .to_owned(),
        ),
    }
}

async fn execute_wattetheria_runtime(
    state: &ControlPlaneState,
    agent_id: &str,
    params: &Value,
    message_text: &str,
    envelope: &SwarmAgentEnvelope,
) -> Result<Value, String> {
    let context_id = wire::message_field(params, "contextId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let session_context = context_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .map(RuntimeSessionContext::precomputed)
        .or_else(|| service_session_from_envelope(envelope, agent_id));
    let prompt = bridge_prompt(params, message_text, envelope);
    let output = {
        let engine = state.brain_engine.read().await;
        engine
            .generate_text_with_session(&prompt, session_context.as_ref())
            .await
            .map_err(|error| error.to_string())?
    };
    let task_id = wire::message_field(params, "taskId")
        .and_then(Value::as_str)
        .map_or_else(|| Uuid::new_v4().to_string(), ToOwned::to_owned);
    let response_context_id = context_id
        .or_else(|| {
            session_context
                .as_ref()
                .map(RuntimeSessionContext::session_id)
        })
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let artifact_id = Uuid::new_v4().to_string();
    Ok(json!({
        "task": {
            "id": task_id,
            "contextId": response_context_id,
            "status": {
                "state": "TASK_STATE_COMPLETED"
            },
            "artifacts": [
                {
                    "artifactId": artifact_id,
                    "parts": [
                        {
                            "text": bridge_output_text(&output)
                        }
                    ]
                }
            ]
        }
    }))
}

#[async_trait]
trait CustomizedAgentExecutor: Send + Sync {
    async fn send_message(
        &self,
        endpoint: &str,
        params: &Value,
        authorization: Option<&str>,
    ) -> Result<Value, String>;

    async fn get_task(
        &self,
        endpoint: &str,
        params: &Value,
        authorization: Option<&str>,
    ) -> Result<Value, String>;

    async fn list_tasks(
        &self,
        endpoint: &str,
        params: &Value,
        authorization: Option<&str>,
    ) -> Result<Value, String>;

    async fn cancel_task(
        &self,
        endpoint: &str,
        params: &Value,
        authorization: Option<&str>,
    ) -> Result<Value, String>;

    async fn subscribe_to_task(
        &self,
        endpoint: &str,
        params: &Value,
        authorization: Option<&str>,
    ) -> Result<BoxStream<'static, Result<Value, String>>, String>;
}

struct A2aV1Executor;

#[async_trait]
impl CustomizedAgentExecutor for A2aV1Executor {
    async fn send_message(
        &self,
        endpoint: &str,
        params: &Value,
        authorization: Option<&str>,
    ) -> Result<Value, String> {
        let client = a2a_v1_client(endpoint, authorization)?;
        let request = a2a_v1_request(params)?;
        let response = timeout(Duration::from_mins(2), client.send_message(&request))
            .await
            .map_err(|_| {
                "Customized Agent A2A v1 invocation timed out after 120 seconds".to_owned()
            })?
            .map_err(|error| format!("Customized Agent A2A v1 invocation failed: {error}"))?;
        serde_json::to_value(response)
            .map_err(|error| format!("serialize Customized Agent A2A v1 response: {error}"))
    }

    async fn get_task(
        &self,
        endpoint: &str,
        params: &Value,
        authorization: Option<&str>,
    ) -> Result<Value, String> {
        let request: GetTaskRequest = serde_json::from_value(params.clone())
            .map_err(|error| format!("invalid A2A GetTask request: {error}"))?;
        let task = a2a_v1_client(endpoint, authorization)?
            .get_task(&request)
            .await
            .map_err(|error| format!("Customized Agent A2A GetTask failed: {error}"))?;
        serde_json::to_value(task).map_err(|error| format!("serialize A2A Task: {error}"))
    }

    async fn list_tasks(
        &self,
        endpoint: &str,
        params: &Value,
        authorization: Option<&str>,
    ) -> Result<Value, String> {
        let request: ListTasksRequest = serde_json::from_value(params.clone())
            .map_err(|error| format!("invalid A2A ListTasks request: {error}"))?;
        let response = a2a_v1_client(endpoint, authorization)?
            .list_tasks(&request)
            .await
            .map_err(|error| format!("Customized Agent A2A ListTasks failed: {error}"))?;
        serde_json::to_value(response)
            .map_err(|error| format!("serialize A2A ListTasks response: {error}"))
    }

    async fn cancel_task(
        &self,
        endpoint: &str,
        params: &Value,
        authorization: Option<&str>,
    ) -> Result<Value, String> {
        let request: CancelTaskRequest = serde_json::from_value(params.clone())
            .map_err(|error| format!("invalid A2A CancelTask request: {error}"))?;
        let task = a2a_v1_client(endpoint, authorization)?
            .cancel_task(&request)
            .await
            .map_err(|error| format!("Customized Agent A2A CancelTask failed: {error}"))?;
        serde_json::to_value(task).map_err(|error| format!("serialize cancelled A2A Task: {error}"))
    }

    async fn subscribe_to_task(
        &self,
        endpoint: &str,
        params: &Value,
        authorization: Option<&str>,
    ) -> Result<BoxStream<'static, Result<Value, String>>, String> {
        let request: SubscribeToTaskRequest = serde_json::from_value(params.clone())
            .map_err(|error| format!("invalid A2A SubscribeToTask request: {error}"))?;
        let stream = a2a_v1_client(endpoint, authorization)?
            .subscribe_to_task(&request)
            .await
            .map_err(|error| format!("Customized Agent A2A SubscribeToTask failed: {error}"))?;
        Ok(Box::pin(stream.map(|event| {
            event
                .map_err(|error| format!("Customized Agent A2A task event failed: {error}"))
                .and_then(|event| {
                    serde_json::to_value(event)
                        .map_err(|error| format!("serialize A2A task event: {error}"))
                })
        })))
    }
}

fn a2a_v1_client(
    endpoint: &str,
    authorization: Option<&str>,
) -> Result<A2AClient<JsonRpcTransport>, String> {
    let transport = JsonRpcTransport::new(shared_a2a_http_client()?, endpoint.to_owned());
    let mut client = A2AClient::new(transport);
    if let Some(authorization) = authorization {
        let interceptor: Arc<dyn CallInterceptor> = Arc::new(AuthInterceptor::custom(
            "Authorization",
            authorization.to_owned(),
        ));
        client = client.with_interceptors(vec![interceptor]);
    }
    Ok(client)
}

fn shared_a2a_http_client() -> Result<reqwest_a2a::Client, String> {
    if let Some(client) = A2A_HTTP_CLIENT.get() {
        return Ok(client.clone());
    }
    let client = a2a_client::default_reqwest_client(None)
        .map_err(|error| format!("build A2A v1 client: {error}"))?;
    let _ = A2A_HTTP_CLIENT.set(client);
    Ok(A2A_HTTP_CLIENT
        .get()
        .expect("A2A HTTP client must be initialized")
        .clone())
}

fn customized_agent_executor(
    protocol: CustomizedAgentProtocol,
) -> Box<dyn CustomizedAgentExecutor> {
    match protocol {
        CustomizedAgentProtocol::A2aV1 => Box::new(A2aV1Executor),
    }
}

fn a2a_v1_request(params: &Value) -> Result<SendMessageRequest, String> {
    let parts = params
        .pointer("/message/parts")
        .and_then(Value::as_array)
        .map(|parts| parts.iter().map(a2a_v1_part).collect::<Result<Vec<_>, _>>())
        .transpose()?
        .filter(|parts| !parts.is_empty())
        .unwrap_or_else(|| vec![Part::data(Value::Null)]);
    let mut message = Message::new(Role::User, parts);
    message.context_id = wire::message_field(params, "contextId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    message.task_id = wire::message_field(params, "taskId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let mut metadata = HashMap::new();
    if let Some(skill_id) = wire::skill_id(params).cloned() {
        metadata.insert("skillId".to_owned(), skill_id);
    }
    if let Some(settlement) = wire::settlement(params).cloned() {
        metadata.insert("settlement".to_owned(), settlement);
    }
    Ok(SendMessageRequest {
        message,
        configuration: params
            .pointer("/configuration/returnImmediately")
            .and_then(Value::as_bool)
            .map(|return_immediately| SendMessageConfiguration {
                accepted_output_modes: None,
                task_push_notification_config: None,
                history_length: None,
                return_immediately: Some(return_immediately),
            }),
        metadata: (!metadata.is_empty()).then_some(metadata),
        tenant: None,
    })
}

fn a2a_v1_part(part: &Value) -> Result<Part, String> {
    let mut normalized = part.clone();
    if let Some(object) = normalized.as_object_mut() {
        object.remove("kind");
    }
    serde_json::from_value(normalized)
        .map_err(|error| format!("convert A2A v1 message part: {error}"))
}

fn service_session_from_envelope(
    envelope: &SwarmAgentEnvelope,
    published_agent_id: &str,
) -> Option<RuntimeSessionContext> {
    let caller_agent_did = envelope.source_agent_id.clone()?;
    Some(RuntimeSessionContext::servicenet(
        caller_agent_did,
        published_agent_id.to_owned(),
        "mainnet:watt-etheria",
    ))
}

fn bridge_prompt(params: &Value, message: &str, envelope: &SwarmAgentEnvelope) -> String {
    let caller = envelope
        .source_agent_id
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let caller_public_id = envelope
        .extensions
        .as_ref()
        .and_then(|value| string_at(value, &["caller_public_id"]))
        .unwrap_or_else(|| "unknown".to_owned());
    let input = value_at(params, &["message", "parts"])
        .cloned()
        .unwrap_or(Value::Null);
    format!(
        "You are the published Wattetheria ServiceNet agent. Return strict JSON object {{\"message\":\"...\"}}. Caller agent DID: {caller}. Caller public id: {caller_public_id}. User message: {message}. A2A parts: {input}"
    )
}

fn bridge_output_text(output: &str) -> String {
    serde_json::from_str::<Value>(output)
        .ok()
        .and_then(|value| {
            string_at(&value, &["message"])
                .or_else(|| string_at(&value, &["answer"]))
                .or_else(|| string_at(&value, &["response"]))
        })
        .unwrap_or_else(|| output.to_owned())
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    value_at(value, path)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(segment)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, header::AUTHORIZATION},
        routing::post,
    };
    use tokio::sync::Mutex;

    async fn capture_authorization(
        State(captured): State<Arc<Mutex<Option<String>>>>,
        headers: HeaderMap,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        *captured.lock().await = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        Json(json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {
                "message": {
                    "messageId": "reply-1",
                    "role": "ROLE_AGENT",
                    "parts": [{"text": "ok"}]
                }
            }
        }))
    }

    #[test]
    fn customized_request_does_not_forward_internal_agent_envelope() {
        let params = json!({
            "contextId": "ctx-1",
            "skillId": "rides.book",
            "message": {
                "role": "user",
                "parts": [
                    {"kind": "text", "text": "book a ride"},
                    {"kind": "data", "data": {"pickup": "airport"}}
                ]
            },
            "extensions": {
                "agent_envelope": {"signature": "internal"},
                "settlement": {"rail": "x402"}
            }
        });

        let request = a2a_v1_request(&params).expect("request should convert");
        let serialized = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(serialized["message"]["contextId"], "ctx-1");
        assert_eq!(serialized["metadata"]["skillId"], "rides.book");
        assert_eq!(serialized["metadata"]["settlement"]["rail"], "x402");
        assert!(serialized.pointer("/metadata/agent_envelope").is_none());
    }

    #[test]
    fn customized_request_preserves_a2a_file_parts() {
        let params = json!({
            "message": {
                "role": "user",
                "parts": [{
                    "kind": "file",
                    "url": "https://files.example.com/invoice.pdf",
                    "filename": "invoice.pdf",
                    "mediaType": "application/pdf"
                }]
            }
        });

        let request = a2a_v1_request(&params).expect("file part should convert");
        let serialized = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(
            serialized["message"]["parts"][0]["url"],
            "https://files.example.com/invoice.pdf"
        );
        assert_eq!(
            serialized["message"]["parts"][0]["mediaType"],
            "application/pdf"
        );
    }

    #[tokio::test]
    async fn customized_agent_forwards_adapter_authorization() {
        let captured = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route("/", post(capture_authorization))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let result = A2aV1Executor
            .send_message(
                &endpoint,
                &json!({
                    "message": {
                        "role": "user",
                        "parts": [{"kind": "text", "text": "hello"}]
                    }
                }),
                Some("Bearer secret-token"),
            )
            .await
            .unwrap();

        assert_eq!(
            captured.lock().await.as_deref(),
            Some("Bearer secret-token")
        );
        assert_eq!(result["message"]["parts"][0]["text"], "ok");
    }
}
