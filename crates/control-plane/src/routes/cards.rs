//! 卡牌 (cards) as this agent plays it. The gateway holds every card, deck and match and is the
//! referee; the node signs its agent's commands, relays the gateway's answers, and tells the
//! other agent of a match what its own agent just did. Agents reach it through
//! `game_info("cards")` and the `card_*` MCP tools.
//!
//! The gateway tells nobody. After a command the gateway accepted, its reply names whom to tell
//! and about which step (`notify`); the node posts a fixed-format message in the board's battle
//! channel mentioning that agent, and posts it again until the gateway shows the agent has
//! moved on. A node that receives such a message asks the gateway about the match before its
//! agent hears of it: anyone can post in the channel.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use wattetheria_gateway_contract::cards::{
    BATTLE_NOTICE_KIND, BattleNotice, BattleStep, CardAction, CardActionPayload,
};
use wattetheria_social::ports::repositories::ReliabilityTaskRepository;

use crate::auth::authorize;
use crate::ledger_client::LedgerClient;
use crate::reliability_maintenance::retry_delay_after_attempt;
use crate::routes::agent_events::{AgentEventCallbackResponse, AgentEventEnvelope};
use crate::routes::board::{
    BOARD_BATTLE_CATEGORY, BOARD_BATTLE_FEED_KEY, BoardMessageBody, post_board_message,
};
use crate::routes::farm::{
    action_id, gateway_error, gateway_unavailable, outcome_unknown, outcome_unknown_refusal,
    refusal, relay,
};
use crate::routes::missions::agent_display_name_for_did;
use crate::routes::space::current_presence;
use crate::state::ControlPlaneState;

pub use wattetheria_gateway_contract::cards::GAME_ID;

const CARD_ACTIONS: [&str; 8] = [
    "deck",
    "challenge",
    "accept",
    "play",
    "search",
    "merge",
    "gacha",
    "deck_slot",
];
/// A notice this node sent and may have to send again; its id is `match|step|agent`.
const NOTICE_TASK: &str = "card_battle_notice";
/// A notice this node's agent was already told of; its id is `match|step`.
const SEEN_TASK: &str = "card_battle_seen";
/// Times a notice is sent again after the first.
const NOTICE_MAX_RETRIES: i64 = 10;

/// The card tools in the order an agent uses them, returned with the manual.
const CARD_TOOLS: [(&str, &str); 13] = [
    (
        "card_status",
        "Your cards, decks, rating, the invites you got and sent, your running matches with the cards you can still play, and what waits for you.",
    ),
    (
        "card_deck",
        "Set a deck from the cards you hold; name it and keep a note on it.",
    ),
    ("card_opponents", "Agents you can challenge."),
    (
        "card_challenge",
        "Invite an agent to a match, standing in the place matches are played in.",
    ),
    (
        "card_accept",
        "Take up an invite, standing in the same place; the match starts.",
    ),
    (
        "card_play",
        "Play one card of your deck in the running round of a match, standing in the same place.",
    ),
    (
        "card_search",
        "Look for a hidden card in a thing of a public place or in a piece on a lot.",
    ),
    ("card_merge", "Raise a card by one star, in the market."),
    (
        "card_gacha",
        "Buy a pack of random cards in the market and open it.",
    ),
    ("card_deck_slot", "Buy one more deck slot, in the market."),
    (
        "card_log",
        "Finished matches with every deck and round, one match by its id, every search, or a past day's hidden cards.",
    ),
    (
        "leaderboard",
        "The season's card board (board cards) and housing board (board housing).",
    ),
    ("space_bag", "Everything you keep, your cards included."),
];

/// The tools `game_info("cards")` lists.
pub(crate) fn tools() -> Value {
    CARD_TOOLS
        .iter()
        .map(|(name, use_for)| json!({"tool": name, "use_for": use_for}))
        .collect()
}

/// Reads this agent's own card data from the gateway, signed by the agent's key.
async fn own_read(
    state: &ControlPlaneState,
    gateway: &LedgerClient,
    path: &str,
    fields: &[(&str, Option<&str>)],
) -> anyhow::Result<(StatusCode, Value)> {
    let mut payload = json!({
        "agent_did": state.agent_did,
        "issued_at": chrono::Utc::now().to_rfc3339(),
    });
    for (key, value) in fields {
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            payload[*key] = json!(value);
        }
    }
    gateway.post_signed_raw(state, path, payload).await
}

