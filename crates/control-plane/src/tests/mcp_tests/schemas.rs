use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_tools_list_surfaces_precise_input_schemas_for_agent_tools() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .all(|tool| tool["name"] != "upsert_local_friend")
    );

    let publish_mission = find_tool(tools, "publish_mission");
    assert_schema_requires(
        publish_mission,
        &["title", "description", "domain", "payload"],
    );
    assert_eq!(
        publish_mission["inputSchema"]["properties"]["title"]["type"].as_str(),
        Some("string")
    );
    assert_eq!(
        publish_mission["inputSchema"]["properties"]["title"]["maxLength"].as_u64(),
        Some(256)
    );
    assert_eq!(
        publish_mission["inputSchema"]["properties"]["description"]["maxLength"].as_u64(),
        Some(8192)
    );
    assert!(
        publish_mission["inputSchema"]["properties"]["payload"]["description"]
            .as_str()
            .is_some_and(
                |description| description.contains("No language, script, or symbol restriction")
            )
    );
    assert_eq!(
        publish_mission["inputSchema"]["properties"]["scope"]["enum"][0].as_str(),
        Some("real_world")
    );
    assert_eq!(
        publish_mission["inputSchema"]["properties"]["scope"]["enum"][1].as_str(),
        Some("in_world")
    );
    assert_schema_omits(
        publish_mission,
        &[
            "publisher",
            "publisher_kind",
            "lat",
            "lng",
            "coordinate_source",
            "reward",
        ],
    );
    assert_eq!(
        publish_mission["inputSchema"]["properties"]
            .get("body")
            .and_then(Value::as_object),
        None
    );
    assert!(
        !publish_mission["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("settlement_delegation")
    );

    let publish_delegated_mission = find_tool(tools, "publish_delegated_mission");
    assert_schema_requires(
        publish_delegated_mission,
        &[
            "title",
            "description",
            "domain",
            "payload",
            "settlement_delegation",
        ],
    );
    assert_schema_omits(
        publish_delegated_mission,
        &["publisher", "publisher_kind", "reward"],
    );
    assert!(
        publish_delegated_mission["inputSchema"]["properties"]["settlement_delegation"]
            ["description"]
            .as_str()
            .is_some_and(|description| description.contains("servicenet-agent"))
    );

    let publish_collective_mission = find_tool(tools, "publish_collective_mission");
    assert_schema_requires(
        publish_collective_mission,
        &[
            "hive_id",
            "title",
            "description",
            "domain",
            "payload",
            "mode",
            "min_participants",
        ],
    );
    assert_schema_omits(
        publish_collective_mission,
        &[
            "publisher",
            "publisher_kind",
            "lat",
            "lng",
            "coordinate_source",
            "reward",
        ],
    );
    let collective_required = publish_collective_mission["inputSchema"]["required"]
        .as_array()
        .unwrap();
    assert!(!collective_required.iter().any(|field| field == "agents"));
    assert!(!collective_required.iter().any(|field| field == "scope"));
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["scope"]["enum"][0].as_str(),
        Some("real_world")
    );
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["scope"]["enum"][1].as_str(),
        Some("in_world")
    );
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["mode"]["enum"][1].as_str(),
        Some("stigmergy")
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]["mode"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("Defaults to committee"))
    );
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["mode"]["default"].as_str(),
        Some("committee")
    );
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["skills"]["type"].as_str(),
        Some("array")
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .get("agents")
            .is_none()
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("min_participants")
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]["min_participants"]["description"]
            .as_str()
            .is_some_and(|description| !description.contains("stigmergy mode"))
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("join_window_ms")
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]["join_window_ms"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("committee join window"))
    );
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["kickoff"]["type"].as_str(),
        Some("boolean")
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]["kickoff"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("never starts Wattswarm"))
    );

    let start_collective_mission = find_tool(tools, "start_collective_mission");
    assert_schema_requires(start_collective_mission, &["run_id"]);
    assert_schema_omits(
        start_collective_mission,
        &["joined_count", "participant_count"],
    );
    assert_eq!(
        start_collective_mission["inputSchema"]["properties"]["force"]["type"].as_str(),
        Some("boolean")
    );

    let collective_result = find_tool(tools, "get_collective_mission_result");
    assert!(
        collective_result["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("mission_id")
    );
    assert!(
        collective_result["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("run_id")
    );

    let list_payments = find_tool(tools, "list_agent_payments");
    assert_eq!(
        list_payments["inputSchema"]["properties"]["target_kind"]["enum"][0].as_str(),
        Some("network_agent")
    );
    assert_eq!(
        list_payments["inputSchema"]["properties"]["target_kind"]["enum"][1].as_str(),
        Some("service_agent")
    );
    assert_eq!(
        list_payments["inputSchema"]["properties"]["target_kind"]["enum"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        list_payments["inputSchema"]["properties"]["target_address"]["type"].as_str(),
        Some("string")
    );
    assert_schema_omits(list_payments, &["counterpart_public_id", "display_name"]);

    let propose_payment = find_tool(tools, "propose_agent_payment");
    assert_schema_requires(
        propose_payment,
        &[
            "target_kind",
            "target_address",
            "amount",
            "currency",
            "rail",
        ],
    );
    assert_schema_omits(
        propose_payment,
        &[
            "public_id",
            "display_name",
            "counterpart_public_id",
            "agent_id",
            "recipient_address",
        ],
    );
    assert_eq!(
        propose_payment["inputSchema"]["properties"]["target_kind"]["enum"][1].as_str(),
        Some("service_agent")
    );
    assert_eq!(
        propose_payment["inputSchema"]["properties"]["target_kind"]["enum"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        propose_payment["inputSchema"]["properties"]["layer"]["enum"][1].as_str(),
        Some("web3")
    );

    let create_hive = find_tool(tools, "create_hive");
    assert_schema_omits(
        create_hive,
        &[
            "public_id",
            "initial_message",
            "lat",
            "lng",
            "coordinate_source",
        ],
    );
    assert_eq!(
        create_hive["inputSchema"]["properties"]["scope_hint"]["description"].as_str(),
        Some(
            "Wattswarm scope hint. Valid values are `global`, `region:<id>`, `node:<id>`, `local:<id>`, or `group:<id>`. For Hives, use `group:<hive-or-topic-id>`; do not use `topic:<id>`."
        )
    );
    let list_private_hives = find_tool(tools, "list_private_hives");
    assert_schema_requires(list_private_hives, &[]);
    assert!(
        list_private_hives["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("include_inactive")
    );
    assert_eq!(
        list_private_hives["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/hives")
    );

    let create_private_hive = find_tool(tools, "create_private_hive");
    assert_schema_requires(create_private_hive, &["feed_key", "display_name"]);
    assert_schema_omits(
        create_private_hive,
        &[
            "public_id",
            "initial_message",
            "lat",
            "lng",
            "coordinate_source",
        ],
    );
    assert_eq!(
        create_private_hive["inputSchema"]["properties"]["scope_hint"]["description"].as_str(),
        Some(
            "Optional private Wattswarm scope hint. Defaults to a unique `group:dm-<id>` value suitable for sharing out of band with invited friends."
        )
    );
    let post_hive_message = find_tool(tools, "post_hive_message");
    assert_schema_omits(post_hive_message, &["public_id"]);
    assert!(
        post_hive_message["inputSchema"]["properties"]["content"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("No language or script restriction"))
    );
    let subscribe_hive = find_tool(tools, "subscribe_hive");
    assert_schema_omits(subscribe_hive, &["public_id"]);
    let unsubscribe_hive = find_tool(tools, "unsubscribe_hive");
    assert_schema_requires(unsubscribe_hive, &["hive_id"]);
    assert_schema_omits(unsubscribe_hive, &["public_id", "active"]);
    let invite_private_hive_participant = find_tool(tools, "invite_private_hive_participant");
    assert_schema_requires(
        invite_private_hive_participant,
        &[
            "hive_id",
            "counterpart_public_id",
            "display_name",
            "hive_name",
        ],
    );
    assert_schema_omits(
        invite_private_hive_participant,
        &["public_id", "shared_secret_b64"],
    );
    let list_friends = find_tool(tools, "list_friends");
    assert_eq!(
        list_friends["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-friends")
    );
    assert!(
        list_friends["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("display_name")
    );
    let list_nearby = find_tool(tools, "list_nearby");
    assert_eq!(
        list_nearby["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/nearby")
    );
    assert!(
        list_nearby["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    let list_friend_requests = find_tool(tools, "list_friend_requests");
    assert_eq!(
        list_friend_requests["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests")
    );
    assert!(
        list_friend_requests["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("limit")
    );
    assert_schema_omits(list_friend_requests, &["direction", "state"]);
    let list_sent_friend_requests = find_tool(tools, "list_sent_friend_requests");
    assert_eq!(
        list_sent_friend_requests["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/sent-friend-requests")
    );
    let get_friend_request = find_tool(tools, "get_friend_request");
    assert_schema_requires(get_friend_request, &[]);
    assert_schema_optional(get_friend_request, "request_id");
    assert_schema_optional(get_friend_request, "display_name");
    assert_eq!(
        get_friend_request["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests/{request_id}")
    );
    let accept_friend_request = find_tool(tools, "accept_friend_request");
    assert_schema_requires(accept_friend_request, &[]);
    assert_schema_optional(accept_friend_request, "request_id");
    assert_schema_optional(accept_friend_request, "display_name");
    assert_eq!(
        accept_friend_request["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests/{request_id}/accept")
    );
    let reject_friend_request = find_tool(tools, "reject_friend_request");
    assert_schema_requires(reject_friend_request, &[]);
    assert_schema_optional(reject_friend_request, "request_id");
    assert_schema_optional(reject_friend_request, "display_name");
    assert_eq!(
        reject_friend_request["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests/{request_id}/reject")
    );
    let request_agent_friend = find_tool(tools, "request_agent_friend");
    assert!(
        request_agent_friend["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("target_agent_did")
    );
    assert!(
        !request_agent_friend["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field.as_str() == Some("remote_node_id"))
    );
    assert_schema_optional(request_agent_friend, "display_name");
    assert_schema_omits(request_agent_friend, &["public_id", "action"]);
    let search_agents = find_tool(tools, "search_agents");
    assert_schema_requires(search_agents, &[]);
    assert_schema_optional(search_agents, "public_id");
    assert_schema_optional(search_agents, "display_name");
    assert_eq!(
        search_agents["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agents/search")
    );
    assert_eq!(
        search_agents["_meta"]["wattetheria"]["readOnly"].as_bool(),
        Some(true)
    );
    let get_agent_card = find_tool(tools, "get_agent_card");
    assert_schema_requires(get_agent_card, &[]);
    assert_schema_optional(get_agent_card, "public_id");
    assert_schema_optional(get_agent_card, "display_name");
    assert_eq!(
        get_agent_card["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-card")
    );
    assert_eq!(
        get_agent_card["_meta"]["wattetheria"]["readOnly"].as_bool(),
        Some(true)
    );
    let remove_agent_friend = find_tool(tools, "remove_agent_friend");
    assert!(
        remove_agent_friend["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("target_agent_did")
    );
    assert!(
        remove_agent_friend["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("display_name")
    );
    assert!(
        !remove_agent_friend["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field.as_str() == Some("remote_node_id"))
    );
    assert_schema_omits(remove_agent_friend, &["public_id", "action"]);
    assert_eq!(
        remove_agent_friend["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-friends")
    );
    let list_agent_dm_threads = find_tool(tools, "list_agent_dm_threads");
    assert_eq!(
        list_agent_dm_threads["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-dm/threads")
    );
    assert!(
        list_agent_dm_threads["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("display_name")
    );
    let list_agent_dm_messages = find_tool(tools, "list_agent_dm_messages");
    assert_eq!(
        list_agent_dm_messages["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-dm/messages")
    );
    assert!(
        list_agent_dm_messages["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("display_name")
    );
    let send_agent_dm_message = find_tool(tools, "send_agent_dm_message");
    assert_schema_requires(send_agent_dm_message, &["content"]);
    assert!(
        send_agent_dm_message["inputSchema"]["properties"]["content"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("No language or script restriction"))
    );
    assert!(
        send_agent_dm_message["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("display_name")
    );
    assert_schema_omits(send_agent_dm_message, &["public_id"]);
    assert_eq!(
        send_agent_dm_message["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-dm/messages")
    );

    let settle_payment = find_tool(tools, "settle_agent_payment");
    assert_schema_requires(settle_payment, &["payment_id", "settlement_receipt"]);
    assert_schema_omits(
        settle_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let submit_payment = find_tool(tools, "submit_agent_payment");
    assert_schema_requires(submit_payment, &["payment_id"]);
    assert!(
        submit_payment["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("settlement_receipt")
    );
    assert!(
        !submit_payment["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field.as_str() == Some("settlement_receipt"))
    );
    assert_schema_omits(
        submit_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let get_payment = find_tool(tools, "get_agent_payment");
    assert_schema_requires(get_payment, &["payment_id"]);
    assert_schema_omits(
        get_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let authorize_payment = find_tool(tools, "authorize_agent_payment");
    assert_schema_requires(authorize_payment, &["payment_id"]);
    assert_schema_optional(authorize_payment, "sender_address");
    assert_schema_omits(
        authorize_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let reject_payment = find_tool(tools, "reject_agent_payment");
    assert_schema_requires(reject_payment, &["payment_id", "reject_reason"]);
    assert_schema_omits(
        reject_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let cancel_payment = find_tool(tools, "cancel_agent_payment");
    assert_schema_requires(cancel_payment, &["payment_id"]);
    assert_schema_omits(
        cancel_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let get_servicenet_receipt = find_tool(tools, "get_servicenet_receipt");
    assert_schema_requires(get_servicenet_receipt, &["receipt_id"]);

    let get_servicenet_agent = find_tool(tools, "get_servicenet_agent");
    assert_schema_requires(get_servicenet_agent, &["service_address"]);
    assert_schema_omits(get_servicenet_agent, &["agent_id"]);
    assert_eq!(
        get_servicenet_agent["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/servicenet/agents/{service_address}")
    );

    let send_service_agent_message = find_tool(tools, "send_service_agent_message");
    assert_schema_requires(send_service_agent_message, &["service_address"]);
    assert_schema_omits(send_service_agent_message, &["agent_id", "agent_name"]);
    assert_schema_optional(send_service_agent_message, "return_immediately");
    assert_eq!(
        send_service_agent_message["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/servicenet/agents/{service_address}/messages/send")
    );

    let get_service_agent_task = find_tool(tools, "get_service_agent_task");
    assert_schema_requires(get_service_agent_task, &["service_address", "task_id"]);
    assert_schema_omits(get_service_agent_task, &["agent_id"]);
    assert_eq!(
        get_service_agent_task["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/servicenet/agents/{service_address}/tasks/{task_id}/get")
    );
    assert_eq!(
        get_service_agent_task["_meta"]["wattetheria"]["readOnly"].as_bool(),
        Some(true)
    );

    let list_service_agent_tasks = find_tool(tools, "list_service_agent_tasks");
    assert_schema_requires(list_service_agent_tasks, &["service_address"]);
    assert_schema_optional(list_service_agent_tasks, "context_id");

    let cancel_service_agent_task = find_tool(tools, "cancel_service_agent_task");
    assert_schema_requires(cancel_service_agent_task, &["service_address", "task_id"]);
    assert_eq!(
        cancel_service_agent_task["_meta"]["wattetheria"]["readOnly"].as_bool(),
        Some(false)
    );

    let subscribe_service_agent_task = find_tool(tools, "subscribe_service_agent_task");
    assert_schema_requires(
        subscribe_service_agent_task,
        &["service_address", "task_id"],
    );
    assert_schema_optional(subscribe_service_agent_task, "max_events");

    for hidden_tool in [
        "send_mailbox_message",
        "list_mailbox_messages",
        "ack_mailbox_message",
        "invoke_servicenet_agent_sync",
        "invoke_servicenet_agent_async",
        "get_servicenet_agent_task",
    ] {
        assert!(tools.iter().all(|tool| tool["name"] != hidden_tool));
    }

    let list_missions = find_tool(tools, "list_missions");
    assert_eq!(
        list_missions["description"].as_str(),
        Some("Browse the bounded Wattetheria network mission market from the configured gateway.")
    );
    assert_eq!(
        list_missions["inputSchema"]["properties"]["limit"]["type"].as_str(),
        Some("integer")
    );
    assert_eq!(
        list_missions["inputSchema"]["properties"]["offset"]["type"].as_str(),
        Some("integer")
    );

    let claim_mission = find_tool(tools, "claim_mission");
    assert_schema_requires(claim_mission, &["mission_id", "agent_did"]);
    assert_eq!(
        claim_mission["inputSchema"]["properties"]["claim_route"]["description"].as_str(),
        Some("Claim route object returned by list_missions.")
    );
    assert_eq!(
        claim_mission["inputSchema"]["properties"]["mission_scope_hint"]["type"].as_str(),
        Some("string")
    );
    let complete_mission = find_tool(tools, "complete_mission");
    assert_schema_requires(complete_mission, &["mission_id", "agent_did"]);
    assert_eq!(
        complete_mission["inputSchema"]["properties"]["result"]["description"].as_str(),
        Some(
            "Ordinary mission completion result to publish in the mission_completed lifecycle notice."
        )
    );
    assert_eq!(
        complete_mission["inputSchema"]["properties"]["claim_route"]["description"].as_str(),
        Some("Claim route object returned by list_missions for network missions.")
    );
    let settle_mission = find_tool(tools, "settle_mission");
    assert_schema_requires(settle_mission, &["mission_id"]);
    assert_eq!(
        settle_mission["inputSchema"]["properties"]["candidate_id"]["description"].as_str(),
        Some(
            "Explicit Wattswarm candidate ID to accept before settling candidate-backed task results."
        )
    );
}

fn assert_schema_omits(tool: &Value, omitted: &[&str]) {
    let properties = tool["inputSchema"]["properties"].as_object().unwrap();
    for field in omitted {
        assert!(
            !properties.contains_key(*field),
            "expected {} schema to hide local identity field {field}",
            tool["name"].as_str().unwrap()
        );
    }
}
