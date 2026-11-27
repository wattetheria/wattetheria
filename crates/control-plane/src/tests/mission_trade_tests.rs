//! Missions reserved for one agent, claim bans, item hand-ins and sent-back results, against
//! the stand-in for the gateway's mission API.

use super::*;

async fn guide() -> Json<Value> {
    Json(json!({
        "market": [{"en": "The mission market is where agents publish what they want done.", "zh": "任务市场是 agent 发布需求的地方。"}],
        "in_world": [{"en": "An in_world mission cannot be completed empty-handed.", "zh": "in_world 任务不能空手完成。"}],
        "real_world": [{"en": "Items cannot be pledged on a real_world mission.", "zh": "real_world 任务不能押物品。"}],
        "rules": {"fee_bps": 500, "pledge_secs": 21600, "confirm_secs": 86400},
    }))
}

struct Node {
    app: Router,
    state: crate::state::ControlPlaneState,
    token: String,
    public_id: String,
    gateway: mission_gateway::SharedMissionGateway,
    bridge: Arc<MockSwarmBridge>,
    _dir: tempfile::TempDir,
}

/// A node whose gateway is the mission stand-in, with the gateway's guide, and whose registry
/// knows the agents in `known` (display name, DID).
async fn trade_node(known: &[(&str, &str)]) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: known
            .iter()
            .map(|(name, did)| {
                let public_id = scoped_id(&name.to_lowercase(), did);
                (
                    public_id.clone(),
                    SwarmDiscoveredAgent {
                        status: Some("active".to_owned()),
                        public_id,
                        remote_node_id: format!("node-{name}"),
                        target_agent_did: (*did).to_owned(),
                        display_name: Some((*name).to_owned()),
                        source_agent_card: None,
                    },
                )
            })
            .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let (dir, mut state, token, _) =
        build_test_state_with_bridge(50, dir, identity, event_log, bridge.clone());
    let (url, gateway) = mission_gateway::spawn_mission_gateway(
        Router::new().route("/api/missions/guide", get(guide)),
    )
    .unwrap();
    // The configured URL is the gateway's ingest endpoint, as on a real node.
    state.gateway_urls = vec![format!("{url}/api/ingest/snapshot")];
    let app = crate::app(state.clone());
    let public_id = bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    Node {
        app,
        state,
        token,
        public_id,
        gateway,
        bridge,
        _dir: dir,
    }
}

impl Node {
    async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        let (status, text) = request_text(
            self.app.clone(),
            axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", format!("Bearer {}", self.token))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn publish(
        &self,
        scope: &str,
        bounty: i64,
        assignee: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut body = json!({
            "title": "Wanted: one soul gem",
            "description": "I pay Watt for one soul gem.",
            "publisher": self.public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "scope": scope,
            "subnet_id": null,
            "zone_id": null,
            "required_role": null,
            "required_faction": null,
            "reward": {"agent_watt": bounty, "reputation": 0, "capacity": 0},
            "payload": {"wanted": "soul_gem"},
        });
        if let Some(assignee) = assignee {
            body["assignee"] = json!(assignee);
        }
        self.post("/v1/wattetheria/missions", body).await
    }

    async fn published(&self, scope: &str, bounty: i64, assignee: Option<&str>) -> String {
        let (status, mission) = self.publish(scope, bounty, assignee).await;
        assert_eq!(status, StatusCode::CREATED, "{mission}");
        mission["mission_id"].as_str().unwrap().to_owned()
    }

    async fn claim(&self, mission_id: &str, agent_did: &str) -> (StatusCode, Value) {
        self.post(
            &format!("/v1/wattetheria/missions/{mission_id}/claim"),
            json!({"agent_did": agent_did}),
        )
        .await
    }

    /// What the claimer's own node does before its claim reaches this node: asks the gateway.
    async fn asked_by(&self, mission_id: &str, agent_did: &str) {
        self.gateway.lock().await.ask(mission_id, agent_did);
    }

    /// Calls the `get_mission` tool and returns its answer.
    async fn get_mission(&self, arguments: Value) -> Value {
        self.tool("get_mission", arguments).await
    }

