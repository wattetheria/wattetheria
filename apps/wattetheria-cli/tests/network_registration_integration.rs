use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::tempdir;

struct FlowFiles<'a> {
    autonomous: &'a Path,
    mainnet: &'a Path,
    request: &'a Path,
    trust: &'a Path,
    credential: &'a Path,
    revocation: &'a Path,
}

fn run_cli(data_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wattetheria-client-cli"))
        .arg("network")
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn create_and_inspect_request(files: &FlowFiles<'_>) {
    assert_success(&run_cli(files.autonomous, &["authority-init"]));
    assert_success(&run_cli(files.mainnet, &["authority-init"]));
    assert_success(&run_cli(
        files.autonomous,
        &[
            "create-request",
            "--network-did",
            "did:watt:network:campus-a",
            "--network-id",
            "campus-a",
            "--name",
            "Campus A",
            "--mainnet-did",
            "did:watt:network:mainnet",
            "--federation-endpoint",
            "iroh=campus-a-endpoint",
            "--out",
            files.request.to_str().unwrap(),
        ],
    ));
    let inspect = run_cli(
        files.mainnet,
        &[
            "inspect-request",
            "--request",
            files.request.to_str().unwrap(),
        ],
    );
    assert_success(&inspect);
    assert_eq!(
        serde_json::from_slice::<Value>(&inspect.stdout).unwrap()["valid"],
        true
    );
}

fn issue_and_import_credential(files: &FlowFiles<'_>) {
    assert_success(&run_cli(
        files.mainnet,
        &[
            "export-trust-bundle",
            "--mainnet-did",
            "did:watt:network:mainnet",
            "--network-id",
            "mainnet",
            "--out",
            files.trust.to_str().unwrap(),
        ],
    ));
    assert_success(&run_cli(
        files.mainnet,
        &[
            "issue-credential",
            "--request",
            files.request.to_str().unwrap(),
            "--mainnet-did",
            "did:watt:network:mainnet",
            "--network-id",
            "mainnet",
            "--out",
            files.credential.to_str().unwrap(),
        ],
    ));
    assert_success(&run_cli(
        files.autonomous,
        &[
            "verify-credential",
            "--credential",
            files.credential.to_str().unwrap(),
            "--request",
            files.request.to_str().unwrap(),
            "--trust-bundle",
            files.trust.to_str().unwrap(),
        ],
    ));
    assert_success(&run_cli(
        files.autonomous,
        &[
            "import-credential",
            "--credential",
            files.credential.to_str().unwrap(),
            "--request",
            files.request.to_str().unwrap(),
            "--trust-bundle",
            files.trust.to_str().unwrap(),
        ],
    ));
}

fn revoke_and_import_revocation(files: &FlowFiles<'_>) {
    let credential_json: Value =
        serde_json::from_slice(&fs::read(files.credential).unwrap()).unwrap();
    let credential_id = credential_json["payload"]["credential_id"]
        .as_str()
        .unwrap();
    assert_success(&run_cli(
        files.mainnet,
        &[
            "revoke-credential",
            "--credential-id",
            credential_id,
            "--reason",
            "registration withdrawn",
            "--mainnet-did",
            "did:watt:network:mainnet",
            "--out",
            files.revocation.to_str().unwrap(),
        ],
    ));
    assert_success(&run_cli(
        files.autonomous,
        &[
            "import-revocation",
            "--revocation",
            files.revocation.to_str().unwrap(),
            "--trust-bundle",
            files.trust.to_str().unwrap(),
        ],
    ));
    let list = run_cli(
        files.autonomous,
        &[
            "list-credentials",
            "--subject-network-did",
            "did:watt:network:campus-a",
        ],
    );
    assert_success(&list);
    let records: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(records[0]["status"], "revoked");
}

#[test]
fn autonomous_network_registration_round_trip_uses_offline_files() {
    let dir = tempdir().unwrap();
    let autonomous = dir.path().join("autonomous");
    let mainnet = dir.path().join("mainnet");
    let request = dir.path().join("request.json");
    let trust = dir.path().join("trust.json");
    let credential = dir.path().join("credential.json");
    let revocation = dir.path().join("revocation.json");
    let files = FlowFiles {
        autonomous: &autonomous,
        mainnet: &mainnet,
        request: &request,
        trust: &trust,
        credential: &credential,
        revocation: &revocation,
    };

    create_and_inspect_request(&files);
    issue_and_import_credential(&files);
    revoke_and_import_revocation(&files);
}
