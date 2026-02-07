//! Non-authoritative observatory API for signed summaries, rankings, and mirror replication.

use anyhow::Result;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use tower_http::cors::{Any, CorsLayer};

use wattetheria_kernel::signing::verify_payload;
use wattetheria_kernel::types::{SignedSummary, TaskStats};

#[derive(Debug, Clone, Serialize)]
pub struct HeatPoint {
    pub subnet_id: String,
    pub active_agents: usize,
    pub total_watt: i64,
    pub total_power: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RankingEntry {
    pub agent_id: String,
    pub subnet_id: String,
    pub metric: String,
    pub value: i64,
    pub power: i64,
    pub watt: i64,
    pub task_stats: TaskStats,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventStreamEntry {
    pub timestamp: i64,
    pub agent_id: String,
    pub subnet_id: String,
    pub events_digest: String,
    pub watt: i64,
    pub power: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanetHealthEntry {
    pub subnet_id: String,
    pub active_agents: usize,
    pub total_watt: i64,
    pub total_power: i64,
    pub total_contribution: i64,
    pub avg_success_rate: f64,
    pub health_score: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreConfig {
    pub max_entries: usize,
    pub max_entry_age_sec: i64,
    pub max_ingest_per_agent_per_minute: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_entry_age_sec: 7 * 24 * 3600,
            max_ingest_per_agent_per_minute: 30,
        }
    }
}

impl StoreConfig {
    #[must_use]
    pub fn from_env() -> Self {
        let default = Self::default();
        let max_entries = std::env::var("OBS_MAX_ENTRIES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default.max_entries);

        let max_entry_age_sec = std::env::var("OBS_MAX_AGE_SEC")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default.max_entry_age_sec);

        let max_ingest_per_agent_per_minute = std::env::var("OBS_MAX_INGEST_PER_AGENT_MIN")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default.max_ingest_per_agent_per_minute);

        Self {
            max_entries,
            max_entry_age_sec,
            max_ingest_per_agent_per_minute,
        }
    }
}

#[derive(Debug)]
pub struct SummaryStore {
    entries: BTreeMap<String, SignedSummary>,
    ingest_windows: BTreeMap<String, Vec<i64>>,
    config: StoreConfig,
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    accepted: bool,
    total: usize,
    ingested: usize,
}

#[derive(Debug, Deserialize)]
struct RankingQuery {
    metric: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct EventQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct PlanetQuery {
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct SummarySignable<'a> {
    agent_id: &'a str,
    timestamp: i64,
    subnet_id: &'a Option<String>,
    power: i64,
    watt: i64,
    task_stats: &'a TaskStats,
    events_digest: &'a str,
}

impl Default for SummaryStore {
    fn default() -> Self {
        Self::with_config(StoreConfig::default())
    }
}

impl SummaryStore {
    #[must_use]
    pub fn with_config(config: StoreConfig) -> Self {
        Self {
            entries: BTreeMap::new(),
            ingest_windows: BTreeMap::new(),
            config,
        }
    }

    pub fn ingest(&mut self, summary: SignedSummary) -> Result<IngestResponse> {
        self.insert_summary(summary, true)?;
        self.apply_retention(Utc::now().timestamp());
        Ok(IngestResponse {
            accepted: true,
            total: self.entries.len(),
            ingested: 1,
        })
    }

    pub fn ingest_batch(&mut self, summaries: Vec<SignedSummary>) -> Result<IngestResponse> {
        let mut ingested = 0;
        for summary in summaries {
            self.insert_summary(summary, false)?;
            ingested += 1;
        }
        self.apply_retention(Utc::now().timestamp());
        Ok(IngestResponse {
            accepted: true,
            total: self.entries.len(),
            ingested,
        })
    }

    fn insert_summary(&mut self, summary: SignedSummary, enforce_rate_limit: bool) -> Result<()> {
        Self::verify_summary(&summary)?;

        if enforce_rate_limit {
            self.enforce_agent_rate_limit(&summary.agent_id)?;
        }

        // Deduplicate by agent, timestamp, and digest tuple.
        let key = format!(
            "{}:{}:{}",
            summary.agent_id, summary.timestamp, summary.events_digest
        );
        self.entries.insert(key, summary);
        Ok(())
    }

