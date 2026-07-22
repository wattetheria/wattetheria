use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use reqwest::Url;
use serde_json::{Value, json};

use super::{
    ServiceNetCancelAgentTaskRequest, ServiceNetClient, ServiceNetClientError,
    ServiceNetGetAgentTaskRequest, ServiceNetInvokeRequest, ServiceNetInvokeResponse,
    ServiceNetListAgentTasksRequest, ServiceNetSubscribeAgentTaskRequest,
    direct::build_direct_response,
};

const AGENT_ENVELOPE_HEADER: &str = "x-wattetheria-agent-envelope";
const DEFAULT_SUBSCRIPTION_MAX_EVENTS: u32 = 20;
const MAX_SUBSCRIPTION_MAX_EVENTS: u32 = 100;
const DEFAULT_SUBSCRIPTION_WAIT_MS: u64 = 30_000;
const MAX_SUBSCRIPTION_WAIT_MS: u64 = 120_000;

pub(super) async fn get_agent_task(
    client: &ServiceNetClient,
    agent_id: &str,
    task_id: &str,
    request: &ServiceNetGetAgentTaskRequest,
    published_agent: &Value,
) -> std::result::Result<ServiceNetInvokeResponse, ServiceNetClientError> {
    let raw = task_jsonrpc_request(
        client,
        published_agent,
        "GetTask",
        json!({"id": task_id, "historyLength": request.history_length}),
        request.auth_token.as_deref(),
        request.auth_context_id,
        request.agent_envelope.as_ref(),
    )
    .await
    .map_err(|error| ServiceNetClient::client_error(&error))?;
    let response = build_direct_response(agent_id, &ServiceNetInvokeRequest::default(), raw)
        .map_err(|error| ServiceNetClient::client_error(&error))?;
    verify_direct_task_response(
        client,
        published_agent,
        request.agent_envelope.as_ref(),
        &response,
    )?;
    Ok(response)
}

pub(super) async fn list_agent_tasks(
    client: &ServiceNetClient,
    agent_id: &str,
    request: &ServiceNetListAgentTasksRequest,
    published_agent: &Value,
) -> std::result::Result<ServiceNetInvokeResponse, ServiceNetClientError> {
    let raw = task_jsonrpc_request(
        client,
        published_agent,
        "ListTasks",
        json!({
            "contextId": request.context_id,
            "status": request.status,
            "pageSize": request.page_size,
            "pageToken": request.page_token,
            "historyLength": request.history_length,
            "statusTimestampAfter": request.status_timestamp_after,
            "includeArtifacts": request.include_artifacts,
        }),
        request.auth_token.as_deref(),
        request.auth_context_id,
        request.agent_envelope.as_ref(),
    )
    .await
    .map_err(|error| ServiceNetClient::client_error(&error))?;
    let response = build_direct_response(agent_id, &ServiceNetInvokeRequest::default(), raw)
        .map_err(|error| ServiceNetClient::client_error(&error))?;
    verify_direct_task_response(
        client,
        published_agent,
        request.agent_envelope.as_ref(),
        &response,
    )?;
    Ok(response)
}

pub(super) async fn cancel_agent_task(
    client: &ServiceNetClient,
    agent_id: &str,
    task_id: &str,
    request: &ServiceNetCancelAgentTaskRequest,
    published_agent: &Value,
) -> std::result::Result<ServiceNetInvokeResponse, ServiceNetClientError> {
    let raw = task_jsonrpc_request(
        client,
        published_agent,
        "CancelTask",
        json!({"id": task_id}),
        request.auth_token.as_deref(),
        request.auth_context_id,
        request.agent_envelope.as_ref(),
    )
    .await
    .map_err(|error| ServiceNetClient::client_error(&error))?;
    let response = build_direct_response(agent_id, &ServiceNetInvokeRequest::default(), raw)
        .map_err(|error| ServiceNetClient::client_error(&error))?;
    verify_direct_task_response(
        client,
        published_agent,
        request.agent_envelope.as_ref(),
        &response,
    )?;
    Ok(response)
}

