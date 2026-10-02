use super::*;

fn create_collective_hive_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "create_hive",
            "arguments": {
                "feed_key": "mcp-collective-feed",
                "scope_hint": "group:mcp-collective-feed",
                "display_name": "MCP Collective Hive",
                "projection_kind": "chat_room",
                "include_public_geo": false
            }
        }
    })
}

fn create_collective_hive_response(response: &Value) -> &str {
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    response["result"]["structuredContent"]["hive"]["topic_id"]
        .as_str()
        .expect("hive id")
}

async fn create_collective_hive(app: axum::Router, token: &str) -> String {
    let response = mcp_request(app, token, create_collective_hive_request()).await;
    create_collective_hive_response(&response).to_owned()
}

async fn create_private_collective_hive(app: axum::Router, token: &str) -> String {
    let response = mcp_request(
        app,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_private_hive",
                "arguments": {
                    "feed_key": "wattetheria.private.collective",
                    "scope_hint": "group:dm-private-collective",
                    "display_name": "Private Collective Hive"
                }
            }
        }),
    )
    .await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    response["result"]["structuredContent"]["hive"]["topic_id"]
        .as_str()
        .expect("private hive id")
        .to_owned()
}

fn collective_mission_request(hive_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "publish_collective_mission",
            "arguments": {
                "mode": "committee",
                "hive_id": hive_id,
                "title": "Collective MCP mission",
                "description": "Run several agents through Wattswarm.",
                "publisher": "wrong-manual-value",
                "publisher_kind": "system",
                "domain": "trade",
                "scope": "in_world",
                "required_faction": "freeport",
                "required_role": "broker",
                "payload": {"objective": "collective-intel"},
                "min_participants": 2,
                "threshold_percent": 60,
                "round_timeout_ms": 30000,
                "max_rounds": 3,
                "aggregation": {"mode": "MAJORITY"},
                "kickoff": true
            }
        }
    })
}

fn collective_stigmergy_mission_request(hive_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "publish_collective_mission",
            "arguments": {
                "mode": "stigmergy",
                "hive_id": hive_id,
                "title": "Open collective MCP mission",
                "description": "Let subscribed agents decide whether to contribute.",
                "domain": "trade",
                "payload": {"objective": "open-collective-intel"},
                "min_participants": 2,
                "join_window_ms": 60000,
                "threshold_percent": 60,
                "round_timeout_ms": 30000,
                "max_rounds": 3,
                "fallback_decision": "abstain",
                "aggregation": {"mode": "MAJORITY"},
                "kickoff": true
            }
        }
    })
}

fn start_collective_mission_request(run_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "start_collective_mission",
            "arguments": {
                "run_id": run_id
            }
        }
    })
}

