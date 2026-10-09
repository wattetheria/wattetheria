use std::{net::IpAddr, net::SocketAddr, time::Duration};

use base64::Engine;
use reqwest::{
    Client, Url,
    header::{CONTENT_TYPE, HeaderMap, HeaderValue},
    redirect::Policy,
};
use standardwebhooks::{
    HEADER_WEBHOOK_ID, HEADER_WEBHOOK_SIGNATURE, HEADER_WEBHOOK_TIMESTAMP, Webhook,
};

pub(super) const MAX_REQUEST_BYTES: usize = 256 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Posts signed events to the operator-configured event webhook.
#[derive(Default)]
pub(crate) struct WebhookClient {
    #[cfg(test)]
    pub(crate) callback_client: Option<Client>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebhookFailure {
    InvalidUrl,
    InvalidSecret,
    InvalidPayload,
    RequestTooLarge,
    Transport,
}

impl WebhookClient {
    pub(crate) async fn verify_callback(
        &self,
        url: &str,
        secret: &str,
        subscription_id: &str,
    ) -> Result<(), &'static str> {
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let client = self
                .subscription_client(url)
                .await
                .map_err(|error| match error {
                    WebhookFailure::InvalidUrl => "challenge_failed",
                    _ => "connection_refused",
                })?;
            verify_callback_with_client(&client, url, secret, subscription_id).await
        })
        .await
        .map_err(|_| "timeout")?
    }

    pub(crate) async fn post_subscription(
        &self,
        url: &str,
        secret: &str,
        old_secret: Option<&str>,
        event_id: &str,
        body: &[u8],
        subscription_id: &str,
    ) -> Result<u16, WebhookFailure> {
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let client = self.subscription_client(url).await?;
            let mut headers = HeaderMap::new();
            headers.insert(
                "X-MCP-Subscription-Id",
                HeaderValue::from_str(subscription_id)
                    .map_err(|_| WebhookFailure::InvalidPayload)?,
            );
            post_event_with_client(&client, url, secret, old_secret, event_id, body, &headers).await
        })
        .await
        .map_err(|_| WebhookFailure::Transport)?
    }

    /// The destination comes from the deployment environment, so http and
    /// private addresses are allowed; `extra_headers` carry the receiver's own
    /// authentication.
    pub(crate) async fn post_event(
        &self,
        url: &str,
        secret: &str,
        old_secret: Option<&str>,
        event_id: &str,
        body: &[u8],
        extra_headers: &HeaderMap,
    ) -> Result<u16, WebhookFailure> {
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let client = client_for_url(url).await?;
            post_event_with_client(
                &client,
                url,
                secret,
                old_secret,
                event_id,
                body,
                extra_headers,
            )
            .await
        })
        .await
        .map_err(|_| WebhookFailure::Transport)?
    }

    async fn subscription_client(&self, url: &str) -> Result<Client, WebhookFailure> {
        validate_callback_url(url)?;
        #[cfg(test)]
        if let Some(client) = &self.callback_client {
            return Ok(client.clone());
        }
        client_for_destination(url, true).await
    }
}

async fn verify_callback_with_client(
    client: &Client,
    url: &str,
    secret: &str,
    subscription_id: &str,
) -> Result<(), &'static str> {
    use subtle::ConstantTimeEq;
    let challenge = uuid::Uuid::new_v4().to_string();
    let body =
        serde_json::to_vec(&serde_json::json!({"type": "verification", "challenge": challenge}))
            .map_err(|_| "challenge_failed")?;
    let message_id = format!("msg_verification_{}", uuid::Uuid::new_v4());
    let mut headers =
        signed_headers(secret, None, &message_id, &body).map_err(|_| "challenge_failed")?;
    headers.insert(
        "X-MCP-Subscription-Id",
        HeaderValue::from_str(subscription_id).map_err(|_| "challenge_failed")?,
    );
    let response = client
        .post(url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "timeout"
            } else if error.is_connect() {
                "connection_refused"
            } else {
                "challenge_failed"
            }
        })?;
    if !response.status().is_success() {
        return Err(if response.status().is_server_error() {
            "http_5xx"
        } else {
            "http_4xx"
        });
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "challenge_failed")? {
        if bytes.len() + chunk.len() > 4096 {
            return Err("challenge_failed");
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "challenge_failed")?;
    let echoed = value["challenge"].as_str().ok_or("challenge_failed")?;
    if bool::from(challenge.as_bytes().ct_eq(echoed.as_bytes())) {
        Ok(())
    } else {
        Err("challenge_failed")
    }
}

