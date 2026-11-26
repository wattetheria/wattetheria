//! Who may take a mission, and item trades carried by one.
//!
//! The gateway ledger keeps the rules: the agent a bounty is reserved for, the claim bans, the
//! items pledged for a bounty and the deadlines of that exchange. This node asks it before a
//! claim leaves, tells it when the publisher accepts a claimer, signs its agent's pledge and
//! rejection, and ends a mission on the board once the ledger has given every side its own
//! back. A gateway that does not know these routes (`404`) leaves missions as they were.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use wattetheria_kernel::civilization::missions::{
    CivilMission, MissionScope, MissionStatus, NetworkMissionClaimRegistry,
};
use wattetheria_kernel::identity::fingerprint_from_did_key;
use wattetheria_kernel::local_db;

use crate::auth::{authorize, internal_error};
use crate::ledger_client::{EscrowTerms, LedgerClient};
use crate::routes::missions::{agent_display_name_for_did, announce_mission_cancelled};
use crate::state::ControlPlaneState;

/// Ledger states of a bounty's terms in which every side already has its own back.
const RETURNED_STATES: [&str; 4] = ["rejected", "cancelled", "pledge_expired", "confirm_expired"];

/// The mission tools in the order an agent uses them, returned with the guide. Each line says
/// what the tool is for and nothing about what the rules make of it: the rules are the
/// gateway's to state and to change.
const TRADE_TOOLS: [(&str, &str); 7] = [
    (
        "publish_mission",
        "Publish what you want, with the Watt you offer and, to reserve it, the agent's name.",
    ),
    ("claim_mission", "Ask to take a mission."),
    (
        "pledge_mission_items",
        "As the approved claimer, hand the items to the system.",
    ),
    (
        "get_mission",
        "See who holds a mission, what the system holds for it and its deadlines.",
    ),
    (
        "settle_mission",
        "As the publisher, accept what was handed in.",
    ),
    (
        "reject_result",
        "As the publisher, your decision on the result when it reaches you: send it back.",
    ),
    (
        "mission_guide",
        "The mission market, its rules and your own claim standing.",
    ),
];

pub(crate) fn scope_name(scope: &MissionScope) -> &'static str {
    match scope {
        MissionScope::RealWorld => "real_world",
        MissionScope::InWorld => "in_world",
    }
}

fn bounty(mission: &CivilMission) -> i64 {
    mission
        .reward
        .as_ref()
        .map_or(0, |reward| reward.agent_watt)
}

/// Whether the mission's bounty is locked with claim terms: it is `in_world`, or it names the
/// agent that may take it. Other bounties are locked as plain ones, as before.
pub(crate) fn has_claim_terms(mission: &CivilMission) -> bool {
    bounty(mission) > 0
        && (mission.scope == MissionScope::InWorld || mission.assignee_did.is_some())
}

/// Whether the mission cannot be completed empty-handed: an `in_world` mission with a bounty,
/// handed in with pledged items or with a result that shows the work.
pub(crate) fn needs_something_handed_in(mission: &CivilMission) -> bool {
    has_claim_terms(mission) && mission.scope == MissionScope::InWorld
}

/// Whether a completion result says anything: not missing, not blank, not an empty list or
/// object. What it says is for the publisher to judge.
fn shows_work(result: Option<&Value>) -> bool {
    match result {
        None | Some(Value::Null) => false,
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(fields)) => !fields.is_empty(),
        Some(_) => true,
    }
}

/// The claim terms sent with the mission's bounty lock.
pub(crate) fn escrow_terms(mission: &CivilMission) -> Option<EscrowTerms<'_>> {
    has_claim_terms(mission).then(|| EscrowTerms {
        scope: scope_name(&mission.scope),
        assignee: mission.assignee_did.as_deref(),
        assignee_label: mission.assignee_display_name.as_deref(),
    })
}

fn refusal(status: StatusCode, code: &str, message: impl Into<String>, details: Value) -> Response {
    let mut body = json!({"error": message.into(), "code": code});
    if details != Value::Null {
        body["details"] = details;
    }
    (status, Json(body)).into_response()
}

fn ledger_unreachable(what: &str, detail: impl std::fmt::Display) -> Response {
    refusal(
        StatusCode::BAD_GATEWAY,
        "ledger_unavailable",
        format!("{what}: the ledger could not be asked ({detail}); try again"),
        Value::Null,
    )
}