pub(super) async fn subscribe_agent_task(
    client: &ServiceNetClient,
    agent_id: &str,
    task_id: &str,
    request: &ServiceNetSubscribeAgentTaskRequest,
    published_agent: &Value,
) -> std::result::Result<ServiceNetInvokeResponse, ServiceNetClientError> {
    let direct_url =
        direct_url(published_agent).map_err(|error| ServiceNetClient::client_error(&error))?;
    reject_unresolved_direct_auth_context(request.auth_context_id)
        .map_err(|error| ServiceNetClient::client_error(&error))?;
    let envelope = encoded_agent_envelope(request.agent_envelope.as_ref())
        .map_err(|error| ServiceNetClient::client_error(&error))?;
    let mut builder = client
        .http_client
        .post(direct_url.clone())
        .header("A2A-Version", "1.0")
        .header("Accept", "text/event-stream")
        .header(AGENT_ENVELOPE_HEADER, envelope)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": uuid::Uuid::new_v4().to_string(),
            "method": "SubscribeToTask",
            "params": {"id": task_id},
        }));
    if let Some(token) = request.auth_token.as_deref() {
        builder = builder.bearer_auth(token);
    }
    let response = builder
        .send()
        .await
        .with_context(|| format!("request Wattetheria Adapter subscription {direct_url}"))
        .map_err(|error| ServiceNetClient::client_error(&error))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(ServiceNetClient::client_error(&anyhow::anyhow!(
            "Wattetheria Adapter {direct_url} returned {status}: {body}"
        )));
    }
    let max_events = request
        .max_events
        .unwrap_or(DEFAULT_SUBSCRIPTION_MAX_EVENTS)
        .clamp(1, MAX_SUBSCRIPTION_MAX_EVENTS) as usize;
    let wait_ms = request
        .wait_timeout_ms
        .unwrap_or(DEFAULT_SUBSCRIPTION_WAIT_MS)
        .clamp(1, MAX_SUBSCRIPTION_WAIT_MS);
    let events = collect_sse_events(response.bytes_stream(), max_events, wait_ms)
        .await
        .map_err(|error| ServiceNetClient::client_error(&error))?;
    verify_subscription_events(
        client,
        published_agent,
        request.agent_envelope.as_ref(),
        agent_id,
        &events,
    )?;
    Ok(ServiceNetInvokeResponse {
        agent_id: agent_id.to_owned(),
        status: "subscribed".to_owned(),
        receipt_id: None,
        task_id: Some(task_id.to_owned()),
        context_id: None,
        message: None,
        output: Some(json!({"events": events})),
        settlement: None,
        payment_receipt: None,
        service_signature: None,
        raw: json!({"result": {"events": events}}),
    })
}

async fn task_jsonrpc_request(
    client: &ServiceNetClient,
    published_agent: &Value,
    method: &str,
    params: Value,
    auth_token: Option<&str>,
    auth_context_id: Option<uuid::Uuid>,
    agent_envelope: Option<&Value>,
) -> Result<Value> {
    let direct_url = direct_url(published_agent)?;
    reject_unresolved_direct_auth_context(auth_context_id)?;
    let envelope = encoded_agent_envelope(agent_envelope)?;
    let mut builder = client
        .http_client
        .post(direct_url.clone())
        .header("A2A-Version", "1.0")
        .header(AGENT_ENVELOPE_HEADER, envelope)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": uuid::Uuid::new_v4().to_string(),
            "method": method,
            "params": params,
        }));
    if let Some(token) = auth_token {
        builder = builder.bearer_auth(token);
    }
    let response = builder
        .send()
        .await
        .with_context(|| format!("request Wattetheria Adapter {direct_url}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("read Wattetheria Adapter response")?;
    if !status.is_success() {
        bail!("Wattetheria Adapter {direct_url} returned {status}: {body}");
    }
    let raw: Value = serde_json::from_str(&body)
        .with_context(|| format!("parse Wattetheria Adapter response from {direct_url}"))?;
    if let Some(error) = raw.get("error") {
        bail!("Wattetheria Adapter rejected {method}: {error}");
    }
    Ok(raw)
}

fn direct_url(published_agent: &Value) -> Result<Url> {
    let direct_url = published_agent
        .pointer("/invoke/direct_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("published wattetheria_direct agent is missing invoke.direct_url")?;
    Url::parse(direct_url).context("parse published Adapter URL")
}

fn reject_unresolved_direct_auth_context(auth_context_id: Option<uuid::Uuid>) -> Result<()> {
    if auth_context_id.is_some() {
        bail!(
            "wattetheria_direct invocation cannot resolve a ServiceNet auth_context_id; provide auth_token directly"
        );
    }
    Ok(())
}

fn encoded_agent_envelope(envelope: Option<&Value>) -> Result<String> {
    let envelope = envelope.context("A2A task request is missing agent_envelope")?;
    Ok(STANDARD.encode(serde_json::to_vec(envelope)?))
}

fn verify_direct_task_response(
    client: &ServiceNetClient,
    published_agent: &Value,
    envelope: Option<&Value>,
    response: &ServiceNetInvokeResponse,
) -> std::result::Result<(), ServiceNetClientError> {
    client.verify_a2a_task_response(published_agent, envelope, response)
}

async fn collect_sse_events(
    mut stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
    max_events: usize,
    wait_timeout_ms: u64,
) -> Result<Vec<Value>> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(wait_timeout_ms);
    let mut buffer = Vec::new();
    let mut events = Vec::new();
    while events.len() < max_events {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Ok(Some(chunk)) = tokio::time::timeout(remaining, stream.next()).await else {
            break;
        };
        buffer.extend_from_slice(&chunk.context("read A2A subscription stream")?);
        while let Some(end) = sse_event_end(&buffer) {
            let event = buffer.drain(..end).collect::<Vec<_>>();
            if let Some(value) = parse_sse_event(&event)? {
                events.push(value);
                if events.len() == max_events {
                    break;
                }
            }
        }
    }
    Ok(events)
}

fn sse_event_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n" || window == b"\r\r")
        .map(|index| index + 2)
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| index + 4)
        })
}

