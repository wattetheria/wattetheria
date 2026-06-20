use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;
use wattetheria_kernel::local_db;
use wattetheria_kernel::servicenet::{
    ServiceNetGetAgentTaskRequest, ServiceNetInvokeRequest, ServiceNetInvokeResponse,
};
use wattetheria_social::ports::repositories::ReliabilityTaskRepository;

use crate::state::ControlPlaneState;

const SERVICENET_ASYNC_OBJECT_KIND: &str = "servicenet_async_invocation";
const SERVICENET_ASYNC_INITIAL_POLL_DELAY_SEC: i64 = 10;
const SERVICENET_ASYNC_POLL_DELAY_SEC: [i64; 6] = [15, 30, 60, 120, 300, 600];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ServiceNetAsyncInvocation {
    pub(crate) invocation_id: String,
    pub(crate) agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) receipt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context_id: Option<String>,
    #[serde(default)]
    pub(crate) status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent_envelope: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_payload: Option<Value>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completed_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ServiceNetAsyncInvocationStore {
    #[serde(default)]
    pub(crate) invocations: BTreeMap<String, ServiceNetAsyncInvocation>,
}

pub(crate) fn record_servicenet_async_invocation(
    state: &ControlPlaneState,
    agent_id: &str,
    request: &ServiceNetInvokeRequest,
    response: &ServiceNetInvokeResponse,
    agent_envelope: Value,
) -> anyhow::Result<Option<String>> {
    let Some(invocation_id) = servicenet_async_invocation_id(response) else {
        return Ok(None);
    };
    let now = chrono::Utc::now().timestamp();
    let mut store = load_servicenet_async_invocations(state)?;
    store.invocations.insert(
        invocation_id.clone(),
        ServiceNetAsyncInvocation {
            invocation_id: invocation_id.clone(),
            agent_id: agent_id.to_owned(),
            task_id: response.task_id.clone().or_else(|| request.task_id.clone()),
            receipt_id: response.receipt_id.map(|receipt_id| receipt_id.to_string()),
            context_id: response
                .context_id
                .clone()
                .or_else(|| request.context_id.clone()),
            status: response.status.clone(),
            agent_envelope: Some(agent_envelope),
            last_payload: Some(serde_json::to_value(response).unwrap_or(Value::Null)),
            created_at: now,
            updated_at: now,
            completed_at: None,
        },
    );
    save_servicenet_async_invocations(state, &store)?;
    state
        .social_store
        .defer_reliability_task(
            SERVICENET_ASYNC_OBJECT_KIND,
            &invocation_id,
            now,
            now + SERVICENET_ASYNC_INITIAL_POLL_DELAY_SEC,
            None,
        )
        .map_err(anyhow::Error::msg)?;
    Ok(Some(invocation_id))
}

pub(crate) async fn maintain_servicenet_async_invocations(
    state: &ControlPlaneState,
    now: i64,
    limit: usize,
) -> anyhow::Result<usize> {
    if limit == 0 || state.servicenet_client.is_none() {
        return Ok(0);
    }
    let due = due_servicenet_async_invocations(state, now, limit)?;
    if due.is_empty() {
        return Ok(0);
    }
    let mut processed = 0;
    for invocation in due {
        maintain_servicenet_async_invocation(state, invocation, now)
            .await
            .with_context(|| "maintain ServiceNet async invocation")?;
        processed += 1;
    }
    Ok(processed)
}

fn due_servicenet_async_invocations(
    state: &ControlPlaneState,
    now: i64,
    limit: usize,
) -> anyhow::Result<Vec<ServiceNetAsyncInvocation>> {
    let store = load_servicenet_async_invocations(state)?;
    let mut due = Vec::new();
    for invocation in store.invocations.values() {
        if invocation.completed_at.is_some() {
            continue;
        }
        let task = state
            .social_store
            .get_reliability_task(SERVICENET_ASYNC_OBJECT_KIND, &invocation.invocation_id)
            .map_err(anyhow::Error::msg)?;
        let next_due = task.as_ref().map_or(
            invocation.updated_at + SERVICENET_ASYNC_INITIAL_POLL_DELAY_SEC,
            |task| task.next_attempt_at,
        );
        if next_due <= now {
            due.push((next_due, invocation.updated_at, invocation.clone()));
        }
    }
    due.sort_by_key(|item| (item.0, item.1));
    Ok(due
        .into_iter()
        .take(limit)
        .map(|(_, _, invocation)| invocation)
        .collect())
}

async fn maintain_servicenet_async_invocation(
    state: &ControlPlaneState,
    invocation: ServiceNetAsyncInvocation,
    now: i64,
) -> anyhow::Result<()> {
    let Some(client) = state.servicenet_client.as_deref() else {
        return Ok(());
    };
    let poll_result = if let Some(task_id) = invocation.task_id.as_deref() {
        client
            .get_agent_task(
                &invocation.agent_id,
                task_id,
                &ServiceNetGetAgentTaskRequest::default(),
            )
            .await
            .map(|response| serde_json::to_value(response).unwrap_or(Value::Null))
    } else if let Some(receipt_id) = invocation.receipt_id.as_deref() {
        let receipt_id = Uuid::parse_str(receipt_id).context("parse ServiceNet receipt id")?;
        client.get_receipt(&receipt_id).await
    } else {
        return complete_servicenet_async_invocation(
            state,
            invocation,
            now,
            json!({"status": "failed", "error": "missing task_id and receipt_id"}),
        )
        .await;
    };

    match poll_result {
        Ok(payload) => {
            update_polled_servicenet_async_invocation(state, invocation, now, payload).await
        }
        Err(error) => {
            let message = error.to_string();
            defer_servicenet_async_invocation(state, &invocation, now, Some(&message))
        }
    }
}