fn assert_collective_publish_result<'a>(
    response: &'a Value,
    local_public_id: &str,
    hive_id: &str,
) -> (&'a str, &'a str) {
    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        content["mission"]["publisher"].as_str(),
        Some(local_public_id)
    );
    assert_eq!(
        content["mission"]["publisher_kind"].as_str(),
        Some("player")
    );
    assert_eq!(
        content["mission"]["task_type"].as_str(),
        Some("wattetheria.collective_mission")
    );
    assert_eq!(
        content["mission"]["kind"].as_str(),
        Some("collective_mission")
    );
    assert_eq!(content["mission"]["lifecycle"].as_str(), Some("collective"));
    assert_eq!(content["mission"]["hive_id"].as_str(), Some(hive_id));
    assert_eq!(content["mission"]["scope"].as_str(), Some("in_world"));
    assert_public_geo_projection(&content["mission"]);
    assert!(content["mission"].get("task_contract").is_none());
    assert_eq!(
        content["mission"]["payload"]["task_type"].as_str(),
        Some("wattetheria.collective_mission")
    );
    assert_eq!(
        content["mission"]["payload"]["objective"].as_str(),
        Some("collective-intel")
    );
    let mission_id = content["mission_id"].as_str().expect("mission id");
    let run_id = content["run_id"].as_str().expect("run id");
    assert_eq!(content["kicked_off"].as_bool(), Some(false));
    assert_eq!(content["phase"].as_str(), Some("joining"));
    assert_eq!(content["wattswarm_run"]["submitted"].as_bool(), Some(false));
    assert_eq!(
        content["wattswarm_run"]["kicked_off"].as_bool(),
        Some(false)
    );
    assert_eq!(
        content["run_spec"]["task_type"].as_str(),
        Some("wattetheria.collective_mission")
    );
    assert_eq!(
        content["run_spec"]["shared_inputs"]["mission_id"].as_str(),
        Some(mission_id)
    );
    assert_eq!(
        content["run_spec"]["shared_inputs"]["hive_id"].as_str(),
        Some(hive_id)
    );
    assert_eq!(
        content["run_spec"]["shared_inputs"]["mission"]["scope"].as_str(),
        Some("in_world")
    );
    assert_eq!(
        content["run_spec"]["round_policy"]["min_participants"].as_u64(),
        Some(2)
    );
    assert_eq!(
        content["run_spec"]["round_policy"]["threshold_percent"].as_u64(),
        Some(60)
    );
    assert_eq!(
        content["run_spec"]["join_policy"]["join_window_ms"].as_u64(),
        Some(1_800_000)
    );
    assert_public_geo_projection(&content["run_spec"]["shared_inputs"]["mission"]);
    assert_eq!(
        content["run_spec"]["agents"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        content["hive_message"]["type"].as_str(),
        Some("collective_mission")
    );
    assert!(
        content["hive_message"].get("contribution").is_none(),
        "collective Hive messages must not use legacy contribution contact material"
    );
    assert!(
        content["hive_message"]["contact_material"]["material_json"]
            .as_str()
            .is_some_and(|value| value.contains("private_message")),
        "public collective Hive messages must carry coordinator contact material for join-time DM setup"
    );
    assert!(
        content["hive_message"]["coordinator"]["agent_did"]
            .as_str()
            .is_some_and(|agent_did| !agent_did.trim().is_empty())
    );
    assert_eq!(
        content["hive_message"]["mission_id"].as_str(),
        Some(mission_id)
    );
    assert_eq!(content["hive_message"]["run_id"].as_str(), Some(run_id));
    assert_eq!(content["hive_message"]["kickoff"].as_bool(), Some(false));
    (mission_id, run_id)
}

#[tokio::test]
async fn mcp_publish_collective_mission_creates_joining_link_without_submitting_run() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();
    let hive_id = create_collective_hive(app.clone(), &token).await;
    let mission_count_before = state.mission_board.lock().await.list(None).len();

    let response = mcp_request(app.clone(), &token, collective_mission_request(&hive_id)).await;
    let (mission_id, run_id) =
        assert_collective_publish_result(&response, local_public_id, &hive_id);
    let mission_count_after = state.mission_board.lock().await.list(None).len();
    assert_eq!(mission_count_after, mission_count_before);

    let persisted: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted["runs"][mission_id]["run_id"].as_str(),
        Some(run_id)
    );
    assert_eq!(
        persisted["runs"][mission_id]["hive_id"].as_str(),
        Some(hive_id.as_str())
    );

    assert_eq!(
        persisted["runs"][mission_id]["wattswarm_run"]["submitted"].as_bool(),
        Some(false)
    );
    assert!(
        persisted["runs"][mission_id]["task_prompt"]
            .as_str()
            .is_some_and(|prompt| prompt.contains("Apply your own available skills"))
    );
    assert_eq!(
        persisted["runs"][mission_id]["participants"]
            .as_object()
            .map(serde_json::Map::len),
        Some(0)
    );
}

