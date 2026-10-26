use super::*;

fn config(value: &Value) -> RequestConfig {
    RequestConfig::parse(&value.to_string()).unwrap()
}

#[test]
fn header_only_configuration_preserves_exact_body_without_extra_signature() {
    let config = config(&json!({"headers": {"x-time": {"$ref": "/timestamp"}}}));
    let original = b"{ \"hello\" : \"world\" }";
    let result = config
        .prepare_at(original, &HeaderMap::new(), 123, "nonce")
        .unwrap();
    assert_eq!(result.body, original);
    assert_eq!(result.headers.len(), 1);
    assert_eq!(result.headers["x-time"], "123");
}

#[test]
fn structured_templates_preserve_types_and_escape_json_strings() {
    let event = json!({"eventId":"evt_1","data":{"text":"\"quoted\"\n中文","a/b":[true,null,42]}});
    let config = config(&json!({"body_template": {
        "id": {"$ref":"/event/eventId"},
        "array": {"$ref":"/event/data/a~1b"},
        "content": {"$json":{"text":{"$json":{"$ref":"/event"}}}},
        "token":"secret\"\nvalue"
    }}));
    let result = config
        .prepare_at(
            &serde_json::to_vec(&event).unwrap(),
            &HeaderMap::new(),
            123,
            "nonce",
        )
        .unwrap();
    let body: Value = serde_json::from_slice(&result.body).unwrap();
    assert_eq!(body["id"], "evt_1");
    assert_eq!(body["array"], event["data"]["a/b"]);
    assert_eq!(body["token"], "secret\"\nvalue");
    let content: Value = serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(content["text"].as_str().unwrap()).unwrap(),
        event
    );
}

