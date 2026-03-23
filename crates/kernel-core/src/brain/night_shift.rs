//! Night Shift report builder from event-log windows.

use serde::{Deserialize, Serialize};

use crate::event_log::EventRecord;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NightShiftTotals {
    pub events: usize,
    pub completed_tasks: usize,
    pub failed_verifications: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NightShiftDeltas {
    pub watt: i64,
    pub reputation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyCard {
    pub event_type: String,
    pub timestamp: i64,
    pub highlight: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineItem {
    pub timestamp: i64,
    pub event_type: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NightShiftReport {
    pub generated_at: i64,
    pub since: i64,
    pub totals: NightShiftTotals,
    pub deltas: NightShiftDeltas,
    pub key_cards: Vec<KeyCard>,
    pub timeline: Vec<TimelineItem>,
}

#[must_use]
pub fn generate_night_shift_report(
    events: &[EventRecord],
    since: i64,
    now: i64,
) -> NightShiftReport {
    let window: Vec<&EventRecord> = events
        .iter()
        .filter(|event| event.timestamp >= since && event.timestamp <= now)
        .collect();

    let mut watt = 0;
    let mut reputation = 0;
    let mut completed = 0;
    let mut failed = 0;

    let timeline = window
        .iter()
        .map(|event| {
            if event.event_type == "TASK_SETTLED" {
                completed += 1;
                watt += event.payload["reward"]["watt"].as_i64().unwrap_or(0);
                reputation += event.payload["reward"]["reputation"].as_i64().unwrap_or(0);
            }
            if event.event_type == "TASK_VERIFIED"
                && !event.payload["accepted"].as_bool().unwrap_or(true)
            {
                failed += 1;
            }

            TimelineItem {
                timestamp: event.timestamp,
                event_type: event.event_type.clone(),
                summary: event.payload["task_id"].as_str().map_or_else(
                    || event.event_type.clone(),
                    |task_id| format!("{}:{task_id}", event.event_type),
                ),
            }
        })
        .collect::<Vec<_>>();

    let mut scored: Vec<(i64, &EventRecord)> = window
        .iter()
        .map(|event| {
            let score = if event.event_type == "TASK_SETTLED" {
                event.payload["reward"]["watt"].as_i64().unwrap_or(0).abs() + 10
            } else {
                1
            };
            (score, *event)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));

    let key_cards = scored
        .into_iter()
        .take(5)
        .map(|(_, event)| KeyCard {
            event_type: event.event_type.clone(),
            timestamp: event.timestamp,
            highlight: if event.event_type == "TASK_SETTLED" {
                format!(
                    "Watt +{}, Rep +{}",
                    event.payload["reward"]["watt"].as_i64().unwrap_or(0),
                    event.payload["reward"]["reputation"].as_i64().unwrap_or(0)
                )
            } else {
                event.event_type.clone()
            },
        })
        .collect();

    NightShiftReport {
        generated_at: now,
        since,
        totals: NightShiftTotals {
            events: window.len(),
            completed_tasks: completed,
            failed_verifications: failed,
        },
        deltas: NightShiftDeltas { watt, reputation },
        key_cards,
        timeline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn report_aggregates_deltas() {
        let events = vec![
            EventRecord {
                id: "1".to_string(),
                event_type: "TASK_SETTLED".to_string(),
                payload: json!({"task_id":"t1","reward":{"watt":12,"reputation":3}}),
                timestamp: 1000,
                agent_did: "a".to_string(),
                prev_hash: None,
                signature: "s".to_string(),
                hash: "h".to_string(),
            },
            EventRecord {
                id: "2".to_string(),
                event_type: "TASK_VERIFIED".to_string(),
                payload: json!({"task_id":"t2","accepted":false}),
                timestamp: 1010,
                agent_did: "a".to_string(),
                prev_hash: Some("h".to_string()),
                signature: "s".to_string(),
                hash: "h2".to_string(),
            },
        ];
        let report = generate_night_shift_report(&events, 900, 1100);
        assert_eq!(report.totals.completed_tasks, 1);
        assert_eq!(report.deltas.watt, 12);
        assert_eq!(report.totals.failed_verifications, 1);
    }
}
