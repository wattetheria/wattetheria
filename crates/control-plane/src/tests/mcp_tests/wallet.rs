use super::*;
use axum::extract::State as AxumState;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::test]
async fn mcp_lists_read_only_watt_wallet_tools_with_precise_schemas() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();

    let balance = find_tool(tools, "get_watt_balance");
    assert_eq!(balance["_meta"]["wattetheria"]["method"], "GET");
    assert_eq!(
        balance["_meta"]["wattetheria"]["path"],
        "/v1/wattetheria/wallet"
    );
    assert_eq!(balance["_meta"]["wattetheria"]["readOnly"], true);
    assert_eq!(balance["inputSchema"]["properties"], json!({}));
    assert_eq!(balance["inputSchema"]["additionalProperties"], false);

    let entries = find_tool(tools, "list_watt_ledger_entries");
    assert_eq!(entries["_meta"]["wattetheria"]["method"], "GET");
    assert_eq!(
        entries["_meta"]["wattetheria"]["path"],
        "/v1/wattetheria/wallet/entries"
    );
    assert_eq!(entries["_meta"]["wattetheria"]["readOnly"], true);
    let properties = entries["inputSchema"]["properties"].as_object().unwrap();
    assert_eq!(
        properties.keys().collect::<Vec<_>>(),
        ["before_seq", "limit"],
        "only the two paging arguments are accepted"
    );
    assert_eq!(properties["limit"]["type"], "integer");
    assert_eq!(properties["before_seq"]["type"], "integer");
    assert_eq!(entries["inputSchema"]["additionalProperties"], false);
    assert_schema_requires(entries, &[]);
}

#[tokio::test]
async fn mcp_watt_wallet_tools_report_a_missing_gateway_ledger_instead_of_inventing_watt() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    for (name, arguments) in [
        ("get_watt_balance", json!({})),
        ("list_watt_ledger_entries", json!({"limit": 5})),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments}
            }),
        )
        .await;
        assert_eq!(response["result"]["isError"], true, "{name}: {response}");
        assert_eq!(
            response["result"]["_meta"]["httpStatus"], 503,
            "{name}: {response}"
        );
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        assert_eq!(payload["code"], "ledger_unavailable", "{name}: {payload}");
    }
}

#[tokio::test]
async fn mcp_lists_send_watt_with_a_precise_schema() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    let send = find_tool(tools, "send_watt");
    assert_eq!(send["_meta"]["wattetheria"]["method"], "POST");
    assert_eq!(
        send["_meta"]["wattetheria"]["path"],
        "/v1/wattetheria/wallet/send"
    );
    assert_eq!(send["_meta"]["wattetheria"]["readOnly"], false);
    let properties = send["inputSchema"]["properties"].as_object().unwrap();
    assert_eq!(
        properties.keys().collect::<Vec<_>>(),
        ["amount", "memo", "to", "to_kind", "transfer_id"]
    );
    assert_eq!(properties["amount"]["type"], "integer");
    assert_eq!(properties["amount"]["minimum"], 1);
    assert_eq!(
        properties["to_kind"]["enum"],
        json!(["agent", "service_agent"])
    );
    assert_eq!(send["inputSchema"]["additionalProperties"], false);
    assert_schema_requires(send, &["to", "amount", "transfer_id"]);
}

#[tokio::test]
async fn mcp_send_watt_validates_before_touching_the_ledger() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let own_did = state.agent_did.clone();
    for (arguments, expected) in [
        (
            json!({"to": own_did, "amount": 5, "transfer_id": "t"}),
            "your own account",
        ),
        (
            json!({"to": "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK", "amount": 5}),
            "transfer_id is required",
        ),
        (
            json!({"to": "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK", "amount": 5, "transfer_id": "  "}),
            "transfer_id is required",
        ),
        (
            json!({"to": "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK", "amount": 0}),
            "positive",
        ),
        (
            json!({"to": "   ", "amount": 5, "transfer_id": "t"}),
            "to is required",
        ),
        (
            json!({"to": "nobody-here", "amount": 5, "transfer_id": "t"}),
            "not an Agent DID, a known public_id, or the display name",
        ),
        (
            json!({"to": "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK", "amount": 5, "memo": "x".repeat(201)}),
            "memo must be at most 200",
        ),
        (
            json!({"to": "shop@wattetheria", "to_kind": "service_agent", "amount": 5, "transfer_id": "t"}),
            "servicenet is not configured",
        ),
        // A DID the ledger would happily credit but nobody could ever sign for.
        (
            json!({"to": "did:not-real", "amount": 5, "transfer_id": "t"}),
            "not a valid agent did:key",
        ),
        (
            json!({"to": "did:key:zNotAKey", "amount": 5, "transfer_id": "t"}),
            "not a valid agent did:key",
        ),
        (
            json!({"to": "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK", "amount": 5, "transfer_id": "has space"}),
            "transfer_id must not contain whitespace",
        ),
        (
            json!({"to": "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK", "amount": 5, "transfer_id": "x".repeat(129)}),
            "transfer_id must be at most 128",
        ),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "send_watt", "arguments": arguments}
            }),
        )
        .await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(response["result"]["_meta"]["httpStatus"], 400, "{response}");
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(expected), "expected `{expected}` in {text}");
    }

    // A well-formed send with no gateway reports the missing ledger, like the reads do.
    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {"name": "send_watt", "arguments": {
                "to": "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK", "amount": 5,
                "transfer_id": "t"
            }}
        }),
    )
    .await;
    assert_eq!(response["result"]["_meta"]["httpStatus"], 503, "{response}");
}

