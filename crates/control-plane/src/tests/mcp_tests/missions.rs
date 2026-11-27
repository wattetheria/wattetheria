use super::*;

#[tokio::test]
async fn mcp_complete_mission_publishes_ordinary_lifecycle_notice_without_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge_handle);
    let agent_did = state.agent_did.clone();
    let public_id = bootstrap_broker_identity(app.clone(), &token, &agent_did).await;
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "MCP ordinary complete",
            "description": "MCP complete_mission stays on ordinary mission lifecycle.",
            "publisher": public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {
                "agent_watt": 0,
                "reputation": 0,
                "capacity": 0
            },
            "payload": {"objective": "ordinary-mcp-complete"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().expect("mission_id");
    let _claimed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({
            "mission_id": mission_id,
            "agent_did": agent_did,
        }),
    )
    .await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "complete_mission",
                "arguments": {
                    "mission_id": mission_id,
                    "agent_did": agent_did,
                    "result": {"ok": true, "summary": "done"}
                }
            }
        }),
    )
    .await;

    let completed = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(completed["status"].as_str(), Some("completed"));
    assert_eq!(
        completed["mission_lifecycle_notice"]["kind"].as_str(),
        Some("mission_completed")
    );
    assert!(completed.get("candidate_id").is_none());
    assert!(completed.get("swarm_candidate").is_none());

    let messages = bridge.messages.lock().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].content["kind"].as_str(),
        Some("mission_claim_approved")
    );
    assert_eq!(
        messages[1].content["kind"].as_str(),
        Some("mission_completed")
    );
}

#[tokio::test]
async fn mcp_publish_mission_uses_current_local_public_identity() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "publish_mission",
                "arguments": {
                    "title": "MCP local publisher",
                    "description": "Publisher should be injected by the local MCP server.",
                    "publisher": "wrong-manual-value",
                    "publisher_kind": "system",
                    "domain": "trade",
                    "payload": {"objective": "identity-default"}
                }
            }
        }),
    )
    .await;

    let mission = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(mission["publisher"].as_str(), Some(local_public_id));
    assert_eq!(mission["publisher_kind"].as_str(), Some("player"));
    let mission_id = mission["mission_id"].as_str().expect("mission id");
    assert_eq!(mission["task_id"].as_str(), Some(mission_id));
    assert_eq!(mission["task_type"].as_str(), Some("wattetheria.mission"));
    assert_eq!(mission["scope"].as_str(), Some("real_world"));
    assert_eq!(
        mission["mission_scope_hint"].as_str(),
        Some(format!("group:{mission_id}").as_str())
    );
    assert_eq!(
        mission["swarm_scope"],
        json!({"kind": "group", "id": mission_id})
    );
    assert_eq!(
        mission["task_contract"]["task_id"].as_str(),
        Some(mission_id)
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["swarm_scope"],
        json!({"kind": "group", "id": mission_id})
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["mission_scope_hint"].as_str(),
        mission["mission_scope_hint"].as_str()
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["scope"].as_str(),
        Some("real_world")
    );
    assert!(mission.get("reward").is_none());
    assert!(mission["task_contract"]["inputs"].get("reward").is_none());
    assert_public_geo_projection(mission);
    assert_public_geo_projection(&mission["task_contract"]["inputs"]);
}

