use crate::state::{ControlPlaneState, StreamEvent};
use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Status};
use tracing::{debug, warn};
use wattetheria_kernel::civilization::topics::TopicProfile;
use wattetheria_kernel::local_db::LocalDb;
use wattetheria_kernel::swarm_sync::{SwarmTaskRunProjectionSnapshot, SwarmTopicActivitySnapshot};

#[allow(
    clippy::doc_markdown,
    clippy::default_trait_access,
    clippy::too_many_lines
)]
pub mod proto {
    tonic::include_proto!("wattswarm.wattetheria.sync");
}

use proto::wattetheria_sync_service_client::WattetheriaSyncServiceClient;
use proto::{ProjectionFrame, ProjectionStreamRequest};

const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;
const DEFAULT_STREAM_LIMIT: u32 = 50;
const TOPIC_SUPERVISOR_INTERVAL_SEC: u64 = 10;
const RECONNECT_DELAY_SEC: u64 = 2;
pub const DEFAULT_WATTSWARM_SYNC_GRPC_PORT: u16 = 7791;

const NETWORK_PROJECTION_CACHE_DOMAIN: &str = "wattswarm.sync.cache.network_projection";
const TASK_RUN_PROJECTION_CACHE_DOMAIN: &str = "wattswarm.sync.cache.task_run_projection";
const CACHE_MAX_AGE_SEC: i64 = 30;

#[must_use]
pub fn spawn_wattswarm_sync_bridge(
    state: ControlPlaneState,
    grpc_endpoint: Option<String>,
) -> Option<JoinHandle<()>> {
    let grpc_endpoint = grpc_endpoint
        .map(|endpoint| endpoint.trim().to_string())
        .filter(|endpoint| !endpoint.is_empty())?;
    Some(tokio::spawn(async move {
        let network_task = tokio::spawn(run_network_projection_stream(
            state.clone(),
            grpc_endpoint.clone(),
        ));
        let task_run_task = tokio::spawn(run_task_run_projection_stream(
            state.clone(),
            grpc_endpoint.clone(),
        ));
        let topic_task = tokio::spawn(run_topic_projection_supervisor(state, grpc_endpoint));
        let _ = tokio::join!(network_task, task_run_task, topic_task);
    }))
}

async fn connect_client(grpc_endpoint: &str) -> Result<WattetheriaSyncServiceClient<Channel>> {
    let endpoint = Endpoint::from_shared(grpc_endpoint.to_string())
        .with_context(|| format!("invalid wattswarm sync gRPC endpoint {grpc_endpoint}"))?;
    let channel = endpoint
        .connect()
        .await
        .with_context(|| format!("connect wattswarm sync gRPC endpoint {grpc_endpoint}"))?;
    Ok(WattetheriaSyncServiceClient::new(channel))
}

async fn run_network_projection_stream(state: ControlPlaneState, grpc_endpoint: String) {
    loop {
        match network_projection_session(state.clone(), &grpc_endpoint).await {
            Ok(()) => debug!("wattswarm network projection stream closed cleanly"),
            Err(error) => warn!("wattswarm network projection stream failed: {error:#}"),
        }
        sleep(Duration::from_secs(RECONNECT_DELAY_SEC)).await;
    }
}

async fn network_projection_session(state: ControlPlaneState, grpc_endpoint: &str) -> Result<()> {
    let mut client = connect_client(grpc_endpoint).await?;
    let request = Request::new(ProjectionStreamRequest {
        poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
        limit: DEFAULT_STREAM_LIMIT,
        feed_key: String::new(),
        scope_hint: String::new(),
        subscriber_node_id: String::new(),
    });
    let mut stream = client
        .stream_network_projection(request)
        .await
        .context("open wattswarm network projection stream")?
        .into_inner();
    while let Some(frame) = stream
        .message()
        .await
        .context("read wattswarm network projection frame")?
    {
        emit_projection_frame(&state, "wattswarm.sync.network_projection", &frame).await;
    }
    Ok(())
}

async fn run_task_run_projection_stream(state: ControlPlaneState, grpc_endpoint: String) {
    loop {
        match task_run_projection_session(state.clone(), &grpc_endpoint).await {
            Ok(()) => debug!("wattswarm task/run projection stream closed cleanly"),
            Err(error) => warn!("wattswarm task/run projection stream failed: {error:#}"),
        }
        sleep(Duration::from_secs(RECONNECT_DELAY_SEC)).await;
    }
}

async fn task_run_projection_session(state: ControlPlaneState, grpc_endpoint: &str) -> Result<()> {
    let mut client = connect_client(grpc_endpoint).await?;
    let request = Request::new(ProjectionStreamRequest {
        poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
        limit: DEFAULT_STREAM_LIMIT,
        feed_key: String::new(),
        scope_hint: String::new(),
        subscriber_node_id: String::new(),
    });
    let mut stream = client
        .stream_task_run_projection(request)
        .await
        .context("open wattswarm task/run projection stream")?
        .into_inner();
    while let Some(frame) = stream
        .message()
        .await
        .context("read wattswarm task/run projection frame")?
    {
        emit_projection_frame(&state, "wattswarm.sync.task_run_projection", &frame).await;
    }
    Ok(())
}

