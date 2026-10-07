use super::*;
use crate::mcp_events::webhook::tests::{CapturedRequest, read_http_request};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use tokio::io::AsyncWriteExt;
use tokio_rustls::{
    TlsAcceptor,
    rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer},
};

pub(super) struct Receiver {
    pub url: String,
    pub client: reqwest::Client,
    pub requests: Arc<tokio::sync::Mutex<Vec<CapturedRequest>>>,
    pub status: Arc<AtomicU16>,
    pub wrong_challenge: Arc<AtomicBool>,
    pub delivery_gate: Arc<tokio::sync::Semaphore>,
    pub delivery_started: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Receiver {
    pub async fn start() -> Self {
        let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
        let certificate = rcgen::generate_simple_self_signed(vec!["webhook.test".into()]).unwrap();
        let der = certificate.cert.der().clone();
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![der.clone()],
                PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .http1_only()
            .resolve("webhook.test", address)
            .add_root_certificate(reqwest::Certificate::from_der(der.as_ref()).unwrap())
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();
        let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let status = Arc::new(AtomicU16::new(200));
        let wrong_challenge = Arc::new(AtomicBool::new(false));
        let delivery_gate = Arc::new(tokio::sync::Semaphore::new(1));
        let delivery_started = Arc::new(tokio::sync::Notify::new());
        let (gate, started) = (delivery_gate.clone(), delivery_started.clone());
        let (records, response_status, wrong) =
            (requests.clone(), status.clone(), wrong_challenge.clone());
        let task = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let Ok(mut stream) = acceptor.accept(socket).await else {
                    continue;
                };
                let captured = read_http_request(&mut stream).await;
                let request: Value = serde_json::from_slice(&captured.body).unwrap();
                let response = if request["type"] == "verification" {
                    json!({"challenge": if wrong.load(Ordering::SeqCst) { json!("wrong") } else { request["challenge"].clone() }})
                } else { json!({}) }.to_string();
                records.lock().await.push(captured);
                let _permit = if request["type"] == "verification" {
                    None
                } else {
                    started.notify_one();
                    Some(gate.acquire().await.unwrap())
                };
                let status = response_status.load(Ordering::SeqCst);
                let header = format!(
                    "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                );
                stream.write_all(header.as_bytes()).await.unwrap();
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.flush().await.unwrap();
            }
        });
        Self {
            url: format!("https://webhook.test:{}/hook", address.port()),
            client,
            requests,
            status,
            wrong_challenge,
            delivery_gate,
            delivery_started,
            task,
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}
