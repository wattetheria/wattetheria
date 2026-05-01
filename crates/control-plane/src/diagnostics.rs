use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use tracing::warn;
use uuid::Uuid;

const DIAGNOSTIC_LOG_RELATIVE_PATH: &str = "diagnostics/local_node.jsonl";
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DiagnosticEntry {
    pub id: String,
    pub timestamp: String,
    pub level: String,
    pub component: String,
    pub category: String,
    pub phase: String,
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct DiagnosticEvent {
    pub level: &'static str,
    pub component: &'static str,
    pub category: &'static str,
    pub phase: &'static str,
    pub status: &'static str,
    pub message: String,
    pub trace_id: Option<String>,
    pub correlation_id: Option<String>,
    pub event_id: Option<String>,
    pub network_id: Option<String>,
    pub scope_hint: Option<String>,
    pub source_node_id: Option<String>,
    pub target_node_id: Option<String>,
    pub object_kind: Option<String>,
    pub object_id: Option<String>,
    pub details: Value,
}

impl DiagnosticEvent {
    pub(crate) fn new(
        level: &'static str,
        component: &'static str,
        category: &'static str,
        phase: &'static str,
        status: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            level,
            component,
            category,
            phase,
            status,
            message: message.into(),
            trace_id: None,
            correlation_id: None,
            event_id: None,
            network_id: None,
            scope_hint: None,
            source_node_id: None,
            target_node_id: None,
            object_kind: None,
            object_id: None,
            details: json!({}),
        }
    }

    pub(crate) fn event_id(mut self, event_id: Option<String>) -> Self {
        self.event_id = event_id;
        self
    }

    pub(crate) fn correlation_id(mut self, correlation_id: Option<String>) -> Self {
        self.correlation_id = correlation_id;
        self
    }

    pub(crate) fn source_node_id(mut self, source_node_id: Option<String>) -> Self {
        self.source_node_id = source_node_id;
        self
    }

    pub(crate) fn object(
        mut self,
        object_kind: impl Into<String>,
        object_id: Option<String>,
    ) -> Self {
        self.object_kind = Some(object_kind.into());
        self.object_id = object_id;
        self
    }

    pub(crate) fn details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DiagnosticFilter {
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

pub(crate) fn record_diagnostic(data_dir: &Path, event: DiagnosticEvent) {
    if let Err(error) = append_diagnostic(data_dir, event) {
        warn!("diagnostic log append failed: {error:#}");
    }
}

pub(crate) fn append_diagnostic(
    data_dir: &Path,
    event: DiagnosticEvent,
) -> Result<DiagnosticEntry> {
    let entry = DiagnosticEntry {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now().to_rfc3339(),
        level: event.level.to_owned(),
        component: event.component.to_owned(),
        category: event.category.to_owned(),
        phase: event.phase.to_owned(),
        status: event.status.to_owned(),
        message: event.message,
        trace_id: event.trace_id,
        correlation_id: event.correlation_id,
        event_id: event.event_id,
        network_id: event.network_id,
        scope_hint: event.scope_hint,
        source_node_id: event.source_node_id,
        target_node_id: event.target_node_id,
        object_kind: event.object_kind,
        object_id: event.object_id,
        details: event.details,
    };
    let path = diagnostic_log_path(data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create diagnostics directory")?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open diagnostics log {}", path.display()))?;
    file.write_all(serde_json::to_string(&entry)?.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(entry)
}

pub(crate) fn list_diagnostics(
    data_dir: &Path,
    filter: &DiagnosticFilter,
) -> Result<Vec<DiagnosticEntry>> {
    let path = diagnostic_log_path(data_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read diagnostics log {}", path.display()))?;
    let limit = filter.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut entries = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<DiagnosticEntry>(line).context("parse diagnostic row"))
        .collect::<Result<Vec<_>>>()?;
    entries.retain(|entry| diagnostic_matches(entry, filter));
    entries.reverse();
    entries.truncate(limit);
    Ok(entries)
}

fn diagnostic_log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DIAGNOSTIC_LOG_RELATIVE_PATH)
}

fn diagnostic_matches(entry: &DiagnosticEntry, filter: &DiagnosticFilter) -> bool {
    matches_text(&entry.level, filter.level.as_deref())
        && matches_text(&entry.component, filter.component.as_deref())
        && matches_text(&entry.category, filter.category.as_deref())
        && matches_text(&entry.phase, filter.phase.as_deref())
        && matches_option(entry.trace_id.as_deref(), filter.trace_id.as_deref())
        && matches_option(entry.event_id.as_deref(), filter.event_id.as_deref())
        && matches_option(entry.object_id.as_deref(), filter.object_id.as_deref())
        && matches_option(
            entry.source_node_id.as_deref(),
            filter.source_node_id.as_deref(),
        )
        && matches_search(entry, filter.search.as_deref())
}

fn matches_text(actual: &str, expected: Option<&str>) -> bool {
    expected
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "all")
        .is_none_or(|expected| actual.eq_ignore_ascii_case(expected))
}

fn matches_option(actual: Option<&str>, expected: Option<&str>) -> bool {
    expected
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none_or(|expected| actual == Some(expected))
}

fn matches_search(entry: &DiagnosticEntry, search: Option<&str>) -> bool {
    let Some(search) = search.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    let haystack = json!({
        "id": entry.id,
        "level": entry.level,
        "component": entry.component,
        "category": entry.category,
        "phase": entry.phase,
        "status": entry.status,
        "message": entry.message,
        "trace_id": entry.trace_id,
        "correlation_id": entry.correlation_id,
        "event_id": entry.event_id,
        "network_id": entry.network_id,
        "scope_hint": entry.scope_hint,
        "source_node_id": entry.source_node_id,
        "target_node_id": entry.target_node_id,
        "object_kind": entry.object_kind,
        "object_id": entry.object_id,
        "details": entry.details,
    })
    .to_string()
    .to_lowercase();
    haystack.contains(&search.to_lowercase())
}