#[tokio::test]
async fn mcp_publish_mission_with_a_watt_bounty_needs_a_gateway_ledger() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let mut offline = state.clone();
    offline.gateway_urls.clear();
    let response = mcp_request(
        crate::app(offline),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "publish_mission",
                "arguments": {
                    "title": "Bounty without a ledger",
                    "description": "This node has no gateway, so the bounty cannot be locked.",
                    "domain": "trade",
                    "payload": {"objective": "bounty"},
                    "reward": {"agent_watt": 40}
                }
            }
        }),
    )
    .await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert_eq!(response["result"]["_meta"]["httpStatus"], 503);
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "gateway_unavailable"
    );
    assert!(
        mission_gateway::mission_gateway(&state)
            .lock()
            .await
            .commands("publish")
            .is_empty(),
        "a mission the gateway never saw is not published"
    );

    // A reward with only agent_watt deserializes: reputation and capacity default to 0.
    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "publish_mission",
                "arguments": {
                    "title": "Zero bounty",
                    "description": "Reputation only.",
                    "domain": "trade",
                    "payload": {"objective": "free"},
                    "reward": {"agent_watt": 0, "reputation": 3}
                }
            }
        }),
    )
    .await;
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert_eq!(
        response["result"]["structuredContent"]["reward"],
        json!({"agent_watt": 0, "reputation": 3, "capacity": 0})
    );
}

#[tokio::test]
async fn mcp_publish_mission_limits_human_text_without_language_whitelists() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let rejected = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "publish_mission",
                "arguments": {
                    "title": "中".repeat(257),
                    "description": "描述",
                    "domain": "trade",
                    "payload": {"objective": "too-long-title"}
                }
            }
        }),
    )
    .await;
    assert_eq!(rejected["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        rejected["result"]["_meta"]["httpStatus"].as_u64(),
        Some(400)
    );
    assert!(
        rejected["result"]["structuredContent"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("title must be at most 256 characters"))
    );

    let accepted = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "publish_mission",
                "arguments": {
                    "title": "中文 English العربية 日本語 🙂",
                    "description": "Без language whitelist: हिन्दी 한국어",
                    "domain": "trade",
                    "payload": {"objective": "multilingual"}
                }
            }
        }),
    )
    .await;
    assert_eq!(accepted["result"]["isError"].as_bool(), Some(false));
}

#[tokio::test]
async fn mcp_publish_tools_keep_watt_bounties_and_real_asset_delegation_apart() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let delegation = json!({
        "enabled": true,
        "layer": "web3",
        "provider": "servicenet-agent",
        "provider_agent_id": "escrow-agent-123",
        "provider_agent_name": "Some Escrow Agent",
        "asset": "USDC",
        "amount": "25.00",
        "provider_receipt": {"status": "funded"}
    });
    let cases = [
        (
            "publish_delegated_mission",
            json!({
                "title": "Delegated with Watt",
                "description": "A real-asset mission must not also carry a Watt reward.",
                "domain": "trade",
                "payload": {"objective": "mixed"},
                "settlement_delegation": delegation,
                "reward": {"agent_watt": 10}
            }),
            "does not accept a Watt reward",
        ),
        (
            "publish_delegated_mission",
            json!({
                "title": "Delegated without delegation",
                "description": "Must not silently become a plain mission.",
                "domain": "trade",
                "payload": {"objective": "missing"}
            }),
            "requires settlement_delegation with enabled: true",
        ),
        (
            "publish_delegated_mission",
            json!({
                "title": "Delegated but disabled",
                "description": "enabled: false is not a delegation.",
                "domain": "trade",
                "payload": {"objective": "disabled"},
                "settlement_delegation": {"enabled": false}
            }),
            "requires settlement_delegation with enabled: true",
        ),
        (
            "publish_mission",
            json!({
                "title": "Watt tool with delegation",
                "description": "The Watt tool must not take real-asset settlement.",
                "domain": "trade",
                "payload": {"objective": "wrong-tool"},
                "settlement_delegation": delegation
            }),
            "does not accept settlement_delegation",
        ),
    ];
    for (id, (tool, arguments, expected)) in cases.into_iter().enumerate() {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": tool, "arguments": arguments}
            }),
        )
        .await;
        assert_eq!(response["result"]["isError"], true, "{tool}: {response}");
        assert_eq!(response["result"]["_meta"]["httpStatus"], 400, "{response}");
        let error = response["result"]["structuredContent"]["error"]
            .as_str()
            .unwrap_or_default();
        assert!(error.contains(expected), "{tool}: {error}");
    }
    assert!(
        mission_gateway::mission_gateway(&state)
            .lock()
            .await
            .commands("publish")
            .is_empty(),
        "no rejected call may publish a mission"
    );
}

