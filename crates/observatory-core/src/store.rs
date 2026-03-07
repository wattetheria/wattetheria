use crate::models::{
    EventStreamEntry, HeatPoint, IngestResponse, PlanetHealthEntry, RankingEntry, StoreConfig,
};
use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use wattetheria_kernel::signing::verify_payload;
use wattetheria_kernel::types::{SignedSummary, TaskStats};

#[derive(Debug)]
pub struct SummaryStore {
    entries: BTreeMap<String, SignedSummary>,
    ingest_windows: BTreeMap<String, Vec<i64>>,
    config: StoreConfig,
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

fn prune_window(entries: &mut Vec<i64>, now: i64, window_sec: i64) {
    let min_ts = now - window_sec;
    entries.retain(|timestamp| *timestamp >= min_ts);
}
