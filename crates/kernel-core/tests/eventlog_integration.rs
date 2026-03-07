//! Integration test for event-log replay and time-window querying.

use serde_json::json;
use tempfile::tempdir;
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::identity::Identity;

#[test]
fn event_log_replay_and_since_work() {
    let tmp = tempdir().unwrap();
    let log = EventLog::new(tmp.path().join("events.jsonl")).unwrap();
    let identity = Identity::new_random();

    log.append_signed("A", json!({"v":1}), &identity).unwrap();
    log.append_signed("B", json!({"v":2}), &identity).unwrap();

    let all = log.get_all().unwrap();
    let ts = all.first().unwrap().timestamp;
    let since_first = log.since(ts).unwrap();
    assert_eq!(since_first.len(), 2);

    let sum = log
        .replay(0_i64, |acc, row| {
            acc + row.payload["v"].as_i64().unwrap_or(0)
        })
        .unwrap();
    assert_eq!(sum, 3);
}