#[tokio::test]
async fn mcp_publish_delegated_mission_surfaces_servicenet_settlement_details() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "publish_delegated_mission",
                "arguments": {
                    "title": "Funded ServiceNet escrow task",
                    "description": "Publish a real reward mission with third-party settlement metadata.",
                    "domain": "trade",
                    "payload": {"objective": "escrow-backed"},
                    "settlement_delegation": {
                        "enabled": true,
                        "layer": "web3",
                        "provider": "servicenet-agent",
                        "provider_agent_id": "escrow-agent-123",
                        "provider_agent_name": "Some Escrow Agent",
                        "network": "base-sepolia",
                        "asset": "USDC",
                        "amount": "10000000",
                        "funding_proof": {
                            "type": "evm_tx",
                            "tx_hash": "0x3333333333333333333333333333333333333333333333333333333333333333",
                            "chain_id": 84532,
                            "to": "0x1111111111111111111111111111111111111111"
                        },
                        "provider_receipt": {
                            "receipt_id": "receipt-servicenet-1",
                            "status": "funded",
                            "task_id": "provider-task-1",
                            "raw": {"provider_rule": "external"}
                        },
                        "terms": {
                            "summary": "Provider-defined settlement rules.",
                            "url": "https://escrow.example/terms",
                            "raw": {"max_revision_count": 1}
                        }
                    }
                }
            }
        }),
    )
    .await;

    let mission = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let mission_id = mission["mission_id"].as_str().expect("mission id");
    let delegation = &mission["settlement_delegation"];
    assert_eq!(delegation["provider"].as_str(), Some("servicenet-agent"));
    assert_eq!(delegation["layer"].as_str(), Some("web3"));
    assert_eq!(
        delegation["provider_agent_id"].as_str(),
        Some("escrow-agent-123")
    );
    assert_eq!(
        delegation["provider_agent_name"].as_str(),
        Some("Some Escrow Agent")
    );
    assert_eq!(delegation["network"].as_str(), Some("base-sepolia"));
    assert_eq!(delegation["status"].as_str(), Some("funded"));
    assert_eq!(delegation["asset"].as_str(), Some("USDC"));
    assert_eq!(
        delegation["provider_receipt"]["receipt_id"].as_str(),
        Some("receipt-servicenet-1")
    );
    assert_eq!(
        mission["payload"]["settlement_delegation"],
        mission["settlement_delegation"]
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["settlement_delegation"],
        mission["settlement_delegation"]
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["mission_id"].as_str(),
        Some(mission_id)
    );
    assert!(mission.get("reward").is_none());
    assert!(mission["task_contract"]["inputs"].get("reward").is_none());
}

