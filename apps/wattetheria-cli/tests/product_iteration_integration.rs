// Integration tests for skill, mcp, data, and upgrade-check workflows.

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
fn skill_install_and_test_creates_policy_pending() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("node");
    let skill_dir = tmp.path().join("echo-skill");

    fs::create_dir_all(skill_dir.join("schemas")).unwrap();
    fs::create_dir_all(skill_dir.join("conformance")).unwrap();
    fs::write(skill_dir.join("schemas/input.json"), "{}").unwrap();
    fs::write(
        skill_dir.join("conformance/report.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "suite": "wattetheria-conformance",
            "passed": true,
            "timestamp": 1_700_000_000
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        skill_dir.join("manifest.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "echo-skill",
            "version": "0.1.0",
            "entry": "builtin:echo",
            "required_caps": ["p2p.publish"],
            "trust": "verified",
            "conformance_report": "conformance/report.json"
        }))
        .unwrap(),
    )
    .unwrap();

    let init = run_cli(&["init", "--data-dir", data_dir.to_str().unwrap()]);
    assert!(init.status.success());

    let install = run_cli(&[
        "skill",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "install",
        skill_dir.to_str().unwrap(),
    ]);
    assert!(install.status.success());

    let test = run_cli(&[
        "skill",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "test",
        "echo-skill",
    ]);
    assert!(!test.status.success());
    let stderr = String::from_utf8_lossy(&test.stderr);
    assert!(stderr.contains("capability approval required"));

    let policy_path = data_dir.join("policy/state.json");
    let raw = fs::read_to_string(policy_path).unwrap();
    assert!(raw.contains("p2p.publish"));
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
fn brain_plan_skill_calls_supports_enable_flag() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("node");

    let init = run_cli(&["init", "--data-dir", data_dir.to_str().unwrap()]);
    assert!(init.status.success());

    let disabled = run_cli(&[
        "brain",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "plan-skill-calls",
    ]);
    assert!(disabled.status.success());
    let disabled_stdout = String::from_utf8_lossy(&disabled.stdout);
    assert!(disabled_stdout.trim_end().ends_with("[]"));

    let enabled = run_cli(&[
        "brain",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "plan-skill-calls",
        "--enable",
    ]);
    assert!(enabled.status.success());
    let enabled_stdout = String::from_utf8_lossy(&enabled.stdout);
    assert!(enabled_stdout.contains("echo-skill"));
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

#[cfg(unix)]
#[test]
fn process_skill_install_and_test_executes() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("node");
    let skill_dir = tmp.path().join("process-skill");

    fs::create_dir_all(skill_dir.join("schemas")).unwrap();
    fs::create_dir_all(skill_dir.join("bin")).unwrap();
    fs::write(skill_dir.join("schemas/input.json"), "{}").unwrap();
    fs::write(
        skill_dir.join("manifest.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "process-skill",
            "version": "0.1.0",
            "entry": "process:bin/skill.sh",
            "required_caps": [],
            "trust": "untrusted"
        }))
        .unwrap(),
    )
    .unwrap();

    let script_path = skill_dir.join("bin/skill.sh");
    fs::write(
        &script_path,
        "#!/usr/bin/env sh\nINPUT=$(cat)\nprintf '{\"handled\":true,\"input\":%s}\\n' \"$INPUT\"\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();

    let init = run_cli(&["init", "--data-dir", data_dir.to_str().unwrap()]);
    assert!(init.status.success());

    let install = run_cli(&[
        "skill",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "install",
        skill_dir.to_str().unwrap(),
    ]);
    assert!(install.status.success());

    let test = run_cli(&[
        "skill",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "test",
        "process-skill",
        "--input",
        "{\"hello\":\"world\"}",
    ]);
    assert!(test.status.success());
    let stdout = String::from_utf8_lossy(&test.stdout);
    assert!(stdout.contains("\"handled\": true"));
    assert!(stdout.contains("\"hello\": \"world\""));
}