async fn update_polled_servicenet_async_invocation(
    state: &ControlPlaneState,
    invocation: ServiceNetAsyncInvocation,
    now: i64,
    payload: Value,
) -> anyhow::Result<()> {
    let status = servicenet_payload_status(&payload);
    if terminal_servicenet_status(&status) {
        return complete_servicenet_async_invocation(state, invocation, now, payload).await;
    }
    let mut store = load_servicenet_async_invocations(state)?;
    if let Some(stored) = store.invocations.get_mut(&invocation.invocation_id) {
        stored.status = status;
        stored.last_payload = Some(payload);
        stored.updated_at = now;
    }
    save_servicenet_async_invocations(state, &store)?;
    defer_servicenet_async_invocation(state, &invocation, now, None)
}

async fn complete_servicenet_async_invocation(
    state: &ControlPlaneState,
    mut invocation: ServiceNetAsyncInvocation,
    now: i64,
    payload: Value,
) -> anyhow::Result<()> {
    invocation.status = servicenet_payload_status(&payload);
    invocation.last_payload = Some(payload.clone());
    invocation.completed_at = Some(now);
    invocation.updated_at = now;
    let mut store = load_servicenet_async_invocations(state)?;
    store
        .invocations
        .insert(invocation.invocation_id.clone(), invocation.clone());
    save_servicenet_async_invocations(state, &store)?;
    state
        .social_store
        .clear_reliability_task(SERVICENET_ASYNC_OBJECT_KIND, &invocation.invocation_id)
        .map_err(anyhow::Error::msg)?;
    let agent_envelope = invocation.agent_envelope.as_ref();
    Box::pin(super::notify_local_agent_of_third_party_result(
        state,
        "async_result",
        &invocation.agent_id,
        invocation.task_id.as_deref(),
        &payload,
        agent_envelope,
    ))
    .await;
    Ok(())
}

fn defer_servicenet_async_invocation(
    state: &ControlPlaneState,
    invocation: &ServiceNetAsyncInvocation,
    now: i64,
    last_error: Option<&str>,
) -> anyhow::Result<()> {
    let attempt_count = state
        .social_store
        .get_reliability_task(SERVICENET_ASYNC_OBJECT_KIND, &invocation.invocation_id)
        .map_err(anyhow::Error::msg)?
        .map_or(0, |task| task.attempt_count);
    state
        .social_store
        .record_reliability_attempt(
            SERVICENET_ASYNC_OBJECT_KIND,
            &invocation.invocation_id,
            now,
            now + servicenet_async_retry_delay_after_attempt(attempt_count),
            last_error,
        )
        .map_err(anyhow::Error::msg)?;
    Ok(())
}

fn servicenet_async_retry_delay_after_attempt(attempt_count: i64) -> i64 {
    let index = usize::try_from(attempt_count.max(0)).unwrap_or(usize::MAX);
    SERVICENET_ASYNC_POLL_DELAY_SEC
        .get(index)
        .copied()
        .unwrap_or(*SERVICENET_ASYNC_POLL_DELAY_SEC.last().unwrap_or(&600))
}

fn servicenet_async_invocation_id(response: &ServiceNetInvokeResponse) -> Option<String> {
    response
        .task_id
        .as_ref()
        .map(ToOwned::to_owned)
        .or_else(|| response.receipt_id.map(|receipt_id| receipt_id.to_string()))
}

fn servicenet_payload_status(payload: &Value) -> String {
    payload
        .get("status")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/receipt/status").and_then(Value::as_str))
        .unwrap_or("unknown")
        .to_owned()
}

fn terminal_servicenet_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "complete" | "succeeded" | "success" | "failed" | "error" | "cancelled"
    )
}

fn load_servicenet_async_invocations(
    state: &ControlPlaneState,
) -> anyhow::Result<ServiceNetAsyncInvocationStore> {
    state
        .local_db
        .load_domain_or_default(local_db::domain::SERVICENET_ASYNC_INVOCATIONS)
}

fn save_servicenet_async_invocations(
    state: &ControlPlaneState,
    store: &ServiceNetAsyncInvocationStore,
) -> anyhow::Result<()> {
    state
        .local_db
        .save_domain(local_db::domain::SERVICENET_ASYNC_INVOCATIONS, store)
}

#[cfg(test)]
mod tests {
    use super::servicenet_async_retry_delay_after_attempt;

    #[test]
    fn retry_delay_uses_first_backoff_for_first_recorded_attempt() {
        assert_eq!(servicenet_async_retry_delay_after_attempt(0), 15);
        assert_eq!(servicenet_async_retry_delay_after_attempt(1), 30);
        assert_eq!(servicenet_async_retry_delay_after_attempt(5), 600);
        assert_eq!(servicenet_async_retry_delay_after_attempt(6), 600);
    }
}