#[tokio::test]
async fn mcp_list_missions_reads_configured_gateway_tasks() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    {
        let gateway = mission_gateway::mission_gateway(&state);
        let mut gateway = gateway.lock().await;
        for record in gateway_missions_fixture(&state).await {
            gateway.seed(record);
        }
    }

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_missions",
                "arguments": {
                    "limit": 1,
                    "offset": 1,
                    "status": "published"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["source"].as_str(),
        Some("wattetheria-gateway.api_missions")
    );
    assert_eq!(content["scope"].as_str(), Some("network"));
    assert_eq!(
        content["pagination"].as_str(),
        Some("gateway_limit_client_offset")
    );
    assert_eq!(content["limit"].as_u64(), Some(1));
    assert_eq!(content["offset"].as_u64(), Some(1));
    assert_eq!(content["known_count"].as_u64(), Some(2));
    assert_eq!(content["has_more"].as_bool(), Some(false));
    let missions = content["missions"].as_array().unwrap();
    assert_eq!(missions.len(), 1);
    // Newest first: the second open mission published is the first one listed.
    assert_eq!(
        missions[0]["mission_id"].as_str(),
        Some("mission-gateway-1")
    );
    assert_eq!(missions[0]["task_id"].as_str(), Some("mission-gateway-1"));
    assert_eq!(missions[0]["status"].as_str(), Some("published"));
    assert_eq!(
        missions[0]["publisher_agent_did"].as_str(),
        Some("did:key:node-alpha")
    );
    assert_gateway_claim_route(&missions[0], "mission-gateway-1", "node-alpha");

    // The first page knows a second one follows.
    let first = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_missions",
                "arguments": {"limit": 1, "offset": 0, "status": "published"}
            }
        }),
    )
    .await;
    let first = &first["result"]["structuredContent"];
    assert_eq!(first["missions"].as_array().unwrap().len(), 1, "{first}");
    assert_eq!(first["has_more"].as_bool(), Some(true), "{first}");
    assert_eq!(first["next_offset"].as_u64(), Some(1), "{first}");
}

#[tokio::test]
async fn mcp_list_missions_summarizes_a_delegated_settlement() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    {
        let gateway = mission_gateway::mission_gateway(&state);
        let mut gateway = gateway.lock().await;
        for record in gateway_missions_fixture(&state).await {
            gateway.seed(record);
        }
    }
    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "list_missions", "arguments": {"limit": 1, "status": "published"}}
        }),
    )
    .await;
    let missions = response["result"]["structuredContent"]["missions"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(missions.len(), 1);
    assert_eq!(
        missions[0]["mission_id"].as_str(),
        Some("mission-gateway-2")
    );
    assert_gateway_settlement_summary(&missions[0]);
    assert_gateway_claim_route(&missions[0], "mission-gateway-2", "node-beta");
}

/// Three missions other nodes published, oldest first: one settled, then two open (the
/// newer with a delegated settlement).
async fn gateway_missions_fixture(state: &ControlPlaneState) -> Vec<Value> {
    let contract = |mission_id: &'static str, node: &'static str, delegation: Option<Value>| async move {
        let mut contract = state
            .swarm_bridge
            .sample_task_contract(mission_id)
            .await
            .unwrap();
        contract.task_type = "wattetheria.mission".to_owned();
        contract.inputs = json!({
            "mission_id": mission_id,
            "publisher_wattswarm_node_id": node,
            "mission_feed_key": "wattetheria.missions",
            "mission_scope_hint": format!("group:{mission_id}"),
            "swarm_scope": {"kind": "group", "id": mission_id},
            "domain": "trade",
        });
        if let Some(delegation) = delegation {
            contract.inputs["settlement_delegation"] = delegation;
        }
        contract
    };
    let delegated = contract(
        "mission-gateway-2",
        "node-beta",
        Some(gateway_servicenet_settlement_delegation()),
    )
    .await;
    let plain = contract("mission-gateway-1", "node-alpha", None).await;
    vec![
        mission_gateway::remote_record(
            "mission-gateway-settled",
            "Settled Gateway Mission",
            "did:key:node-gamma",
            "settled",
            None,
        ),
        mission_gateway::remote_record(
            "mission-gateway-1",
            "Gateway Mission One",
            "did:key:node-alpha",
            "published",
            Some(&plain),
        ),
        mission_gateway::remote_record(
            "mission-gateway-2",
            "Gateway Mission Two",
            "did:key:node-beta",
            "published",
            Some(&delegated),
        ),
    ]
}

