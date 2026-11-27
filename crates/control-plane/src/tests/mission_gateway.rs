//! A stand-in for the gateway's mission API, the only keeper of mission status. Every test
//! node gets one: it keeps the missions the node publishes and takes part in, applies the
//! gateway's rules to each signed command and records every command it received.

use super::*;
use std::collections::BTreeSet;
use std::sync::LazyLock;

const LIVE: [&str; 4] = ["published", "claimed", "running", "completed"];
const PLEDGE_SECS: i64 = 21_600;

/// What the stand-in keeps.
#[derive(Default)]
pub(super) struct MissionGateway {
    /// Records as the gateway sends them, by mission id.
    pub(super) missions: BTreeMap<String, Value>,
    /// Mission ids in publishing order.
    order: Vec<String>,
    /// (publisher, request id) → the id of the mission that publish made.
    requests: BTreeMap<(String, String), String>,
    /// (mission id, claimer) → `requested` | `approved` | `rejected` | `withdrawn`, with the
    /// claimer label.
    claims: BTreeMap<(String, String), (String, Option<String>)>,
    /// Collective participants by mission id.
    participants: BTreeMap<String, Vec<Value>>,
    /// Agents under a claim ban.
    pub(super) banned: Vec<String>,
    /// The publisher's Watt balance, when a test limits it: a larger bounty is refused the
    /// way the ledger refuses it.
    pub(super) balance: Option<i64>,
    /// Every signed command: (action, payload).
    commands: Vec<(String, Value)>,
    /// How many of the next publishes are recorded without their answer reaching the node.
    pub(super) lost_publish_answers: usize,
}

pub(super) type SharedMissionGateway = Arc<Mutex<MissionGateway>>;

impl MissionGateway {
    /// The payloads of the commands of one action (`publish`, `claims`, `approve`,
    /// `reject_claim`, `withdraw_claim`, `pledge`, `deliver`, `reject`, `settle`, `cancel`, `participants`,
    /// `start`, `finalize`), oldest first.
    pub(super) fn commands(&self, action: &str) -> Vec<Value> {
        self.commands
            .iter()
            .filter(|(called, _)| called == action)
            .map(|(_, payload)| payload.clone())
            .collect()
    }

    /// The mission a publish request (publisher, request id) made, as it stands now: the same
    /// request sent again is answered with it, unless it was cancelled, which frees the request
    /// for a new mission.
    fn made_by(&self, request: &(String, String)) -> Option<Value> {
        self.requests
            .get(request)
            .and_then(|mission_id| self.missions.get(mission_id))
            .filter(|mission| mission["status"] != "cancelled")
            .cloned()
    }

    pub(super) fn record(&self, mission_id: &str) -> Option<Value> {
        self.missions.get(mission_id).cloned()
    }

    pub(super) fn status(&self, mission_id: &str) -> Option<String> {
        self.missions
            .get(mission_id)
            .and_then(|record| record["status"].as_str())
            .map(ToOwned::to_owned)
    }

    /// Puts a record in as if another node had published it.
    pub(super) fn seed(&mut self, record: Value) {
        let mission_id = record["mission_id"].as_str().unwrap().to_owned();
        if !self.missions.contains_key(&mission_id) {
            self.order.push(mission_id.clone());
        }
        self.missions.insert(mission_id, record);
    }

    /// Records that `claimer` asked for the mission, as its own node does before the claim
    /// reaches the publisher.
    pub(super) fn ask(&mut self, mission_id: &str, claimer: &str) {
        self.claims.insert(
            (mission_id.to_owned(), claimer.to_owned()),
            ("requested".to_owned(), None),
        );
    }

    /// Changes a record by hand, as the gateway's own clock or another agent would.
    pub(super) fn set(&mut self, mission_id: &str, field: &str, value: Value) {
        self.missions.get_mut(mission_id).unwrap()[field] = value;
    }

    fn expire_due(&mut self, mission_id: &str) {
        let now = Utc::now();
        let Some(record) = self.missions.get_mut(mission_id) else {
            return;
        };
        let past = |field: &str| {
            record[field]
                .as_str()
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                .is_some_and(|at| at < now)
        };
        let due = match record["status"].as_str() {
            Some("published" | "claimed") if past("expires_at") => true,
            Some("claimed") if past("pledge_deadline") => true,
            _ => false,
        };
        if due {
            record["status"] = json!("cancelled");
            record["ended_at"] = json!(now.to_rfc3339());
        }
    }

