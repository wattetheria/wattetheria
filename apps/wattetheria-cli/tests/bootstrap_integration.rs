// Integration tests for init/up/doctor bootstrap workflows.

use serde_json::Value;
use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wattetheria-client-cli"))
        .args(args)
        .output()
        .unwrap()
}

fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn update_config(data_dir: &Path, bind: &str, recovery_sources: &[String]) {
    let path = data_dir.join("config.json");
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["control_plane_bind"] = Value::String(bind.to_string());
    config["control_plane_endpoint"] = Value::String(format!("http://{bind}"));
    config["recovery_sources"] = Value::Array(
        recovery_sources
            .iter()
            .map(|source| Value::String(source.clone()))
            .collect(),
    );
    fs::write(path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
}

fn stop_daemon(data_dir: &Path) {
    let pid_path = data_dir.join("daemon.pid");
    if !pid_path.exists() {
        return;
    }
    let pid = fs::read_to_string(&pid_path).unwrap_or_default();
    let pid = pid.trim();
    if pid.is_empty() {
        return;
    }
    let _ = Command::new("kill").arg(pid).status();
}

#[test]
fn init_command_creates_expected_layout() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("node");

    let output = run_cli(&["init", "--data-dir", data_dir.to_str().unwrap()]);

    assert!(output.status.success());
    assert!(data_dir.join("identity.json").exists());
    assert!(data_dir.join(".watt-wallet/metadata.json").exists());
    assert!(data_dir.join(".watt-wallet/keystore.json").exists());
    let identity: Value =
        serde_json::from_str(&fs::read_to_string(data_dir.join("identity.json")).unwrap()).unwrap();
    assert!(identity.get("private_key").is_none());
    assert!(data_dir.join("events.jsonl").exists());
    assert!(data_dir.join("control.token").exists());
    assert!(data_dir.join("config.json").exists());
    assert!(data_dir.join("audit").exists());
    assert!(data_dir.join("snapshots").exists());
}

#[test]
fn doctor_fails_without_running_control_plane() {
    let tmp = tempdir().unwrap();
    let data_dir = tmp.path().join("node");

    let init = run_cli(&["init", "--data-dir", data_dir.to_str().unwrap()]);
    assert!(init.status.success());

    let doctor = run_cli(&["doctor", "--data-dir", data_dir.to_str().unwrap()]);

    assert!(!doctor.status.success());
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(stdout.contains("control_plane_health"));
}

#[test]
fn up_recovers_corrupt_events_from_peer_export() {
    let tmp = tempdir().unwrap();
    let data_a = tmp.path().join("node-a");
    let data_b = tmp.path().join("node-b");

    let init_a = run_cli(&["init", "--data-dir", data_a.to_str().unwrap()]);
    assert!(init_a.status.success());
    let init_b = run_cli(&["init", "--data-dir", data_b.to_str().unwrap()]);
    assert!(init_b.status.success());

    let port_a = pick_free_port();
    let bind_a = format!("127.0.0.1:{port_a}");
    update_config(&data_a, &bind_a, &[]);

    // Seed node-a with signed events to prove peer export recovery works end-to-end.
    let brain_a = run_cli(&[
        "brain",
        "--data-dir",
        data_a.to_str().unwrap(),
        "humanize-night-shift",
        "--hours",
        "1",
    ]);
    assert!(brain_a.status.success());

    let up_a = run_cli(&["up", "--data-dir", data_a.to_str().unwrap()]);
    let daemon_log_a = fs::read_to_string(data_a.join("daemon.log")).unwrap_or_default();
    assert!(
        up_a.status.success(),
        "up_a failed\nstdout:\n{}\nstderr:\n{}\ndaemon_log:\n{}",
        String::from_utf8_lossy(&up_a.stdout),
        String::from_utf8_lossy(&up_a.stderr),
        daemon_log_a,
    );

    let port_b = pick_free_port();
    let bind_b = format!("127.0.0.1:{port_b}");
    let source = format!("http://{bind_a}/v1/events/export");
    update_config(&data_b, &bind_b, &[source]);

    fs::write(data_b.join("events.jsonl"), "corrupted-jsonl\n").unwrap();

    let up_b = run_cli(&["up", "--data-dir", data_b.to_str().unwrap()]);
    let daemon_log = fs::read_to_string(data_b.join("daemon.log")).unwrap_or_default();
    stop_daemon(&data_b);
    stop_daemon(&data_a);
    assert!(
        up_b.status.success(),
        "up_b failed\nstdout:\n{}\nstderr:\n{}\ndaemon_log:\n{}",
        String::from_utf8_lossy(&up_b.stdout),
        String::from_utf8_lossy(&up_b.stderr),
        daemon_log,
    );

    let recovered = fs::read_to_string(data_b.join("events.jsonl")).unwrap();
    assert!(recovered.contains("BRAIN_INVOKE_REQUEST"));
    assert!(recovered.contains("BRAIN_INVOKE_RESULT"));
}
