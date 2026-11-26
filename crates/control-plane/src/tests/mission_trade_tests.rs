//! Missions reserved for one agent, claim bans and item trades, against a stand-in for the
//! gateway ledger's claim and trade routes.

use super::*;
use wattetheria_kernel::civilization::missions::MissionStatus;

/// What the stand-in gateway keeps: escrows, the terms each was locked with, who asked for a
/// bounty, every signed command it received, and the network's mission market.
#[derive(Default)]
struct Gateway {
    market: Vec<Value>,
    escrows: BTreeMap<String, Value>,
    trades: BTreeMap<String, Value>,
    asked: Vec<(String, String)>,
    banned: Vec<String>,
    calls: Vec<(String, Value)>,
}

type Shared = Arc<Mutex<Gateway>>;
type GatewayState = axum::extract::State<Shared>;

fn refused(status: StatusCode, code: &str) -> axum::response::Response {
    (status, Json(json!({"error": code, "code": code}))).into_response()
}

#[allow(clippy::too_many_lines)]
fn gateway_router(gateway: Shared) -> Router {
    async fn lock(
        axum::extract::State(gateway): GatewayState,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut gateway = gateway.lock().await;
        let payload = body["payload"].clone();
        let escrow_id = payload["escrow_id"].as_str().unwrap().to_owned();
        gateway.escrows.insert(
            escrow_id.clone(),
            json!({
                "escrow_id": escrow_id,
                "payer_account": payload["payer"],
                "amount": payload["amount"],
                "status": "locked",
            }),
        );
        if payload["terms"].is_object() {
            gateway.trades.insert(
                escrow_id.clone(),
                json!({
                    "escrow_id": escrow_id,
                    "scope": payload["terms"]["scope"],
                    "assignee": payload["terms"]["assignee"],
                    "status": "open",
                    "claimer": null,
                    "held": null,
                }),
            );
        }
        gateway.calls.push(("lock".to_owned(), body));
        Json(json!({"escrow_id": escrow_id, "status": "locked"}))
    }
    async fn escrow(
        axum::extract::State(gateway): GatewayState,
        Path(escrow_id): Path<String>,
    ) -> axum::response::Response {
        match gateway.lock().await.escrows.get(&escrow_id) {
            Some(escrow) => Json(escrow.clone()).into_response(),
            None => refused(StatusCode::NOT_FOUND, "not_found"),
        }
    }
    async fn resolve(
        axum::extract::State(gateway): GatewayState,
        Path((escrow_id, action)): Path<(String, String)>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        let mut gateway = gateway.lock().await;
        let payload = body["payload"].clone();
        gateway.calls.push((action.clone(), body));
        let Some(mut trade) = gateway.trades.get(&escrow_id).cloned() else {
            // A bounty locked without terms: only the ledger's own movements exist for it.
            return match action.as_str() {
                "release" | "refund" => {
                    let status = if action == "release" {
                        "released"
                    } else {
                        "refunded"
                    };
                    if let Some(escrow) = gateway.escrows.get_mut(&escrow_id) {
                        escrow["status"] = json!(status);
                    }
                    Json(json!({"escrow_id": escrow_id, "status": status})).into_response()
                }
                _ => refused(StatusCode::NOT_FOUND, "no_terms"),
            };
        };
        let live = matches!(
            trade["status"].as_str(),
            Some("open" | "claimed" | "delivered" | "pledged")
        );
        if !live {
            return refused(StatusCode::CONFLICT, "trade_closed");
        }
        match action.as_str() {
            "claims" => {
                let claimer = payload["claimer"].as_str().unwrap().to_owned();
                if trade["assignee"]
                    .as_str()
                    .is_some_and(|assignee| assignee != claimer)
                {
                    return refused(StatusCode::FORBIDDEN, "assignee_only");
                }
                if gateway.banned.contains(&claimer) {
                    return refused(StatusCode::FORBIDDEN, "claimer_banned");
                }
                gateway.asked.push((escrow_id.clone(), claimer));
            }
            "approve" => {
                let claimer = payload["claimer"].as_str().unwrap().to_owned();
                if !gateway
                    .asked
                    .contains(&(escrow_id.clone(), claimer.clone()))
                {
                    return refused(StatusCode::CONFLICT, "claim_not_requested");
                }
                trade["claimer"] = json!(claimer);
                trade["status"] = json!("claimed");
            }
            "pledge" => {
                if trade["claimer"] != payload["claimer"] {
                    return refused(StatusCode::FORBIDDEN, "not_the_claimer");
                }
                trade["held"] = json!({
                    "kind": payload["kind"],
                    "item": payload["item"],
                    "units": payload["units"],
                });
                trade["status"] = json!("pledged");
            }
            "deliver" => {
                if trade["status"] == "claimed" {
                    if trade["scope"] == "in_world" && payload["has_result"] != true {
                        return refused(StatusCode::CONFLICT, "delivery_required");
                    }
                    trade["status"] = json!("delivered");
                }
            }
            "reject" => {
                if trade["scope"] != "in_world" {
                    return refused(StatusCode::CONFLICT, "not_in_world");
                }
                if !matches!(trade["status"].as_str(), Some("pledged" | "delivered")) {
                    return refused(StatusCode::CONFLICT, "nothing_handed_in");
                }
                // The hand-in goes back and the bounty stays locked: the mission goes on.
                trade["held"] = Value::Null;
                trade["status"] = json!("claimed");
            }
            "release" => {
                if trade["scope"] == "in_world"
                    && !matches!(trade["status"].as_str(), Some("pledged" | "delivered"))
                {
                    return refused(StatusCode::CONFLICT, "conflict");
                }
                trade["status"] = json!("settled");
                gateway.escrows.get_mut(&escrow_id).unwrap()["status"] = json!("released");
            }
            "refund" => {
                trade["status"] = json!("cancelled");
                gateway.escrows.get_mut(&escrow_id).unwrap()["status"] = json!("refunded");
            }
            _ => return refused(StatusCode::NOT_FOUND, "not_found"),
        }
        gateway.trades.insert(escrow_id, trade.clone());
        Json(trade).into_response()
    }
    async fn approve(
        state: GatewayState,
        Path(escrow_id): Path<String>,
        body: Json<Value>,
    ) -> axum::response::Response {
        resolve(state, Path((escrow_id, "approve".to_owned())), body).await
    }
    async fn trade(
        axum::extract::State(gateway): GatewayState,
        Path(escrow_id): Path<String>,
    ) -> axum::response::Response {
        match gateway.lock().await.trades.get(&escrow_id) {
            Some(trade) => Json(trade.clone()).into_response(),
            None => refused(StatusCode::NOT_FOUND, "no_terms"),
        }
    }
    async fn standing(
        axum::extract::State(gateway): GatewayState,
        Path(agent_did): Path<String>,
    ) -> Json<Value> {
        let banned = gateway.lock().await.banned.contains(&agent_did);
        Json(json!({
            "agent_did": agent_did,
            "can_claim": !banned,
            "banned_until": banned.then_some("2026-12-02T00:00:00Z"),
            "timeouts": if banned { 3 } else { 0 },
            "bans": i32::from(banned),
            "free_timeouts": 2,
        }))
    }
    async fn guide() -> Json<Value> {
        Json(json!({
            "market": [{"en": "The mission market is where agents publish what they want done.", "zh": "任务市场是 agent 发布需求的地方。"}],
            "in_world": [{"en": "An in_world mission cannot be completed empty-handed.", "zh": "in_world 任务不能空手完成。"}],
            "real_world": [{"en": "Items cannot be pledged on a real_world mission.", "zh": "real_world 任务不能押物品。"}],
            "rules": {"fee_bps": 500, "pledge_secs": 21600, "confirm_secs": 86400},
        }))
    }
    async fn market(axum::extract::State(gateway): GatewayState) -> Json<Value> {
        Json(json!(gateway.lock().await.market))
    }
    Router::new()
        .route("/api/missions", get(market))
        .route("/api/ledger/escrows", post(lock))
        .route("/api/ledger/escrows/{escrow_id}", get(escrow))
        .route("/api/ledger/escrows/{escrow_id}/trade", get(trade))
        .route(
            "/api/ledger/escrows/{escrow_id}/claims/approve",
            post(approve),
        )
        .route("/api/ledger/escrows/{escrow_id}/{action}", post(resolve))
        .route("/api/missions/claimers/{agent_did}", get(standing))
        .route("/api/missions/guide", get(guide))
        .with_state(gateway)
}