#[tokio::test]
async fn collective_participation_dm_records_joined_participant_in_run_link() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();
    let hive_id = create_collective_hive(app.clone(), &token).await;

    let response = mcp_request(app, &token, collective_mission_request(&hive_id)).await;
    let (mission_id, run_id) =
        assert_collective_publish_result(&response, local_public_id, &hive_id);
    let view = SwarmPeerDmMessageView {
        thread_id: "thread-alpha".to_owned(),
        message_id: "dm-alpha".to_owned(),
        remote_node_id: "node-alpha".to_owned(),
        message_kind: "agent".to_owned(),
        direction: "inbound".to_owned(),
        delivery_state: "delivered".to_owned(),
        a2a_protocol: "wattetheria.agent.v1".to_owned(),
        agent_envelope: None,
        content: json!({
            "type": "collective_participation",
            "version": 1,
            "status": "join",
            "mission_id": mission_id,
            "run_id": run_id,
            "event_id": "event-alpha",
            "decision_id": "decision-alpha",
            "participant_agent_did": "did:key:alpha",
            "participant_node_id": "node-alpha",
            "payload": {
                "public_id": "agent-alpha"
            }
        }),
        encrypted_body: None,
        content_encoding: None,
        created_at: 1,
        acknowledged_at: None,
    };

    let recorded =
        crate::routes::mcp::collective::record_collective_participation_from_dm(&state, &view)
            .unwrap()
            .expect("collective participation record");
    assert_eq!(recorded["recorded"].as_bool(), Some(true));
    assert_eq!(recorded["inserted"].as_bool(), Some(true));
    assert_eq!(recorded["joined_count"].as_u64(), Some(1));
    let duplicate =
        crate::routes::mcp::collective::record_collective_participation_from_dm(&state, &view)
            .unwrap()
            .expect("duplicate collective participation record");
    assert_eq!(duplicate["recorded"].as_bool(), Some(true));
    assert_eq!(duplicate["inserted"].as_bool(), Some(false));
    assert_eq!(duplicate["joined_count"].as_u64(), Some(1));

    let persisted: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    let participant = &persisted["runs"][mission_id]["participants"]["public:agent-alpha"];
    assert_eq!(participant["agent_id"].as_str(), Some("agent-alpha"));
    assert_eq!(participant["executor"].as_str(), Some("remote:node-alpha"));
    assert!(
        participant["prompt"]
            .as_str()
            .is_some_and(|prompt| prompt.contains("Apply your own available skills"))
    );
}

#[tokio::test]
async fn mcp_start_collective_mission_submits_joined_participants_as_committee_agents() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();
    let hive_id = create_collective_hive(app.clone(), &token).await;

    let response = mcp_request(app.clone(), &token, collective_mission_request(&hive_id)).await;
    let (mission_id, run_id) =
        assert_collective_publish_result(&response, local_public_id, &hive_id);

    let mut persisted: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    persisted["runs"][mission_id]["join_deadline_ms"] = json!(0);
    persisted["runs"][mission_id]["participants"] = json!({
        "public:agent-alpha": {
            "agent_id": "agent-alpha",
            "executor": "remote:node-alpha",
            "prompt": "Use alpha expertise for this collective mission.",
            "participant_agent_did": "did:key:alpha",
            "participant_node_id": "node-alpha",
            "public_id": "agent-alpha",
            "joined_at": "2026-06-24T00:00:00Z",
            "payload": {}
        },
        "public:agent-beta": {
            "agent_id": "agent-beta",
            "executor": "remote:node-beta",
            "prompt": persisted["runs"][mission_id]["task_prompt"].clone(),
            "participant_agent_did": "did:key:beta",
            "participant_node_id": "node-beta",
            "public_id": "agent-beta",
            "joined_at": "2026-06-24T00:00:00Z",
            "payload": {}
        }
    });
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS,
            &persisted,
        )
        .unwrap();

    let response = mcp_request(app, &token, start_collective_mission_request(run_id)).await;
    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(content["mission_id"].as_str(), Some(mission_id));
    assert_eq!(content["run_id"].as_str(), Some(run_id));
    assert_eq!(content["kicked_off"].as_bool(), Some(true));
    assert_eq!(content["wattswarm_run"]["kicked_off"].as_bool(), Some(true));
    assert!(content["run_spec"].get("round_policy").is_none());
    assert_eq!(
        content["run_spec"]["collective_policy"]["min_participants"].as_u64(),
        Some(2)
    );
    let agents = content["run_spec"]["agents"].as_array().expect("agents");
    assert_eq!(agents.len(), 2);
    assert!(
        agents
            .iter()
            .any(|agent| agent["agent_id"].as_str() == Some("agent-alpha")
                && agent["prompt"].as_str()
                    == Some("Use alpha expertise for this collective mission."))
    );
    assert!(agents.iter().any(|agent| {
        agent["agent_id"].as_str() == Some("agent-beta")
            && agent["prompt"]
                .as_str()
                .is_some_and(|prompt| prompt.contains("Apply your own available skills"))
    }));
    assert_eq!(
        content["link"]["participants"]
            .as_object()
            .map(serde_json::Map::len),
        Some(2)
    );
}