/// A gateway ledger with one funded account: transfers over the balance get 402, the rest
/// are recorded so the test can inspect the signed payload the node sent.
#[derive(Clone, Default)]
struct MockLedger {
    transfers: Arc<Mutex<Vec<Value>>>,
}

const MOCK_BALANCE: i64 = 100;

async fn spawn_mock_ledger() -> (String, MockLedger) {
    let mock = MockLedger::default();
    let router = axum::Router::new()
        .route(
            "/api/ledger/accounts/{account}",
            axum::routing::get(
                |AxumState(mock): AxumState<MockLedger>,
                 axum::extract::Path(account): axum::extract::Path<String>| async move {
                    let sent: i64 = mock
                        .transfers
                        .lock()
                        .await
                        .iter()
                        .filter(|payload| payload["from"] == account)
                        .filter_map(|payload| payload["amount"].as_i64())
                        .sum();
                    Json(json!({
                        "account_id": account,
                        "balance": MOCK_BALANCE - sent,
                        "locked_in_escrow": 0,
                        "exists": true
                    }))
                },
            ),
        )
        .route(
            "/api/ledger/transfers",
            axum::routing::post(
                |AxumState(mock): AxumState<MockLedger>, Json(body): Json<Value>| async move {
                    assert!(body["signature"].as_str().is_some_and(|s| !s.is_empty()));
                    assert!(body["public_key"].as_str().is_some_and(|s| !s.is_empty()));
                    let payload = body["payload"].clone();
                    let amount = payload["amount"].as_i64().unwrap();
                    let mut transfers = mock.transfers.lock().await;
                    if let Some(seq) = transfers
                        .iter()
                        .position(|posted| posted["transfer_id"] == payload["transfer_id"])
                    {
                        // The gateway posts `transfer:<transfer_id>` once and hands the same
                        // entry back on a retry, without a second debit; the same key with
                        // different terms is a conflict.
                        if transfers[seq]["from"] != payload["from"]
                            || transfers[seq]["to"] != payload["to"]
                            || transfers[seq]["amount"] != payload["amount"]
                        {
                            return (
                                axum::http::StatusCode::CONFLICT,
                                Json(json!({"error": format!(
                                    "idempotency key transfer:{} was already used for a different posting",
                                    payload["transfer_id"].as_str().unwrap()
                                )})),
                            );
                        }
                        return (
                            axum::http::StatusCode::OK,
                            Json(json!({"entry": {
                                "entry_id": format!("transfer:{}", payload["transfer_id"].as_str().unwrap()),
                                "kind": "transfer",
                                "debit_account": transfers[seq]["from"],
                                "credit_account": transfers[seq]["to"],
                                "amount": transfers[seq]["amount"],
                                "memo": transfers[seq]["memo"],
                                "seq": seq + 1,
                            }})),
                        );
                    }
                    let sent: i64 = transfers
                        .iter()
                        .filter_map(|payload| payload["amount"].as_i64())
                        .sum();
                    if sent + amount > MOCK_BALANCE {
                        return (
                            axum::http::StatusCode::PAYMENT_REQUIRED,
                            Json(json!({"error": format!(
                                "insufficient balance: {} has {}, needs {amount}",
                                payload["from"], MOCK_BALANCE - sent
                            )})),
                        );
                    }
                    transfers.push(payload.clone());
                    (
                        axum::http::StatusCode::CREATED,
                        Json(json!({"entry": {
                            "entry_id": format!("transfer:{}", payload["transfer_id"].as_str().unwrap()),
                            "kind": "transfer",
                            "debit_account": payload["from"],
                            "credit_account": payload["to"],
                            "amount": amount,
                            "memo": payload["memo"],
                            "seq": transfers.len(),
                        }})),
                    )
                },
            ),
        )
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (url, mock)
}