    fn involves(&self, record: &Value, agent: &str) -> bool {
        let mission_id = record["mission_id"].as_str().unwrap_or_default();
        record["publisher"] == agent
            || record["claimer"] == agent
            || self
                .claims
                .contains_key(&(mission_id.to_owned(), agent.to_owned()))
            || self
                .participants
                .get(mission_id)
                .is_some_and(|joined| joined.iter().any(|taker| taker["agent_did"] == agent))
    }

    fn takers(&self, mission_id: &str) -> Vec<Value> {
        let mut takers = self
            .claims
            .iter()
            .filter(|((id, _), _)| id == mission_id)
            .map(|((_, claimer), (status, label))| {
                json!({"agent_did": claimer, "label": label, "status": status})
            })
            .collect::<Vec<_>>();
        takers.extend(
            self.participants
                .get(mission_id)
                .cloned()
                .unwrap_or_default(),
        );
        takers
    }
}

fn refused(status: StatusCode, code: &str) -> axum::response::Response {
    (status, Json(json!({"error": code, "code": code}))).into_response()
}

fn conflict(code: &str) -> axum::response::Response {
    refused(StatusCode::CONFLICT, code)
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn text(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// A mission id as the gateway makes it; its prefix says which of its tables keeps the mission.
fn new_mission_id(scope: &Value) -> String {
    let prefix = if scope == "in_world" { "iw_" } else { "rw_" };
    format!("{prefix}{}", uuid::Uuid::new_v4())
}

/// The payload of a publish as the gateway keeps it: the id it made is written into the
/// wattswarm route fields and into the task contract, which the node sent without one.
fn named_payload(sent: &Value, mission_id: &str) -> Value {
    let mut payload = if sent.is_object() {
        sent.clone()
    } else {
        json!({})
    };
    let group = json!(format!("group:{mission_id}"));
    let scope = json!({"kind": "group", "id": mission_id});
    payload["task_id"] = json!(mission_id);
    if payload.get("mission_scope_hint").is_some() {
        payload["mission_scope_hint"] = group.clone();
    }
    if payload.get("swarm_scope").is_some() {
        payload["swarm_scope"] = scope.clone();
    }
    if let Some(contract) = payload
        .get_mut("task_contract")
        .filter(|contract| contract.is_object())
    {
        contract["task_id"] = json!(mission_id);
        if let Some(inputs) = contract
            .get_mut("inputs")
            .filter(|inputs| inputs.is_object())
        {
            inputs["mission_id"] = json!(mission_id);
            inputs["mission_scope_hint"] = group;
            inputs["swarm_scope"] = scope;
        }
    }
    payload
}

type GatewayState = axum::extract::State<SharedMissionGateway>;

async fn publish(
    axum::extract::State(gateway): GatewayState,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let payload = body["payload"].clone();
    if payload["action"] != "publish" {
        return refused(StatusCode::BAD_REQUEST, "action_mismatch");
    }
    let mut gateway = gateway.lock().await;
    gateway
        .commands
        .push(("publish".to_owned(), payload.clone()));
    let Some(request_id) = text(&payload["request_id"]) else {
        return refused(StatusCode::BAD_REQUEST, "request_id is required");
    };
    let request = (
        payload["publisher"].as_str().unwrap_or_default().to_owned(),
        request_id.to_owned(),
    );
    if let Some(mut existing) = gateway.made_by(&request) {
        existing["already_published"] = json!(true);
        return Json(existing).into_response();
    }
    let Some(title) = text(&payload["title"]) else {
        return refused(StatusCode::BAD_REQUEST, "title is required");
    };
    let amount = payload["reward"]["amount"].as_i64().unwrap_or(0);
    let kind = payload["reward"]["kind"].as_str().map_or_else(
        || if amount > 0 { "watt" } else { "none" }.to_owned(),
        ToOwned::to_owned,
    );
    match kind.as_str() {
        "none" if amount != 0 => return refused(StatusCode::BAD_REQUEST, "reward_amount"),
        "watt" if amount <= 0 => return refused(StatusCode::BAD_REQUEST, "reward_amount"),
        "none" | "watt" => {}
        _ => return refused(StatusCode::BAD_REQUEST, "reward_kind"),
    }
    if kind == "watt"
        && let Some(balance) = gateway.balance
        && amount > balance
    {
        return (
            StatusCode::PAYMENT_REQUIRED,
            Json(json!({"error": format!("insufficient balance: needs {amount}, has {balance}")})),
        )
            .into_response();
    }
    let mission_kind = payload["kind"].as_str().unwrap_or("standard").to_owned();
    if mission_kind == "collective" {
        if kind != "none" {
            return refused(StatusCode::BAD_REQUEST, "collective_reward");
        }
        if !payload["assignee"].is_null() {
            return refused(StatusCode::BAD_REQUEST, "assignee");
        }
    }
    let at = now();
    let mission_id = new_mission_id(&payload["scope"]);
    let record = json!({
        "mission_id": mission_id,
        "kind": mission_kind,
        "scope": payload["scope"],
        "publisher": payload["publisher"],
        "publisher_label": payload["publisher_label"],
        "title": title,
        "description": payload["description"],
        "domain": payload["domain"],
        "payload": named_payload(&payload["payload"], &mission_id),
        "reward": {
            "kind": kind,
            "amount": amount,
            "reputation": payload["reward"]["reputation"].as_i64().unwrap_or(0),
            "capacity": payload["reward"]["capacity"].as_i64().unwrap_or(0),
        },
        "escrow_id": (kind == "watt").then(|| mission_id.clone()),
        "assignee": payload["assignee"],
        "assignee_label": payload["assignee_label"],
        "organization_id": payload["organization_id"],
        "place": payload["place"],
        "status": "published",
        "claimer": null,
        "claimer_label": null,
        "claimed_at": null,
        "pledge_deadline": null,
        "result": null,
        "delivered_at": null,
        "held": null,
        "expires_at": payload["expires_at"],
        "settled_at": null,
        "ended_at": null,
        "created_at": at,
        "updated_at": at,
    });
    gateway.requests.insert(request, mission_id);
    gateway.seed(record.clone());
    if gateway.lost_publish_answers > 0 {
        gateway.lost_publish_answers -= 1;
        return StatusCode::BAD_GATEWAY.into_response();
    }
    Json(record).into_response()
}

async fn list(
    axum::extract::State(gateway): GatewayState,
    Query(query): Query<BTreeMap<String, String>>,
) -> Json<Value> {
    let mut gateway = gateway.lock().await;
    let ids = gateway.order.clone();
    for id in &ids {
        gateway.expire_due(id);
    }
    let statuses = query
        .get("status")
        .map(|status| {
            status
                .split(',')
                .map(str::trim)
                .filter(|status| !status.is_empty())
                .map(ToOwned::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let limit = query
        .get("limit")
        .and_then(|limit| limit.parse::<usize>().ok())
        .unwrap_or(50);
    let field_matches = |record: &Value, field: &str| {
        query
            .get(field)
            .is_none_or(|wanted| record[field].as_str() == Some(wanted.as_str()))
    };
    let missions = ids
        .iter()
        .rev()
        .filter_map(|id| gateway.missions.get(id))
        .filter(|record| {
            statuses.is_empty()
                || record["status"]
                    .as_str()
                    .is_some_and(|status| statuses.contains(status))
        })
        .filter(|record| {
            ["scope", "kind", "publisher", "claimer", "organization_id"]
                .iter()
                .all(|field| field_matches(record, field))
        })
        .filter(|record| {
            query
                .get("involving")
                .is_none_or(|agent| gateway.involves(record, agent))
        })
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    Json(json!({"missions": missions, "next_before": null}))
}

async fn get_one(
    axum::extract::State(gateway): GatewayState,
    Path(mission_id): Path<String>,
) -> axum::response::Response {
    let mut gateway = gateway.lock().await;
    gateway.expire_due(&mission_id);
    let Some(mut record) = gateway.record(&mission_id) else {
        return refused(StatusCode::NOT_FOUND, "not_found");
    };
    record["takers"] = json!(gateway.takers(&mission_id));
    Json(record).into_response()
}

async fn decide(
    state: GatewayState,
    Path((mission_id, decision)): Path<(String, String)>,
    body: Json<Value>,
) -> axum::response::Response {
    command(
        state,
        Path((mission_id, format!("claims/{decision}"))),
        body,
    )
    .await
}

#[allow(clippy::too_many_lines)]
async fn command(
    axum::extract::State(gateway): GatewayState,
    Path((mission_id, action)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    let payload = body["payload"].clone();
    // The gateway takes a signature only for the action it was given for.
    let signed_for = match action.as_str() {
        "claims" => "claim",
        "claims/approve" => "approve_claim",
        "claims/reject" => "reject_claim",
        "claims/withdraw" => "withdraw_claim",
        other => other,
    };
    if payload["action"] != signed_for {
        return refused(StatusCode::BAD_REQUEST, "action_mismatch");
    }
    let mut gateway = gateway.lock().await;
    let action = match action.as_str() {
        "claims/approve" => "approve".to_owned(),
        "claims/reject" => "reject_claim".to_owned(),
        "claims/withdraw" => "withdraw_claim".to_owned(),
        other => other.to_owned(),
    };
    gateway.commands.push((action.clone(), payload.clone()));
    if payload["mission_id"] != json!(mission_id) {
        return refused(StatusCode::BAD_REQUEST, "mission_id_mismatch");
    }
    gateway.expire_due(&mission_id);
    let Some(mut record) = gateway.record(&mission_id) else {
        return refused(StatusCode::NOT_FOUND, "not_found");
    };
    let status = record["status"].as_str().unwrap_or_default().to_owned();
    let collective = record["kind"] == "collective";
    let in_world = record["scope"] == "in_world";
    let handed_in = status == "completed";
    let by_publisher = payload["publisher"] == record["publisher"];
    // Rejecting a mission already rejected changes nothing.
    if action == "reject" && by_publisher && status == "rejected" {
        return Json(record).into_response();
    }
    if !LIVE.contains(&status.as_str()) {
        return conflict("mission_closed");
    }
    let at = now();
    match action.as_str() {
        "claims" => {
            let claimer = payload["claimer"].as_str().unwrap_or_default().to_owned();
            if record["assignee"]
                .as_str()
                .is_some_and(|assignee| assignee != claimer)
            {
                return refused(StatusCode::FORBIDDEN, "assignee_only");
            }
            if gateway.banned.contains(&claimer) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "error": "claimer_banned",
                        "code": "claimer_banned",
                        "details": {"banned_until": "2026-12-02T00:00:00Z"},
                    })),
                )
                    .into_response();
            }
            if status != "published" {
                if record["claimer"] == json!(claimer) {
                    return Json(record).into_response();
                }
                return conflict("already_claimed");
            }
            let gave_up = gateway
                .claims
                .get(&(mission_id.clone(), claimer.clone()))
                .is_some_and(|(asked, _)| asked == "withdrawn");
            if gave_up {
                return conflict("gave_up");
            }
            gateway.claims.insert(
                (mission_id.clone(), claimer),
                (
                    "requested".to_owned(),
                    payload["claimer_label"].as_str().map(ToOwned::to_owned),
                ),
            );
        }
        "approve" => {
            if !by_publisher {
                return refused(StatusCode::FORBIDDEN, "not_the_publisher");
            }
            let claimer = payload["claimer"].as_str().unwrap_or_default().to_owned();
            if status != "published" {
                if record["claimer"] == json!(claimer) {
                    return Json(record).into_response();
                }
                return conflict("already_claimed");
            }
            let key = (mission_id.clone(), claimer.clone());
            let Some((asked, label)) = gateway.claims.get(&key).cloned() else {
                return conflict("claim_not_requested");
            };
            if asked == "rejected" || asked == "withdrawn" {
                return conflict("claim_not_requested");
            }
            gateway
                .claims
                .insert(key, ("approved".to_owned(), label.clone()));
            record["claimer"] = json!(claimer);
            record["claimer_label"] = json!(label);
            record["claimed_at"] = json!(at);
            record["status"] = json!("claimed");
            if in_world {
                record["pledge_deadline"] =
                    json!((Utc::now() + chrono::Duration::seconds(PLEDGE_SECS)).to_rfc3339());
            }
        }
        "reject_claim" => {
            if !by_publisher {
                return refused(StatusCode::FORBIDDEN, "not_the_publisher");
            }
            let claimer = payload["claimer"].as_str().unwrap_or_default().to_owned();
            if record["claimer"] == json!(claimer) {
                return conflict(if handed_in { "handed_in" } else { "claimed" });
            }
            let key = (mission_id.clone(), claimer);
            let Some((_, label)) = gateway.claims.get(&key).cloned() else {
                return conflict("claim_not_requested");
            };
            gateway.claims.insert(key, ("rejected".to_owned(), label));
        }
        "withdraw_claim" => {
            let claimer = payload["claimer"].as_str().unwrap_or_default().to_owned();
            let holds = record["claimer"] == json!(claimer);
            if holds && handed_in {
                return conflict("handed_in");
            }
            let key = (mission_id.clone(), claimer);
            match gateway.claims.get(&key).cloned() {
                // Given up before: nothing changes.
                Some((asked, _)) if asked == "withdrawn" => return Json(record).into_response(),
                Some((asked, label)) if asked != "rejected" => {
                    gateway.claims.insert(key, ("withdrawn".to_owned(), label));
                }
                _ => return conflict("claim_not_requested"),
            }
            if holds {
                for field in ["claimer", "claimer_label", "claimed_at", "pledge_deadline"] {
                    record[field] = Value::Null;
                }
                record["status"] = json!("published");
            }
        }
        "pledge" => {
            if record["claimer"] != payload["claimer"] {
                return refused(StatusCode::FORBIDDEN, "not_the_claimer");
            }
            if !in_world {
                return conflict("not_in_world");
            }
            if status != "claimed" {
                return conflict("already_handed_in");
            }
            record["held"] = json!({
                "kind": payload["kind"],
                "item": payload["item"],
                "units": payload["units"],
                "fee_bps": 500,
                "pledged_at": at,
                "confirm_deadline": (Utc::now() + chrono::Duration::days(1)).to_rfc3339(),
            });
            record["status"] = json!("completed");
            record["delivered_at"] = json!(at);
        }
        "deliver" => {
            if collective {
                return conflict("collective_mission");
            }
            if payload["signer"] != record["publisher"] && payload["signer"] != record["claimer"] {
                return refused(StatusCode::FORBIDDEN, "not_a_party");
            }
            let result = payload["result"].clone();
            match status.as_str() {
                "claimed" if in_world && result.is_null() => {
                    return conflict("delivery_required");
                }
                "claimed" => {
                    record["result"] = result;
                    record["delivered_at"] = json!(at);
                    record["status"] = json!("completed");
                }
                // A mission is handed in once: handing in again changes nothing.
                "completed" => return Json(record).into_response(),
                _ => return conflict("not_claimed"),
            }
        }
        "reject" => {
            if !by_publisher {
                return refused(StatusCode::FORBIDDEN, "not_the_publisher");
            }
            if !handed_in {
                return conflict("nothing_handed_in");
            }
            // The mission ends as rejected; the reward and held items go back.
            record["status"] = json!("rejected");
            record["ended_at"] = json!(at);
        }
        "settle" => {
            if !by_publisher {
                return refused(StatusCode::FORBIDDEN, "not_the_publisher");
            }
            if collective {
                return conflict("collective_mission");
            }
            if record["claimer"].is_null() {
                return conflict("not_claimed");
            }
            if in_world && !handed_in {
                return conflict("nothing_handed_in");
            }
            record["status"] = json!("settled");
            record["settled_at"] = json!(at);
            record["ended_at"] = json!(at);
        }
        "cancel" => {
            if !by_publisher {
                return refused(StatusCode::FORBIDDEN, "not_the_publisher");
            }
            if handed_in {
                return conflict("handed_in");
            }
            if record["status"] == "claimed" {
                return conflict("claimed");
            }
            record["status"] = json!("cancelled");
            record["ended_at"] = json!(at);
        }
        "participants" => {
            if !collective {
                return conflict("not_collective");
            }
            if status != "published" {
                return conflict("already_started");
            }
            let joined = gateway.participants.entry(mission_id.clone()).or_default();
            if !joined
                .iter()
                .any(|taker| taker["agent_did"] == payload["agent"])
            {
                joined.push(json!({
                    "agent_did": payload["agent"],
                    "label": payload["label"],
                    "detail": payload["detail"],
                    "status": "joined",
                }));
            }
        }
        "start" => {
            if !by_publisher {
                return refused(StatusCode::FORBIDDEN, "not_the_publisher");
            }
            if !collective {
                return conflict("not_collective");
            }
            match status.as_str() {
                "published" => record["status"] = json!("running"),
                "running" => return Json(record).into_response(),
                _ => return conflict("not_open"),
            }
        }
        "finalize" => {
            if !by_publisher {
                return refused(StatusCode::FORBIDDEN, "not_the_publisher");
            }
            if !collective {
                return conflict("not_collective");
            }
            if status != "running" {
                return conflict("not_running");
            }
            record["result"] = payload["result"].clone();
            record["delivered_at"] = json!(at);
            record["status"] = json!("settled");
            record["settled_at"] = json!(at);
            record["ended_at"] = json!(at);
        }
        _ => return refused(StatusCode::NOT_FOUND, "not_found"),
    }
    record["updated_at"] = json!(at);
    gateway.missions.insert(mission_id, record.clone());
    Json(record).into_response()
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

/// What the settled missions of one agent add up to per domain, over every mission kept.
async fn completed(
    axum::extract::State(gateway): GatewayState,
    Path(agent_did): Path<String>,
) -> Json<Value> {
    let gateway = gateway.lock().await;
    let mut totals: BTreeMap<String, [i64; 4]> = BTreeMap::new();
    for record in gateway.missions.values().filter(|record| {
        record["status"] == "settled" && record["claimer"].as_str() == Some(agent_did.as_str())
    }) {
        let Some(domain) = record["domain"].as_str() else {
            continue;
        };
        let reward = &record["reward"];
        let sum = totals.entry(domain.to_owned()).or_default();
        sum[0] += 1;
        if reward["kind"] == "watt" {
            sum[1] += reward["amount"].as_i64().unwrap_or(0);
        }
        sum[2] += reward["reputation"].as_i64().unwrap_or(0);
        sum[3] += reward["capacity"].as_i64().unwrap_or(0);
    }
    let completed = totals
        .into_iter()
        .map(|(domain, [missions, watt, reputation, capacity])| {
            json!({
                "domain": domain, "missions": missions, "watt": watt,
                "reputation": reputation, "capacity": capacity,
            })
        })
        .collect::<Vec<_>>();
    Json(json!({"agent_did": agent_did, "completed": completed}))
}

/// The mission API of the stand-in; a test that needs other gateway routes merges its own.
pub(super) fn mission_gateway_router(gateway: SharedMissionGateway) -> Router {
    Router::new()
        .route("/api/missions", get(list).post(publish))
        .route("/api/missions/claimers/{agent_did}", get(standing))
        .route("/api/missions/completed/{agent_did}", get(completed))
        .route("/api/missions/{mission_id}", get(get_one))
        .route("/api/missions/{mission_id}/claims/{decision}", post(decide))
        .route("/api/missions/{mission_id}/{action}", post(command))
        .with_state(gateway)
}

static GATEWAYS: LazyLock<std::sync::Mutex<BTreeMap<String, SharedMissionGateway>>> =
    LazyLock::new(Default::default);

/// Serves `router` on a fresh local port from the current runtime; `None` outside one.
pub(super) fn serve_gateway(router: Router) -> Option<String> {
    let runtime = tokio::runtime::Handle::try_current().ok()?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    runtime.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(listener, router).await.unwrap();
    });
    Some(format!("http://{address}"))
}

