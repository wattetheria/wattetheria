//! Hash-chained event log with signature verification and replay helpers.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::identity::Identity;
use crate::signing::{canonical_bytes, sign_payload, verify_payload};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub payload: Value,
    pub timestamp: i64,
    pub agent_id: String,
    pub prev_hash: Option<String>,
    pub signature: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UnsignedEventRecord {
    id: String,
    #[serde(rename = "type")]
    event_type: String,
    payload: Value,
    timestamp: i64,
    agent_id: String,
    prev_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create event log dir")?;
        }
        if !path.as_ref().exists() {
            fs::write(path.as_ref(), "").context("initialize event log")?;
        }
        Ok(Self {
            path: path.as_ref().to_path_buf(),
        })
    }

    pub fn get_all(&self) -> Result<Vec<EventRecord>> {
        let raw = fs::read_to_string(&self.path).context("read event log")?;
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }

        raw.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<EventRecord>(line).context("parse event log row"))
            .collect()
    }

    pub fn last(&self) -> Result<Option<EventRecord>> {
        Ok(self.get_all()?.pop())
    }

    /// Internal `last()` that reads without acquiring a file lock.
    /// Must only be called while holding an exclusive lock externally.
    fn last_unlocked(&self) -> Result<Option<EventRecord>> {
        let mut file = File::open(&self.path).context("read event log for last")?;
        let mut raw = String::new();
        file.read_to_string(&mut raw)
            .context("read event log content")?;
        if raw.trim().is_empty() {
            return Ok(None);
        }
        let last_line = raw.lines().rfind(|line| !line.trim().is_empty());
        match last_line {
            Some(line) => {
                let record: EventRecord =
                    serde_json::from_str(line).context("parse last event log row")?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    pub fn append_signed(
        &self,
        event_type: impl Into<String>,
        payload: Value,
        identity: &Identity,
    ) -> Result<EventRecord> {
        // Acquire an exclusive lock on the event log file to prevent TOCTOU races
        // between reading the last hash and appending the new event.
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .context("open event log for locked append")?;
        lock_file
            .lock_exclusive()
            .context("acquire event log lock")?;

        let prev_hash = self.last_unlocked()?.map(|e| e.hash);
        let unsigned = UnsignedEventRecord {
            id: Uuid::new_v4().to_string(),
            event_type: event_type.into(),
            payload,
            timestamp: Utc::now().timestamp(),
            agent_id: identity.agent_id.clone(),
            prev_hash,
        };
        let signature = sign_payload(&unsigned, identity)?;
        let hash = hash_record(&unsigned, &signature)?;

        let event = EventRecord {
            id: unsigned.id,
            event_type: unsigned.event_type,
            payload: unsigned.payload,
            timestamp: unsigned.timestamp,
            agent_id: unsigned.agent_id,
            prev_hash: unsigned.prev_hash,
            signature,
            hash,
        };
        self.append_raw(&event)?;

        lock_file.unlock().context("release event log lock")?;
        Ok(event)
    }

    pub fn append_external(&self, event: &EventRecord) -> Result<()> {
        // Acquire an exclusive lock for the same TOCTOU safety as append_signed.
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .context("open event log for locked external append")?;
        lock_file
            .lock_exclusive()
            .context("acquire event log lock for external append")?;

        // Enforce append-only ordering before accepting remote events.
        let last_hash = self.last_unlocked()?.map(|e| e.hash);
        if last_hash != event.prev_hash {
            lock_file.unlock().ok();
            bail!("event prev_hash mismatch");
        }
        if !verify_event_signature(event)? {
            lock_file.unlock().ok();
            bail!("invalid event signature");
        }
        let expected_hash = hash_record(&event.to_unsigned(), &event.signature)?;
        if expected_hash != event.hash {
            lock_file.unlock().ok();
            bail!("invalid event hash");
        }
        let result = self.append_raw(event);
        lock_file.unlock().ok();
        result
    }

    pub fn verify_chain(&self) -> Result<(bool, Option<String>)> {
        let events = self.get_all()?;
        let mut prev_hash: Option<String> = None;
        for event in events {
            // Verify causal ordering, author signature, and content hash.
            if event.prev_hash != prev_hash {
                return Ok((false, Some("prev_hash mismatch".to_string())));
            }
            if !verify_event_signature(&event)? {
                return Ok((false, Some("signature mismatch".to_string())));
            }
            let expected_hash = hash_record(&event.to_unsigned(), &event.signature)?;
            if expected_hash != event.hash {
                return Ok((false, Some("hash mismatch".to_string())));
            }
            prev_hash = Some(event.hash);
        }
        Ok((true, None))
    }

    pub fn since(&self, since_ts: i64) -> Result<Vec<EventRecord>> {
        Ok(self
            .get_all()?
            .into_iter()
            .filter(|event| event.timestamp >= since_ts)
            .collect())
    }

    pub fn replay<T>(&self, init: T, reducer: impl Fn(T, &EventRecord) -> T) -> Result<T> {
        let state = self.get_all()?.iter().fold(init, reducer);
        Ok(state)
    }

    fn append_raw(&self, event: &EventRecord) -> Result<()> {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .context("open event log for append")?;
        file.write_all(serde_json::to_string(event)?.as_bytes())?;
        file.write_all(b"\n")?;
        Ok(())
    }
}

impl EventRecord {
    fn to_unsigned(&self) -> UnsignedEventRecord {
        UnsignedEventRecord {
            id: self.id.clone(),
            event_type: self.event_type.clone(),
            payload: self.payload.clone(),
            timestamp: self.timestamp,
            agent_id: self.agent_id.clone(),
            prev_hash: self.prev_hash.clone(),
        }
    }
}

fn verify_event_signature(event: &EventRecord) -> Result<bool> {
    verify_payload(&event.to_unsigned(), &event.signature, &event.agent_id)
}

fn hash_record(unsigned: &UnsignedEventRecord, signature: &str) -> Result<String> {
    // Include both canonical payload and signature bytes in the record hash.
    let mut hasher = Sha256::new();
    hasher.update(canonical_bytes(unsigned)?);
    hasher.update(signature.as_bytes());
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn verifies_and_detects_tampering() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let log = EventLog::new(&path).unwrap();
        let identity = Identity::new_random();

        log.append_signed("TASK_PUBLISHED", json!({"task_id":"a"}), &identity)
            .unwrap();
        log.append_signed("TASK_SETTLED", json!({"task_id":"a"}), &identity)
            .unwrap();

        assert!(log.verify_chain().unwrap().0);

        let mut rows = log.get_all().unwrap();
        rows[0].payload = json!({"task_id":"tampered"});
        fs::write(
            &path,
            rows.into_iter()
                .map(|row| serde_json::to_string(&row).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();

        assert!(!log.verify_chain().unwrap().0);
    }
}
