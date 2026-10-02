use super::*;

async fn call_mcp_tool(app: Router, token: &str, name: &str, arguments: Value) -> Value {
    mcp_request(
        app,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }),
    )
    .await
}

fn load_contribution_log(
    state: &ControlPlaneState,
) -> wattetheria_kernel::economy::ContributionEventLog {
    state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::CONTRIBUTION_EVENT_LOG)
        .unwrap()
}

fn watt_balance_of(
    state: &ControlPlaneState,
    event: &wattetheria_kernel::economy::ContributionEvent,
) -> i64 {
    let balances: wattetheria_kernel::economy::WalletBalanceState = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::WATT_BALANCE_STATE)
        .unwrap();
    balances
        .get(&event.controller_id, event.public_id.as_deref())
        .unwrap()
        .watt_balance
}

#[tokio::test]
async fn mcp_success_records_contribution_reward_event() {
    let (_dir, app, token, _policy, state) = build_test_app(100);

    let response = call_mcp_tool(
        app.clone(),
        &token,
        "update_agent_name",
        json!({"display_name": "Rewarded Name"}),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let log = load_contribution_log(&state);
    let event = log
        .events
        .values()
        .find(|event| event.action_type == "mcp.tool.success")
        .unwrap();
    assert_eq!(
        event.receipt["tool_name"].as_str(),
        Some("update_agent_name")
    );
    assert_eq!(watt_balance_of(&state, event), 1);
}

#[tokio::test]
async fn mcp_read_only_tools_record_no_contribution_reward() {
    let (_dir, app, token, _policy, state) = build_test_app(100);

    for tool in ["client_export", "list_nearby", "list_friend_requests"] {
        call_mcp_tool(app.clone(), &token, tool, json!({})).await;
    }

    assert!(load_contribution_log(&state).events.is_empty());
    assert!(crate::routes::mcp::is_read_only_mcp_tool(
        "list_agent_dm_messages"
    ));
    assert!(crate::routes::mcp::is_read_only_mcp_tool(
        "get_service_agent_task"
    ));
    assert!(!crate::routes::mcp::is_read_only_mcp_tool(
        "send_agent_dm_message"
    ));
    assert!(!crate::routes::mcp::is_read_only_mcp_tool(
        "send_service_agent_message"
    ));
}

#[tokio::test]
async fn prune_removes_read_only_mcp_rewards_and_reprojects_balance() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    call_mcp_tool(
        app.clone(),
        &token,
        "update_agent_name",
        json!({"display_name": "Kept Name"}),
    )
    .await;
    let mut log = load_contribution_log(&state);
    let kept = log.events.values().next().unwrap().clone();
    for (action_type, tool_name) in [
        ("mcp.tool.success", "list_nearby"),
        ("mcp.tool.success", "list_agent_dm_messages"),
        (
            "servicenet.agent.invoke.success",
            "list_service_agent_tasks",
        ),
    ] {
        let mut farmed = kept.clone();
        farmed.event_id = format!("reward:farmed:{tool_name}");
        farmed.action_type = action_type.to_owned();
        farmed.receipt = json!({"tool_name": tool_name, "result": {"content": []}});
        log.append(farmed);
    }
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::CONTRIBUTION_EVENT_LOG,
            &log,
        )
        .unwrap();
    crate::routes::reward_view::refresh_known_wallet_balances(&state)
        .await
        .unwrap();
    assert_eq!(watt_balance_of(&state, &kept), 4);

    let removed = crate::prune_read_only_mcp_contributions(&state)
        .await
        .unwrap();

    assert_eq!(removed, 3);
    let log = load_contribution_log(&state);
    assert_eq!(log.events.len(), 1);
    assert!(log.events.contains_key(&kept.event_id));
    assert_eq!(watt_balance_of(&state, &kept), 1);
    assert_eq!(
        crate::prune_read_only_mcp_contributions(&state)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn mcp_success_receipt_redacts_sensitive_arguments_and_results() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state.clone());

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_service_agent_message",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "message": "hello servicenet",
                    "auth_token": "servicenet-secret-token",
                    "auth_context_id": "00000000-0000-0000-0000-00000000abcd",
                    "input": {
                        "api_key": "input-api-secret",
                        "safe_value": "kept"
                    },
                    "settlement": {
                        "layer": "web3",
                        "rail": "x402",
                        "request": {
                            "settlement_receipt": alpha_x402_settlement_receipt(),
                            "client_secret": "settlement-client-secret",
                            "payment_account_ref": "payment-account-123"
                        }
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let log: wattetheria_kernel::economy::ContributionEventLog = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::CONTRIBUTION_EVENT_LOG)
        .unwrap();
    let event = log
        .events
        .values()
        .find(|event| event.action_type == "servicenet.agent.invoke.success")
        .unwrap();
    let receipt = &event.receipt;
    assert_eq!(
        receipt["arguments"]["auth_token"].as_str(),
        Some("[REDACTED]")
    );
    assert_eq!(
        receipt["arguments"]["auth_context_id"].as_str(),
        Some("[REDACTED]")
    );
    assert_eq!(
        receipt["arguments"]["input"]["api_key"].as_str(),
        Some("[REDACTED]")
    );
    assert_eq!(
        receipt["arguments"]["input"]["safe_value"].as_str(),
        Some("kept")
    );
    assert_eq!(
        receipt["arguments"]["settlement"]["request"]["client_secret"].as_str(),
        Some("[REDACTED]")
    );
    assert_eq!(
        receipt["result"]["structuredContent"]["settlement"]["request"]["client_secret"].as_str(),
        Some("[REDACTED]")
    );
    assert_eq!(
        receipt["result"]["structuredContent"]["settlement"]["request"]["payment_account_ref"]
            .as_str(),
        Some("payment-account-123")
    );
    let receipt_json = serde_json::to_string(receipt).unwrap();
    assert!(!receipt_json.contains("servicenet-secret-token"));
    assert!(!receipt_json.contains("00000000-0000-0000-0000-00000000abcd"));
    assert!(!receipt_json.contains("input-api-secret"));
    assert!(!receipt_json.contains("settlement-client-secret"));

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_tools_call_writes_product_diagnostics() {
    let (_dir, app, token, _policy, state) = build_test_app(100);

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "client_export",
                "arguments": {}
            }
        }),
    )
    .await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));

    let entries = crate::diagnostics::list_diagnostics(
        &state.data_dir,
        &crate::diagnostics::DiagnosticFilter {
            component: Some("wattetheria.mcp".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase == "tool.call.received"
                && entry.details["tool_name"].as_str() == Some("client_export"))
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase == "tool.call.succeeded"
                && entry.details["tool_name"].as_str() == Some("client_export"))
    );
}

#[tokio::test]
async fn mcp_array_payload_tools_return_object_structured_content() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    for (id, tool_name) in [
        (1, "list_friends"),
        (2, "list_agent_dm_threads"),
        (3, "list_agent_dm_messages"),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": tool_name,
                    "arguments": {}
                }
            }),
        )
        .await;

        assert_eq!(response["result"]["isError"].as_bool(), Some(false));
        let structured_content = &response["result"]["structuredContent"];
        assert!(structured_content.is_object(), "{tool_name}");
        assert!(structured_content["items"].is_array(), "{tool_name}");
        let text_payload: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert!(text_payload.is_object(), "{tool_name}");
        assert!(text_payload["items"].is_array(), "{tool_name}");
    }
}