fn no_ledger() -> Response {
    refusal(
        StatusCode::SERVICE_UNAVAILABLE,
        "ledger_unavailable",
        "this needs a gateway ledger and this node has none configured",
        Value::Null,
    )
}

/// The gateway's answer handed on as it is, status included.
fn relay(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// A refusal the ledger's rules gave (`403`, `409`), as opposed to a route it does not have
/// (`404`) or a failure.
fn is_rule_refusal(status: StatusCode) -> bool {
    matches!(status, StatusCode::FORBIDDEN | StatusCode::CONFLICT)
}

/// The agent a publisher named for a mission.
pub(crate) struct Assignee {
    pub(crate) did: String,
    pub(crate) display_name: Option<String>,
}

/// Finds the agent a publisher named, by display name (names are unique in the registry and an
/// agent whose node is offline is found too) or by `did:key`. The DID is what the mission
/// keeps. Refused when nobody carries the name, or when it is the publisher itself.
pub(crate) async fn resolve_assignee(
    state: &ControlPlaneState,
    name: &str,
) -> Result<Assignee, Response> {
    let name = name.trim().trim_start_matches('@').trim();
    if name.is_empty() {
        return Err(refusal(
            StatusCode::BAD_REQUEST,
            "assignee_invalid",
            "assignee must be an agent's display name",
            Value::Null,
        ));
    }
    let assignee = if name.starts_with("did:") {
        if fingerprint_from_did_key(name).is_err() {
            return Err(refusal(
                StatusCode::BAD_REQUEST,
                "assignee_invalid",
                "assignee DID must be a valid agent did:key",
                Value::Null,
            ));
        }
        Assignee {
            did: name.to_owned(),
            display_name: agent_display_name_for_did(state, name).await,
        }
    } else {
        let agents = state
            .swarm_bridge
            .search_agent_display_name(name, None)
            .await
            .map_err(|error| internal_error(&error))?;
        let Some(agent) = agents
            .into_iter()
            .find(|agent| fingerprint_from_did_key(&agent.target_agent_did).is_ok())
        else {
            return Err(refusal(
                StatusCode::NOT_FOUND,
                "assignee_not_found",
                format!(
                    "no agent named {name} was found, so the mission was not published; check the name"
                ),
                json!({"assignee": name}),
            ));
        };
        Assignee {
            did: agent.target_agent_did,
            display_name: Some(agent.display_name.unwrap_or_else(|| name.to_owned())),
        }
    };
    if assignee.did == state.identity.agent_did || assignee.did == state.agent_did {
        return Err(refusal(
            StatusCode::BAD_REQUEST,
            "assignee_is_publisher",
            "a mission cannot be reserved for the agent that publishes it",
            Value::Null,
        ));
    }
    Ok(assignee)
}

fn reserved_refusal(
    assignee: &str,
    assignee_label: Option<&str>,
    claimer_is_you: bool,
) -> Response {
    let reserved_for = assignee_label.unwrap_or(assignee);
    let message = if claimer_is_you {
        format!("this mission is reserved for {reserved_for}; you cannot claim it")
    } else {
        format!("this mission is reserved for {reserved_for}; nobody else can take it")
    };
    refusal(
        StatusCode::FORBIDDEN,
        "assignee_only",
        message,
        json!({"assignee": assignee, "assignee_label": assignee_label}),
    )
}

/// Refuses `claimer` when the ledger records a claim ban on it. A ledger that cannot be asked,
/// or that keeps no such record, lets the claim through.
async fn refuse_banned_claimer(
    ledger: &LedgerClient,
    claimer: &str,
    claimer_is_you: bool,
) -> Result<(), Response> {
    let standing = match ledger.claim_standing(claimer).await {
        Ok((status, standing)) if status.is_success() => standing,
        Ok(_) => return Ok(()),
        Err(error) => {
            tracing::warn!(claimer, "claim standing could not be read: {error:#}");
            return Ok(());
        }
    };
    if standing["can_claim"].as_bool() != Some(false) {
        return Ok(());
    }
    let until = standing["banned_until"].as_str().unwrap_or("a later time");
    let message = if claimer_is_you {
        format!(
            "you may not claim missions until {until}: missions you claimed were left to time out"
        )
    } else {
        format!(
            "{claimer} may not claim missions until {until}: missions it claimed were left to time out"
        )
    };
    Err(refusal(
        StatusCode::FORBIDDEN,
        "claimer_banned",
        message,
        standing,
    ))
}

/// Before this node's agent sends a claim for another node's mission: refuses at once when the
/// mission is reserved for someone else or the agent is under a claim ban, and registers the
/// request with the ledger when the bounty carries claim terms. `inputs` are the mission's
/// task inputs as its publisher announced them.
pub(crate) async fn ask_ledger_for_claim(
    state: &ControlPlaneState,
    mission_id: &str,
    claimer: &str,
    inputs: &Value,
) -> Result<(), Response> {
    let assignee = inputs["assignee_did"]
        .as_str()
        .map(str::trim)
        .filter(|assignee| !assignee.is_empty());
    if let Some(assignee) = assignee
        && assignee != claimer
    {
        return Err(reserved_refusal(
            assignee,
            inputs["assignee_display_name"].as_str(),
            true,
        ));
    }
    let Some(ledger) = LedgerClient::from_state(state) else {
        return Ok(());
    };
    if claimer != state.identity.agent_did {
        // Only this node's own agent can ask the ledger; the publisher's node checks the rest.
        return Ok(());
    }
    let expects_terms = inputs_expect_terms(inputs);
    let label = agent_display_name_for_did(state, claimer).await;
    match ledger
        .request_claim(state, mission_id, label.as_deref())
        .await
    {
        Ok((status, _)) if status.is_success() => Ok(()),
        Ok((status, body)) if is_rule_refusal(status) => Err(relay(status, body)),
        Ok((StatusCode::NOT_FOUND, _)) => refuse_banned_claimer(&ledger, claimer, true).await,
        Ok((status, _)) if expects_terms => Err(ledger_unreachable("claim not sent", status)),
        Err(error) if expects_terms => {
            Err(ledger_unreachable("claim not sent", format!("{error:#}")))
        }
        Ok(_) | Err(_) => Ok(()),
    }
}

/// Before the publisher's board gives a mission to `claimer`: refuses anyone but the named
/// agent and anyone under a claim ban, and records the accepted claimer with the ledger when
/// the bounty carries claim terms, which starts that claimer's time to deliver.
pub(crate) async fn approve_claim_at_ledger(
    state: &ControlPlaneState,
    mission: &CivilMission,
    claimer: &str,
) -> Result<(), Response> {
    let claimer_is_you = claimer == state.identity.agent_did;
    if let Some(assignee) = mission.assignee_did.as_deref()
        && !mission.allows_claimer(claimer)
    {
        return Err(reserved_refusal(
            assignee,
            mission.assignee_display_name.as_deref(),
            claimer_is_you,
        ));
    }
    let Some(ledger) = LedgerClient::from_state(state) else {
        return Ok(());
    };
    let Some(escrow_id) = mission
        .escrow_id
        .as_deref()
        .filter(|_| has_claim_terms(mission))
    else {
        return refuse_banned_claimer(&ledger, claimer, claimer_is_you).await;
    };
    if claimer_is_you {
        let label = agent_display_name_for_did(state, claimer).await;
        match ledger
            .request_claim(state, escrow_id, label.as_deref())
            .await
        {
            Ok((status, body)) if is_rule_refusal(status) => return Err(relay(status, body)),
            Ok(_) => {}
            Err(error) => {
                return Err(ledger_unreachable(
                    "claim not recorded",
                    format!("{error:#}"),
                ));
            }
        }
    }
    match ledger.approve_claim(state, escrow_id, claimer).await {
        Ok((status, _)) if status.is_success() => Ok(()),
        // The bounty was locked without terms (before this node sent them), or the gateway
        // does not keep any: the claim goes on as it always did.
        Ok((StatusCode::NOT_FOUND, _)) => {
            refuse_banned_claimer(&ledger, claimer, claimer_is_you).await
        }
        Ok((status, body)) if is_rule_refusal(status) => Err(relay(status, body)),
        Ok((status, _)) => Err(ledger_unreachable("claim not recorded", status)),
        Err(error) => Err(ledger_unreachable(
            "claim not recorded",
            format!("{error:#}"),
        )),
    }
}

/// Tells the ledger the mission's work was handed in, with `result` as what shows it. The
/// ledger's own refusals come back to the agent as they are: a bounty it already closed, and an
/// `in_world` mission completed empty-handed (no items pledged, no result). With `strict`, a
/// ledger that cannot be reached fails the call so the agent retries; otherwise it is logged.
pub(crate) async fn report_delivery(
    state: &ControlPlaneState,
    mission_id: &str,
    result: Option<&Value>,
    strict: bool,
) -> Result<(), Response> {
    let Some(ledger) = LedgerClient::from_state(state) else {
        return Ok(());
    };
    match ledger
        .report_delivery(state, mission_id, shows_work(result))
        .await
    {
        Ok((status, _)) if status.is_success() || status == StatusCode::NOT_FOUND => Ok(()),
        Ok((status, body)) if is_rule_refusal(status) => {
            if matches!(
                body["code"].as_str(),
                Some("trade_closed" | "delivery_required")
            ) {
                return Err(relay(status, body));
            }
            // Not recorded as this bounty's claimer, or nobody is: a mission from before the
            // terms existed. Its completion goes on as it always did.
            Ok(())
        }
        Ok((status, _)) if strict => Err(ledger_unreachable("completion not sent", status)),
        Err(error) if strict => Err(ledger_unreachable(
            "completion not sent",
            format!("{error:#}"),
        )),
        Ok((status, _)) => {
            tracing::warn!(mission_id, %status, "mission delivery not reported to the ledger");
            Ok(())
        }
        Err(error) => {
            tracing::warn!(
                mission_id,
                "mission delivery not reported to the ledger: {error:#}"
            );
            Ok(())
        }
    }
}

/// Whether the task inputs of another node's mission say its bounty carries claim terms.
pub(crate) fn inputs_expect_terms(inputs: &Value) -> bool {
    inputs["reward"]["agent_watt"].as_i64().unwrap_or(0) > 0
        && (inputs["scope"].as_str() == Some("in_world")
            || inputs["assignee_did"]
                .as_str()
                .is_some_and(|assignee| !assignee.trim().is_empty()))
}

/// Ends the mission on this node's board when the ledger has already given every side its own
/// back (the publisher turned the items down, or a deadline of the trade passed), and announces
/// the end. Returns the ledger state that ended it, `None` when the mission goes on.
pub(crate) async fn end_returned_mission(
    state: &ControlPlaneState,
    mission_id: &str,
) -> Option<String> {
    let mission = state.mission_board.lock().await.get(mission_id)?;
    if !matches!(
        mission.status,
        MissionStatus::Claimed | MissionStatus::Completed
    ) || !has_claim_terms(&mission)
    {
        return None;
    }
    let escrow_id = mission.escrow_id.as_deref()?;
    let ledger = LedgerClient::from_state(state)?;
    let trade = match ledger.trade(escrow_id).await {
        Ok((status, trade)) if status.is_success() => trade,
        Ok(_) => return None,
        Err(error) => {
            tracing::warn!(mission_id, "mission trade could not be read: {error:#}");
            return None;
        }
    };
    let ended = trade["status"]
        .as_str()
        .filter(|status| RETURNED_STATES.contains(status))?
        .to_owned();
    close_on_board(state, mission_id, &ended).await;
    Some(ended)
}

async fn close_on_board(state: &ControlPlaneState, mission_id: &str, ended: &str) {
    let closed = {
        let mut board = state.mission_board.lock().await;
        let Ok((mission, closed)) = board.close_returned(mission_id) else {
            return;
        };
        if let Err(error) = state
            .local_db
            .save_domain(local_db::domain::MISSION_BOARD, &*board)
        {
            tracing::warn!(
                mission_id,
                "mission board not saved after a trade ended: {error:#}"
            );
        }
        closed.then_some(mission)
    };
    if let Some(mission) = closed {
        tracing::info!(
            mission_id,
            ended,
            "mission ended: the ledger returned its bounty"
        );
        announce_mission_cancelled(state, &mission, Some(format!("trade_{ended}")), None).await;
    }
}

/// The refusal for an action on a mission whose trade the ledger already ended.
pub(crate) fn trade_ended_response(mission_id: &str, ended: &str) -> Response {
    refusal(
        StatusCode::CONFLICT,
        "trade_ended",
        format!(
            "mission {mission_id} is over ({ended}): each side has its own back; publish again to trade again"
        ),
        json!({"mission_id": mission_id, "trade_status": ended}),
    )
}

/// Ends, on this node's board, every claimed or completed mission whose bounty the ledger
/// already returned. Returns how many ended.
pub(crate) async fn end_returned_missions(state: &ControlPlaneState) -> usize {
    let candidates: Vec<String> = {
        let board = state.mission_board.lock().await;
        board
            .list(None)
            .into_iter()
            .filter(|mission| {
                matches!(
                    mission.status,
                    MissionStatus::Claimed | MissionStatus::Completed
                ) && mission.escrow_id.is_some()
                    && has_claim_terms(mission)
            })
            .map(|mission| mission.mission_id)
            .collect()
    };
    let mut ended = 0;
    for mission_id in candidates {
        if end_returned_mission(state, &mission_id).await.is_some() {
            ended += 1;
        }
    }
    ended
}

fn body_string(body: &Value, field: &str) -> Option<String> {
    body[field]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

async fn response_json(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// `POST /v1/wattetheria/missions/{mission_id}/pledge`: this node's agent, the mission's
/// approved claimer, hands `units` of `item` from its `kind` bag to the ledger, then reports
/// the mission completed with what it pledged. The ledger's refusal comes back as it is.
pub(crate) async fn mission_pledge_by_id(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(mission_id): Path<String>,
    Json(mut body): Json<Value>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let (Some(kind), Some(item), Some(units)) = (
        body_string(&body, "kind"),
        body_string(&body, "item"),
        body["units"].as_i64(),
    ) else {
        return refusal(
            StatusCode::BAD_REQUEST,
            "invalid",
            "pledge needs kind, item and units",
            Value::Null,
        );
    };
    let Some(ledger) = LedgerClient::from_state(&state) else {
        return no_ledger();
    };
    let trade = match ledger
        .pledge_items(&state, &mission_id, &kind, &item, units)
        .await
    {
        Ok((status, trade)) if status.is_success() => trade,
        Ok((status, body)) => return relay(status, body),
        Err(error) => return ledger_unreachable("items not pledged", format!("{error:#}")),
    };
    // The ledger took the pledge, so the mission was this agent's to hand in: after a hand-in
    // its publisher sent back, the claim record on this node says so again.
    let pledger =
        body_string(&body, "agent_did").unwrap_or_else(|| state.identity.agent_did.clone());
    reopen_claim_record(&state, &mission_id, &pledger);
    // The pledge is the delivery: report the mission completed so its publisher can look at
    // what is held and settle or reject.
    if let Some(object) = body.as_object_mut() {
        for field in ["kind", "item", "units"] {
            object.remove(field);
        }
        object
            .entry("agent_did")
            .or_insert_with(|| json!(state.identity.agent_did));
        object
            .entry("result")
            .or_insert_with(|| json!({"pledged": {"kind": kind, "item": item, "units": units}}));
    }
    let completion = crate::routes::missions::mission_complete_by_id(
        State(state),
        headers,
        Path(mission_id.clone()),
        Json(body),
    )
    .await;
    let (status, completion) = response_json(completion).await;
    if status.is_success() {
        return Json(json!({"mission_id": mission_id, "trade": trade, "completion": completion}))
            .into_response();
    }
    Json(json!({
        "mission_id": mission_id,
        "trade": trade,
        "completion_error": completion,
        "notice": "The items are held by the system, but the mission was not reported completed. Call complete_mission for this mission so its publisher is told.",
    }))
    .into_response()
}

/// What the publisher sending a hand-in back (`reject_result`, `request_retry`) does to an
/// `in_world` mission with a bounty: the ledger returns held items to the claimer and gives
/// the claimer a fresh time to hand in again, the Watt stays locked, and on this node's board
/// the mission is the claimer's again with the turned-down result forgotten. The mission does
/// not end: only a deadline of the ledger ends it. Returns what the ledger answered, or `None`
/// for a mission of any other kind (another scope, no bounty, not on this board) and for one
/// the ledger has nothing handed in for: the decision is then only the answer to the claimer
/// it always was.
pub(crate) async fn send_back_hand_in(
    state: &ControlPlaneState,
    mission_id: &str,
) -> Result<Option<Value>, Response> {
    let Some(mission) = state.mission_board.lock().await.get(mission_id) else {
        return Ok(None);
    };
    if !needs_something_handed_in(&mission) {
        return Ok(None);
    }
    let Some(ledger) = LedgerClient::from_state(state) else {
        return Ok(None);
    };
    let escrow_id = mission.escrow_id.as_deref().unwrap_or(mission_id);
    let trade = match ledger.reject_pledge(state, escrow_id).await {
        Ok((status, trade)) if status.is_success() => trade,
        Ok((status, _)) if status == StatusCode::NOT_FOUND || is_rule_refusal(status) => {
            // Nothing is handed in at the ledger, or a deadline ended the mission first; the
            // board follows whatever the ledger says.
            end_returned_mission(state, mission_id).await;
            return Ok(None);
        }
        Ok((status, _)) => return Err(ledger_unreachable("hand-in not sent back", status)),
        Err(error) => {
            return Err(ledger_unreachable(
                "hand-in not sent back",
                format!("{error:#}"),
            ));
        }
    };
    let reopened = {
        let mut board = state.mission_board.lock().await;
        let reopened = board.reopen_sent_back(mission_id);
        if matches!(reopened, Ok((_, true))) {
            let _ = state
                .local_db
                .save_domain(local_db::domain::MISSION_BOARD, &*board);
        }
        reopened
    };
    let mission = reopened.ok().map(|(mission, _)| mission);
    Ok(Some(json!({"trade": trade, "mission": mission})))
}

/// On the claimer's node: puts this agent's record of an `in_world` mission it handed in back
/// from `completed` to `claimed`, so it can hand in again. Records of any other scope, and
/// records in any other state, are left as they are.
fn reopen_claim_record(state: &ControlPlaneState, mission_id: &str, agent_did: &str) {
    let Ok(mut registry) = state
        .local_db
        .load_domain_or_default::<NetworkMissionClaimRegistry>(
            local_db::domain::NETWORK_MISSION_CLAIMS,
        )
    else {
        return;
    };
    let Some(record) = registry.records().into_iter().find(|record| {
        record.mission_id == mission_id
            && record.agent_did == agent_did
            && record.status.as_deref() == Some("completed")
            && record.metadata.scope.as_deref() == Some("in_world")
    }) else {
        return;
    };
    if registry
        .update_status(mission_id, &record.task_id, agent_did, "claimed")
        .is_some()
        && let Err(error) = state
            .local_db
            .save_domain(local_db::domain::NETWORK_MISSION_CLAIMS, &registry)
    {
        tracing::warn!(mission_id, "mission claim record not reopened: {error:#}");
    }
}

/// On the claimer's node, before a hand-in: when the ledger says an `in_world` mission this
/// agent already handed in is back with it (its publisher sent the hand-in back), the claim
/// record is reopened. The ledger is asked, so a lost notice from the publisher does not leave
/// the claimer unable to hand in again.
pub(crate) async fn reopen_sent_back_claim(
    state: &ControlPlaneState,
    mission_id: &str,
    agent_did: &str,
) {
    let handed_in_here = state
        .local_db
        .load_domain_or_default::<NetworkMissionClaimRegistry>(
            local_db::domain::NETWORK_MISSION_CLAIMS,
        )
        .is_ok_and(|registry| {
            registry.records().iter().any(|record| {
                record.mission_id == mission_id
                    && record.agent_did == agent_did
                    && record.status.as_deref() == Some("completed")
                    && record.metadata.scope.as_deref() == Some("in_world")
            })
        });
    if !handed_in_here {
        return;
    }
    let Some(ledger) = LedgerClient::from_state(state) else {
        return;
    };
    match ledger.trade(mission_id).await {
        Ok((status, trade))
            if status.is_success()
                && trade["status"].as_str() == Some("claimed")
                && trade["claimer"].as_str() == Some(agent_did) =>
        {
            reopen_claim_record(state, mission_id, agent_did);
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(mission_id, "mission trade could not be read: {error:#}");
        }
    }
}

/// `GET /v1/wattetheria/missions/guide`: the gateway's guide to the mission market (what it is,
/// the rules of each scope, the numbers in force), with this agent's own claim standing and
/// the mission tools in the order an agent uses them.
pub(crate) async fn mission_guide(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(ledger) = LedgerClient::from_state(&state) else {
        return no_ledger();
    };
    let (status, mut body) = match ledger.get_public("/api/missions/guide", &[]).await {
        Ok(reply) => reply,
        Err(error) => return ledger_unreachable("mission guide not read", format!("{error:#}")),
    };
    if status.is_success() {
        if let Ok((standing_status, standing)) =
            ledger.claim_standing(&state.identity.agent_did).await
            && standing_status.is_success()
        {
            body["standing"] = standing;
        }
        body["tools"] = TRADE_TOOLS
            .iter()
            .map(|(name, use_for)| json!({"tool": name, "use_for": use_for}))
            .collect();
    }
    relay(status, body)
}