fn parse_sse_event(event: &[u8]) -> Result<Option<Value>> {
    let text = std::str::from_utf8(event).context("decode A2A subscription event")?;
    let data = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_str(&data).context("parse A2A subscription event")?,
    ))
}

fn verify_subscription_events(
    client: &ServiceNetClient,
    published_agent: &Value,
    envelope: Option<&Value>,
    agent_id: &str,
    events: &[Value],
) -> std::result::Result<(), ServiceNetClientError> {
    for event in events {
        let signature = super::service_signature_from_raw(event).ok_or_else(|| {
            ServiceNetClient::client_error(&anyhow::anyhow!(
                "A2A subscription event is missing Service Agent signature"
            ))
        })?;
        let response = ServiceNetInvokeResponse {
            agent_id: agent_id.to_owned(),
            status: "event".to_owned(),
            receipt_id: None,
            task_id: None,
            context_id: None,
            message: None,
            output: None,
            settlement: None,
            payment_receipt: None,
            service_signature: Some(signature),
            raw: event.clone(),
        };
        verify_direct_task_response(client, published_agent, envelope, &response)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
    use futures_util::stream;
    use std::sync::{Arc, Mutex};

    async fn capture_task_request(
        State(captured): State<Arc<Mutex<Vec<Value>>>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        captured.lock().expect("capture lock").push(json!({
            "method": body["method"],
            "authorization": headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            "agent_envelope": headers
                .get(AGENT_ENVELOPE_HEADER)
                .and_then(|value| value.to_str().ok()),
        }));
        Json(json!({
            "jsonrpc": "2.0",
            "id": body["id"],
            "result": {"metadata": {}},
        }))
    }

    #[tokio::test]
    async fn direct_task_transport_uses_published_adapter_url_and_signed_envelope_header() {
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = Router::new()
            .route("/provider/adapter", post(capture_task_request))
            .with_state(Arc::clone(&captured));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let address = listener.local_addr().expect("listener address");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock Adapter should run");
        });
        let client = ServiceNetClient::new("http://127.0.0.1:1").expect("client should build");
        let published = json!({
            "invoke": {"direct_url": format!("http://{address}/provider/adapter")}
        });
        let envelope = json!({
            "target_agent_id": "agent-1",
            "extensions": {"issued_at_ms": 9_007_199_254_740_993_u64},
        });

        for (method, params) in [
            ("GetTask", json!({"id": "task-1"})),
            ("ListTasks", json!({})),
            ("CancelTask", json!({"id": "task-1"})),
        ] {
            task_jsonrpc_request(
                &client,
                &published,
                method,
                params,
                Some("secret-token"),
                None,
                Some(&envelope),
            )
            .await
            .expect("direct A2A Task request should succeed");
        }

        let captured = captured.lock().expect("capture lock");
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[0]["method"], "GetTask");
        assert_eq!(captured[1]["method"], "ListTasks");
        assert_eq!(captured[2]["method"], "CancelTask");
        assert_eq!(captured[0]["authorization"], "Bearer secret-token");
        let encoded = captured[0]["agent_envelope"]
            .as_str()
            .expect("agent envelope header should be present");
        assert_eq!(
            STANDARD.decode(encoded).expect("header should decode"),
            serde_json::to_vec(&envelope).expect("envelope should serialize")
        );
    }

    #[tokio::test]
    async fn subscription_timeout_preserves_events_already_received() {
        let first = stream::iter(vec![Ok::<_, reqwest::Error>(bytes::Bytes::from_static(
            b"data: {\"result\":{\"task\":{\"id\":\"task-1\"}}}\n\n",
        ))]);
        let stalled = stream::pending::<Result<bytes::Bytes, reqwest::Error>>();

        let events = collect_sse_events(first.chain(stalled), 2, 10)
            .await
            .expect("partial subscription batch should be retained");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["result"]["task"]["id"], "task-1");
    }
}