    fn enforce_agent_rate_limit(&mut self, agent_id: &str) -> Result<()> {
        let now = Utc::now().timestamp();
        let window = self.ingest_windows.entry(agent_id.to_string()).or_default();
        prune_window(window, now, 60);
        if window.len() >= self.config.max_ingest_per_agent_per_minute {
            anyhow::bail!("rate_limit_exceeded_for_agent");
        }
        window.push(now);
        Ok(())
    }

    fn verify_summary(summary: &SignedSummary) -> Result<()> {
        let signable = SummarySignable {
            agent_id: &summary.agent_id,
            timestamp: summary.timestamp,
            subnet_id: &summary.subnet_id,
            power: summary.power,
            watt: summary.watt,
            task_stats: &summary.task_stats,
            events_digest: &summary.events_digest,
        };
        if !verify_payload(&signable, &summary.signature, &summary.agent_id)? {
            anyhow::bail!("invalid signed summary signature");
        }
        Ok(())
    }

    fn apply_retention(&mut self, now: i64) {
        let min_ts = now - self.config.max_entry_age_sec;
        self.entries
            .retain(|_, summary| summary.timestamp >= min_ts);
        self.prune_rate_windows(now);

        if self.entries.len() <= self.config.max_entries {
            return;
        }

        let mut ordered: Vec<(String, i64)> = self
            .entries
            .iter()
            .map(|(key, value)| (key.clone(), value.timestamp))
            .collect();
        ordered.sort_by_key(|(_, timestamp)| *timestamp);

        let to_drop = self.entries.len() - self.config.max_entries;
        for key in ordered.into_iter().take(to_drop).map(|(key, _)| key) {
            self.entries.remove(&key);
        }
    }

    fn prune_rate_windows(&mut self, now: i64) {
        self.ingest_windows.retain(|_, window| {
            prune_window(window, now, 60);
            !window.is_empty()
        });
    }

    #[must_use]
    pub fn export_snapshot(&self) -> Vec<SignedSummary> {
        self.entries.values().cloned().collect()
    }

    #[must_use]
    pub fn heatmap(&self) -> Vec<HeatPoint> {
        let mut points: BTreeMap<String, HeatPoint> = BTreeMap::new();
        for summary in self.entries.values() {
            let subnet = summary
                .subnet_id
                .clone()
                .unwrap_or_else(|| "global".to_string());
            let point = points.entry(subnet.clone()).or_insert_with(|| HeatPoint {
                subnet_id: subnet,
                active_agents: 0,
                total_watt: 0,
                total_power: 0,
            });
            point.active_agents += 1;
            point.total_watt += summary.watt;
            point.total_power += summary.power;
        }
        let mut out: Vec<_> = points.into_values().collect();
        out.sort_by_key(|item| Reverse(item.total_watt));
        out
    }

    #[must_use]
    pub fn planet_health(&self, limit: usize) -> Vec<PlanetHealthEntry> {
        let mut grouped: BTreeMap<String, Vec<&SignedSummary>> = BTreeMap::new();
        for summary in self.entries.values() {
            let subnet = summary
                .subnet_id
                .clone()
                .unwrap_or_else(|| "global".to_string());
            grouped.entry(subnet).or_default().push(summary);
        }

        let mut rows: Vec<PlanetHealthEntry> = grouped
            .into_iter()
            .map(|(subnet_id, summaries)| {
                let active_agents = summaries.len();
                let total_watt: i64 = summaries.iter().map(|s| s.watt).sum();
                let total_power: i64 = summaries.iter().map(|s| s.power).sum();
                let total_contribution: i64 =
                    summaries.iter().map(|s| s.task_stats.contribution).sum();
                let avg_success_rate = if active_agents == 0 {
                    0.0
                } else {
                    let active_agents_u32 =
                        u32::try_from(active_agents).expect("active_agents fits in u32");
                    summaries
                        .iter()
                        .map(|s| s.task_stats.success_rate)
                        .sum::<f64>()
                        / f64::from(active_agents_u32)
                };

                // Integer score keeps ranking deterministic and avoids float precision drift.
                let health_score = total_contribution
                    .saturating_mul(5)
                    .saturating_add(total_power.saturating_mul(3))
                    .saturating_add(total_watt);

                PlanetHealthEntry {
                    subnet_id,
                    active_agents,
                    total_watt,
                    total_power,
                    total_contribution,
                    avg_success_rate,
                    health_score,
                }
            })
            .collect();

        rows.sort_by_key(|item| Reverse(item.health_score));
        rows.truncate(limit);
        rows
    }

