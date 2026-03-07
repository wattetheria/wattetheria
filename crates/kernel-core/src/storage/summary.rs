//! Signed summary generation for observatory ingestion.

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::event_log::EventRecord;
use crate::identity::Identity;
use crate::signing::sign_payload;
use crate::types::{AgentStats, SignedSummary, TaskStats};

#[derive(Debug, Serialize)]
struct SummarySignable<'a> {
    agent_id: &'a str,
    timestamp: i64,
    subnet_id: &'a Option<String>,
    power: i64,
    watt: i64,
    reputation: i64,
    capacity: i64,
    task_stats: &'a TaskStats,
    events_digest: &'a str,
}

pub fn build_signed_summary(
    identity: &Identity,
    subnet_id: Option<String>,
    ledger: &AgentStats,
    recent_events: &[EventRecord],
) -> Result<SignedSummary> {
    let events_digest = digest_events(recent_events);
    let timestamp = Utc::now().timestamp();

    let completed = recent_events
        .iter()
        .filter(|event| event.event_type == "TASK_SETTLED")
        .count() as u64;
    let verified = recent_events
        .iter()
        .filter(|event| event.event_type == "TASK_VERIFIED")
        .count() as u64;
    let accepted = recent_events
        .iter()
        .filter(|event| {
            event.event_type == "TASK_VERIFIED"
                && event.payload["accepted"].as_bool().unwrap_or(false)
        })
        .count() as u64;

    let verified_u32 = u32::try_from(verified).unwrap_or(u32::MAX);
    let accepted_u32 = u32::try_from(accepted).unwrap_or(u32::MAX);

    let task_stats = TaskStats {
        completed,
        success_rate: if verified_u32 == 0 {
            1.0
        } else {
            f64::from(accepted_u32) / f64::from(verified_u32)
        },
        contribution: ledger.capacity,
    };

    let signable = SummarySignable {
        agent_id: &identity.agent_id,
        timestamp,
        subnet_id: &subnet_id,
        power: ledger.power,
        watt: ledger.watt,
        reputation: ledger.reputation,
        capacity: ledger.capacity,
        task_stats: &task_stats,
        events_digest: &events_digest,
    };

    let signature = sign_payload(&signable, identity)?;
    Ok(SignedSummary {
        agent_id: identity.agent_id.clone(),
        timestamp,
        subnet_id,
        power: ledger.power,
        watt: ledger.watt,
        reputation: ledger.reputation,
        capacity: ledger.capacity,
        task_stats,
        events_digest,
        signature,
    })
}

fn digest_events(events: &[EventRecord]) -> String {
    // Keep digest deterministic by folding precomputed event hashes in order.
    let mut hasher = Sha256::new();
    for event in events {
        hasher.update(event.hash.as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::signing::verify_payload;

    #[test]
    fn summary_is_signed() {
        let identity = Identity::new_random();
        let summary = build_signed_summary(&identity, None, &AgentStats::default(), &[]).unwrap();

        let signable = SummarySignable {
            agent_id: &summary.agent_id,
            timestamp: summary.timestamp,
            subnet_id: &summary.subnet_id,
            power: summary.power,
            watt: summary.watt,
            reputation: summary.reputation,
            capacity: summary.capacity,
            task_stats: &summary.task_stats,
            events_digest: &summary.events_digest,
        };
        assert!(verify_payload(&signable, &summary.signature, &summary.agent_id).unwrap());
    }
}
