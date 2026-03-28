// Integration tests for CLI report and summary commands.
use std::fs;

use std::process::Command;
use tempfile::tempdir;
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::identity::Identity;

#[test]
fn night_shift_command_outputs_report() {
    let tmp = tempdir().unwrap();
    let data = tmp.path();
    let log = EventLog::new(data.join("events.jsonl")).unwrap();
    let identity = Identity::new_random();
    log.append_signed(
        "TASK_SETTLED",
        serde_json::json!({"task_id":"t1","reward":{"watt":5,"reputation":1}}),
        &identity,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_wattetheria-client-cli"))
        .arg("night-shift")
        .arg("--event-log")
        .arg(data.join("events.jsonl"))
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("completed_tasks"));
}

#[test]
fn post_summary_dry_run_outputs_signed_summary() {
    let tmp = tempdir().unwrap();
    let data = tmp.path();
    let identity = Identity::new_random();
    identity.save(data.join("identity.json")).unwrap();
    fs::write(data.join("events.jsonl"), "").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_wattetheria-client-cli"))
        .arg("post-summary")
        .arg("--identity")
        .arg(data.join("identity.json"))
        .arg("--events")
        .arg(data.join("events.jsonl"))
        .arg("--dry-run")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("events_digest"));
}