    #[must_use]
    pub fn rankings(&self, metric: &str, limit: usize) -> Vec<RankingEntry> {
        // Keep only the latest summary per agent before ranking.
        let mut latest: BTreeMap<String, SignedSummary> = BTreeMap::new();
        for summary in self.entries.values() {
            let keep = latest
                .get(&summary.agent_id)
                .is_none_or(|old| old.timestamp < summary.timestamp);
            if keep {
                latest.insert(summary.agent_id.clone(), summary.clone());
            }
        }

        let mut entries: Vec<_> = latest
            .into_values()
            .map(|summary| {
                let value = match metric {
                    "power" => summary.power,
                    "contribution" => summary.task_stats.contribution,
                    _ => summary.watt,
                };
                RankingEntry {
                    agent_id: summary.agent_id,
                    subnet_id: summary.subnet_id.unwrap_or_else(|| "global".to_string()),
                    metric: metric.to_string(),
                    value,
                    power: summary.power,
                    watt: summary.watt,
                    task_stats: summary.task_stats,
                    timestamp: summary.timestamp,
                }
            })
            .collect();

        entries.sort_by_key(|entry| Reverse(entry.value));
        entries.truncate(limit);
        entries
    }

    #[must_use]
    pub fn event_stream(&self, limit: usize) -> Vec<EventStreamEntry> {
        let mut rows: Vec<_> = self
            .entries
            .values()
            .map(|summary| EventStreamEntry {
                timestamp: summary.timestamp,
                agent_id: summary.agent_id.clone(),
                subnet_id: summary
                    .subnet_id
                    .clone()
                    .unwrap_or_else(|| "global".to_string()),
                events_digest: summary.events_digest.clone(),
                watt: summary.watt,
                power: summary.power,
            })
            .collect();
        rows.sort_by_key(|item| Reverse(item.timestamp));
        rows.truncate(limit);
        rows
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn config(&self) -> &StoreConfig {
        &self.config
    }
}

pub type SharedStore = Arc<RwLock<SummaryStore>>;

pub fn app(store: SharedStore) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/api/docs", get(api_docs))
        .route("/api/summaries", post(ingest_summary))
        .route("/api/heatmap", get(heatmap))
        .route("/api/rankings", get(rankings))
        .route("/api/events", get(events))
        .route("/api/planets", get(planets))
        .route("/api/mirror/export", get(mirror_export))
        .route("/api/mirror/import", post(mirror_import))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
                .allow_headers(Any),
        )
        .with_state(store)
}

async fn index() -> impl IntoResponse {
    Html(include_str!("index.html"))
}

async fn healthz(State(store): State<SharedStore>) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    Json(serde_json::json!({
        "ok": true,
        "entries": guard.len(),
        "config": guard.config(),
    }))
}

async fn api_docs() -> impl IntoResponse {
    Json(serde_json::json!({
        "service": "wattetheria-observatory",
        "authoritative": false,
        "routes": [
            {"method": "POST", "path": "/api/summaries", "description": "ingest signed summary"},
            {"method": "GET", "path": "/api/heatmap", "description": "subnet activity aggregates"},
            {"method": "GET", "path": "/api/rankings", "description": "watt/power/contribution rankings"},
            {"method": "GET", "path": "/api/events", "description": "recent summary event stream"},
            {"method": "GET", "path": "/api/planets", "description": "planet health indicators"},
            {"method": "GET", "path": "/api/mirror/export", "description": "export snapshot for mirror sync"},
            {"method": "POST", "path": "/api/mirror/import", "description": "import mirrored snapshot"}
        ]
    }))
}

async fn ingest_summary(
    State(store): State<SharedStore>,
    Json(summary): Json<SignedSummary>,
) -> impl IntoResponse {
    let mut guard = store.write().expect("store write lock");
    match guard.ingest(summary) {
        Ok(result) => (
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(result).expect("serialize ingest result")),
        )
            .into_response(),
        Err(error) if error.to_string().contains("rate_limit_exceeded_for_agent") => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn heatmap(State(store): State<SharedStore>) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    Json(guard.heatmap())
}