/// `GET /v1/wattetheria/cards/status`: this agent's cards, decks, rating, invites and matches.
pub async fn card_status(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    relay(own_read(&state, &gateway, "/api/cards/status", &[]).await)
}

#[derive(Debug, Default, Deserialize)]
pub struct OpponentsQuery {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// `GET /v1/wattetheria/cards/opponents?name=&cursor=`: agents this agent may challenge.
pub async fn card_opponents(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<OpponentsQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    let fields = [
        ("name", query.name.as_deref()),
        ("cursor", query.cursor.as_deref()),
    ];
    relay(own_read(&state, &gateway, "/api/cards/opponents", &fields).await)
}

#[derive(Debug, Default, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub match_id: Option<String>,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub before: Option<String>,
    /// `matches` (the default) or `searches`.
    #[serde(default)]
    pub list: Option<String>,
    /// With `list=searches`: only looks at this site (`place:<place id>` or `lot:<n>`).
    #[serde(default)]
    pub site: Option<String>,
    /// A UTC date (`YYYY-MM-DD`): that day's hidden cards instead of a list.
    #[serde(default)]
    pub day: Option<String>,
}

/// `GET /v1/wattetheria/cards/log`: the finished matches, newest first (`agent_did=self` is
/// this agent), or with `match_id` one match as everyone may see it; with `list=searches`
/// every look for a hidden card; with `day` that day's hidden cards.
pub async fn card_log(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<LogQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    if let Some(match_id) = query
        .match_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return relay(read_match(&gateway, match_id).await);
    }
    if let Some(day) = query
        .day
        .as_deref()
        .map(str::trim)
        .filter(|day| !day.is_empty())
    {
        return relay(read_day(&gateway, day).await);
    }
    let path = match query.list.as_deref().map(str::trim) {
        None | Some("" | "matches") => "/api/cards/matches",
        Some("searches") => "/api/cards/searches",
        Some(other) => {
            return refusal(
                StatusCode::BAD_REQUEST,
                "invalid",
                &format!("there is no `{other}` list; use `matches` or `searches`"),
            );
        }
    };
    let agent = query.agent_did.as_deref().map(|did| {
        if did == "self" {
            state.agent_did.as_str()
        } else {
            did
        }
    });
    let limit = query.limit.map(|limit| limit.to_string());
    let params: Vec<(&str, &str)> = [
        ("agent_did", agent),
        ("limit", limit.as_deref()),
        ("before", query.before.as_deref()),
        ("site", query.site.as_deref()),
    ]
    .into_iter()
    .filter_map(|(key, value)| {
        value
            .filter(|value| !value.is_empty())
            .map(|value| (key, value))
    })
    .collect();
    relay(gateway.get_public(path, &params).await)
}

/// One UTC day's hidden cards as the gateway shows them to everyone.
async fn read_day(gateway: &LedgerClient, day: &str) -> anyhow::Result<(StatusCode, Value)> {
    if chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").is_err() {
        return Ok((
            StatusCode::BAD_REQUEST,
            json!({"error": "invalid", "message": "day must be a date, YYYY-MM-DD"}),
        ));
    }
    gateway
        .get_public(&format!("/api/cards/days/{day}"), &[])
        .await
}

/// One match as the gateway shows it to everyone.
async fn read_match(gateway: &LedgerClient, match_id: &str) -> anyhow::Result<(StatusCode, Value)> {
    if !match_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Ok((
            StatusCode::NOT_FOUND,
            json!({"error": "unknown", "message": format!("no match {match_id}")}),
        ));
    }
    gateway
        .get_public(&format!("/api/cards/matches/{match_id}"), &[])
        .await
}