/// Starts a stand-in gateway serving the mission API and `extra` routes; returns its URL.
pub(super) fn spawn_mission_gateway(extra: Router) -> Option<(String, SharedMissionGateway)> {
    let gateway = SharedMissionGateway::default();
    let url = serve_gateway(mission_gateway_router(gateway.clone()).merge(extra))?;
    GATEWAYS
        .lock()
        .unwrap()
        .insert(url.clone(), gateway.clone());
    Some((url, gateway))
}

/// The stand-in gateway a test node sends its mission commands to.
pub(super) fn mission_gateway(state: &ControlPlaneState) -> SharedMissionGateway {
    let url = state.gateway_urls.first().expect("node has a gateway");
    GATEWAYS
        .lock()
        .unwrap()
        .iter()
        .find(|(known, _)| url == *known || url.starts_with(&format!("{known}/")))
        .map(|(_, gateway)| gateway.clone())
        .expect("node's gateway is a mission stand-in")
}

/// Publishes `mission` on the node's gateway the way the node itself does, and returns the
/// record the gateway keeps, which carries the id the gateway made for it.
pub(super) async fn gateway_publish(
    state: &ControlPlaneState,
    mission: &wattetheria_kernel::civilization::missions::CivilMission,
) -> Value {
    crate::routes::gateway_missions::publish(
        state,
        &uuid::Uuid::new_v4().to_string(),
        mission,
        None,
        &crate::routes::gateway_missions::PublishTerms::kind(
            crate::routes::gateway_missions::KIND_STANDARD,
        ),
    )
    .await
    .expect("publish on the stand-in gateway")
}

