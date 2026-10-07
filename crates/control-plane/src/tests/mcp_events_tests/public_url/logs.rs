use super::*;
use std::io::Write;

#[derive(Clone)]
struct LogCapture(Arc<std::sync::Mutex<Vec<u8>>>);

impl Write for LogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn public_mcp_logs_redact_the_secret_on_success_and_errors() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let captured = LogCapture(Arc::new(std::sync::Mutex::new(Vec::new())));
    // Parallel tests can first register a callsite without a thread-local subscriber.
    let _inactive_dispatch = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    let (dir, secret) = tracing::subscriber::with_default(subscriber, || {
        runtime.block_on(async {
            let (dir, state, secret) = public_app().await;
            let (status, body) = public_call(
                &state,
                &secret,
                "tools/call",
                json!({"name": "get_agent_identity", "arguments": {}}),
                false,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.get("error").is_none());
            let (status, _) = public_call(&state, &secret, "unknown-method", json!({}), true).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            let response = public_mcp_app(state.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/mcp/{secret}"))
                        .header("content-type", "application/json")
                        .body(Body::from("not-json"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                public_call(&state, "incorrect", "tools/list", json!({}), false)
                    .await
                    .0,
                StatusCode::NOT_FOUND
            );
            (dir, secret)
        })
    });
    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("/mcp/***"), "{logs}");
    assert!(!logs.contains(&secret), "{logs}");
    for relative in ["diagnostics/local_node.jsonl", "audit/control_plane.jsonl"] {
        let path = dir.path().join(relative);
        if path.exists() {
            assert!(!std::fs::read_to_string(path).unwrap().contains(&secret));
        }
    }
}