fn gateway_servicenet_settlement_delegation() -> Value {
    json!({
        "enabled": true,
        "layer": "web3",
        "provider": "servicenet-agent",
        "provider_agent_id": "escrow-agent-123",
        "provider_agent_name": "Some Escrow Agent",
        "network": "base-sepolia",
        "status": "funded",
        "asset": "USDC",
        "amount": "2500000",
        "funding_proof": {
            "type": "evm_tx",
            "tx_hash": "0x3333333333333333333333333333333333333333333333333333333333333333",
            "chain_id": 84532,
            "to": "0x1111111111111111111111111111111111111111"
        },
        "provider_receipt": {
            "receipt_id": "receipt-gateway-2",
            "status": "funded",
            "task_id": "provider-task-2"
        },
        "terms": {
            "summary": "Provider-defined settlement rules.",
            "url": "https://escrow.example/terms"
        }
    })
}

fn assert_gateway_settlement_summary(mission: &Value) {
    assert_eq!(mission["reward_type"].as_str(), Some("delegated"));
    assert_eq!(mission["has_settlement_delegation"].as_bool(), Some(true));
    assert_eq!(mission["settlement_layer"].as_str(), Some("web3"));
    assert_eq!(
        mission["settlement_provider"].as_str(),
        Some("servicenet-agent")
    );
    assert_eq!(
        mission["settlement_provider_agent_id"].as_str(),
        Some("escrow-agent-123")
    );
    assert_eq!(
        mission["settlement_provider_agent_name"].as_str(),
        Some("Some Escrow Agent")
    );
    assert_eq!(mission["settlement_network"].as_str(), Some("base-sepolia"));
    assert_eq!(mission["settlement_chain_id"].as_u64(), Some(84532));
    assert_eq!(mission["settlement_status"].as_str(), Some("funded"));
    assert_eq!(
        mission["settlement_receipt_id"].as_str(),
        Some("receipt-gateway-2")
    );
    assert_eq!(mission["settlement_asset"].as_str(), Some("USDC"));
    assert_eq!(mission["settlement_amount"].as_str(), Some("2500000"));
    assert_eq!(
        mission["settlement_funding_tx"].as_str(),
        Some("0x3333333333333333333333333333333333333333333333333333333333333333")
    );
    assert_eq!(
        mission["settlement_terms_url"].as_str(),
        Some("https://escrow.example/terms")
    );
    assert_eq!(
        mission["settlement_delegation"],
        mission["task_contract"]["inputs"]["settlement_delegation"]
    );
}

#[tokio::test]
async fn mcp_claim_mission_reports_duplicate_network_claim() {
    let (dir, app, token, _policy, state) = build_test_app(100);
    let mission_id = "mission-mcp-duplicate-claim";
    let agent_did = state.agent_did.clone();
    seed_mcp_gateway_remote_mission(&state, mission_id).await;
    append_bad_audit_row(dir.path());

    let first = mcp_claim_mission(app.clone(), &token, mission_id, &agent_did).await;
    assert_eq!(first["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        first["result"]["structuredContent"]["status"].as_str(),
        Some("network_claim_submitted")
    );
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let saved_claim = registry
        .records()
        .into_iter()
        .find(|claim| claim.mission_id == mission_id)
        .expect("network claim saved");
    assert_eq!(
        saved_claim.metadata.title.as_deref(),
        Some("Remote mission")
    );
    assert_eq!(saved_claim.metadata.domain.as_deref(), Some("trade"));
    assert_eq!(
        saved_claim.metadata.publisher_id.as_deref(),
        Some("publisher-public")
    );
    assert_eq!(saved_claim.metadata.reward_watt, Some(0));

    let second = mcp_claim_mission(app, &token, mission_id, &agent_did).await;
    assert_eq!(second["result"]["isError"].as_bool(), Some(true));
    let content = &second["result"]["structuredContent"];
    assert_eq!(content["code"].as_str(), Some("mission_already_claimed"));
    assert_eq!(content["claim_status"].as_str(), Some("already_claimed"));
    assert_eq!(content["mission_id"].as_str(), Some(mission_id));
    assert_eq!(content["task_id"].as_str(), Some(mission_id));
    assert_eq!(content["agent_did"].as_str(), Some(agent_did.as_str()));
    assert_eq!(second["result"]["_meta"]["httpStatus"].as_u64(), Some(409));
}

#[tokio::test]
async fn mcp_claim_mission_reports_gateway_claimed_status() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let mission_id = "mission-mcp-gateway-claimed";
    let agent_did = state.agent_did.clone();
    seed_mcp_gateway_remote_mission_with_status(&state, mission_id, "claimed").await;
    mission_gateway::mission_gateway(&state).lock().await.set(
        mission_id,
        "claimer",
        json!("did:key:another-claimer"),
    );

    let response = mcp_claim_mission(app, &token, mission_id, &agent_did).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["code"].as_str(), Some("already_claimed"));
    assert_eq!(
        response["result"]["_meta"]["httpStatus"].as_u64(),
        Some(409)
    );
}