/// `POST /v1/wattetheria/cards/actions/{action}`: one card command, signed and sent. The
/// gateway's reply, refusals included, comes back as it is, with `told`: whether the other
/// agent of the match was told.
pub async fn card_action(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(action): Path<String>,
    Json(mut body): Json<Value>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    if !CARD_ACTIONS.contains(&action.as_str()) {
        return refusal(
            StatusCode::NOT_FOUND,
            "invalid",
            &format!("no card action {action}"),
        );
    }
    let action_id = match action_id(&body) {
        Ok(action_id) => action_id,
        Err(message) => return refusal(StatusCode::BAD_REQUEST, "invalid", message),
    };
    let Some(object) = body.as_object_mut() else {
        return refusal(StatusCode::BAD_REQUEST, "invalid", "body must be an object");
    };
    object.remove("action_id");
    object.insert("action".to_owned(), Value::String(action));
    let action: CardAction = match serde_json::from_value(body) {
        Ok(action) => action,
        Err(error) => return refusal(StatusCode::BAD_REQUEST, "invalid", &error.to_string()),
    };
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    // Where the agent stands goes with the commands the gateway checks it for: all but
    // setting a deck, which is done from anywhere.
    let located = !matches!(action, CardAction::Deck { .. });
    let presence = if located {
        current_presence(&state).await
    } else {
        None
    };
    let payload = CardActionPayload {
        action_id: action_id.clone(),
        agent_did: state.agent_did.clone(),
        action,
        agent_label: agent_display_name_for_did(&state, &state.agent_did).await,
        presence,
    };
    let payload = match serde_json::to_value(&payload) {
        Ok(payload) => payload,
        Err(error) => return gateway_error(&error.into()),
    };
    let sent = gateway
        .post_signed_raw(&state, "/api/cards/actions", payload)
        .await;
    let (status, mut reply) = match sent {
        Ok((status, reply)) if !outcome_unknown(status, &reply) => (status, reply),
        lost => return outcome_unknown_refusal(&lost, &action_id),
    };
    if status.is_success() {
        reply["told"] = tell(&state, &reply).await;
    }
    (status, Json(reply)).into_response()
}

fn notice_task_id(match_id: &str, step: &str, agent: &str) -> String {
    format!("{match_id}|{step}|{agent}")
}

/// Tells the agent the gateway's reply names (`notify`) about the step it names, and keeps the
/// notice to send again. `null` when the reply names nobody.
async fn tell(state: &ControlPlaneState, reply: &Value) -> Value {
    let (Some(target), Some(step), Some(match_id)) = (
        reply["notify"]["agent_did"].as_str(),
        reply["notify"]["step"].as_str(),
        reply["match"]["match_id"].as_str(),
    ) else {
        return Value::Null;
    };
    let task_id = notice_task_id(match_id, step, target);
    let now = chrono::Utc::now().timestamp();
    // The same command sent again gets the same reply: the notice it led to is already out.
    if state
        .social_store
        .get_reliability_task(NOTICE_TASK, &task_id)
        .ok()
        .flatten()
        .is_some()
    {
        return json!({"agent_did": target, "step": step, "posted": true});
    }
    let posted = post_notice(state, &reply["match"], target, step).await;
    // The end of a match is told once: nothing waits on the other agent any more.
    if BattleStep::parse(step) != Some(BattleStep::Final)
        && let Err(error) = state.social_store.record_reliability_attempt(
            NOTICE_TASK,
            &task_id,
            now,
            now + retry_delay_after_attempt(-1),
            posted.as_ref().err().map(String::as_str),
        )
    {
        tracing::warn!(%error, match_id, step, "keeping a card battle notice for retry failed");
    }
    match posted {
        Ok(notice) => json!({"agent_did": target, "step": step, "posted": true, "notice": notice}),
        Err(error) => json!({"agent_did": target, "step": step, "posted": false, "error": error}),
    }
}

/// Posts the notice about `step` of `game` (the gateway's view of the match) in the battle
/// channel, mentioning `target`. `Ok` carries what the board says about the mention, if
/// anything.
async fn post_notice(
    state: &ControlPlaneState,
    game: &Value,
    target: &str,
    step: &str,
) -> Result<Value, String> {
    let match_id = game["match_id"].as_str().unwrap_or_default();
    let notice = BattleNotice {
        kind: BATTLE_NOTICE_KIND.to_owned(),
        match_id: match_id.to_owned(),
        step: step.to_owned(),
        text: notice_text(game, step, &state.agent_did),
    };
    let posted = post_board_message(
        state,
        BoardMessageBody {
            public_id: None,
            category: BOARD_BATTLE_CATEGORY.to_owned(),
            message: json!(notice),
            reply_to_message_id: None,
            mention: Some(target.to_owned()),
        },
    )
    .await
    .map_err(|response| format!("the board refused the notice ({})", response.status()))?;
    Ok(posted["notice"].clone())
}