    /// Calls one MCP tool and returns its structured answer.
    async fn tool(&self, name: &str, arguments: Value) -> Value {
        let response = request_json(
            self.app.clone(),
            axum::http::Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("authorization", format!("Bearer {}", self.token))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {"name": name, "arguments": arguments},
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        response["result"]["structuredContent"].clone()
    }

    /// The mission's status as the gateway keeps it, read the way the node reads it.
    async fn status(&self, mission_id: &str) -> String {
        crate::routes::gateway_missions::fetch(&self.state, mission_id)
            .await
            .unwrap()
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn calls(&self, action: &str) -> Vec<Value> {
        self.gateway.lock().await.commands(action)
    }

    /// The claimer's own node pledges items for the mission.
    async fn pledged_by_claimer(&self, mission_id: &str) {
        let mut gateway = self.gateway.lock().await;
        gateway.set(mission_id, "status", json!("completed"));
        gateway.set(
            mission_id,
            "held",
            json!({"kind": "crop", "item": "coffee", "units": 5}),
        );
    }

    /// A deadline of the mission passes.
    async fn passed(&self, mission_id: &str, deadline: &str) {
        let past = (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
        self.gateway
            .lock()
            .await
            .set(mission_id, deadline, json!(past));
    }

    /// A mission another node published on wattswarm.
    async fn remote(&self, mission_id: &str, assignee: Option<&str>) {
        let mut contract = self
            .state
            .swarm_bridge
            .sample_task_contract(mission_id)
            .await
            .unwrap();
        contract.task_type = "wattetheria.mission".to_owned();
        contract.inputs = json!({
            "mission_id": mission_id,
            "publisher_wattswarm_node_id": "publisher-node",
            "mission_feed_key": "wattetheria.missions",
            "mission_scope_hint": format!("group:{mission_id}"),
            "swarm_scope": {"kind": "group", "id": mission_id},
            "domain": "trade",
            "scope": "in_world",
        });
        let mut record = mission_gateway::remote_record(
            mission_id,
            "Wanted far away",
            "did:key:publisher-far",
            "published",
            Some(&contract),
        );
        record["assignee"] = json!(assignee);
        self.gateway.lock().await.seed(record);
    }
}

#[tokio::test]
async fn a_mission_reserved_for_one_agent_refuses_everyone_else() {
    let momo = Identity::new_random().agent_did;
    let other = Identity::new_random().agent_did;
    let node = trade_node(&[("Momo", &momo)]).await;
    let me = node.state.identity.agent_did.clone();

    let (status, refused) = node.publish("in_world", 50, Some("Nobody")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{refused}");
    assert_eq!(refused["code"], "assignee_not_found");
    assert_eq!(
        node.calls("publish").await,
        Vec::<Value>::new(),
        "an unknown name publishes nothing"
    );

    let (status, mission) = node.publish("in_world", 50, Some("Momo")).await;
    assert_eq!(status, StatusCode::CREATED, "{mission}");
    let mission_id = mission["mission_id"].as_str().unwrap().to_owned();
    assert_eq!(mission["assignee_did"], json!(momo), "{mission}");
    assert_eq!(mission["assignee_display_name"], "Momo");
    assert_eq!(
        mission["task_contract"]["inputs"]["assignee_did"],
        json!(momo),
        "the reservation is announced with the mission: {mission}"
    );
    let publish = node.calls("publish").await;
    assert_eq!(publish[0]["assignee"], json!(momo));
    assert_eq!(publish[0]["assignee_label"], "Momo");
    assert_eq!(publish[0]["scope"], "in_world");
    assert_eq!(publish[0]["reward"]["kind"], "watt");
    assert_eq!(publish[0]["reward"]["amount"], 50);

    // This node's own agent is not the named one: the gateway refuses its request.
    let (status, refused) = node.claim(&mission_id, &me).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["code"], "assignee_only");
    assert_eq!(node.status(&mission_id).await, "published");

    // The publisher approves only an agent that asked the gateway itself.
    let (status, refused) = node.claim(&mission_id, &other).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "claim_not_requested");
    let (status, refused) = node.claim(&mission_id, &momo).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "claim_not_requested");
    assert_eq!(node.status(&mission_id).await, "published");
    node.asked_by(&mission_id, &momo).await;
    let (status, claimed) = node.claim(&mission_id, &momo).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(node.status(&mission_id).await, "claimed");
    let approved = node.calls("approve").await;
    assert_eq!(approved.last().unwrap()["claimer"], json!(momo));
    assert_eq!(approved.last().unwrap()["publisher"], json!(me));
}

#[tokio::test]
async fn a_bounty_open_to_anyone_is_published_with_its_reward_and_no_assignee() {
    let worker = Identity::new_random().agent_did;
    let node = trade_node(&[]).await;
    let mission_id = node.published("real_world", 40, None).await;
    let publish = node.calls("publish").await;
    assert!(publish[0]["assignee"].is_null(), "{}", publish[0]);
    assert_eq!(publish[0]["scope"], "real_world");
    assert_eq!(
        (
            &publish[0]["reward"]["kind"],
            &publish[0]["reward"]["amount"]
        ),
        (&json!("watt"), &json!(40))
    );

    // The node names the request, never the mission: the gateway makes the mission's id.
    assert!(mission_id.starts_with("rw_"), "{mission_id}");
    assert!(publish[0]["request_id"].is_string(), "{}", publish[0]);
    assert!(publish[0].get("mission_id").is_none(), "{}", publish[0]);
    let sent = &publish[0]["payload"];
    assert!(sent.get("task_id").is_none(), "{sent}");
    assert_eq!(sent["task_contract"]["task_id"], "", "{sent}");
    assert_eq!(sent["task_contract"]["inputs"]["mission_id"], "", "{sent}");
    // The gateway writes the id into the contract it keeps, which is what a claiming node
    // imports, and the publisher puts that same contract on wattswarm.
    let scope = json!({"kind": "group", "id": mission_id});
    let hint = format!("group:{mission_id}");
    let kept = node.gateway.lock().await.record(&mission_id).unwrap();
    let kept = &kept["payload"];
    assert_eq!(kept["task_id"], json!(mission_id), "{kept}");
    assert_eq!(kept["swarm_scope"], scope, "{kept}");
    assert_eq!(kept["mission_scope_hint"], json!(hint), "{kept}");
    let on_swarm = node.bridge.published_tasks.lock().await.clone();
    assert_eq!(on_swarm.len(), 2, "{on_swarm:?}");
    let submitted = &on_swarm[0]["submitted"];
    assert_eq!(submitted, &kept["task_contract"]);
    assert_eq!(submitted["task_id"], json!(mission_id));
    assert_eq!(submitted["inputs"]["mission_id"], json!(mission_id));
    assert_eq!(submitted["inputs"]["swarm_scope"], scope);
    assert_eq!(submitted["inputs"]["mission_scope_hint"], json!(hint));
    let announced = &on_swarm[1]["announced"];
    assert_eq!(announced["task_id"], json!(mission_id));
    assert_eq!(announced["scope_hint"], json!(hint));
    assert_eq!(announced["summary"]["mission_id"], json!(mission_id));

    node.asked_by(&mission_id, &worker).await;
    let (status, claimed) = node.claim(&mission_id, &worker).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(node.calls("approve").await[0]["claimer"], json!(worker));
}

#[tokio::test]
async fn an_agent_under_a_claim_ban_cannot_claim_any_mission() {
    let node = trade_node(&[]).await;
    let me = node.state.identity.agent_did.clone();
    node.gateway.lock().await.banned.push(me.clone());

    // A mission with no bounty at all: the ban still holds.
    let free = node.published("real_world", 0, None).await;
    let (status, refused) = node.claim(&free, &me).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["code"], "claimer_banned");
    assert_eq!(refused["details"]["banned_until"], "2026-12-02T00:00:00Z");
    assert_eq!(node.status(&free).await, "published");
    assert_eq!(node.calls("approve").await, Vec::<Value>::new());

    // Another node's mission: the claim is refused before it is sent on wattswarm.
    node.remote("elsewhere", None).await;
    let (status, refused) = node.claim("elsewhere", &me).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["code"], "claimer_banned");
    let claims: NetworkMissionClaimRegistry = node
        .state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    assert!(claims.records().is_empty(), "no claim was sent");
}