async fn run_topic_projection_supervisor(state: ControlPlaneState, grpc_endpoint: String) {
    let mut topic_tasks: HashMap<String, JoinHandle<()>> = HashMap::new();

    loop {
        let topics = state.topic_registry.lock().await.list();
        let active_keys = topics
            .iter()
            .filter(|topic| topic.active)
            .map(topic_stream_key)
            .collect::<BTreeSet<_>>();

        topic_tasks.retain(|key, handle| {
            let keep = active_keys.contains(key) && !handle.is_finished();
            if !keep {
                handle.abort();
            }
            keep
        });

        for topic in topics.into_iter().filter(|topic| topic.active) {
            let key = topic_stream_key(&topic);
            if topic_tasks.contains_key(&key) {
                continue;
            }
            let state_clone = state.clone();
            let grpc_endpoint_clone = grpc_endpoint.clone();
            topic_tasks.insert(
                key,
                tokio::spawn(async move {
                    run_topic_projection_stream(state_clone, grpc_endpoint_clone, topic).await;
                }),
            );
        }

        sleep(Duration::from_secs(TOPIC_SUPERVISOR_INTERVAL_SEC)).await;
    }
}

async fn run_topic_projection_stream(
    state: ControlPlaneState,
    grpc_endpoint: String,
    topic: TopicProfile,
) {
    loop {
        match topic_projection_session(state.clone(), &grpc_endpoint, &topic).await {
            Ok(()) => debug!(
                feed_key = %topic.feed_key,
                scope_hint = %topic.scope_hint,
                "wattswarm topic projection stream closed cleanly"
            ),
            Err(error) => {
                if is_retryable_status(&error) {
                    debug!(
                        feed_key = %topic.feed_key,
                        scope_hint = %topic.scope_hint,
                        "wattswarm topic projection stream ended and will retry: {error:#}"
                    );
                } else {
                    warn!(
                        feed_key = %topic.feed_key,
                        scope_hint = %topic.scope_hint,
                        "wattswarm topic projection stream failed: {error:#}"
                    );
                }
            }
        }
        sleep(Duration::from_secs(RECONNECT_DELAY_SEC)).await;
    }
}

async fn topic_projection_session(
    state: ControlPlaneState,
    grpc_endpoint: &str,
    topic: &TopicProfile,
) -> Result<()> {
    let mut client = connect_client(grpc_endpoint).await?;
    let request = Request::new(ProjectionStreamRequest {
        poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
        limit: DEFAULT_STREAM_LIMIT,
        feed_key: topic.feed_key.clone(),
        scope_hint: topic.scope_hint.clone(),
        subscriber_node_id: String::new(),
    });
    let mut stream = client
        .stream_topic_activity(request)
        .await
        .with_context(|| {
            format!(
                "open wattswarm topic projection stream {}@{}",
                topic.feed_key, topic.scope_hint
            )
        })?
        .into_inner();
    while let Some(frame) = stream.message().await.with_context(|| {
        format!(
            "read wattswarm topic projection frame {}@{}",
            topic.feed_key, topic.scope_hint
        )
    })? {
        emit_projection_frame(&state, "wattswarm.sync.topic_activity", &frame).await;
    }
    Ok(())
}

async fn emit_projection_frame(
    state: &ControlPlaneState,
    event_kind: &str,
    frame: &ProjectionFrame,
) {
    let payload = serde_json::from_str::<Value>(&frame.json_payload).unwrap_or_else(|_| {
        json!({
            "raw_json_payload": frame.json_payload,
        })
    });
    let local_db = state.local_db.clone();
    let kind = frame.kind.clone();
    let payload_clone = payload.clone();
    let _ = tokio::task::spawn_blocking(move || {
        persist_projection_frame(&local_db, &kind, &payload_clone);
    })
    .await;
    let _ = state.stream_tx.send(StreamEvent {
        kind: event_kind.to_string(),
        timestamp: Utc::now().timestamp(),
        payload: json!({
            "source": "wattswarm",
            "projection_kind": frame.kind,
            "cursor": frame.cursor,
            "generated_at": frame.generated_at,
            "payload": payload,
        }),
    });
}

fn persist_projection_frame(local_db: &LocalDb, kind: &str, payload: &Value) {
    match kind {
        "network_projection" => {
            if let Err(error) = local_db.save_domain(NETWORK_PROJECTION_CACHE_DOMAIN, payload) {
                warn!("persist wattswarm network projection cache failed: {error:#}");
            }
        }
        "task_run_projection" => {
            match serde_json::from_value::<SwarmTaskRunProjectionSnapshot>(payload.clone()) {
                Ok(snapshot) => {
                    if let Err(error) =
                        local_db.save_domain(TASK_RUN_PROJECTION_CACHE_DOMAIN, &snapshot)
                    {
                        warn!("persist wattswarm task/run projection cache failed: {error:#}");
                    }
                }
                Err(error) => warn!("decode wattswarm task/run projection cache failed: {error:#}"),
            }
        }
        "topic_activity" => {
            match serde_json::from_value::<SwarmTopicActivitySnapshot>(payload.clone()) {
                Ok(snapshot) => {
                    if let Err(error) = local_db.save_domain(
                        &topic_activity_cache_domain(&snapshot.feed_key, &snapshot.scope_hint),
                        &snapshot,
                    ) {
                        warn!("persist wattswarm topic activity cache failed: {error:#}");
                    }
                }
                Err(error) => warn!("decode wattswarm topic activity cache failed: {error:#}"),
            }
        }
        _ => {}
    }
}

