// Integration tests for mcp, data, oracle, and upgrade-check workflows.

use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new("cargo")
        .arg("run")
        .arg("-p")
        .arg("wattetheria-client-cli")
        .arg("--")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn mcp_add_and_list_roundtrip() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("node");
    let config_path = tmp.path().join("mcp-server.json");

    fs::write(
        &config_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "name": "local-news",
            "url": "http://127.0.0.1:9999",
            "enabled": true,
            "tools_allowlist": ["news.read"],
            "timeout_sec": 5,
            "budget_per_minute": 10
        }))
        .unwrap(),
    )
    .unwrap();

    let init = run_cli(&["init", "--data-dir", data_dir.to_str().unwrap()]);
    assert!(init.status.success());

    let add = run_cli(&[
        "mcp",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "add",
        config_path.to_str().unwrap(),
    ]);
    assert!(add.status.success());

    let list = run_cli(&["mcp", "--data-dir", data_dir.to_str().unwrap(), "list"]);
    assert!(list.status.success());
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("local-news"));
}

#[test]
fn data_snapshot_and_backup_export_work() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("node");
    let archive_path = tmp.path().join("backup.tar.gz");

    let init = run_cli(&["init", "--data-dir", data_dir.to_str().unwrap()]);
    assert!(init.status.success());

    let snapshot = run_cli(&[
        "data",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "snapshot-create",
    ]);
    assert!(snapshot.status.success());

    let backup = run_cli(&[
        "data",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "backup-export",
        archive_path.to_str().unwrap(),
    ]);
    assert!(backup.status.success());
    assert!(archive_path.exists());
}

#[test]
fn upgrade_check_reports_outdated() {
    let out = run_cli(&["upgrade-check", "--current", "0.1.0", "--latest", "0.2.0"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("update_available"));
}

#[test]
fn brain_humanize_night_shift_runs_with_rules_provider() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("node");

    let init = run_cli(&["init", "--data-dir", data_dir.to_str().unwrap()]);
    assert!(init.status.success());

    let out = run_cli(&[
        "brain",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "humanize-night-shift",
        "--hours",
        "1",
    ]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Night Shift Brief"));

    let events = fs::read_to_string(data_dir.join("events.jsonl")).unwrap();
    assert!(events.contains("BRAIN_INVOKE_REQUEST"));
    assert!(events.contains("BRAIN_INVOKE_RESULT"));
}

#[test]
fn oracle_credit_publish_subscribe_pull_roundtrip() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("node");

    let init = run_cli(&["init", "--data-dir", data_dir.to_str().unwrap()]);
    assert!(init.status.success());

    let identity_raw = fs::read_to_string(data_dir.join("identity.json")).unwrap();
    let agent_id = serde_json::from_str::<serde_json::Value>(&identity_raw).unwrap()["agent_id"]
        .as_str()
        .unwrap()
        .to_string();
    let policy_path = data_dir.join("policy/state.json");
    let mut policy: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&policy_path).unwrap()).unwrap();
    policy["grants"] = serde_json::json!([{
        "grant_id": "integration-oracle-publish",
        "created_at": 0,
        "approved_by": "integration-test",
        "subject_pattern": format!("oracle:publisher:{agent_id}"),
        "capability_pattern": "oracle.publish",
        "scope": "permanent",
        "session_id": null
    }]);
    fs::write(policy_path, serde_json::to_string_pretty(&policy).unwrap()).unwrap();

    let credit = run_cli(&[
        "oracle",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "credit",
        "--watt",
        "20",
    ]);
    assert!(credit.status.success());

    let publish = run_cli(&[
        "oracle",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "publish",
        "btc-price",
        "--payload",
        "{\"price\":100000}",
        "--price-watt",
        "2",
    ]);
    assert!(publish.status.success());

    let subscribe = run_cli(&[
        "oracle",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "subscribe",
        "btc-price",
        "--max-price-watt",
        "3",
    ]);
    assert!(subscribe.status.success());

    let pull = run_cli(&[
        "oracle",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "pull",
        "btc-price",
    ]);
    assert!(pull.status.success());
    let stdout = String::from_utf8_lossy(&pull.stdout);
    assert!(stdout.contains("\"charged_watt\": 2"));
    assert!(stdout.contains("\"delivered\": 1"));
}
