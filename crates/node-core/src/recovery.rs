use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::time::Duration;
use tracing::warn;
use wattetheria_kernel::data_ops::recover_if_corrupt_with_sources;
use wattetheria_kernel::event_log::{EventLog, EventRecord};

pub async fn startup_recover_events(
    events_path: &Path,
    snapshots_path: &Path,
    recovery_sources: &[String],
) -> Result<()> {
    let mut local_sources = Vec::new();
    let mut http_sources = Vec::new();

    for source in recovery_sources {
        if source.starts_with("http://") || source.starts_with("https://") {
            http_sources.push(source.clone());
        } else {
            local_sources.push(PathBuf::from(source));
        }
    }

    match recover_if_corrupt_with_sources(events_path, snapshots_path, &local_sources) {
        Ok(Some(snapshot)) => {
            warn!(
                snapshot_id = %snapshot.id,
                events = snapshot.event_count,
                "event log recovered from local snapshot/source during startup"
            );
        }
        Ok(None) => {}
        Err(error) => {
            if http_sources.is_empty() {
                return Err(error).context("startup event-log recovery check");
            }
            warn!(%error, "local recovery path failed; trying remote recovery sources");
        }
    }

    if !event_log_chain_is_valid(events_path)
        && !http_sources.is_empty()
        && let Some(source) = recover_events_from_http_sources(events_path, &http_sources).await?
    {
        warn!(source = %source, "event log recovered from remote source during startup");
    }

    if !event_log_chain_is_valid(events_path) {
        bail!("event log remains invalid after startup recovery attempts");
    }

    Ok(())
}

async fn recover_events_from_http_sources(
    events_path: &Path,
    sources: &[String],
) -> Result<Option<String>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .context("build recovery http client")?;

    for source in sources {
        let response = match client.get(source).send().await {
            Ok(response) => response,
            Err(error) => {
                warn!(source = %source, %error, "failed to fetch recovery source");
                continue;
            }
        };

        if !response.status().is_success() {
            warn!(source = %source, status = %response.status(), "recovery source returned non-success status");
            continue;
        }

        let body = match response.text().await {
            Ok(body) => body,
            Err(error) => {
                warn!(source = %source, %error, "failed to read recovery response body");
                continue;
            }
        };

        let rows = match parse_recovery_rows(&body) {
            Ok(rows) => rows,
            Err(error) => {
                warn!(source = %source, %error, "recovery source payload parse failed");
                continue;
            }
        };

        if rows.is_empty() {
            continue;
        }

        if write_candidate_events(events_path, &rows).is_err() {
            continue;
        }
        if event_log_chain_is_valid(events_path) {
            return Ok(Some(source.clone()));
        }
    }

    Ok(None)
}

fn parse_recovery_rows(raw: &str) -> Result<Vec<EventRecord>> {
    if let Ok(rows) = serde_json::from_str::<Vec<EventRecord>>(raw) {
        return Ok(rows);
    }

    if let Ok(value) = serde_json::from_str::<Value>(raw)
        && let Some(events) = value.get("events")
    {
        let rows: Vec<EventRecord> = serde_json::from_value(events.clone())
            .context("parse events array from recovery payload")?;
        return Ok(rows);
    }

    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<EventRecord>(line).context("parse recovery jsonl row"))
        .collect()
}

fn write_candidate_events(events_path: &Path, rows: &[EventRecord]) -> Result<()> {
    let mut content = String::new();
    for row in rows {
        content.push_str(&serde_json::to_string(row)?);
        content.push('\n');
    }
    std::fs::write(events_path, content).context("write candidate recovered events")
}

fn event_log_chain_is_valid(events_path: &Path) -> bool {
    EventLog::new(events_path)
        .and_then(|log| log.verify_chain())
        .is_ok_and(|(ok, _)| ok)
}