async fn rankings(
    State(store): State<SharedStore>,
    Query(query): Query<RankingQuery>,
) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    let metric = query.metric.as_deref().unwrap_or("watt");
    let limit = query.limit.unwrap_or(20).max(1);
    Json(guard.rankings(metric, limit))
}

async fn events(
    State(store): State<SharedStore>,
    Query(query): Query<EventQuery>,
) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    Json(guard.event_stream(query.limit.unwrap_or(50).max(1)))
}

async fn planets(
    State(store): State<SharedStore>,
    Query(query): Query<PlanetQuery>,
) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    Json(guard.planet_health(query.limit.unwrap_or(20).max(1)))
}

async fn mirror_export(State(store): State<SharedStore>) -> impl IntoResponse {
    let guard = store.read().expect("store read lock");
    Json(guard.export_snapshot())
}

async fn mirror_import(
    State(store): State<SharedStore>,
    Json(summaries): Json<Vec<SignedSummary>>,
) -> impl IntoResponse {
    let mut guard = store.write().expect("store write lock");
    match guard.ingest_batch(summaries) {
        Ok(result) => (
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(result).expect("serialize ingest result")),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

fn prune_window(entries: &mut Vec<i64>, now: i64, window_sec: i64) {
    let min_ts = now - window_sec;
    entries.retain(|timestamp| *timestamp >= min_ts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;
    use wattetheria_kernel::identity::Identity;
    use wattetheria_kernel::summary::build_signed_summary;
    use wattetheria_kernel::types::AgentStats;

    #[tokio::test]
    async fn ingest_and_query() {
        let store = Arc::new(RwLock::new(SummaryStore::default()));
        let app = app(store.clone());

        let identity = Identity::new_random();
        let summary = build_signed_summary(
            &identity,
            Some("planet-a".to_string()),
            &AgentStats {
                power: 5,
                watt: 120,
                reputation: 4,
                capacity: 40,
            },
            &[],
        )
        .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/api/summaries")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&summary).unwrap()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);

        let req = Request::builder()
            .uri("/api/heatmap")
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/rankings?metric=watt")
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/events?limit=10")
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/planets?limit=10")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rejects_invalid_signature() {
        let store = Arc::new(RwLock::new(SummaryStore::default()));
        let app = app(store);
        let identity = Identity::new_random();
        let mut summary =
            build_signed_summary(&identity, None, &AgentStats::default(), &[]).unwrap();
        summary.watt = 999;

        let req = Request::builder()
            .method("POST")
            .uri("/api/summaries")
            .header("content-type", "application/json")
            .body(Body::from(json!(summary).to_string()))
            .unwrap();

        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rate_limit_returns_429() {
        let store = Arc::new(RwLock::new(SummaryStore::with_config(StoreConfig {
            max_entries: 100,
            max_entry_age_sec: 3600,
            max_ingest_per_agent_per_minute: 1,
        })));
        let app = app(store);

        let identity = Identity::new_random();
        let summary = build_signed_summary(&identity, None, &AgentStats::default(), &[]).unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/api/summaries")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&summary).unwrap()))
            .unwrap();
        let first = app.clone().oneshot(req).await.unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);

        let req = Request::builder()
            .method("POST")
            .uri("/api/summaries")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&summary).unwrap()))
            .unwrap();
        let second = app.oneshot(req).await.unwrap();
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn mirror_roundtrip_works() {
        let store = Arc::new(RwLock::new(SummaryStore::default()));
        let app = app(store);
        let identity = Identity::new_random();
        let summary = build_signed_summary(
            &identity,
            Some("planet-x".to_string()),
            &AgentStats {
                power: 8,
                watt: 88,
                reputation: 1,
                capacity: 3,
            },
            &[],
        )
        .unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/api/mirror/import")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&vec![summary]).unwrap()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);

        let req = Request::builder()
            .uri("/api/mirror/export")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let body = res.into_body().collect().await.unwrap().to_bytes();
        let exported: Vec<SignedSummary> = serde_json::from_slice(&body).unwrap();
        assert_eq!(exported.len(), 1);
    }
}