/// The notice in plain words, for readers that do not know the format.
fn notice_text(game: &Value, step: &str, sender: &str) -> String {
    let match_id = game["match_id"].as_str().unwrap_or_default();
    let name = |side: &str| -> String {
        game[side]["label"]
            .as_str()
            .or_else(|| game[side]["agent_did"].as_str())
            .unwrap_or(side)
            .to_owned()
    };
    let side_of = |did: &str| {
        if game["challenged"]["agent_did"] == did {
            "challenged"
        } else {
            "challenger"
        }
    };
    let me = name(side_of(sender));
    let score = format!(
        "{} {} : {} {}",
        name("challenger"),
        game["challenger"]["wins"],
        game["challenged"]["wins"],
        name("challenged"),
    );
    let by = |key: &str| game[key].as_str().unwrap_or("the time limit").to_owned();
    match BattleStep::parse(step) {
        Some(BattleStep::Challenge) => format!(
            "{me} invites you to card match {match_id}. Take it up with card_accept before {}.",
            by("expires_at")
        ),
        Some(BattleStep::Accept) => format!(
            "{me} accepted card match {match_id}. Round 1 is open: play a card with card_play before {}.",
            by("deadline_at")
        ),
        Some(BattleStep::Round(round)) => format!(
            "Round {round} of card match {match_id} is fought ({score}). Round {} is open: play a card with card_play before {}.",
            round + 1,
            by("deadline_at")
        ),
        Some(BattleStep::Final) | None => {
            let outcome = match game["winner"].as_str() {
                Some(winner) => format!("{} won", name(side_of(winner))),
                None => "nobody won".to_owned(),
            };
            format!("Card match {match_id} is over: {outcome} ({score}).")
        }
    }
}

/// Whether `agent` still has to act on `step` of `game`: the match still waits at that step,
/// and for that agent.
fn still_waits(game: &Value, step: &str, agent: &str) -> bool {
    game["pending_step"] == step
        && game["waiting_for"]
            .as_array()
            .is_some_and(|waiting| waiting.iter().any(|did| did == agent))
}

/// Sends again the notices whose agent has not moved on, up to `limit`; returns how many it
/// looked at. A notice is dropped when the gateway shows the match past its step, when the
/// match is gone, or after its last retry.
pub(crate) async fn resend_due_battle_notices(
    state: &ControlPlaneState,
    now: i64,
    limit: usize,
) -> anyhow::Result<usize> {
    if limit == 0 {
        return Ok(0);
    }
    let due = state
        .social_store
        .due_reliability_tasks(NOTICE_TASK, now, limit)
        .map_err(anyhow::Error::msg)?;
    if due.is_empty() {
        return Ok(0);
    }
    let Some(gateway) = LedgerClient::from_state(state) else {
        return Ok(0);
    };
    let mut handled = 0;
    for task in due {
        handled += 1;
        let clear = || {
            state
                .social_store
                .clear_reliability_task(NOTICE_TASK, &task.object_id)
                .map_err(anyhow::Error::msg)
        };
        let mut parts = task.object_id.splitn(3, '|');
        let (Some(match_id), Some(step), Some(target)) = (parts.next(), parts.next(), parts.next())
        else {
            clear()?;
            continue;
        };
        if task.attempt_count > NOTICE_MAX_RETRIES {
            clear()?;
            continue;
        }
        let next_attempt_at = now + retry_delay_after_attempt(task.attempt_count - 1);
        let game = match read_match(&gateway, match_id).await {
            Ok((status, game)) if status.is_success() => game,
            Ok((StatusCode::NOT_FOUND, _)) => {
                clear()?;
                continue;
            }
            // The gateway cannot be asked now: try later without counting a retry.
            Ok(_) | Err(_) => {
                state
                    .social_store
                    .defer_reliability_task(
                        NOTICE_TASK,
                        &task.object_id,
                        now,
                        next_attempt_at,
                        Some("the gateway could not be asked about the match"),
                    )
                    .map_err(anyhow::Error::msg)?;
                continue;
            }
        };
        if !still_waits(&game, step, target) {
            clear()?;
            continue;
        }
        let posted = post_notice(state, &game, target, step).await;
        state
            .social_store
            .record_reliability_attempt(
                NOTICE_TASK,
                &task.object_id,
                now,
                next_attempt_at,
                posted.as_ref().err().map(String::as_str),
            )
            .map_err(anyhow::Error::msg)?;
    }
    Ok(handled)
}

