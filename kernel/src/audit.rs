//! Append-only audit trail for control-plane and policy operations.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: String,
    pub timestamp: i64,
    pub category: String,
    pub action: String,
    pub status: String,
    pub actor: Option<String>,
    pub subject: Option<String>,
    pub capability: Option<String>,
    pub reason: Option<String>,
    pub duration_ms: Option<u64>,
    pub details: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create audit directory")?;
        }
        if !path.as_ref().exists() {
            fs::write(path.as_ref(), "").context("initialize audit log")?;
        }
        Ok(Self {
            path: path.as_ref().to_path_buf(),
        })
    }

    pub fn append(&self, mut entry: AuditEntry) -> Result<()> {
        if entry.id.is_empty() {
            entry.id = Uuid::new_v4().to_string();
        }
        if entry.timestamp == 0 {
            entry.timestamp = Utc::now().timestamp();
        }

        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .context("open audit log")?;
        file.write_all(serde_json::to_string(&entry)?.as_bytes())?;
        file.write_all(b"\n")?;
        Ok(())
    }

    pub fn list_recent(&self, limit: usize) -> Result<Vec<AuditEntry>> {
        let raw = fs::read_to_string(&self.path).context("read audit log")?;
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }

        let mut entries: Vec<AuditEntry> = raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<AuditEntry>(line).context("parse audit row"))
            .collect::<Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp));
        entries.truncate(limit);
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn audit_log_roundtrip() {
        let dir = tempdir().unwrap();
        let log = AuditLog::new(dir.path().join("audit.jsonl")).unwrap();

        log.append(AuditEntry {
            id: String::new(),
            timestamp: 0,
            category: "control".to_string(),
            action: "state.query".to_string(),
            status: "ok".to_string(),
            actor: Some("local".to_string()),
            subject: Some("agent".to_string()),
            capability: None,
            reason: None,
            duration_ms: Some(4),
            details: Some(serde_json::json!({"foo":"bar"})),
        })
        .unwrap();

        let rows = log.list_recent(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "state.query");
    }
}