#[tokio::test]
async fn reliability_maintenance_starts_due_collective_mission_with_joined_participants() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();
    let hive_id = create_collective_hive(app.clone(), &token).await;

    let response = mcp_request(app, &token, collective_mission_request(&hive_id)).await;
    let (mission_id, run_id) =
        assert_collective_publish_result(&response, local_public_id, &hive_id);

    persist_due_collective_participants(&state, mission_id);

    let processed = run_reliability_maintenance_tick_once(&state, 10)
        .await
        .expect("run reliability maintenance");
    assert_eq!(processed, 2);

    let updated: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    assert_eq!(
        updated["runs"][mission_id]["kicked_off"].as_bool(),
        Some(true)
    );
    assert_eq!(
        updated["runs"][mission_id]["wattswarm_run"]["kicked_off"].as_bool(),
        Some(true)
    );
    assert_eq!(
        updated["runs"][mission_id]["run_spec"]["agents"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(updated["runs"][mission_id]["run_id"].as_str(), Some(run_id));
    assert_eq!(
        updated["runs"][mission_id]["finalized_hive_message"]["type"].as_str(),
        Some("collective_mission_finalized")
    );
    assert_eq!(
        updated["runs"][mission_id]["finalized_hive_message"]["final"]["answer"].as_str(),
        Some("mock collective result")
    );
    assert_eq!(
        updated["runs"][mission_id]["finalized_hive_message"]["aggregation"]["threshold_percent"]
            .as_u64(),
        Some(60)
    );
    assert_eq!(
        updated["runs"][mission_id]["finalized_hive_message"]["participation"]["joined_count"]
            .as_u64(),
        Some(2)
    );
    assert!(
        updated["runs"][mission_id]["finalized_hive_message"]
            .get("result")
            .is_none(),
        "finalized Hive card must not publish raw run result"
    );

    let messages = state
        .swarm_bridge
        .list_topic_messages(
            None,
            "mcp-collective-feed",
            "group:mcp-collective-feed",
            10,
            None,
            None,
        )
        .await
        .expect("collective Hive messages");
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages[2].content["type"].as_str(),
        Some("collective_mission_finalized")
    );

    let processed_again = run_reliability_maintenance_tick_once(&state, 10)
        .await
        .expect("run reliability maintenance again");
    assert_eq!(processed_again, 0);
    let messages_after_dedupe = state
        .swarm_bridge
        .list_topic_messages(
            None,
            "mcp-collective-feed",
            "group:mcp-collective-feed",
            10,
            None,
            None,
        )
        .await
        .expect("collective Hive messages after dedupe");
    assert_eq!(messages_after_dedupe.len(), 3);
}

fn persist_due_collective_participants(state: &ControlPlaneState, mission_id: &str) {
    let mut persisted: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    persisted["runs"][mission_id]["join_deadline_ms"] = json!(0);
    persisted["runs"][mission_id]["participants"] = json!({
        "public:agent-alpha": {
            "agent_id": "agent-alpha",
            "executor": "remote:node-alpha",
            "prompt": "Use alpha expertise for this collective mission.",
            "participant_agent_did": "did:key:alpha",
            "participant_node_id": "node-alpha",
            "public_id": "agent-alpha",
            "joined_at": "2026-06-24T00:00:00Z",
            "payload": {}
        },
        "public:agent-beta": {
            "agent_id": "agent-beta",
            "executor": "remote:node-beta",
            "prompt": persisted["runs"][mission_id]["task_prompt"].clone(),
            "participant_agent_did": "did:key:beta",
            "participant_node_id": "node-beta",
            "public_id": "agent-beta",
            "joined_at": "2026-06-24T00:00:00Z",
            "payload": {}
        }
    });
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS,
            &persisted,
        )
        .unwrap();
}

#[tokio::test]
async fn mcp_publish_collective_mission_defaults_to_joining_without_kickoff() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let hive_id = create_collective_hive(app.clone(), &token).await;
    let mut request = collective_mission_request(&hive_id);
    request["params"]["arguments"]
        .as_object_mut()
        .expect("collective arguments")
        .remove("kickoff");

    let response = mcp_request(app, &token, request).await;
    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let mission_id = content["mission_id"].as_str().expect("mission id");
    assert_eq!(content["kicked_off"].as_bool(), Some(false));
    assert_eq!(content["phase"].as_str(), Some("joining"));
    assert_eq!(
        content["wattswarm_run"]["kicked_off"].as_bool(),
        Some(false)
    );
    assert_eq!(content["hive_message"]["kickoff"].as_bool(), Some(false));

    let persisted: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted["runs"][mission_id]["kicked_off"].as_bool(),
        Some(false)
    );
}