#[tokio::test]
async fn a_claim_for_another_nodes_mission_is_asked_on_the_gateway_first() {
    let momo = Identity::new_random().agent_did;
    let node = trade_node(&[]).await;
    let me = node.state.identity.agent_did.clone();

    // Reserved for someone else: the gateway refuses, nothing is sent on wattswarm.
    node.remote("m-1", Some(&momo)).await;
    let (status, refused) = node.claim("m-1", &me).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["code"], "assignee_only");
    let claims: NetworkMissionClaimRegistry = node
        .state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    assert!(claims.records().is_empty(), "no claim was sent");

    // Open to anyone: the request is registered on the gateway under this agent's signature,
    // then the claim goes to the publisher.
    node.remote("m-2", None).await;
    let (status, claimed) = node.claim("m-2", &me).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(claimed["status"], "network_claim_submitted", "{claimed}");
    let asked = node.calls("claims").await;
    assert_eq!(asked.last().unwrap()["mission_id"], "m-2");
    assert_eq!(asked.last().unwrap()["claimer"], json!(me));
    assert_eq!(
        node.status("m-2").await,
        "published",
        "the publisher has not approved the claim yet"
    );
}

#[tokio::test]
async fn the_claimer_pledges_items_and_the_publisher_settles() {
    let node = trade_node(&[]).await;
    let me = node.state.identity.agent_did.clone();
    let mission_id = node.published("in_world", 60, None).await;

    // One agent on both sides: its node asks the gateway and approves the claim itself.
    let (status, claimed) = node.claim(&mission_id, &me).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(node.calls("claims").await[0]["claimer"], json!(me));
    assert_eq!(node.calls("approve").await[0]["claimer"], json!(me));

    let (status, pledged) = node
        .post(
            &format!("/v1/wattetheria/missions/{mission_id}/pledge"),
            json!({"kind": "hunt", "item": "soul_gem", "units": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{pledged}");
    assert_eq!(
        pledged["mission"]["gateway_status"], "completed",
        "{pledged}"
    );
    assert_eq!(pledged["mission"]["held"]["item"], "soul_gem");
    assert_eq!(pledged["completion"]["status"], "completed", "{pledged}");
    assert_eq!(node.status(&mission_id).await, "completed");
    assert_eq!(
        node.calls("pledge").await[0],
        json!({
            "action": "pledge",
            "mission_id": mission_id,
            "claimer": me,
            "kind": "hunt",
            "item": "soul_gem",
            "units": 1,
            "issued_at": node.calls("pledge").await[0]["issued_at"],
            "nonce": node.calls("pledge").await[0]["nonce"],
        })
    );
    assert!(
        node.calls("deliver").await.is_empty(),
        "the pledge is the hand-in"
    );

    // The publisher reads the mission before settling: what the system holds and the reward.
    let read = node.get_mission(json!({"mission_id": mission_id})).await;
    let mission = &read["missions"][0];
    assert_eq!(read["count"], 1, "{read}");
    assert_eq!(mission["status"], "completed", "{read}");
    assert_eq!(mission["held"]["units"], 1);
    assert_eq!(mission["reward_terms"]["amount"], 60, "{read}");

    let (status, settled) = node
        .post(
            &format!("/v1/wattetheria/missions/{mission_id}/settle"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(node.status(&mission_id).await, "settled");
    assert_eq!(node.calls("settle").await[0]["publisher"], json!(me));

    // Sent again, the settle pays nothing twice but sends its notice to the claimer again.
    let (status, again) = node
        .post(
            &format!("/v1/wattetheria/missions/{mission_id}/settle"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(node.calls("settle").await.len(), 1, "{again}");
    assert_eq!(
        again["mission_lifecycle_notice"]["kind"], "mission_settled",
        "{again}"
    );
}

/// Every mission states the same facts in the same fields, however its record lays them out:
/// `ours` published by this node, `far` by another.
fn assert_the_same_facts_in_the_same_fields(ours: &Value, far: &Value) {
    assert_eq!(ours["scope"], "in_world", "{ours}");
    assert_eq!(ours["domain"], "trade");
    assert_eq!(ours["status"], "published");
    assert_eq!(
        ours["region"],
        // This node's location was derived, so it names no place.
        json!({"lat": ours["lat"], "lng": ours["lng"], "city": null, "country": null}),
        "{ours}"
    );
    let published_at = ours["published_at"].as_str().expect("an RFC 3339 time");
    assert!(
        chrono::DateTime::parse_from_rfc3339(published_at).is_ok(),
        "{published_at}"
    );
    assert_eq!(far["scope"], "real_world", "{far}");
    assert_eq!(far["domain"], "trade");
    assert_eq!(far["status"], "published");
    assert_eq!(
        far["region"],
        json!({"lat": 40.7128, "lng": -74.006, "city": "New York", "country": "United States"})
    );
    assert_eq!(far["published_at"], "2026-11-18T11:06:40Z");
}

#[tokio::test]
async fn a_mission_is_read_in_full_by_any_part_of_its_title_or_by_its_id() {
    let node = trade_node(&[]).await;
    let (status, gem) = node
        .post(
            "/v1/wattetheria/missions",
            json!({
                "title": "Want one Soul Gem",
                "description": "Buying a soul gem.",
                "publisher": node.public_id,
                "publisher_kind": "player",
                "domain": "trade",
                "scope": "in_world",
                "reward": {"agent_watt": 80, "reputation": 0, "capacity": 0},
                "payload": {},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{gem}");
    let gem_id = gem["mission_id"].as_str().unwrap().to_owned();
    {
        let mut gateway = node.gateway.lock().await;
        // Another node's mission, with its place and time as the gateway keeps them.
        let mut far = mission_gateway::remote_record(
            "m-far",
            "Soul gem wanted, paying well",
            "did:key:node-far",
            "published",
            None,
        );
        far["place"] = json!({"lat": 40.7128, "lng": -74.006, "city": "New York",
                              "country": "United States", "source": "ip"});
        far["created_at"] = json!("2026-11-18T11:06:40Z");
        gateway.seed(far);
        // A record that states almost nothing.
        gateway
            .seed(json!({"mission_id": "m-water", "title": "Water my field", "status": "claimed"}));
    }

    // Any part of the title, whatever the case, each mission once.
    let read = node.get_mission(json!({"title": "soul GEM"})).await;
    assert_eq!(read["count"], 2, "{read}");
    assert_eq!(read["has_more"], false);
    let ours = read["missions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mission| mission["mission_id"] == json!(gem_id))
        .expect("ours is found");
    assert_eq!(ours["description"], "Buying a soul gem.");
    assert_eq!(ours["reward_terms"]["amount"], 80, "{read}");
    let far = read["missions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mission| mission["mission_id"] == "m-far")
        .expect("far is found");
    assert_eq!(far["publisher_agent_did"], "did:key:node-far");

    assert_the_same_facts_in_the_same_fields(ours, far);

    // The market list states them the same way.
    let listed = node.tool("list_missions", json!({})).await;
    let far_listed = listed["missions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mission| mission["mission_id"] == "m-far")
        .expect("m-far is on the market");
    for stated in ["scope", "domain", "region", "published_at", "status"] {
        assert_eq!(far_listed[stated], far[stated], "{stated}: {listed}");
    }

    // The exact title comes first, and limit cuts the rest off.
    let exact = node
        .get_mission(json!({"title": "want one soul gem", "limit": 1}))
        .await;
    assert_eq!(exact["missions"][0]["mission_id"], json!(gem_id));
    let cut = node.get_mission(json!({"title": "soul", "limit": 1})).await;
    assert_eq!((&cut["count"], &cut["known_count"]), (&json!(1), &json!(2)));
    assert_eq!(cut["has_more"], true);

    // By id: exactly that one.
    let by_id = node.get_mission(json!({"mission_id": "m-water"})).await;
    assert_eq!(by_id["count"], 1, "{by_id}");
    assert_eq!(by_id["missions"][0]["title"], "Water my field");
    let water = &by_id["missions"][0];
    assert_eq!(water["status"], "claimed", "{by_id}");
    for unsaid in ["scope", "domain", "region", "published_at"] {
        assert!(
            water.get(unsaid).is_some_and(Value::is_null),
            "{unsaid} is stated as unknown, not left out or guessed: {by_id}"
        );
    }
    let unknown = node.get_mission(json!({"mission_id": "m-none"})).await;
    assert_eq!(unknown["count"], 0, "{unknown}");
    let nothing = node.get_mission(json!({"title": "dragon"})).await;
    assert_eq!(nothing["count"], 0, "{nothing}");
    assert_eq!(nothing["missions"], json!([]));
    let no_question = node.get_mission(json!({})).await;
    assert!(
        no_question["error"].as_str().unwrap().contains("title"),
        "{no_question}"
    );
}

#[tokio::test]
async fn an_in_world_mission_is_not_completed_empty_handed() {
    let momo = Identity::new_random().agent_did;
    let node = trade_node(&[("Momo", &momo)]).await;
    let me = node.state.identity.agent_did.clone();

    let mission_id = node.published("in_world", 60, None).await;
    let (status, claimed) = node.claim(&mission_id, &me).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let (status, refused) = node
        .post(
            &format!("/v1/wattetheria/missions/{mission_id}/complete"),
            json!({"agent_did": me}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "delivery_required");
    assert_eq!(
        node.status(&mission_id).await,
        "claimed",
        "the time to deliver keeps running"
    );

    // A mission that asks for no item is handed in with a result that shows the work, then
    // settled like any other.
    let proof = json!({"note": "field watered", "image": "https://example.test/field.png"});
    let (status, completed) = node
        .post(
            &format!("/v1/wattetheria/missions/{mission_id}/complete"),
            json!({"agent_did": me, "result": proof}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(node.status(&mission_id).await, "completed");
    assert_eq!(node.calls("deliver").await.last().unwrap()["result"], proof);
    let (status, settled) = node
        .post(
            &format!("/v1/wattetheria/missions/{mission_id}/settle"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(node.status(&mission_id).await, "settled");

    // Outside the Space a named mission asks for no item: it completes as it always did.
    let outside = node.published("real_world", 60, Some("Momo")).await;
    node.asked_by(&outside, &momo).await;
    let (status, claimed) = node.claim(&outside, &momo).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let (status, completed) = node
        .post(
            &format!("/v1/wattetheria/missions/{outside}/complete"),
            json!({"agent_did": momo, "result": {"note": "report sent"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(node.status(&outside).await, "completed");
}

/// The publisher's decision on a result that reached it, committed the way its agent does.
async fn decide(node: &Node, mission_id: &str, claimer: &str, action: &str) -> Value {
    authed_post_json(
        node.app.clone(),
        &node.token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": format!("evt-{mission_id}-{action}"),
                "event_type": "task_result_received",
                "source_kind": "task_lifecycle",
                "source_node_id": "claimer-node",
                "payload": {
                    "task_id": mission_id,
                    "candidate_output": {
                        "kind": "wattetheria_mission_result",
                        "mission_id": mission_id,
                        "agent_did": claimer,
                    },
                },
                "requires_commit": true,
            },
            "decision": {
                "decision_id": format!("decision-{mission_id}-{action}"),
                "action": action,
                "route": "wattetheria_commit",
                "reason": "not what I asked for",
                "payload": {"mission_id": mission_id},
            },
        }),
    )
    .await
}

#[tokio::test]
async fn a_rejected_hand_in_ends_the_mission_as_rejected() {
    let seller = Identity::new_random().agent_did;
    let momo = Identity::new_random().agent_did;
    let node = trade_node(&[("Momo", &momo)]).await;
    let complete = |mission_id: String, agent: String, result: Value| {
        let node = &node;
        async move {
            node.post(
                &format!("/v1/wattetheria/missions/{mission_id}/complete"),
                json!({"agent_did": agent, "result": result}),
            )
            .await
        }
    };

    let mission_id = node.published("in_world", 20, None).await;
    node.asked_by(&mission_id, &seller).await;
    let (status, claimed) = node.claim(&mission_id, &seller).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    node.pledged_by_claimer(&mission_id).await;
    let (status, completed) = complete(
        mission_id.clone(),
        seller.clone(),
        json!({"pledged": "coffee"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert!(
        node.calls("deliver").await.is_empty(),
        "pledged items are already the hand-in"
    );

    // The publisher turns the hand-in down with the decision it always had for a result: the
    // gateway is told, and the mission ends as rejected.
    let answer = decide(&node, &mission_id, &seller, "reject_result").await;
    assert_eq!(answer["status"], "rejected", "{answer}");
    assert_eq!(answer["returned"]["gateway_status"], "rejected", "{answer}");
    assert_eq!(node.status(&mission_id).await, "rejected");
    assert_eq!(
        node.calls("reject").await[0]["reason"],
        "not what I asked for"
    );

    // Deciding again reaches the gateway the same way and changes nothing more.
    let answer = decide(&node, &mission_id, &seller, "reject_result").await;
    assert_eq!(answer["status"], "rejected", "{answer}");
    assert_eq!(answer["returned"]["gateway_status"], "rejected", "{answer}");
    assert_eq!(node.status(&mission_id).await, "rejected");
    assert_eq!(node.calls("reject").await.len(), 2);
    assert_eq!(node.calls("cancel").await, Vec::<Value>::new());

    // Outside the Space a result is rejected the same way.
    let outside = node.published("real_world", 20, Some("Momo")).await;
    node.asked_by(&outside, &momo).await;
    let (status, claimed) = node.claim(&outside, &momo).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");

    // Nothing handed in: the decision is only the answer to the claimer.
    let answer = decide(&node, &outside, &momo, "reject_result").await;
    assert_eq!(answer["status"], "rejected", "{answer}");
    assert!(answer.get("returned").is_none(), "{answer}");
    assert_eq!(node.status(&outside).await, "claimed");

    let first = json!({"note": "report sent"});
    let (status, completed) = complete(outside.clone(), momo.clone(), first.clone()).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    let answer = decide(&node, &outside, &momo, "reject_result").await;
    assert_eq!(answer["status"], "rejected", "{answer}");
    assert_eq!(answer["returned"]["gateway_status"], "rejected", "{answer}");
    assert_eq!(answer["returned"]["completion_result"], first, "{answer}");
    assert_eq!(node.status(&outside).await, "rejected");
    assert_eq!(node.calls("reject").await.len(), 4);

    // There is no asking for another try: the node does not take that decision.
    let refused = decide(&node, &outside, &momo, "request_retry").await;
    assert_ne!(refused["status"], "retry_requested", "{refused}");
    assert_eq!(node.calls("reject").await.len(), 4);
}

/// A review of `mission_id` for `claimer`, as the publisher's agent decides it.
fn review(action: &str, mission_id: &str, claimer: &str) -> crate::routes::core::MissionReview {
    crate::routes::core::MissionReview {
        action: action.to_owned(),
        mission_id: mission_id.to_owned(),
        agent_did: claimer.to_owned(),
        task_id: mission_id.to_owned(),
        execution_id: format!("wattetheria:{mission_id}:{claimer}"),
        reason: Some("a person should look".to_owned()),
        target_agent_id: Some(claimer.to_owned()),
        target_node_id: Some("claimer-node".to_owned()),
        context: Value::Null,
    }
}

async fn answer(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn leaving_a_claim_or_a_hand_in_to_a_human_decides_nothing() {
    let seller = Identity::new_random().agent_did;
    let node = trade_node(&[]).await;
    let mission_id = node.published("in_world", 20, None).await;
    node.asked_by(&mission_id, &seller).await;

    // The claim is left to a human: the gateway is not asked and the claimer is told nothing.
    let (status, left) = answer(
        crate::routes::core::review_mission_claim(
            &node.state,
            node.token.clone(),
            review("human_review", &mission_id, &seller),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{left}");
    assert_eq!(left["status"], "human_review", "{left}");
    assert_eq!(node.calls("reject_claim").await, Vec::<Value>::new());
    assert!(node.bridge.task_notices.lock().await.is_empty());

    // The hand-in is left to a human: the mission stays handed in, nothing goes back, and the
    // claimer is told nothing.
    let (status, claimed) = node.claim(&mission_id, &seller).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    node.pledged_by_claimer(&mission_id).await;
    let (status, left) = answer(
        crate::routes::core::review_mission_result(
            &node.state,
            node.token.clone(),
            review("human_review", &mission_id, &seller),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{left}");
    assert_eq!(left["status"], "human_review", "{left}");
    assert_eq!(node.calls("reject").await, Vec::<Value>::new());
    assert_eq!(node.status(&mission_id).await, "completed");
    assert_eq!(
        *node.bridge.task_notices.lock().await,
        vec!["claim decision"],
        "only the approval of the claim was sent"
    );

    // The person then rejects it: now the mission ends and the claimer is told.
    let rejected = node
        .tool("reject_mission_result", json!({"mission_id": mission_id}))
        .await;
    assert_eq!(rejected["status"], "rejected", "{rejected}");
    assert_eq!(node.status(&mission_id).await, "rejected");
    assert_eq!(
        *node.bridge.task_notices.lock().await,
        vec!["claim decision", "completion decision"]
    );
}

#[tokio::test]
async fn the_publisher_sends_nothing_in_a_remote_claimers_name() {
    let seller = Identity::new_random().agent_did;
    let stale = Identity::new_random().agent_did;
    let node = trade_node(&[]).await;
    let me = node.state.identity.agent_did.clone();
    let mission_id = node.published("real_world", 20, None).await;

    // The claimer is on another node: approving it sends the decision and no claim in its name.
    node.asked_by(&mission_id, &seller).await;
    let (status, claimed) = node.claim(&mission_id, &seller).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(
        *node.bridge.task_notices.lock().await,
        vec!["claim decision"]
    );

    // The publisher records the hand-in for that claimer: no hand-in notice goes out in its name.
    let (status, completed) = node
        .post(
            &format!("/v1/wattetheria/missions/{mission_id}/complete"),
            json!({"agent_did": seller, "result": {"link": "https://example.com/done.png"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(node.status(&mission_id).await, "completed");
    assert!(
        completed.get("mission_lifecycle_notice").is_none(),
        "{completed}"
    );
    assert_eq!(
        *node.bridge.task_notices.lock().await,
        vec!["claim decision"]
    );

    // Settling names another agent: the notice still goes to the claimer the gateway recorded.
    let (status, settled) = node
        .post(
            &format!("/v1/wattetheria/missions/{mission_id}/settle"),
            json!({"agent_did": stale}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    let notice = &settled["mission_lifecycle_notice"];
    assert_eq!(notice["target_agent_id"], json!(seller), "{settled}");
    assert_eq!(
        notice["execution_id"],
        json!(format!("wattetheria:{mission_id}:{seller}")),
        "{settled}"
    );

    // This node's own agent claiming its own mission does send its claim, signed by itself.
    let own = node.published("real_world", 21, None).await;
    let (status, claimed) = node.claim(&own, &me).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert!(
        node.bridge
            .task_notices
            .lock()
            .await
            .contains(&format!("claim from {me}")),
    );
}

/// Every mission id the gateway keeps.
async fn kept_missions(node: &Node) -> Vec<String> {
    node.gateway.lock().await.missions.keys().cloned().collect()
}

/// What wattswarm was given for one mission: `submitted` and `announced`, in order.
async fn on_swarm(node: &Node, mission_id: &str) -> Vec<&'static str> {
    node.bridge
        .published_tasks
        .lock()
        .await
        .iter()
        .filter_map(|task| {
            ["submitted", "announced"]
                .into_iter()
                .find(|step| task[*step]["task_id"] == mission_id)
        })
        .collect()
}

#[tokio::test]
async fn publishing_again_after_a_lost_answer_locks_no_second_bounty() {
    let node = trade_node(&[]).await;

    // The gateway records the mission but its answer never reaches the node.
    node.gateway.lock().await.lost_publish_answers = 1;
    let (status, failed) = node.publish("in_world", 10, None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{failed}");
    let kept = kept_missions(&node).await;
    assert_eq!(kept.len(), 1, "the gateway holds the mission");
    assert_eq!(on_swarm(&node, &kept[0]).await, [""; 0]);

    // Asking for the same mission again finishes that one: no second mission, no second lock,
    // and the task reaches wattswarm now.
    let (status, published) = node.publish("in_world", 10, None).await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    assert_eq!(published["mission_id"], json!(kept[0]), "{published}");
    assert!(published.get("already_published").is_none(), "{published}");
    assert_eq!(kept_missions(&node).await, kept);
    assert_eq!(on_swarm(&node, &kept[0]).await, ["submitted", "announced"]);

    // The caller never saw that answer and sends the same mission once more: it is told the
    // mission is there, and nothing is made or announced again.
    let (status, second) = node.publish("in_world", 10, None).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["already_published"], true);
    assert_eq!(second["mission_id"], published["mission_id"]);
    assert_eq!(kept_missions(&node).await, kept);
    assert_eq!(on_swarm(&node, &kept[0]).await, ["submitted", "announced"]);
    assert_eq!(node.calls("cancel").await, Vec::<Value>::new());
}

#[tokio::test]
async fn a_lost_publish_somebody_took_meanwhile_is_finished_not_made_again() {
    let node = trade_node(&[]).await;
    node.gateway.lock().await.lost_publish_answers = 1;
    let (status, failed) = node.publish("in_world", 30, None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{failed}");
    let kept = kept_missions(&node).await;
    let worker = Identity::new_random().agent_did;
    node.asked_by(&kept[0], &worker).await;
    assert_eq!(node.claim(&kept[0], &worker).await.0, StatusCode::OK);

    // The publisher's notices to the claimer need the task on this node's wattswarm, so the
    // repeated publish puts it there; the mission and its claim stay as they are.
    let (status, replay) = node.publish("in_world", 30, None).await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["mission_id"], json!(kept[0]));
    assert_eq!(replay["gateway_status"], "claimed");
    assert_eq!(on_swarm(&node, &kept[0]).await, ["submitted", "announced"]);
    assert_eq!(node.calls("cancel").await, Vec::<Value>::new());
    assert_eq!(kept_missions(&node).await, kept);

    let (status, again) = node.publish("in_world", 30, None).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["already_published"], true);
    assert_eq!(again["gateway_status"], "claimed");
    assert_eq!(kept_missions(&node).await, kept);

    // The same title with another bounty is another mission.
    let other = node.published("in_world", 31, None).await;
    assert_ne!(other, kept[0]);
    assert_eq!(kept_missions(&node).await.len(), 2);
}

#[tokio::test]
async fn the_same_mission_published_twice_at_once_is_one_mission() {
    let node = trade_node(&[]).await;
    let (a, b) = tokio::join!(
        node.publish("in_world", 5, None),
        node.publish("in_world", 5, None)
    );
    // Whichever asks wattswarm before the other has put the task there puts it there too,
    // which wattswarm takes as the same task; neither waits for the other.
    for (status, answer) in [&a, &b] {
        assert!(
            matches!(*status, StatusCode::CREATED | StatusCode::OK),
            "{status} {answer}"
        );
    }
    assert_eq!(a.1["mission_id"], b.1["mission_id"]);
    assert_eq!(kept_missions(&node).await.len(), 1);

    // Another mission is not held up by this one, and is its own.
    let other = node.published("real_world", 5, None).await;
    assert_ne!(json!(other), a.1["mission_id"]);
}

#[tokio::test]
async fn only_a_cancelled_mission_can_be_published_again_unchanged() {
    let node = trade_node(&[]).await;
    let first = node.published("in_world", 5, None).await;
    let (status, cancelled) = node
        .post(
            &format!("/v1/wattetheria/missions/{first}/cancel"),
            json!({}),
        )
        .await;
    assert!(status.is_success(), "{cancelled}");

    let second = node.published("in_world", 5, None).await;
    assert_ne!(second, first);
    assert_eq!(node.status(&first).await, "cancelled");
    assert_eq!(node.status(&second).await, "published");
    assert_eq!(on_swarm(&node, &second).await, ["submitted", "announced"]);

    // A mission that ended any other way keeps its content taken.
    node.gateway
        .lock()
        .await
        .set(&second, "status", json!("rejected"));
    let (status, again) = node.publish("in_world", 5, None).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["already_published"], true);
    assert_eq!(again["mission_id"], json!(second));
    assert_eq!(kept_missions(&node).await.len(), 2);
}

#[tokio::test]
async fn a_mission_that_cannot_be_announced_is_cancelled_only_by_the_publish_that_made_it() {
    let node = trade_node(&[]).await;
    let down = |down: bool| {
        node.bridge
            .announce_down
            .store(down, std::sync::atomic::Ordering::SeqCst);
    };

    // This publish makes the mission and cannot announce it: it takes the mission back, and
    // the same mission can be published once wattswarm answers.
    down(true);
    let (status, failed) = node.publish("in_world", 10, None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{failed}");
    let kept = kept_missions(&node).await;
    assert_eq!(node.status(&kept[0]).await, "cancelled");
    down(false);
    let second = node.published("in_world", 10, None).await;
    assert_ne!(second, kept[0]);

    // A mission that was there before this publish is left alone when announcing fails: it
    // may have been taken. Its task is on wattswarm by then, so the next publish finds both.
    node.gateway.lock().await.lost_publish_answers = 1;
    assert_eq!(
        node.publish("in_world", 20, None).await.0,
        StatusCode::BAD_GATEWAY
    );
    let lost = kept_missions(&node)
        .await
        .into_iter()
        .find(|id| *id != kept[0] && *id != second)
        .unwrap();
    down(true);
    let (status, failed) = node.publish("in_world", 20, None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{failed}");
    assert_eq!(node.status(&lost).await, "published");
    assert_eq!(node.calls("cancel").await.len(), 1);
    assert_eq!(on_swarm(&node, &lost).await, ["submitted"]);
    down(false);
    let (status, found) = node.publish("in_world", 20, None).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["already_published"], true);
    assert_eq!(found["mission_id"], json!(lost));
    assert_eq!(kept_missions(&node).await.len(), 3);
}

#[tokio::test]
async fn a_claim_whose_notice_fails_says_the_request_stands() {
    let node = trade_node(&[]).await;
    let me = node.state.identity.agent_did.clone();
    node.remote("m-1", None).await;

    // The gateway takes the request, then the claim cannot be sent: the answer says so.
    node.bridge
        .claim_down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, failed) = node.claim("m-1", &me).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{failed}");
    assert_eq!(failed["code"], "claim_notice_not_delivered", "{failed}");
    assert_eq!(failed["mission_lifecycle_notice"]["sent"], false);
    assert_eq!(node.calls("claims").await.len(), 1);
    let claims: NetworkMissionClaimRegistry = node
        .state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    assert!(claims.records().is_empty(), "no claim was sent");

    // Claiming again sends it.
    node.bridge
        .claim_down
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let (status, claimed) = node.claim("m-1", &me).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(claimed["status"], "network_claim_submitted", "{claimed}");
}

/// The mission's publisher, on its own node, approves this node's agent.
async fn approved_far_away(node: &Node, mission_id: &str) {
    let me = node.state.identity.agent_did.clone();
    let mut gateway = node.gateway.lock().await;
    gateway.set(mission_id, "claimer", json!(me));
    gateway.set(mission_id, "status", json!("claimed"));
}

#[tokio::test]
async fn the_claimer_gives_a_mission_up_with_a_tool_and_its_publisher_is_told() {
    let node = trade_node(&[]).await;
    let me = node.state.identity.agent_did.clone();
    let down = |down: bool| {
        node.bridge
            .topic_down
            .store(down, std::sync::atomic::Ordering::SeqCst);
    };
    let kept_claims = || -> Vec<String> {
        let claims: NetworkMissionClaimRegistry = node
            .state
            .local_db
            .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
            .unwrap();
        claims
            .records()
            .into_iter()
            .map(|claim| claim.mission_id)
            .collect()
    };

    // A mission this agent never asked for: the gateway refuses, and nobody is told.
    node.remote("m-0", None).await;
    let refused = node
        .tool("withdraw_mission_claim", json!({"mission_id": "m-0"}))
        .await;
    assert_eq!(refused["code"], "claim_not_requested", "{refused}");
    assert!(node.bridge.messages.lock().await.is_empty());

    // It holds m-1 and gives it up while wattswarm is down: the gateway has the change, and
    // the answer says the publisher was not told.
    node.remote("m-1", None).await;
    let (status, claimed) = node.claim("m-1", &me).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    approved_far_away(&node, "m-1").await;
    assert_eq!(kept_claims(), ["m-1"]);
    down(true);
    let untold = node
        .tool(
            "withdraw_mission_claim",
            json!({"mission_id": "m-1", "reason": "cannot find the item"}),
        )
        .await;
    assert_eq!(untold["code"], "withdraw_notice_not_delivered", "{untold}");
    assert_eq!(untold["mission_lifecycle_notice"]["sent"], false);
    assert_eq!(node.status("m-1").await, "published");
    let record = node.gateway.lock().await.record("m-1").unwrap();
    assert!(record["claimer"].is_null(), "{record}");
    assert!(record["pledge_deadline"].is_null(), "{record}");
    assert!(node.bridge.messages.lock().await.is_empty());

    // The same request again tells the publisher, on the mission's own topic, signed by this
    // agent; nothing else changes.
    down(false);
    let answer = node
        .tool(
            "withdraw_mission_claim",
            json!({"mission_id": "m-1", "reason": "cannot find the item"}),
        )
        .await;
    assert_eq!(answer["status"], "mission_claim_withdrawn", "{answer}");
    assert_eq!(answer["mission"]["gateway_status"], "published", "{answer}");
    let notice = &answer["mission_lifecycle_notice"];
    assert_eq!(notice["sent"], true, "{answer}");
    assert_eq!(notice["target_agent_id"], "did:key:publisher-far");
    assert_eq!(notice["target_node_id"], "publisher-node");
    let told = node.calls("withdraw_claim").await;
    assert_eq!(told.len(), 3, "{told:?}");
    assert_eq!(told[1]["claimer"], json!(me));
    assert_eq!(told[1]["reason"], "cannot find the item");
    assert_eq!(node.status("m-1").await, "published");
    {
        let messages = node.bridge.messages.lock().await;
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.feed_key, "wattetheria.missions");
        assert_eq!(message.scope_hint, "group:m-1");
        assert_eq!(message.content["kind"], "mission_claim_withdrawn");
        assert_eq!(message.content["mission_id"], "m-1");
        assert_eq!(message.content["claimer_agent_did"], json!(me));
        assert_eq!(message.content["reason"], "cannot find the item");
        assert_eq!(message.content["mission_status"], "published");
        let envelope = message
            .agent_envelope
            .as_ref()
            .expect("signed by the agent");
        assert_eq!(envelope.source_agent_id.as_deref(), Some(me.as_str()));
        assert_eq!(
            envelope.target_agent_id.as_deref(),
            Some("did:key:publisher-far")
        );
    }

    // This node keeps nothing of the claim, so asking again is the gateway's to answer: an
    // agent that gave a mission up does not take it again.
    assert_eq!(kept_claims(), Vec::<String>::new());
    let (status, back) = node.claim("m-1", &me).await;
    assert_eq!(status, StatusCode::CONFLICT, "{back}");
    assert_eq!(back["code"], "gave_up", "{back}");

    // Once something is handed in the gateway refuses, and its answer comes back.
    node.remote("m-2", None).await;
    let (status, claimed) = node.claim("m-2", &me).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    approved_far_away(&node, "m-2").await;
    node.pledged_by_claimer("m-2").await;
    let refused = node
        .tool("withdraw_mission_claim", json!({"mission_id": "m-2"}))
        .await;
    assert_eq!(refused["code"], "handed_in", "{refused}");
    assert_eq!(node.status("m-2").await, "completed");
    assert_eq!(kept_claims(), ["m-2"]);
    assert_eq!(node.bridge.messages.lock().await.len(), 1);
}

#[tokio::test]
async fn the_publisher_rejects_a_claim_and_a_hand_in_with_tools() {
    let seller = Identity::new_random().agent_did;
    let node = trade_node(&[]).await;
    let mission_id = node.published("in_world", 20, None).await;
    node.asked_by(&mission_id, &seller).await;

    // Nothing handed in yet: there is nothing to reject, and the gateway is not asked.
    let refused = node
        .tool("reject_mission_result", json!({"mission_id": mission_id}))
        .await;
    assert_eq!(refused["code"], "nothing_handed_in", "{refused}");
    assert_eq!(node.calls("reject").await, Vec::<Value>::new());

    // Turning the claim down needs the agent it is about.
    let refused = node
        .tool("reject_mission_claim", json!({"mission_id": mission_id}))
        .await;
    assert_eq!(refused["code"], "invalid", "{refused}");
    assert_eq!(node.calls("reject_claim").await, Vec::<Value>::new());

    // The agent that asked is turned down: the mission stays open for others.
    let answer = node
        .tool(
            "reject_mission_claim",
            json!({
                "mission_id": mission_id,
                "agent_did": seller,
                "claimer_node_id": "claimer-node",
                "reason": "someone else is on it",
            }),
        )
        .await;
    assert_eq!(answer["status"], "rejected", "{answer}");
    assert_eq!(answer["agent_did"], json!(seller), "{answer}");
    assert_eq!(answer["claimer_node_id"], "claimer-node", "{answer}");
    let told = node.calls("reject_claim").await;
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0]["claimer"], json!(seller));
    assert_eq!(told[0]["reason"], "someone else is on it");
    assert_eq!(node.status(&mission_id).await, "published");

    // Another agent's claim is approved, and from then on it stands: the gateway refuses to
    // turn it down, and its answer comes back.
    let buyer = Identity::new_random().agent_did;
    node.asked_by(&mission_id, &buyer).await;
    let (status, claimed) = node.claim(&mission_id, &buyer).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let refused = node
        .tool(
            "reject_mission_claim",
            json!({"mission_id": mission_id, "agent_did": buyer}),
        )
        .await;
    assert_eq!(refused["code"], "claimed", "{refused}");
    assert_eq!(node.status(&mission_id).await, "claimed");

    // It hands items in, and the publisher rejects them: the mission ends as rejected.
    node.pledged_by_claimer(&mission_id).await;
    let answer = node
        .tool(
            "reject_mission_result",
            json!({"mission_id": mission_id, "reason": "not what I asked for"}),
        )
        .await;
    assert_eq!(answer["status"], "rejected", "{answer}");
    assert_eq!(answer["agent_did"], json!(buyer), "{answer}");
    assert_eq!(answer["returned"]["gateway_status"], "rejected", "{answer}");
    assert_eq!(node.status(&mission_id).await, "rejected");
    let told = node.calls("reject").await;
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0]["reason"], "not what I asked for");

    // A claim on a mission that has ended is the gateway's to refuse; its answer comes back.
    let refused = node
        .tool(
            "reject_mission_claim",
            json!({"mission_id": mission_id, "agent_did": buyer}),
        )
        .await;
    assert!(refused.get("code").is_some(), "{refused}");
    assert_eq!(node.status(&mission_id).await, "rejected");
}

#[tokio::test]
async fn the_publisher_cancels_a_mission_with_a_tool() {
    let seller = Identity::new_random().agent_did;
    let node = trade_node(&[]).await;

    // An open mission is cancelled, and the gateway is told why.
    let open = node.published("in_world", 20, None).await;
    let answer = node
        .tool(
            "cancel_mission",
            json!({"mission_id": open, "reason": "no longer needed"}),
        )
        .await;
    assert_eq!(answer["gateway_status"], "cancelled", "{answer}");
    assert_eq!(node.status(&open).await, "cancelled");
    let told = node.calls("cancel").await;
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0]["reason"], "no longer needed");

    // Once somebody has taken the mission the gateway refuses, and its answer comes back.
    let claimed = node.published("in_world", 21, None).await;
    node.asked_by(&claimed, &seller).await;
    let (status, taken) = node.claim(&claimed, &seller).await;
    assert_eq!(status, StatusCode::OK, "{taken}");
    let refused = node
        .tool("cancel_mission", json!({"mission_id": claimed}))
        .await;
    assert_eq!(refused["code"], "claimed", "{refused}");
    assert_eq!(node.status(&claimed).await, "claimed");

    // Once something is handed in the gateway refuses, and its answer comes back.
    let handed_in = node.published("in_world", 22, None).await;
    node.asked_by(&handed_in, &seller).await;
    let (status, taken) = node.claim(&handed_in, &seller).await;
    assert_eq!(status, StatusCode::OK, "{taken}");
    node.pledged_by_claimer(&handed_in).await;
    let refused = node
        .tool("cancel_mission", json!({"mission_id": handed_in}))
        .await;
    assert_eq!(refused["code"], "handed_in", "{refused}");
    assert_eq!(node.status(&handed_in).await, "completed");
}

#[tokio::test]
async fn a_missed_deadline_ends_the_mission() {
    let seller = Identity::new_random().agent_did;
    let node = trade_node(&[]).await;

    // The publisher does not settle in time: settling afterwards is refused.
    let late = node.published("in_world", 20, None).await;
    node.asked_by(&late, &seller).await;
    let (status, claimed) = node.claim(&late, &seller).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    node.pledged_by_claimer(&late).await;
    node.gateway
        .lock()
        .await
        .set(&late, "status", json!("cancelled"));
    let (status, refused) = node
        .post(
            &format!("/v1/wattetheria/missions/{late}/settle"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "mission_closed");
    assert_eq!(node.status(&late).await, "cancelled");

    // The claimer never pledges: the mission ends when its time runs out.
    let idle = node.published("in_world", 21, None).await;
    node.asked_by(&idle, &seller).await;
    let (status, claimed) = node.claim(&idle, &seller).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    node.passed(&idle, "pledge_deadline").await;
    assert_eq!(node.status(&idle).await, "cancelled");
    let (status, refused) = node
        .post(
            &format!("/v1/wattetheria/missions/{idle}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "mission_closed");
}

#[tokio::test]
async fn the_mission_guide_comes_from_the_gateway_with_this_agents_standing() {
    let node = trade_node(&[]).await;
    let rules = node.tool("mission_guide", json!({})).await;
    assert_eq!(rules["rules"]["fee_bps"], 500, "{rules}");
    for part in ["market", "in_world", "real_world"] {
        assert!(rules[part][0]["en"].is_string(), "{part}: {rules}");
    }
    assert_eq!(rules["standing"]["can_claim"], true);
    assert_eq!(
        rules["standing"]["agent_did"],
        json!(node.state.identity.agent_did)
    );
    assert!(
        rules["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["tool"] == "pledge_mission_items"),
        "{rules}"
    );
}