async fn mcp_claim_mission(app: Router, token: &str, mission_id: &str, agent_did: &str) -> Value {
    mcp_request(
        app,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "claim_mission",
                "arguments": {
                    "mission_id": mission_id,
                    "agent_did": agent_did
                }
            }
        }),
    )
    .await
}

fn append_bad_audit_row(data_dir: &std::path::Path) {
    use std::io::Write as _;

    let path = data_dir.join("audit/control_plane.jsonl");
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(b"{not-valid-audit-json}\n").unwrap();
}

async fn seed_mcp_gateway_remote_mission(state: &ControlPlaneState, mission_id: &str) {
    seed_mcp_gateway_remote_mission_with_status(state, mission_id, "published").await;
}

async fn seed_mcp_gateway_remote_mission_with_status(
    state: &ControlPlaneState,
    mission_id: &str,
    status: &str,
) {
    let mut contract = state
        .swarm_bridge
        .sample_task_contract(mission_id)
        .await
        .unwrap();
    contract.task_type = "wattetheria.mission".to_string();
    contract.inputs = json!({
        "kind": "wattetheria_mission",
        "mission_id": mission_id,
        "publisher": "publisher-public",
        "publisher_agent_did": "did:agent:publisher",
        "publisher_display_name": "Remote Publisher",
        "publisher_wattswarm_node_id": "publisher-node",
        "domain": "trade",
        "swarm_scope": {"kind": "group", "id": mission_id},
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": format!("group:{mission_id}"),
        "reward": {"agent_watt": 0},
        "payload": {"work": "deliver"}
    });
    mission_gateway::mission_gateway(state)
        .lock()
        .await
        .seed(mission_gateway::remote_record(
            mission_id,
            "Remote mission",
            "did:agent:publisher",
            status,
            Some(&contract),
        ));
}

fn assert_gateway_claim_route(mission: &Value, mission_id: &str, node_id: &str) {
    let scope_hint = format!("group:{mission_id}");
    assert_eq!(
        mission["publisher_wattswarm_node_id"].as_str(),
        Some(node_id)
    );
    assert_eq!(
        mission["mission_feed_key"].as_str(),
        Some("wattetheria.missions")
    );
    assert_eq!(
        mission["mission_scope_hint"].as_str(),
        Some(scope_hint.as_str())
    );
    assert_eq!(
        mission["swarm_scope"],
        json!({"kind": "group", "id": mission_id})
    );
    assert_eq!(mission["claim_route"]["task_id"].as_str(), Some(mission_id));
    assert_eq!(
        mission["claim_route"]["mission_id"].as_str(),
        Some(mission_id)
    );
    assert_eq!(
        mission["claim_route"]["publisher_wattswarm_node_id"].as_str(),
        Some(node_id)
    );
    assert_eq!(
        mission["claim_route"]["mission_scope_hint"].as_str(),
        Some(scope_hint.as_str())
    );
    assert_eq!(
        mission["claim_route"]["task_contract_available"].as_bool(),
        Some(true)
    );
    assert_eq!(mission["claim_route"]["claim_ready"].as_bool(), Some(true));
}