pub(crate) async fn load_cached_task_run_projection(
    local_db: &Arc<LocalDb>,
) -> Option<SwarmTaskRunProjectionSnapshot> {
    let db = local_db.clone();
    tokio::task::spawn_blocking(move || {
        match db.load_domain_if_fresh(TASK_RUN_PROJECTION_CACHE_DOMAIN, CACHE_MAX_AGE_SEC) {
            Ok(value) => value,
            Err(error) => {
                warn!("load task/run projection cache failed: {error:#}");
                None
            }
        }
    })
    .await
    .unwrap_or(None)
}

pub(crate) async fn load_cached_topic_activity(
    local_db: &Arc<LocalDb>,
    feed_key: &str,
    scope_hint: &str,
) -> Option<SwarmTopicActivitySnapshot> {
    let db = local_db.clone();
    let domain = topic_activity_cache_domain(feed_key, scope_hint);
    tokio::task::spawn_blocking(
        move || match db.load_domain_if_fresh(&domain, CACHE_MAX_AGE_SEC) {
            Ok(value) => value,
            Err(error) => {
                warn!("load topic activity cache failed: {error:#}");
                None
            }
        },
    )
    .await
    .unwrap_or(None)
}

fn topic_activity_cache_domain(feed_key: &str, scope_hint: &str) -> String {
    format!("wattswarm.sync.cache.topic_activity::{feed_key}::{scope_hint}")
}

fn topic_stream_key(topic: &TopicProfile) -> String {
    format!("{}::{}", topic.feed_key, topic.scope_hint)
}

fn is_retryable_status(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<Status>())
        .is_some_and(|status| matches!(status.code(), Code::Unavailable | Code::Cancelled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use wattetheria_kernel::swarm_bridge::{SwarmTopicCursorView, SwarmTopicMessageView};

    #[test]
    fn topic_stream_key_uses_feed_and_scope() {
        let topic = TopicProfile {
            topic_id: "topic-1".to_string(),
            feed_key: "guild.chat".to_string(),
            scope_hint: "guild:defi".to_string(),
            display_name: "DeFi Guild".to_string(),
            summary: None,
            projection_kind: wattetheria_kernel::civilization::topics::TopicProjectionKind::Guild,
            organization_id: None,
            mission_id: None,
            participant_public_ids: Vec::new(),
            created_by_public_id: "did:key:zcreator".to_string(),
            why_this_exists: None,
            active: true,
            created_at: 0,
            updated_at: 0,
        };
        assert_eq!(topic_stream_key(&topic), "guild.chat::guild:defi");
    }

    #[tokio::test]
    async fn task_run_projection_cache_roundtrip() {
        let db = Arc::new(LocalDb::open_in_memory().expect("open local db"));
        let snapshot = SwarmTaskRunProjectionSnapshot {
            generated_at: 42,
            recent_tasks: Vec::new(),
            recent_runs: vec![json!({"run_id": "run-1"})],
        };
        db.save_domain(TASK_RUN_PROJECTION_CACHE_DOMAIN, &snapshot)
            .expect("save task/run projection");
        assert_eq!(load_cached_task_run_projection(&db).await, Some(snapshot));
    }

    #[tokio::test]
    async fn topic_activity_cache_roundtrip() {
        let db = Arc::new(LocalDb::open_in_memory().expect("open local db"));
        let snapshot = SwarmTopicActivitySnapshot {
            generated_at: 42,
            subscriber_node_id: "node-1".to_string(),
            feed_key: "guild.chat".to_string(),
            scope_hint: "guild:defi".to_string(),
            messages: vec![SwarmTopicMessageView {
                message_id: "msg-1".to_string(),
                network_id: "net".to_string(),
                feed_key: "guild.chat".to_string(),
                scope_hint: "guild:defi".to_string(),
                author_node_id: "node-1".to_string(),
                content: json!({"text": "hello"}),
                reply_to_message_id: None,
                created_at: 10,
            }],
            cursor: Some(SwarmTopicCursorView {
                subscriber_node_id: "node-1".to_string(),
                feed_key: "guild.chat".to_string(),
                scope_hint: "guild:defi".to_string(),
                last_event_seq: 1,
                updated_at: 11,
            }),
        };
        db.save_domain(
            &topic_activity_cache_domain("guild.chat", "guild:defi"),
            &snapshot,
        )
        .expect("save topic activity");
        assert_eq!(
            load_cached_topic_activity(&db, "guild.chat", "guild:defi").await,
            Some(snapshot)
        );
    }
}