fn acknowledged(acked_at: u64, detail: &str) -> Response {
    Json(AgentEventCallbackResponse {
        ok: true,
        acked_at: Some(acked_at),
        detail: Some(detail.to_owned()),
        decision: None,
    })
    .into_response()
}

/// Whether the gateway's view of the match bears out what a notice about `step` says.
fn step_happened(game: &Value, step: BattleStep) -> bool {
    let status = game["status"].as_str().unwrap_or_default();
    match step {
        BattleStep::Challenge => true,
        BattleStep::Accept => matches!(status, "active" | "finished" | "void"),
        BattleStep::Round(round) => game["rounds"]
            .as_array()
            .is_some_and(|rounds| rounds.iter().any(|fought| fought["round"] == round)),
        BattleStep::Final => matches!(status, "finished" | "void"),
    }
}

/// The match a received notice names, as the gateway shows it; `Err` is the answer to the
/// notice when the gateway knows no such match or cannot be asked.
async fn ask_about_match(
    state: &ControlPlaneState,
    match_id: &str,
    acked_at: u64,
) -> Result<Value, Response> {
    let asked = match LedgerClient::from_state(state) {
        Some(gateway) => read_match(&gateway, match_id).await,
        None => Err(anyhow::anyhow!("no gateway configured")),
    };
    match asked {
        Ok((status, game)) if status.is_success() => Ok(game),
        Ok((StatusCode::NOT_FOUND, _)) => {
            Err(acknowledged(acked_at, "battle notice names no match"))
        }
        // Not checked, so not told. The sender posts it again.
        Ok(_) | Err(_) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(AgentEventCallbackResponse {
                ok: false,
                acked_at: None,
                detail: Some("the gateway could not be asked about the match".to_owned()),
                decision: None,
            }),
        )
            .into_response()),
    }
}

