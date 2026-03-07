//! Lightweight web-of-trust graph for blacklist propagation and sybil resistance.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustReport {
    pub peer_id: String,
    pub reporter_id: String,
    pub reason: String,
    pub timestamp: i64,
    pub weight: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustConfig {
    pub blacklist_weight_threshold: i64,
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            blacklist_weight_threshold: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebOfTrust {
    config: TrustConfig,
    reporter_weights: BTreeMap<String, i64>,
    reports_by_peer: BTreeMap<String, Vec<TrustReport>>,
    blacklisted: BTreeSet<String>,
}

impl Default for WebOfTrust {
    fn default() -> Self {
        Self::new(TrustConfig::default())
    }
}

impl WebOfTrust {
    #[must_use]
    pub fn new(config: TrustConfig) -> Self {
        Self {
            config,
            reporter_weights: BTreeMap::new(),
            reports_by_peer: BTreeMap::new(),
            blacklisted: BTreeSet::new(),
        }
    }

    pub fn set_reporter_weight(&mut self, reporter_id: &str, weight: i64) {
        self.reporter_weights
            .insert(reporter_id.to_string(), weight.max(1));
    }

    pub fn report_peer(&mut self, peer_id: &str, reporter_id: &str, reason: &str) -> TrustReport {
        let weight = self.reporter_weights.get(reporter_id).copied().unwrap_or(1);
        let report = TrustReport {
            peer_id: peer_id.to_string(),
            reporter_id: reporter_id.to_string(),
            reason: reason.to_string(),
            timestamp: Utc::now().timestamp(),
            weight,
        };
        self.reports_by_peer
            .entry(peer_id.to_string())
            .or_default()
            .push(report.clone());

        if self.total_weight(peer_id) >= self.config.blacklist_weight_threshold {
            self.blacklisted.insert(peer_id.to_string());
        }
        report
    }

    pub fn ingest_remote_blacklist(&mut self, peers: &[String]) {
        for peer in peers {
            self.blacklisted.insert(peer.clone());
        }
    }

    #[must_use]
    pub fn total_weight(&self, peer_id: &str) -> i64 {
        self.reports_by_peer.get(peer_id).map_or(0, |reports| {
            reports.iter().map(|report| report.weight).sum()
        })
    }

    #[must_use]
    pub fn is_blacklisted(&self, peer_id: &str) -> bool {
        self.blacklisted.contains(peer_id)
    }

    #[must_use]
    pub fn export_blacklist(&self) -> Vec<String> {
        self.blacklisted.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_reports_blacklist_after_threshold() {
        let mut trust = WebOfTrust::new(TrustConfig {
            blacklist_weight_threshold: 5,
        });
        trust.set_reporter_weight("validator-a", 3);
        trust.set_reporter_weight("validator-b", 3);

        trust.report_peer("peer-x", "validator-a", "spam");
        assert!(!trust.is_blacklisted("peer-x"));

        trust.report_peer("peer-x", "validator-b", "sybil");
        assert!(trust.is_blacklisted("peer-x"));
    }

    #[test]
    fn trust_ingests_remote_blacklists() {
        let mut trust = WebOfTrust::default();
        trust.ingest_remote_blacklist(&[String::from("peer-z")]);
        assert!(trust.is_blacklisted("peer-z"));
    }
}