/// The mission as the node's view shows it, read from its gateway.
pub(super) async fn gateway_mission(
    state: &ControlPlaneState,
    mission_id: &str,
) -> wattetheria_kernel::civilization::missions::CivilMission {
    let record = crate::routes::gateway_missions::fetch(state, mission_id)
        .await
        .expect("read the stand-in gateway")
        .expect("mission on the gateway");
    crate::routes::gateway_missions::civil_mission(&record).expect("readable mission")
}

/// Every mission the node's view holds.
pub(super) async fn gateway_board(
    state: &ControlPlaneState,
) -> wattetheria_kernel::civilization::missions::MissionBoard {
    crate::routes::gateway_missions::board_view(state)
        .await
        .expect("read the stand-in gateway")
}

/// A gateway record of a mission another node published on wattswarm with `contract`: its
/// route fields travel in the record's payload, as the publishing node puts them there.
pub(super) fn remote_record(
    mission_id: &str,
    title: &str,
    publisher_did: &str,
    status: &str,
    contract: Option<&TaskContract>,
) -> Value {
    let inputs = contract.map_or(Value::Null, |contract| contract.inputs.clone());
    let mut payload = json!({
        "civil": {
            "publisher": inputs.get("publisher").cloned().unwrap_or(json!(publisher_did)),
            "publisher_kind": "player",
        },
        "body": inputs.get("payload").cloned().unwrap_or(json!({})),
        "task_id": contract.map_or(mission_id, |contract| contract.task_id.as_str()),
        "task_type": contract.map_or("wattetheria.mission", |contract| contract.task_type.as_str()),
    });
    if let Some(contract) = contract {
        payload["task_contract"] = json!(contract);
        for field in [
            "publisher_wattswarm_node_id",
            "mission_feed_key",
            "mission_scope_hint",
            "swarm_scope",
            "settlement_delegation",
        ] {
            if let Some(value) = inputs.get(field) {
                payload[field] = value.clone();
            }
        }
    }
    let at = now();
    json!({
        "mission_id": mission_id,
        "kind": "standard",
        "scope": inputs.get("scope").cloned().unwrap_or(json!("real_world")),
        "publisher": publisher_did,
        "publisher_label": inputs.get("publisher_display_name"),
        "title": title,
        "description": inputs.get("description").cloned().unwrap_or(json!("")),
        "domain": inputs.get("domain").cloned().unwrap_or(json!("trade")),
        "payload": payload,
        "reward": {"kind": "none", "amount": 0, "reputation": 0, "capacity": 0},
        "escrow_id": null,
        "assignee": null,
        "assignee_label": null,
        "organization_id": null,
        "place": {"city": null, "country": null, "lat": null, "lng": null, "source": null},
        "status": status,
        "claimer": null,
        "claimer_label": null,
        "claimed_at": null,
        "pledge_deadline": null,
        "result": null,
        "delivered_at": null,
        "held": null,
        "expires_at": null,
        "settled_at": null,
        "ended_at": null,
        "created_at": at,
        "updated_at": at,
    })
}