#[test]
fn sha256_signs_exact_final_bytes_in_configured_order_and_encoding() {
    for (encoding, expected) in [
        (
            "hex",
            "648f347625b406468c39511e3b8e1ec93023f6db139b5368b5b17e32bb173d4c",
        ),
        ("base64", "ZI80diW0BkaMOVEeO44eyTAj9tsTm1NotbF+MrsXPUw="),
    ] {
        let config = config(&json!({
            "body_template":{"hello":"world"},
            "headers":{"x-time":{"$ref":"/timestamp"},"x-nonce":{"$ref":"/nonce"}},
            "signature":{"algorithm":"sha256","secret":"key","input":["$timestamp","$nonce","$secret","$body"],"encoding":encoding,"header":"x-signature"}
        }));
        let result = config
            .prepare_at(b"{}", &HeaderMap::new(), 1_700_000_000, "nonce")
            .unwrap();
        assert_eq!(result.body, br#"{"hello":"world"}"#);
        assert_eq!(result.headers["x-signature"], expected);
        assert_eq!(result.headers["x-time"], "1700000000");
        assert_eq!(result.headers["x-nonce"], "nonce");
    }
}

#[test]
fn signature_only_configuration_keeps_whitespace_and_supports_literal_separators() {
    let config = config(&json!({"signature":{
        "algorithm":"sha256","secret":"key","input":["$secret",".","$body"],"encoding":"hex","header":"x-sig"
    }}));
    let original = b"{ \"hello\": \"world\" }";
    let result = config
        .prepare_at(original, &HeaderMap::new(), 1, "nonce")
        .unwrap();
    assert_eq!(result.body, original);
    assert_eq!(
        result.headers["x-sig"],
        hex::encode(Sha256::digest([b"key.".as_slice(), original].concat()))
    );
}

#[test]
fn invalid_configuration_is_rejected_without_runtime_fallback() {
    for value in [
        json!({"platform":"anything"}),
        json!({"body_template":{"$ref":42}}),
        json!({"body_template":{"$ref":"/event","other":1}}),
        json!({"body_template":{"$json":{},"other":1}}),
        json!({"body_template":{"$ref":"event"}}),
        json!({"headers":{"Content-Length":"1"}}),
        json!({"headers":{"webhook-signature":"forged"}}),
        json!({"signature":{"algorithm":"md5","secret":"key","input":["$body"],"encoding":"hex","header":"x-sig"}}),
        json!({"signature":{"algorithm":"sha256","secret":"key","input":["$unknown"],"encoding":"hex","header":"x-sig"}}),
        json!({"headers":{"X-SIG":"override"},"signature":{"algorithm":"sha256","secret":"key","input":["$body"],"encoding":"hex","header":"x-sig"}}),
        json!({"response":{"pointer":"success","equals":true}}),
    ] {
        assert!(RequestConfig::parse(&value.to_string()).is_err());
    }
}

#[test]
fn missing_references_and_non_string_or_invalid_headers_fail_delivery() {
    for value in [
        json!({"body_template":{"$ref":"/event/missing"}}),
        json!({"headers":{"x-value":{"$ref":"/event/number"}}}),
        json!({"headers":{"x-value":"line\r\nbreak"}}),
    ] {
        assert!(matches!(
            config(&value).prepare_at(br#"{"number":42}"#, &HeaderMap::new(), 1, "nonce"),
            Err(WebhookFailure::InvalidPayload)
        ));
    }
}

#[test]
fn malformed_pointer_escapes_fail_configuration_for_body_and_response() {
    for pointer in ["/event/~2", "/event/trailing~", "/event/~~0"] {
        assert!(
            RequestConfig::parse(&json!({"body_template":{"$ref":pointer}}).to_string()).is_err()
        );
        assert!(
            RequestConfig::parse(
                &json!({"response":{"pointer":pointer,"equals":true}}).to_string()
            )
            .is_err()
        );
    }
    for pointer in ["", "/", "/event/a~0b", "/event/a~1b", "/event/~01"] {
        assert!(
            RequestConfig::parse(&json!({"body_template":{"$ref":pointer}}).to_string()).is_ok()
        );
        assert!(
            RequestConfig::parse(
                &json!({"response":{"pointer":pointer,"equals":true}}).to_string()
            )
            .is_ok()
        );
    }
}

#[test]
fn expanded_body_is_limited_after_template_rendering() {
    let config = config(&json!({"body_template":[{"$ref":"/event/text"},{"$ref":"/event/text"}]}));
    let body = serde_json::to_vec(&json!({"text":"x".repeat(MAX_REQUEST_BYTES / 2)})).unwrap();
    assert!(body.len() < MAX_REQUEST_BYTES);
    assert!(matches!(
        config.prepare_at(&body, &HeaderMap::new(), 1, "nonce"),
        Err(WebhookFailure::RequestTooLarge)
    ));
}

#[tokio::test]
async fn response_condition_checks_real_http_json_and_preserves_error_statuses() {
    for (status, body, expected) in [
        (200, br#"{"success":true}"#.to_vec(), Ok(200)),
        (
            200,
            br#"{"success":false}"#.to_vec(),
            Err(WebhookFailure::ReceiverRejected),
        ),
        (200, b"{}".to_vec(), Err(WebhookFailure::ReceiverRejected)),
        (
            200,
            b"not json".to_vec(),
            Err(WebhookFailure::ReceiverRejected),
        ),
        (
            200,
            vec![b' '; MAX_REQUEST_BYTES + 1],
            Err(WebhookFailure::ReceiverRejected),
        ),
        (410, b"not json".to_vec(), Ok(410)),
        (413, b"not json".to_vec(), Ok(413)),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = axum::Router::new().route(
            "/",
            axum::routing::get(move || {
                let body = body.clone();
                async move { (axum::http::StatusCode::from_u16(status).unwrap(), body) }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let response = reqwest::get(format!("http://{address}/")).await.unwrap();
        let config = config(&json!({"response":{"pointer":"/success","equals":true}}));
        assert_eq!(config.check_response(response).await, expected);
        server.abort();
    }
}