/// Calls `send_watt`; a test that does not care about the idempotency key gets a fresh one.
async fn send(app: Router, token: &str, mut arguments: Value) -> Value {
    if arguments.get("transfer_id").is_none() {
        arguments["transfer_id"] = Value::String(uuid::Uuid::new_v4().to_string());
    }
    mcp_request(
        app,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {"name": "send_watt", "arguments": arguments}
        }),
    )
    .await
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_send_watt_resolves_the_recipient_and_posts_one_signed_transfer() {
    let (ledger_url, mock) = spawn_mock_ledger().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        gateway_urls: vec![ledger_url],
        ..state
    };
    let app = app(state.clone());
    bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    let identity = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let local_public_id = identity
        .public_memory_owner
        .public
        .unwrap_or(identity.public_memory_owner.controller);

    // A friend this node learned socially: known by public_id and by display name.
    let friend_did = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".to_owned();
    let friend_public_id = scoped_id("hark", &friend_did);
    wattetheria_social::application::remote_identity_service::upsert_remote_identity(
        &*state.social_store,
        &wattetheria_social::domain::identities::RemoteIdentityProfile {
            public_id: friend_public_id.clone(),
            agent_did: friend_did.clone(),
            display_name: "Hark".to_owned(),
            description: None,
            capabilities: Vec::new(),
            skills: Vec::new(),
            did_document_json: None,
            active: true,
            last_profile_fetched_at: Some(1),
            created_at: 1,
            updated_at: 1,
        },
    )
    .unwrap();
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{friend_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: friend_public_id.clone(),
            display_name: Some("Hark".to_owned()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: None,
            created_at: 1,
            updated_at: 1,
        },
    )
    .unwrap();

    // An identity this node learned with a malformed DID is refused before the ledger.
    wattetheria_social::application::remote_identity_service::upsert_remote_identity(
        &*state.social_store,
        &wattetheria_social::domain::identities::RemoteIdentityProfile {
            public_id: "bogus.deadbeef".to_owned(),
            agent_did: "did:not-real".to_owned(),
            display_name: "Bogus".to_owned(),
            description: None,
            capabilities: Vec::new(),
            skills: Vec::new(),
            did_document_json: None,
            active: true,
            last_profile_fetched_at: Some(1),
            created_at: 1,
            updated_at: 1,
        },
    )
    .unwrap();
    let bogus = send(
        app.clone(),
        &token,
        json!({"to": "bogus.deadbeef", "amount": 5}),
    )
    .await;
    assert_eq!(bogus["result"]["_meta"]["httpStatus"], 400, "{bogus}");
    assert!(
        bogus.to_string().contains("not a valid agent did:key"),
        "{bogus}"
    );
    assert!(mock.transfers.lock().await.is_empty());

    // By DID: the label is unknown, the memo travels as memo and subject.
    let stranger = "did:key:z6Mkg5K92URgXhcuTfqt9jntq75JgPKgaQj36ougEQ3PrDXM";
    let by_did = send(
        app.clone(),
        &token,
        json!({"to": stranger, "amount": 30, "memo": "  for the map  "}),
    )
    .await;
    assert_eq!(by_did["result"]["isError"], false, "{by_did}");
    let content = &by_did["result"]["structuredContent"];
    assert_eq!(content["ok"], true);
    assert_eq!(content["from_did"], state.agent_did);
    let from_label = content["from_label"].as_str().unwrap().to_owned();
    assert!(!from_label.is_empty(), "{content}");
    assert_eq!(content["to_did"], stranger);
    assert_eq!(content["to_label"], Value::Null);
    assert_eq!(content["amount"], 30);
    assert_eq!(content["memo"], "for the map");
    assert_eq!(content["entry"]["credit_account"], stranger);
    assert_eq!(content["balance"], 70);
    {
        let transfers = mock.transfers.lock().await;
        assert_eq!(transfers.len(), 1);
        let payload = &transfers[0];
        assert_eq!(payload["from"], state.agent_did);
        assert_eq!(payload["to"], stranger);
        assert_eq!(payload["amount"], 30);
        assert_eq!(payload["memo"], "for the map");
        assert_eq!(payload["subject"], "for the map");
        assert_eq!(payload["from_label"], from_label);
        assert_eq!(payload["to_label"], Value::Null);
        let own_fingerprint =
            wattetheria_kernel::identity::fingerprint_from_did_key(&state.agent_did).unwrap();
        assert_eq!(
            payload["transfer_id"],
            format!(
                "{own_fingerprint}:{}",
                content["transfer_id"].as_str().unwrap()
            ),
            "the ledger key is the caller's id scoped to the paying agent"
        );
        assert_eq!(
            content["entry_id"],
            format!("transfer:{}", payload["transfer_id"].as_str().unwrap())
        );
    }

    // By public_id and by friend display name: both land on the friend's DID with its name.
    for to in [friend_public_id.as_str(), "Hark"] {
        let sent = send(app.clone(), &token, json!({"to": to, "amount": 10})).await;
        assert_eq!(sent["result"]["isError"], false, "{to}: {sent}");
        let content = &sent["result"]["structuredContent"];
        assert_eq!(content["to_did"], friend_did, "{to}");
        assert_eq!(content["to_label"], "Hark", "{to}");
        assert_eq!(content["memo"], Value::Null, "{to}");
    }
    assert_eq!(mock.transfers.lock().await.len(), 3);
    assert_eq!(
        send(app.clone(), &token, json!({"to": "Hark", "amount": 1})).await["result"]["structuredContent"]
            ["balance"],
        49
    );

    // Another agent already posted `order-42` on this ledger: ours is scoped, so no collision.
    mock.transfers.lock().await.push(json!({
        "transfer_id": "order-42",
        "from": "did:key:z6Mkg5K92URgXhcuTfqt9jntq75JgPKgaQj36ougEQ3PrDXM",
        "to": friend_did,
        "amount": 0,
        "memo": Value::Null,
    }));

    // A caller-chosen transfer_id is honoured, and replaying it (a retry after a lost reply)
    // returns the same entry without debiting again.
    let first = send(
        app.clone(),
        &token,
        json!({"to": "Hark", "amount": 7, "transfer_id": "order-42"}),
    )
    .await;
    assert_eq!(first["result"]["isError"], false, "{first}");
    assert_eq!(
        first["result"]["structuredContent"]["transfer_id"],
        "order-42"
    );
    assert_eq!(first["result"]["structuredContent"]["balance"], 42);
    let replay = send(
        app.clone(),
        &token,
        json!({"to": "Hark", "amount": 7, "transfer_id": "order-42"}),
    )
    .await;
    assert_eq!(replay["result"]["isError"], false, "{replay}");
    let replayed = &replay["result"]["structuredContent"];
    assert_eq!(replayed["transfer_id"], "order-42");
    assert_eq!(
        replayed["entry"]["entry_id"],
        first["result"]["structuredContent"]["entry_id"]
    );
    assert_eq!(replayed["entry"]["amount"], 7);
    assert_eq!(replayed["balance"], 42, "a replay must not debit again");
    assert_eq!(mock.transfers.lock().await.len(), 6);

    // The same transfer_id with a different amount is a reuse, refused as 409.
    let reused = send(
        app.clone(),
        &token,
        json!({"to": "Hark", "amount": 8, "transfer_id": "order-42"}),
    )
    .await;
    assert_eq!(reused["result"]["_meta"]["httpStatus"], 409, "{reused}");
    let text = reused["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["code"], "transfer_id_reused", "{payload}");
    assert_eq!(payload["transfer_id"], "order-42");
    assert_eq!(payload["retry_with_same_transfer_id"], false);
    assert_eq!(mock.transfers.lock().await.len(), 6);

    // Over the balance: the ledger's 402 comes back as insufficient_balance, nothing posted.
    let too_much = send(app.clone(), &token, json!({"to": "Hark", "amount": 1000})).await;
    assert_eq!(too_much["result"]["isError"], true, "{too_much}");
    assert_eq!(too_much["result"]["_meta"]["httpStatus"], 402, "{too_much}");
    let text = too_much["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["code"], "insufficient_balance", "{payload}");
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .contains("insufficient balance"),
        "{payload}"
    );
    assert!(
        payload["transfer_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert_eq!(payload["retry_with_same_transfer_id"], false);
    assert_eq!(mock.transfers.lock().await.len(), 6);
}

#[tokio::test]
async fn mcp_send_watt_pays_a_servicenet_agent_at_its_service_did() {
    let (ledger_url, mock) = spawn_mock_ledger().await;
    let (servicenet_addr, _servicenet) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        gateway_urls: vec![ledger_url],
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state.clone());

    let paid = send(
        app.clone(),
        &token,
        json!({"to": "Alpha@Wattetheria", "to_kind": "service_agent", "amount": 25, "memo": "coffee"}),
    )
    .await;
    assert_eq!(paid["result"]["isError"], false, "{paid}");
    let content = &paid["result"]["structuredContent"];
    assert_eq!(content["to_did"], mock_service_did("agent-alpha"));
    assert_eq!(content["to_label"], "Agent Alpha");
    assert_eq!(content["amount"], 25);
    let transfers = mock.transfers.lock().await;
    assert_eq!(transfers.len(), 1);
    assert_eq!(transfers[0]["to"], mock_service_did("agent-alpha"));
    assert_eq!(transfers[0]["to_label"], "Agent Alpha");
    drop(transfers);

    let unknown = send(
        app,
        &token,
        json!({"to": "nobody@wattetheria", "to_kind": "service_agent", "amount": 1}),
    )
    .await;
    assert_eq!(unknown["result"]["_meta"]["httpStatus"], 400, "{unknown}");
    assert!(unknown.to_string().contains("was not found"), "{unknown}");
}
