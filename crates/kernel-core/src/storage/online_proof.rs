//! Online presence proof via lease, heartbeat, and spot-check.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub lease_id: String,
    pub agent_did: String,
    pub lease_expiry: i64,
    pub heartbeat_interval_sec: i64,
    pub last_heartbeat: i64,
    pub missed_heartbeats: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpotCheckStatus {
    Ok,
    MissingLease,
    LeaseExpired,
    HeartbeatTimeout,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OnlineProofManager {
    leases: HashMap<String, Lease>,
}

impl OnlineProofManager {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create online proof state directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read online proof state")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse online proof state")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create online proof state directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?)
            .context("write online proof state")
    }

    pub fn create_lease(
        &mut self,
        agent_did: &str,
        ttl_sec: i64,
        heartbeat_interval_sec: i64,
    ) -> Lease {
        let now = Utc::now().timestamp();
        let lease = Lease {
            lease_id: uuid::Uuid::new_v4().to_string(),
            agent_did: agent_did.to_string(),
            lease_expiry: now + ttl_sec,
            heartbeat_interval_sec,
            last_heartbeat: now,
            missed_heartbeats: 0,
        };
        self.leases.insert(agent_did.to_string(), lease.clone());
        lease
    }

    pub fn heartbeat(&mut self, agent_did: &str) -> Option<Lease> {
        let now = Utc::now().timestamp();
        let lease = self.leases.get_mut(agent_did)?;
        lease.last_heartbeat = now;
        lease.missed_heartbeats = 0;
        Some(lease.clone())
    }

    pub fn spot_check(&mut self, agent_did: &str, skew_tolerance_sec: i64) -> SpotCheckStatus {
        let Some(lease) = self.leases.get_mut(agent_did) else {
            return SpotCheckStatus::MissingLease;
        };

        let now = Utc::now().timestamp();
        if lease.lease_expiry < now {
            return SpotCheckStatus::LeaseExpired;
        }

        let expected = lease.last_heartbeat + lease.heartbeat_interval_sec + skew_tolerance_sec;
        if now > expected {
            lease.missed_heartbeats = lease.missed_heartbeats.saturating_add(1);
            return SpotCheckStatus::HeartbeatTimeout;
        }

        SpotCheckStatus::Ok
    }

    #[must_use]
    pub fn get_proof(&self, agent_did: &str) -> Option<serde_json::Value> {
        let lease = self.leases.get(agent_did)?;
        Some(serde_json::json!({
            "lease_id": lease.lease_id,
            "lease_expiry": lease.lease_expiry,
            "heartbeat_interval_sec": lease.heartbeat_interval_sec,
            "last_heartbeat": lease.last_heartbeat,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn persistence_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("online_proof.json");

        let mut mgr = OnlineProofManager::default();
        mgr.create_lease("agent-persist", 300, 10);
        mgr.persist(&path).unwrap();

        let mut loaded = OnlineProofManager::load_or_new(&path).unwrap();
        assert!(loaded.get_proof("agent-persist").is_some());
        assert_eq!(loaded.spot_check("agent-persist", 5), SpotCheckStatus::Ok);
    }

    #[test]
    fn lease_lifecycle() {
        let mut mgr = OnlineProofManager::default();
        let lease = mgr.create_lease("agent-1", 60, 5);
        assert_eq!(lease.agent_did, "agent-1");
        assert_eq!(mgr.spot_check("agent-1", 2), SpotCheckStatus::Ok);
        assert!(mgr.heartbeat("agent-1").is_some());
        assert!(mgr.get_proof("agent-1").is_some());
    }
}