async fn post_event_with_client(
    client: &Client,
    url: &str,
    secret: &str,
    old_secret: Option<&str>,
    event_id: &str,
    body: &[u8],
    extra_headers: &HeaderMap,
) -> Result<u16, WebhookFailure> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(WebhookFailure::RequestTooLarge);
    }
    let mut headers = signed_headers(secret, old_secret, event_id, body)?;
    for (name, value) in extra_headers {
        if !headers.contains_key(name) {
            headers.insert(name.clone(), value.clone());
        }
    }
    let response = client
        .post(url)
        .headers(headers)
        .body(body.to_vec())
        .send()
        .await
        .map_err(|_| WebhookFailure::Transport)?;
    Ok(response.status().as_u16())
}

fn signed_headers(
    secret: &str,
    old_secret: Option<&str>,
    message_id: &str,
    body: &[u8],
) -> Result<HeaderMap, WebhookFailure> {
    let signer = validated_signer(secret)?;
    let old_signer = old_secret.map(validated_signer).transpose()?;
    let timestamp = chrono::Utc::now().timestamp();
    let mut signature = signer
        .sign(message_id, timestamp, body)
        .map_err(|_| WebhookFailure::InvalidPayload)?;
    if let Some(old_signer) = old_signer {
        signature.push(' ');
        signature.push_str(
            &old_signer
                .sign(message_id, timestamp, body)
                .map_err(|_| WebhookFailure::InvalidPayload)?,
        );
    }

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        HEADER_WEBHOOK_ID,
        HeaderValue::from_str(message_id).map_err(|_| WebhookFailure::InvalidPayload)?,
    );
    headers.insert(
        HEADER_WEBHOOK_TIMESTAMP,
        HeaderValue::from_str(&timestamp.to_string())
            .map_err(|_| WebhookFailure::InvalidPayload)?,
    );
    headers.insert(
        HEADER_WEBHOOK_SIGNATURE,
        HeaderValue::from_str(&signature).map_err(|_| WebhookFailure::InvalidPayload)?,
    );
    Ok(headers)
}

fn validated_signer(secret: &str) -> Result<Webhook, WebhookFailure> {
    let encoded = secret
        .strip_prefix("whsec_")
        .ok_or(WebhookFailure::InvalidSecret)?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| WebhookFailure::InvalidSecret)?;
    if !(24..=64).contains(&decoded.len()) {
        return Err(WebhookFailure::InvalidSecret);
    }
    Webhook::new(secret).map_err(|_| WebhookFailure::InvalidSecret)
}

pub(crate) fn validate_secret(secret: &str) -> Result<(), WebhookFailure> {
    validated_signer(secret).map(|_| ())
}

/// Resolves the host once and pins the connection to those addresses;
/// redirects are never followed.
async fn client_for_url(raw_url: &str) -> Result<Client, WebhookFailure> {
    client_for_destination(raw_url, false).await
}

pub(crate) fn validate_callback_url(raw_url: &str) -> Result<(), WebhookFailure> {
    let url = Url::parse(raw_url).map_err(|_| WebhookFailure::InvalidUrl)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(WebhookFailure::InvalidUrl);
    }
    let host = url.host_str().ok_or(WebhookFailure::InvalidUrl)?;
    if host.trim_end_matches('.').eq_ignore_ascii_case("localhost") {
        return Err(WebhookFailure::InvalidUrl);
    }
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>()
        && !public_address(ip)
    {
        return Err(WebhookFailure::InvalidUrl);
    }
    Ok(())
}

async fn client_for_destination(
    raw_url: &str,
    public_only: bool,
) -> Result<Client, WebhookFailure> {
    if public_only {
        validate_callback_url(raw_url)?;
    }
    let url = Url::parse(raw_url).map_err(|_| WebhookFailure::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(WebhookFailure::InvalidUrl);
    }
    let host = url.host_str().ok_or(WebhookFailure::InvalidUrl)?;
    let port = url
        .port_or_known_default()
        .ok_or(WebhookFailure::InvalidUrl)?;
    let ip_host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let addresses: Vec<SocketAddr> = if let Ok(ip) = ip_host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else if host.starts_with('[') {
        return Err(WebhookFailure::InvalidUrl);
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| WebhookFailure::Transport)?
            .collect()
    };
    if addresses.is_empty() {
        return Err(WebhookFailure::Transport);
    }
    if public_only
        && addresses
            .iter()
            .any(|address| !public_address(address.ip()))
    {
        return Err(WebhookFailure::InvalidUrl);
    }
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(0)
        .http1_only()
        .resolve_to_addrs(host, &addresses)
        .build()
        .map_err(|_| WebhookFailure::Transport)
}

fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_documentation()
                && !ip.is_broadcast()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && (b == 18 || b == 19))
                && !(a == 192 && b == 0 && c == 0)
                && !(a == 192 && b == 88 && c == 99)
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_address(IpAddr::V4(v4));
            }
            let segments = ip.segments();
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && segments[1] == 0xdb8)
                && !(segments[0] == 0x2001 && segments[1] < 0x200)
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] < 0x1000)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;

    use rcgen::generate_simple_self_signed;
    use reqwest::header::{CONTENT_LENGTH, HeaderName};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer},
    };

    const SECRET: &str = "whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD";
    const OLD_SECRET: &str = "whsec_AAECAwQFBgcICQoLDA0ODxAREhMUFRYX";

    #[test]
    fn subscription_destinations_reject_non_public_addresses_without_changing_operator_policy() {
        for address in [
            "0.0.0.0",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.2",
            "169.254.169.254",
            "100.64.1.2",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "::",
            "::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "2002:7f00:1::",
        ] {
            let ip: IpAddr = address.parse().unwrap();
            assert!(!public_address(ip), "{address}");
            let host = if ip.is_ipv6() {
                format!("[{address}]")
            } else {
                address.to_owned()
            };
            assert_eq!(
                validate_callback_url(&format!("https://{host}/hook")),
                Err(WebhookFailure::InvalidUrl)
            );
        }
        for address in [
            "8.8.8.8",
            "93.184.216.34",
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
        ] {
            assert!(public_address(address.parse().unwrap()), "{address}");
        }
        for url in [
            "http://callback.example/hook",
            "https://localhost/hook",
            "https://localhost./hook",
            "https://user:password@callback.example/hook",
            "https://callback.example/hook#fragment",
        ] {
            assert_eq!(validate_callback_url(url), Err(WebhookFailure::InvalidUrl));
        }
        assert!(validate_callback_url("https://callback.example/hook?key=abc").is_ok());
    }

    #[tokio::test]
    async fn subscription_delivery_rechecks_the_destination_and_rejects_oversized_bodies() {
        assert_eq!(
            client_for_destination("https://127.0.0.1/hook", true)
                .await
                .err(),
            Some(WebhookFailure::InvalidUrl)
        );
        assert_eq!(
            client_for_destination("https://[::ffff:127.0.0.1]/hook", true)
                .await
                .err(),
            Some(WebhookFailure::InvalidUrl)
        );
        let (client, url, receiver) = local_https_receiver(200).await;
        let oversized = vec![b'a'; MAX_REQUEST_BYTES + 1];
        assert_eq!(
            post_event_with_client(
                &client,
                &url,
                SECRET,
                None,
                "evt_large",
                &oversized,
                &HeaderMap::new()
            )
            .await,
            Err(WebhookFailure::RequestTooLarge)
        );
        receiver.abort();
    }

    #[test]
    fn signatures_verify_with_the_standard_webhooks_sdk() {
        let body = br#"{"type":"message","value":1}"#;
        let headers = signed_headers(SECRET, Some(OLD_SECRET), "evt_1", body).unwrap();
        assert_eq!(headers[HEADER_WEBHOOK_ID], "evt_1");
        Webhook::new(SECRET)
            .unwrap()
            .verify(body, &headers)
            .unwrap();
        Webhook::new(OLD_SECRET)
            .unwrap()
            .verify(body, &headers)
            .unwrap();
    }

    #[test]
    fn rejects_invalid_secrets_and_non_utf8_payloads() {
        for secret in [
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYX",
            "whsec_!",
            "whsec_AAECAwQ=",
        ] {
            assert!(matches!(
                validated_signer(secret),
                Err(WebhookFailure::InvalidSecret)
            ));
        }
        assert_eq!(
            signed_headers(SECRET, None, "evt_1", &[0xff]).unwrap_err(),
            WebhookFailure::InvalidPayload
        );
    }

    #[tokio::test]
    async fn operator_destinations_allow_http_and_private_addresses_only() {
        for url in [
            "http://10.0.0.1/hook?key=abc",
            "https://192.168.0.1/hook",
            "http://[fc00::1]:8080/hook",
            "http://127.0.0.1/hook",
            "https://8.8.8.8/hook",
        ] {
            assert!(client_for_url(url).await.is_ok(), "{url}");
        }
        for url in [
            "ftp://127.0.0.1/hook",
            "http://user:pass@127.0.0.1/hook",
            "http://127.0.0.1/hook#fragment",
            "not a url",
        ] {
            assert_eq!(
                client_for_url(url).await.err(),
                Some(WebhookFailure::InvalidUrl),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn http_delivery_carries_query_receiver_headers_and_signature() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, mut received) = tokio::sync::mpsc::channel(1);
        let router = axum::Router::new().route(
            "/hook",
            axum::routing::post(
                move |axum::extract::RawQuery(query): axum::extract::RawQuery,
                      headers: HeaderMap,
                      body: axum::body::Bytes| {
                    let sender = sender.clone();
                    async move {
                        sender.send((query, headers, body)).await.unwrap();
                        axum::http::StatusCode::OK
                    }
                },
            ),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let url = format!("http://{address}/hook?key=abc");
        let mut extra = HeaderMap::new();
        extra.insert(
            "authorization",
            HeaderValue::from_static("Bearer receiver-key"),
        );
        // A receiver header can never replace the signature headers.
        extra.insert(HEADER_WEBHOOK_ID, HeaderValue::from_static("forged"));
        let body = br#"{"eventId":"evt_env"}"#;
        assert_eq!(
            WebhookClient::default()
                .post_event(&url, SECRET, None, "evt_env", body, &extra)
                .await
                .unwrap(),
            200
        );
        let (query, headers, delivered) = received.recv().await.unwrap();
        assert_eq!(query.as_deref(), Some("key=abc"));
        assert_eq!(headers["authorization"], "Bearer receiver-key");
        assert_eq!(headers[HEADER_WEBHOOK_ID], "evt_env");
        assert_eq!(delivered.as_ref(), body);
        Webhook::new(SECRET)
            .unwrap()
            .verify(&delivered, &headers)
            .unwrap();
        server.abort();
    }

    pub(crate) struct CapturedRequest {
        pub(crate) headers: HeaderMap,
        pub(crate) body: Vec<u8>,
    }

    async fn local_https_receiver(status: u16) -> (Client, String, JoinHandle<CapturedRequest>) {
        let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
        let certified = generate_simple_self_signed(vec!["webhook.test".to_owned()]).unwrap();
        let cert_der = certified.cert.der().clone();
        let key_der = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key_der.into())
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("https://webhook.test:{}/hook", address.port());
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(Policy::none())
            .http1_only()
            .resolve("webhook.test", address)
            .add_root_certificate(reqwest::Certificate::from_der(cert_der.as_ref()).unwrap())
            .build()
            .unwrap();
        let handle = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(socket).await.unwrap();
            let captured = read_http_request(&mut stream).await;
            let response_head = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
            stream.write_all(response_head.as_bytes()).await.unwrap();
            let _ = stream.flush().await;
            captured
        });
        (client, url, handle)
    }

    pub(crate) async fn read_http_request(
        stream: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    ) -> CapturedRequest {
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0u8; 4096];
            let count = stream.read(&mut chunk).await.unwrap();
            assert!(count > 0, "request ended before headers");
            bytes.extend_from_slice(&chunk[..count]);
            assert!(bytes.len() <= MAX_REQUEST_BYTES + 16 * 1024);
            if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let head = std::str::from_utf8(&bytes[..header_end]).unwrap();
        assert!(head.starts_with("POST /hook HTTP/1.1\r\n"));
        let mut headers = HeaderMap::new();
        for line in head.split("\r\n").skip(1).filter(|line| !line.is_empty()) {
            let (name, value) = line.split_once(':').unwrap();
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value.trim()).unwrap(),
            );
        }
        let content_length = headers[CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        while bytes.len() - header_end < content_length {
            let mut chunk = [0u8; 4096];
            let count = stream.read(&mut chunk).await.unwrap();
            assert!(count > 0, "request ended before body");
            bytes.extend_from_slice(&chunk[..count]);
            assert!(bytes.len() <= MAX_REQUEST_BYTES + 16 * 1024);
        }
        CapturedRequest {
            headers,
            body: bytes[header_end..header_end + content_length].to_vec(),
        }
    }

    #[tokio::test]
    async fn real_https_delivery_preserves_bytes_and_returns_receiver_status() {
        let body = br#"{"type":"mcp","data":"exact  bytes"}"#;
        for status in [200, 410, 413] {
            let (client, url, server) = local_https_receiver(status).await;
            let delivered = tokio::time::timeout(
                REQUEST_TIMEOUT,
                post_event_with_client(
                    &client,
                    &url,
                    SECRET,
                    Some(OLD_SECRET),
                    "evt_1",
                    body,
                    &HeaderMap::new(),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(delivered, status);
            let captured = server.await.unwrap();
            assert_eq!(captured.body, body);
            assert_eq!(captured.headers[HEADER_WEBHOOK_ID], "evt_1");
            Webhook::new(SECRET)
                .unwrap()
                .verify(&captured.body, &captured.headers)
                .unwrap();
            Webhook::new(OLD_SECRET)
                .unwrap()
                .verify(&captured.body, &captured.headers)
                .unwrap();
        }
    }
}