struct Node {
    app: Router,
    state: crate::state::ControlPlaneState,
    token: String,
    public_id: String,
    gateway: Shared,
    _dir: tempfile::TempDir,
}

/// A node whose gateway is the stand-in above and whose registry knows the agents in `known`
/// (display name, DID).
async fn trade_node(known: &[(&str, &str)]) -> Node {
    let gateway: Shared = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = gateway_router(gateway.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> = Arc::new(MockSwarmBridge {
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
        build_test_state_with_bridge(50, dir, identity, event_log, bridge);
    state.gateway_urls = vec![format!("http://{address}/api/ingest/snapshot")];
    let app = crate::app(state.clone());
    let public_id = bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    Node {
        app,
        state,
        token,
        public_id,
        gateway,
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

    /// What the claimer's own node does before its claim reaches this node: asks the ledger.
    async fn asked_by(&self, mission_id: &str, agent_did: &str) {
        self.gateway
            .lock()
            .await
            .asked
            .push((mission_id.to_owned(), agent_did.to_owned()));
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

    async fn status(&self, mission_id: &str) -> MissionStatus {
        self.state
            .mission_board
            .lock()
            .await
            .get(mission_id)
            .unwrap()
            .status
    }

    async fn set_trade(&self, mission_id: &str, status: &str) {
        let mut gateway = self.gateway.lock().await;
        gateway.trades.get_mut(mission_id).unwrap()["status"] = json!(status);
        gateway.escrows.get_mut(mission_id).unwrap()["status"] = json!("refunded");
    }

    async fn calls(&self, kind: &str) -> Vec<Value> {
        self.gateway
            .lock()
            .await
            .calls
            .iter()
            .filter(|(called, _)| called == kind)
            .map(|(_, body)| body["payload"].clone())
            .collect()
    }
}

#[tokio::test]
async fn a_mission_reserved_for_one_agent_refuses_everyone_else() {
    let momo = Identity::new_random().agent_did;
    let other = Identity::new_random().agent_did;
    let node = trade_node(&[("Momo", &momo)]).await;

    let (status, refused) = node.publish("in_world", 50, Some("Nobody")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{refused}");
    assert_eq!(refused["code"], "assignee_not_found");
    assert_eq!(
        node.calls("lock").await,
        Vec::<Value>::new(),
        "an unknown name publishes nothing and locks no Watt"
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
    let lock = node.calls("lock").await;
    assert_eq!(
        lock[0]["terms"],
        json!({"scope": "in_world", "assignee": momo, "assignee_label": "Momo"})
    );

    // Anyone else is refused before the board changes, even one that asked the ledger.
    node.asked_by(&mission_id, &other).await;
    let (status, refused) = node.claim(&mission_id, &other).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["code"], "assignee_only");
    assert!(
        refused["error"].as_str().unwrap().contains("Momo"),
        "{refused}"
    );
    assert_eq!(node.status(&mission_id).await, MissionStatus::Open);
    assert_eq!(node.calls("approve").await, Vec::<Value>::new());

    // The named agent must have asked the ledger itself.
    let (status, refused) = node.claim(&mission_id, &momo).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "claim_not_requested");
    node.asked_by(&mission_id, &momo).await;
    let (status, claimed) = node.claim(&mission_id, &momo).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(node.status(&mission_id).await, MissionStatus::Claimed);
    assert_eq!(node.calls("approve").await[0]["claimer"], json!(momo));
}

#[tokio::test]
async fn a_plain_bounty_is_locked_without_terms_and_claimed_as_before() {
    let worker = Identity::new_random().agent_did;
    let node = trade_node(&[]).await;
    let mission_id = node.published("real_world", 40, None).await;
    assert!(
        node.calls("lock").await[0].get("terms").is_none(),
        "a real_world bounty open to anyone carries no claim terms"
    );
    let (status, claimed) = node.claim(&mission_id, &worker).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(node.calls("approve").await, Vec::<Value>::new());
}

#[tokio::test]
async fn an_agent_under_a_claim_ban_cannot_claim_any_mission() {
    let worker = Identity::new_random().agent_did;
    let node = trade_node(&[]).await;
    node.gateway.lock().await.banned.push(worker.clone());

    // A mission with no bounty at all: the ban still holds.
    let free = node.published("real_world", 0, None).await;
    let (status, refused) = node.claim(&free, &worker).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["code"], "claimer_banned");
    assert_eq!(refused["details"]["banned_until"], "2026-12-02T00:00:00Z");
    assert_eq!(node.status(&free).await, MissionStatus::Open);

    // This node's own agent, before its claim for another node's mission is sent.
    let me = node.state.identity.agent_did.clone();
    node.gateway.lock().await.banned.push(me.clone());
    let inputs = json!({"scope": "real_world", "reward": {"agent_watt": 0}});
    let refused =
        crate::routes::mission_trade::ask_ledger_for_claim(&node.state, "elsewhere", &me, &inputs)
            .await
            .expect_err("a banned agent's claim is not sent");
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_claim_for_another_nodes_mission_is_checked_before_it_is_sent() {
    let momo = Identity::new_random().agent_did;
    let node = trade_node(&[]).await;
    let me = node.state.identity.agent_did.clone();

    // Reserved for someone else: refused from what the mission announces, the ledger unasked.
    let reserved = json!({
        "scope": "in_world",
        "reward": {"agent_watt": 30},
        "assignee_did": momo,
        "assignee_display_name": "Momo",
    });
    let refused =
        crate::routes::mission_trade::ask_ledger_for_claim(&node.state, "m-1", &me, &reserved)
            .await
            .expect_err("only the named agent may claim");
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert_eq!(node.calls("claims").await, Vec::<Value>::new());

    // Open to anyone: the request is registered with the ledger under this agent's signature.
    let payer = Identity::new_random().agent_did;
    {
        let mut gateway = node.gateway.lock().await;
        gateway.escrows.insert(
            "m-2".to_owned(),
            json!({"escrow_id": "m-2", "payer_account": payer, "amount": 30, "status": "locked"}),
        );
        gateway.trades.insert(
            "m-2".to_owned(),
            json!({"escrow_id": "m-2", "scope": "in_world", "assignee": null, "status": "open"}),
        );
    }
    let open = json!({"scope": "in_world", "reward": {"agent_watt": 30}});
    crate::routes::mission_trade::ask_ledger_for_claim(&node.state, "m-2", &me, &open)
        .await
        .expect("an open mission may be asked for");
    assert_eq!(node.calls("claims").await[0]["claimer"], json!(me));
}

#[tokio::test]
async fn the_claimer_pledges_items_and_the_publisher_settles() {
    let node = trade_node(&[]).await;
    let me = node.state.identity.agent_did.clone();
    let mission_id = node.published("in_world", 60, None).await;

    // One agent on both sides: its node asks the ledger and approves the claim itself.
    let (status, claimed) = node.claim(&mission_id, &me).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(node.calls("claims").await[0]["claimer"], json!(me));

    let (status, pledged) = node
        .post(
            &format!("/v1/wattetheria/missions/{mission_id}/pledge"),
            json!({"kind": "hunt", "item": "soul_gem", "units": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{pledged}");
    assert_eq!(pledged["trade"]["status"], "pledged");
    assert_eq!(pledged["trade"]["held"]["item"], "soul_gem");
    assert_eq!(pledged["completion"]["status"], "completed", "{pledged}");
    assert_eq!(node.status(&mission_id).await, MissionStatus::Completed);
    let board = node
        .state
        .mission_board
        .lock()
        .await
        .get(&mission_id)
        .unwrap();
    assert_eq!(
        board.completion_result,
        Some(json!({"pledged": {"kind": "hunt", "item": "soul_gem", "units": 1}}))
    );

    // The publisher reads the mission before settling: the result and what the system holds.
    let read = node.get_mission(json!({"mission_id": mission_id})).await;
    let mission = &read["missions"][0];
    assert_eq!(read["count"], 1, "{read}");
    assert_eq!(mission["source"], "this_node", "{read}");
    assert_eq!(mission["status"], "completed");
    assert_eq!(mission["completion_result"]["pledged"]["item"], "soul_gem");
    assert_eq!(mission["bounty"]["status"], "pledged", "{read}");
    assert_eq!(mission["bounty"]["held"]["units"], 1);

    let (status, settled) = node
        .post(
            &format!("/v1/wattetheria/missions/{mission_id}/settle"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(node.status(&mission_id).await, MissionStatus::Settled);
    assert_eq!(node.calls("release").await[0]["payee"], json!(me));
}

/// Every mission states the same facts in the same fields, wherever its record came from: `ours`
/// read from this node's board, `far` from the market.
fn assert_the_same_facts_in_the_same_fields(ours: &Value, far: &Value) {
    assert_eq!(ours["scope"], "in_world", "{ours}");
    assert_eq!(ours["domain"], "trade");
    assert_eq!(ours["status"], "open");
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
    // The mission market is read from the gateway the node's config names.
    let gateway_url = node.state.gateway_urls[0].trim_end_matches("/api/ingest/snapshot");
    std::fs::write(
        node.state.data_dir.join("config.json"),
        json!({"gateway_urls": [gateway_url]}).to_string(),
    )
    .unwrap();
    node.gateway.lock().await.market = vec![
        // The same mission as the network sees it, and two of other nodes.
        json!({"id": gem_id, "title": "Want one Soul Gem", "status": "published"}),
        // A market record keeps what its publisher announced one level down, and its time as
        // Unix seconds.
        json!({"id": "m-far", "title": "Soul gem wanted, paying well", "status": "published",
               "source_node_id": "node-far", "created_at": 1_795_000_000_i64,
               "summary": {"scope": "real_world", "domain": "trade",
                           "lat": 40.7128, "lng": -74.006,
                           "city": "New York", "country": "United States"}}),
        json!({"id": "m-water", "title": "Water my field", "terminal_state": "claimed"}),
    ];

    // Any part of the title, whatever the case; a mission on this node is answered from its
    // board, once, with what the ledger records for its bounty.
    let read = node.get_mission(json!({"title": "soul GEM"})).await;
    assert_eq!(read["count"], 2, "{read}");
    assert_eq!(read["has_more"], false);
    let ours = &read["missions"][0];
    assert_eq!(ours["mission_id"], json!(gem_id), "{read}");
    assert_eq!(ours["source"], "this_node");
    assert_eq!(ours["description"], "Buying a soul gem.");
    assert_eq!(ours["bounty"]["status"], "open", "{read}");
    let far = &read["missions"][1];
    assert_eq!(far["mission_id"], "m-far");
    assert_eq!(far["source"], "network");
    assert!(far["claim_route"].is_object(), "{read}");
    assert!(far.get("bounty").is_none(), "no record, no bounty field");

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

    // By id: exactly that one, wherever it lives.
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
    for nothing in [
        json!({"agent_did": me}),
        json!({"agent_did": me, "result": "  "}),
    ] {
        let (status, refused) = node
            .post(
                &format!("/v1/wattetheria/missions/{mission_id}/complete"),
                nothing,
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "delivery_required");
        assert_eq!(node.status(&mission_id).await, MissionStatus::Claimed);
        let trade = node.gateway.lock().await.trades[&mission_id].clone();
        assert_eq!(
            trade["status"], "claimed",
            "the time to deliver keeps running"
        );
    }

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
    assert_eq!(node.status(&mission_id).await, MissionStatus::Completed);
    let trade = node.gateway.lock().await.trades[&mission_id].clone();
    assert_eq!(trade["status"], "delivered");
    let (status, settled) = node
        .post(
            &format!("/v1/wattetheria/missions/{mission_id}/settle"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(node.status(&mission_id).await, MissionStatus::Settled);

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
    assert_eq!(node.status(&outside).await, MissionStatus::Completed);
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
async fn a_hand_in_sent_back_is_handed_in_again_and_the_mission_goes_on() {
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
    {
        let mut gateway = node.gateway.lock().await;
        let trade = gateway.trades.get_mut(&mission_id).unwrap();
        trade["status"] = json!("pledged");
        trade["held"] = json!({"kind": "crop", "item": "coffee", "units": 5});
    }
    let (status, completed) = complete(
        mission_id.clone(),
        seller.clone(),
        json!({"pledged": "coffee"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{completed}");

    // The publisher turns the hand-in down with the decision it always had for a result: the
    // held items go back, the Watt stays locked, and the mission is the claimer's again.
    let answer = decide(&node, &mission_id, &seller, "reject_result").await;
    assert_eq!(answer["status"], "rejected", "{answer}");
    assert_eq!(answer["returned"]["trade"]["status"], "claimed", "{answer}");
    assert!(answer["returned"]["trade"]["held"].is_null(), "{answer}");
    assert_eq!(answer["returned"]["mission"]["status"], "claimed");
    assert!(
        answer["returned"]["mission"]
            .get("completion_result")
            .is_none_or(Value::is_null),
        "the result that was turned down is forgotten: {answer}"
    );
    assert_eq!(node.status(&mission_id).await, MissionStatus::Claimed);
    assert_eq!(
        node.gateway.lock().await.escrows[&mission_id]["status"],
        "locked"
    );

    // The claimer hands in again; what it hands in now replaces what was sent back.
    let second = json!({"note": "the right beans this time"});
    let (status, completed) = complete(mission_id.clone(), seller.clone(), second.clone()).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(node.status(&mission_id).await, MissionStatus::Completed);
    let board = node
        .state
        .mission_board
        .lock()
        .await
        .get(&mission_id)
        .unwrap();
    assert_eq!(board.completion_result, Some(second));
    assert_eq!(
        node.gateway.lock().await.trades[&mission_id]["status"],
        "delivered"
    );

    // Asking for another try sends the hand-in back the same way, as often as it takes.
    let answer = decide(&node, &mission_id, &seller, "request_retry").await;
    assert_eq!(answer["status"], "retry_requested", "{answer}");
    assert_eq!(node.status(&mission_id).await, MissionStatus::Claimed);
    assert_eq!(node.calls("reject").await.len(), 2);
    assert_eq!(node.calls("refund").await, Vec::<Value>::new());

    // Only a deadline ends it: the claimer lets the fresh time run out.
    node.set_trade(&mission_id, "pledge_expired").await;
    let report = crate::reconcile_mission_escrows(&node.state, 3600).await;
    assert_eq!(report.trades_ended, 1, "{report:?}");
    assert_eq!(node.status(&mission_id).await, MissionStatus::Cancelled);

    // Outside the Space a rejection is the answer to the claimer it always was: the ledger is
    // not asked and the mission stays as it was.
    let outside = node.published("real_world", 20, Some("Momo")).await;
    node.asked_by(&outside, &momo).await;
    let (status, claimed) = node.claim(&outside, &momo).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let (status, completed) = complete(
        outside.clone(),
        momo.clone(),
        json!({"note": "report sent"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    let answer = decide(&node, &outside, &momo, "reject_result").await;
    assert_eq!(answer["status"], "rejected", "{answer}");
    assert!(answer.get("returned").is_none(), "{answer}");
    assert_eq!(node.status(&outside).await, MissionStatus::Completed);
    assert_eq!(node.calls("reject").await.len(), 2);
}

#[tokio::test]
async fn the_claimers_node_lets_it_hand_in_again_once_the_ledger_says_it_was_sent_back() {
    use wattetheria_kernel::civilization::missions::{
        NetworkMissionClaimMetadata, NetworkMissionClaimRegistry,
    };
    let node = trade_node(&[]).await;
    let me = node.state.identity.agent_did.clone();
    let claims_domain = wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS;
    let mut claims = NetworkMissionClaimRegistry::default();
    for (mission_id, scope) in [("m-space", "in_world"), ("m-outside", "real_world")] {
        claims.record(
            mission_id,
            mission_id,
            &me,
            &format!("wattetheria:{mission_id}:{me}"),
            Some("completed".to_owned()),
            NetworkMissionClaimMetadata {
                scope: Some(scope.to_owned()),
                ..NetworkMissionClaimMetadata::default()
            },
        );
        node.gateway.lock().await.trades.insert(
            mission_id.to_owned(),
            json!({"escrow_id": mission_id, "scope": scope, "status": "delivered", "claimer": me}),
        );
    }
    node.state
        .local_db
        .save_domain(claims_domain, &claims)
        .unwrap();
    let status_of = |mission_id: &'static str| {
        let registry: NetworkMissionClaimRegistry = node
            .state
            .local_db
            .load_domain_or_default(claims_domain)
            .unwrap();
        registry.get(mission_id, mission_id, &me).unwrap().status
    };

    // Handed in and waiting for its publisher: nothing to hand in again.
    crate::routes::mission_trade::reopen_sent_back_claim(&node.state, "m-space", &me).await;
    assert_eq!(status_of("m-space").as_deref(), Some("completed"));

    // The publisher sent it back: the ledger says the mission is this agent's to hand in.
    for mission_id in ["m-space", "m-outside"] {
        node.gateway
            .lock()
            .await
            .trades
            .get_mut(mission_id)
            .unwrap()["status"] = json!("claimed");
        crate::routes::mission_trade::reopen_sent_back_claim(&node.state, mission_id, &me).await;
    }
    assert_eq!(status_of("m-space").as_deref(), Some("claimed"));
    assert_eq!(
        status_of("m-outside").as_deref(),
        Some("completed"),
        "a mission outside the Space is left as it was"
    );
}

#[tokio::test]
async fn a_missed_deadline_ends_the_mission() {
    let seller = Identity::new_random().agent_did;
    let node = trade_node(&[]).await;

    let pledged = |mission_id: String| {
        let (node, seller) = (&node, seller.clone());
        async move {
            node.asked_by(&mission_id, &seller).await;
            let (status, claimed) = node.claim(&mission_id, &seller).await;
            assert_eq!(status, StatusCode::OK, "{claimed}");
            {
                let mut gateway = node.gateway.lock().await;
                let trade = gateway.trades.get_mut(&mission_id).unwrap();
                trade["status"] = json!("pledged");
                trade["held"] = json!({"kind": "crop", "item": "coffee", "units": 5});
            }
            let (status, completed) = node
                .post(
                    &format!("/v1/wattetheria/missions/{mission_id}/complete"),
                    json!({"agent_did": seller, "result": {"pledged": "coffee"}}),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{completed}");
            mission_id
        }
    };

    // The publisher neither settles nor rejects in time: settling afterwards is refused.
    let late = pledged(node.published("in_world", 20, None).await).await;
    node.set_trade(&late, "confirm_expired").await;
    let (status, refused) = node
        .post(
            &format!("/v1/wattetheria/missions/{late}/settle"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "trade_ended");
    assert_eq!(node.status(&late).await, MissionStatus::Cancelled);
    assert_eq!(node.calls("release").await, Vec::<Value>::new());

    // The claimer never pledges: the sweep ends the mission on the board.
    let idle = node.published("in_world", 20, None).await;
    node.asked_by(&idle, &seller).await;
    let (status, claimed) = node.claim(&idle, &seller).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    node.set_trade(&idle, "pledge_expired").await;
    let report = crate::reconcile_mission_escrows(&node.state, 3600).await;
    assert_eq!(report.trades_ended, 1, "{report:?}");
    assert_eq!(node.status(&idle).await, MissionStatus::Cancelled);
    let report = crate::reconcile_mission_escrows(&node.state, 3600).await;
    assert_eq!(
        report.trades_ended, 0,
        "an ended mission is ended once: {report:?}"
    );
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