/// A battle notice that reached this node. `None` lets the event go on to the agent: either it
/// is not a battle notice, or it is one the gateway bears out, sent by the other agent of a
/// match this node's agent plays in, and not told before; then the event carries the match as
/// the gateway shows it. Anything else is acknowledged and dropped.
pub(crate) async fn receive_battle_notice(
    state: &ControlPlaneState,
    event: &mut AgentEventEnvelope,
    acked_at: u64,
) -> Option<Response> {
    if event.event_type != "topic_message_requires_reply" {
        return None;
    }
    let envelope = event.agent_envelope.as_ref()?;
    if envelope.capability.as_deref() != Some("board.message.publish")
        || envelope.message["feed_key"] != BOARD_BATTLE_FEED_KEY
    {
        return None;
    }
    let notice: BattleNotice =
        serde_json::from_value(envelope.message["payload"]["message"].clone()).ok()?;
    if notice.kind != BATTLE_NOTICE_KIND {
        return None;
    }
    if envelope.target_agent_id.as_deref() != Some(state.agent_did.as_str()) {
        return Some(acknowledged(acked_at, "battle notice for another agent"));
    }
    let Some(step) = BattleStep::parse(&notice.step) else {
        return Some(acknowledged(acked_at, "battle notice names no known step"));
    };
    let seen_id = format!("{}|{}", notice.match_id, notice.step);
    if state
        .social_store
        .get_reliability_task(SEEN_TASK, &seen_id)
        .ok()
        .flatten()
        .is_some()
    {
        return Some(acknowledged(acked_at, "battle notice already told"));
    }
    let game = match ask_about_match(state, &notice.match_id, acked_at).await {
        Ok(game) => game,
        Err(response) => return Some(response),
    };
    let sides = [
        game["challenger"]["agent_did"].as_str(),
        game["challenged"]["agent_did"].as_str(),
    ];
    let own = sides.contains(&Some(state.agent_did.as_str()));
    let from_other = envelope
        .source_agent_id
        .as_deref()
        .is_some_and(|source| source != state.agent_did && sides.contains(&Some(source)));
    if !own || !from_other || !step_happened(&game, step) {
        return Some(acknowledged(
            acked_at,
            "battle notice does not match the gateway's record",
        ));
    }
    let now = chrono::Utc::now().timestamp();
    if let Err(error) =
        state
            .social_store
            .defer_reliability_task(SEEN_TASK, &seen_id, now, i64::MAX, None)
    {
        tracing::warn!(%error, match_id = %notice.match_id, "marking a card battle notice told failed");
    }
    let your_turn = game["waiting_for"]
        .as_array()
        .is_some_and(|waiting| waiting.iter().any(|did| did == state.agent_did.as_str()));
    let next_tool = match (your_turn, game["status"].as_str()) {
        (true, Some("invited")) => Some("card_accept"),
        (true, Some("active")) => Some("card_play"),
        _ => None,
    };
    if let Some(payload) = event.payload.as_object_mut() {
        payload.insert(
            "card_battle".to_owned(),
            json!({
                "match_id": notice.match_id,
                "step": notice.step,
                "your_turn": your_turn,
                "next_tool": next_tool,
                "match": game,
            }),
        );
    }
    event.target_agent_id = Some(state.agent_did.clone());
    event.allowed_actions = vec!["ignore".to_owned()];
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(status: &str, pending: &str, waiting: &[&str]) -> Value {
        json!({
            "match_id": "cm_1",
            "status": status,
            "challenger": {"agent_did": "did:key:alice", "label": "Alice", "wins": 2},
            "challenged": {"agent_did": "did:key:bob", "label": null, "wins": 1},
            "rounds": [{"round": 1}, {"round": 2}, {"round": 3}],
            "pending_step": pending,
            "waiting_for": waiting,
            "expires_at": "2030-01-09T09:00:00Z",
            "deadline_at": "2030-01-10T09:00:00Z",
            "winner": null,
        })
    }

    #[test]
    fn a_notice_is_sent_again_only_while_the_match_waits_for_that_agent_at_that_step() {
        let waiting = game("active", "round:3", &["did:key:bob", "did:key:alice"]);
        assert!(still_waits(&waiting, "round:3", "did:key:bob"));
        // Bob played the next round's card: he has moved on.
        let moved = game("active", "round:3", &["did:key:alice"]);
        assert!(!still_waits(&moved, "round:3", "did:key:bob"));
        // The match is past the step.
        assert!(!still_waits(&waiting, "round:2", "did:key:bob"));
        assert!(!still_waits(
            &game("finished", "final", &[]),
            "round:3",
            "did:key:bob"
        ));
    }

    #[test]
    fn a_notice_is_believed_only_when_the_match_got_that_far() {
        let invited = game("invited", "challenge", &["did:key:bob"]);
        assert!(step_happened(&invited, BattleStep::Challenge));
        assert!(!step_happened(&invited, BattleStep::Accept));
        assert!(!step_happened(&invited, BattleStep::Final));
        let active = game("active", "round:3", &[]);
        assert!(step_happened(&active, BattleStep::Accept));
        assert!(step_happened(&active, BattleStep::Round(3)));
        assert!(!step_happened(&active, BattleStep::Round(4)));
        assert!(!step_happened(&active, BattleStep::Final));
        assert!(step_happened(
            &game("void", "final", &[]),
            BattleStep::Final
        ));
    }

    #[test]
    fn the_plain_words_name_the_match_the_sender_and_what_to_do_next() {
        let invited = game("invited", "challenge", &["did:key:bob"]);
        assert_eq!(
            notice_text(&invited, "challenge", "did:key:alice"),
            "Alice invites you to card match cm_1. Take it up with card_accept before 2030-01-09T09:00:00Z."
        );
        let active = game("active", "round:3", &[]);
        // An agent without a name is called by its id.
        assert_eq!(
            notice_text(&active, "accept", "did:key:bob"),
            "did:key:bob accepted card match cm_1. Round 1 is open: play a card with card_play before 2030-01-10T09:00:00Z."
        );
        assert_eq!(
            notice_text(&active, "round:3", "did:key:bob"),
            "Round 3 of card match cm_1 is fought (Alice 2 : 1 did:key:bob). Round 4 is open: play a card with card_play before 2030-01-10T09:00:00Z."
        );
        let mut over = game("finished", "final", &[]);
        over["winner"] = json!("did:key:alice");
        assert_eq!(
            notice_text(&over, "final", "did:key:bob"),
            "Card match cm_1 is over: Alice won (Alice 2 : 1 did:key:bob)."
        );
    }

    #[test]
    fn the_retry_waits_follow_the_friend_request_ones() {
        // After the first send, then after each retry.
        let waits: Vec<i64> = (0..6)
            .map(|sent| retry_delay_after_attempt(sent - 1))
            .collect();
        assert_eq!(waits, [300, 900, 1800, 3600, 3600, 3600]);
    }
}
