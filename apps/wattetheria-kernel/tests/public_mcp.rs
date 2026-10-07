use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

struct Kernel(Child);

impl Drop for Kernel {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

async fn start(data: &Path, public: Option<SocketAddr>, use_env: bool) -> (Kernel, Client, String) {
    let control = free_address();
    let log = std::fs::File::create(data.join("kernel.log")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_wattetheria-kernel"));
    command
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "--control-plane-bind",
            &control.to_string(),
            "--agent-event-mode",
            "mcp_events",
            "--mcp-token-auth-required",
        ])
        .env_remove("WATTETHERIA_MCP_PUBLIC_BIND")
        .env_remove("WATTETHERIA_EVENT_WEBHOOK_URL")
        .env("RUST_LOG", "trace")
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    if let Some(public) = public {
        if use_env {
            command.env("WATTETHERIA_MCP_PUBLIC_BIND", public.to_string());
        } else {
            command.args(["--mcp-public-bind", &public.to_string()]);
        }
    }
    let mut kernel = Kernel(command.spawn().unwrap());
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let endpoint = format!("http://{control}");
    for _ in 0..100 {
        assert!(
            kernel.0.try_wait().unwrap().is_none(),
            "kernel exited: {}",
            std::fs::read_to_string(data.join("kernel.log")).unwrap()
        );
        if client
            .get(format!("{endpoint}/supervision"))
            .send()
            .await
            .is_ok()
        {
            return (kernel, client, endpoint);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "kernel did not start: {}",
        std::fs::read_to_string(data.join("kernel.log")).unwrap()
    );
}

async fn mcp(client: &Client, url: &str, method: &str, modern: bool) -> (StatusCode, Value) {
    let version = if modern { "2026-07-28" } else { "2025-11-25" };
    let params = if modern {
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": version,
            "io.modelcontextprotocol/clientCapabilities": {}}})
    } else {
        json!({"protocolVersion": version})
    };
    let response = client
        .post(url)
        .header("authorization", "Bearer incorrect")
        .header("mcp-protocol-version", version)
        .header("mcp-method", method)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.bytes().await.unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

#[tokio::test]
async fn public_mcp_disabled_kernel_starts_only_the_local_control_plane() {
    let dir = tempfile::tempdir().unwrap();
    let (_kernel, client, control) = start(dir.path(), None, false).await;
    assert!(dir.path().join("wattetheria.db").exists());
    assert!(!dir.path().join("mcp_events.sqlite3").exists());
    assert!(!dir.path().join("mcp_url_secret").exists());
    let token = std::fs::read_to_string(dir.path().join("control.token")).unwrap();
    let response = client
        .post(format!("{control}/v1/mcp/public-url/rotate"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(!dir.path().join("mcp_url_secret").exists());
}

#[tokio::test]
async fn public_mcp_kernel_accepts_cli_and_env_bind_and_keeps_the_secret_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut first_secret = None;
    for use_env in [false, true] {
        let public = free_address();
        let (kernel, client, control) = start(dir.path(), Some(public), use_env).await;
        let secret = std::fs::read_to_string(dir.path().join("mcp_url_secret")).unwrap();
        assert_eq!(secret.len(), 43);
        if let Some(first) = &first_secret {
            assert_eq!(&secret, first);
        }
        let url = format!("http://{public}/mcp/{secret}");
        for (method, modern) in [
            ("initialize", false),
            ("tools/list", false),
            ("server/discover", true),
            ("events/list", true),
        ] {
            let (status, body) = mcp(&client, &url, method, modern).await;
            assert_eq!(status, StatusCode::OK, "{method}: {body}");
            assert!(body.get("error").is_none(), "{body}");
        }
        assert_eq!(
            client.get(&url).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .post(format!("http://{public}/agent-events"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let token = std::fs::read_to_string(dir.path().join("control.token")).unwrap();
        assert_eq!(
            mcp(&client, &format!("{control}/mcp"), "tools/list", false)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        if use_env {
            let rotated = client
                .post(format!("{control}/v1/mcp/public-url/rotate"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap();
            assert_eq!(
                mcp(&client, &url, "tools/list", false).await.0,
                StatusCode::NOT_FOUND
            );
            let new_url = format!("http://{public}{}", rotated["path"].as_str().unwrap());
            assert_eq!(
                mcp(&client, &new_url, "tools/list", false).await.0,
                StatusCode::OK
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("control.token")).unwrap(),
                token
            );
        }
        first_secret = Some(secret.clone());
        drop(kernel);
        assert!(
            !std::fs::read_to_string(dir.path().join("kernel.log"))
                .unwrap()
                .contains(&secret)
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn public_mcp_wide_secret_permissions_disable_only_the_public_listener() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let secret_path = dir.path().join("mcp_url_secret");
    std::fs::write(&secret_path, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    std::fs::set_permissions(&secret_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let public = free_address();
    let (_kernel, _client, _control) = start(dir.path(), Some(public), true).await;
    assert!(TcpStream::connect_timeout(&public, Duration::from_millis(100)).is_err());
    let logs = std::fs::read_to_string(dir.path().join("kernel.log")).unwrap();
    assert!(logs.contains("permissions wider than 0600"), "{logs}");
    assert!(logs.contains("local control plane continues"));
    assert!(!logs.contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
}

#[cfg(unix)]
#[tokio::test]
async fn public_mcp_unavailable_revocation_store_does_not_enable_a_secret_url() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("mcp_events.key");
    std::fs::write(&key, [0_u8; 32]).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let public = free_address();
    let (_kernel, client, control) = start(dir.path(), Some(public), true).await;
    assert!(TcpStream::connect_timeout(&public, Duration::from_millis(100)).is_err());
    assert!(!dir.path().join("mcp_url_secret").exists());
    let token = std::fs::read_to_string(dir.path().join("control.token")).unwrap();
    let response = client
        .post(format!("{control}/v1/mcp/public-url/rotate"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let logs = std::fs::read_to_string(dir.path().join("kernel.log")).unwrap();
    assert!(
        logs.contains("subscription revocation store unavailable"),
        "{logs}"
    );
}