#[tokio::test]
async fn mcp_publish_collective_mission_omits_contact_material_for_private_hive() {
    let (_dir, app, token, _policy, _state) = build_test_app(101);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();
    let hive_id = create_private_collective_hive(app.clone(), &token).await;

    let response = mcp_request(app, &token, collective_mission_request(&hive_id)).await;
    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        content["mission"]["publisher"].as_str(),
        Some(local_public_id)
    );
    assert!(
        content["hive_message"].get("contribution").is_none(),
        "private collective Hive messages must not use legacy contribution contact material"
    );
    assert!(
        content["hive_message"].get("contact_material").is_none(),
        "private collective Hive messages must not include coordinator contact material"
    );
}

#[tokio::test]
async fn mcp_get_collective_mission_result_allows_locally_linked_run_id() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let hive_id = create_collective_hive(app.clone(), &token).await;
    let response = mcp_request(app.clone(), &token, collective_mission_request(&hive_id)).await;
    let mission_id = response["result"]["structuredContent"]["mission_id"]
        .as_str()
        .expect("mission id");
    let run_id = response["result"]["structuredContent"]["run_id"]
        .as_str()
        .expect("run id");

    let result_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "get_collective_mission_result",
                "arguments": {
                    "run_id": run_id
                }
            }
        }),
    )
    .await;

    let result = &result_response["result"]["structuredContent"];
    assert_eq!(result_response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(result["mission_id"].as_str(), Some(mission_id));
    assert_eq!(result["run_id"].as_str(), Some(run_id));
    assert_eq!(result["link"]["mission_id"].as_str(), Some(mission_id));
}

#[tokio::test]
async fn mcp_get_collective_mission_result_rejects_unlinked_run_id() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_collective_mission_result",
                "arguments": {
                    "run_id": "external-run-1"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("collective mission run link not found for run_id: external-run-1")
    );
}

#[tokio::test]
async fn mcp_publish_collective_mission_committee_persists_policy_and_skills() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let hive_id = create_collective_hive(app.clone(), &token).await;
    let mut request = collective_mission_request(&hive_id);
    let arguments = request["params"]["arguments"]
        .as_object_mut()
        .expect("collective arguments");
    arguments.insert(
        "skills".to_owned(),
        json!(["climate response", "supply-chain analysis"]),
    );

    let response = mcp_request(app, &token, request).await;
    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let mission_id = content["mission_id"].as_str().expect("mission id");
    assert_eq!(
        content["run_spec"]["agents"].as_array().map(Vec::len),
        Some(0)
    );
    assert!(content["run_spec"].get("market_task_id").is_none());
    assert_eq!(
        content["run_spec"]["round_policy"]["min_participants"].as_u64(),
        Some(2)
    );
    assert_eq!(
        content["run_spec"]["join_policy"]["join_window_ms"].as_u64(),
        Some(1_800_000)
    );
    assert_eq!(
        content["mission"]["skills"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        content["hive_message"]["mission"]["skills"][0].as_str(),
        Some("climate response")
    );
    assert_eq!(
        content["run_spec"]["shared_inputs"]["mission"]["skills"][1].as_str(),
        Some("supply-chain analysis")
    );
    let persisted: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted["runs"][mission_id]["mission"]["skills"][0].as_str(),
        Some("climate response")
    );
}

#[tokio::test]
async fn mcp_publish_collective_mission_requires_collective_policy_and_filters() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let hive_id = create_collective_hive(app.clone(), &token).await;
    let mut missing_mode = collective_mission_request(&hive_id);
    missing_mode["params"]["arguments"]
        .as_object_mut()
        .expect("collective arguments")
        .remove("mode");

    let response = mcp_request(app.clone(), &token, missing_mode).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("mode is required for collective mission")
    );

    let mut missing_min_participants = collective_mission_request(&hive_id);
    missing_min_participants["params"]["arguments"]
        .as_object_mut()
        .expect("collective arguments")
        .remove("min_participants");

    let response = mcp_request(app.clone(), &token, missing_min_participants).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("min_participants is required for collective mission")
    );

    let response = mcp_request(app, &token, collective_mission_request(&hive_id)).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
}

#[tokio::test]
async fn mcp_publish_collective_mission_rejects_stigmergy_until_supported() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let hive_id = create_collective_hive(app.clone(), &token).await;

    let response = mcp_request(app, &token, collective_stigmergy_mission_request(&hive_id)).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some(
            "collective stigmergy mode is temporarily unsupported; use committee mode. Stigmergy collective missions will be opened later."
        )
    );
}
