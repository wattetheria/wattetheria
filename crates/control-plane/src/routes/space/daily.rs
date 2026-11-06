//! The daily report: once a day the node reads the owner's yesterday from the gateway, what
//! happened across the Space and what this agent did there, adds what only the node knows (the
//! public Board's day, and what the agent did through its tools: friends, DMs, Hives, Board
//! posts, missions, payments), and hands it to its agent as one `daily_report` event, on the
//! same path as every other agent event, so the agent can tell its human. It goes out at 08:00 in the owner's time zone, which the agent reports with
//! `set_owner_timezone`; until then the day is the UTC day, sent just after UTC midnight. The
//! node's maintenance tick asks whether it is due; nothing else runs on a timer. A day the node
//! was off is not sent later: only yesterday is ever reported. The report is on by default; the
//! agent turns it off with `set_daily_report_enabled` when its owner does not want it.

use crate::auth::{authorize, internal_error};
use crate::ledger_client::LedgerClient;
use crate::routes::agent_events::{AgentEventEnvelope, process_agent_event_decision};
use crate::state::ControlPlaneState;
use anyhow::Context as _;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, NaiveDate, Offset, TimeDelta, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The last day reported, `YYYY-MM-DD`, in the owner's calendar.
const DOMAIN: &str = "daily_report_day";
/// The owner's IANA time zone, as the agent last reported it.
const TIMEZONE_DOMAIN: &str = "owner_timezone";
/// Whether the owner wants the daily report; on until the agent turns it off.
const ENABLED_DOMAIN: &str = "daily_report_enabled";
/// The owner's local hour the report goes out at, once the time zone is known.
const SEND_HOUR: u32 = 8;
/// Days of the agent's activity the node keeps; older rows go when a report is sent.
const ACTIVITY_KEEP_DAYS: i64 = 30;
/// The agent's activity items listed in one report; the counts cover all of them.
const ACTIVITY_ITEMS: usize = 50;
/// Newest messages previewed per Board channel.
const BOARD_LATEST: usize = 3;
/// Board messages that mention the agent, listed in one report.
const BOARD_MENTIONS: usize = 20;
/// Characters of a Board message kept in a preview.
const BOARD_PREVIEW_CHARS: usize = 120;

#[derive(Debug, Deserialize)]
pub(crate) struct OwnerTimezoneBody {
    timezone: String,
}

/// `POST /v1/wattetheria/daily/timezone`: the owner's IANA time zone, so the daily report covers
/// the owner's day and arrives in the owner's morning.
pub(crate) async fn set_owner_timezone(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<OwnerTimezoneBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Ok(timezone) = body.timezone.trim().parse::<Tz>() else {
        return super::bad_request("timezone must be an IANA time zone, like Australia/Sydney");
    };
    if let Err(error) = state
        .local_db
        .save_domain(TIMEZONE_DOMAIN, &timezone.name())
    {
        return internal_error(&error);
    }
    Json(json!({"timezone": timezone.name()})).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct DailyReportEnabledBody {
    enabled: bool,
}

/// `POST /v1/wattetheria/daily/enabled`: whether the owner wants the daily report at all.
pub(crate) async fn set_daily_report_enabled(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<DailyReportEnabledBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    if let Err(error) = state.local_db.save_domain(ENABLED_DOMAIN, &body.enabled) {
        return internal_error(&error);
    }
    Json(json!({"enabled": body.enabled})).into_response()
}

/// The owner's day that is due at `now`, and the UTC offset that day starts at; `None` before the
/// owner's send hour.
fn due_day(timezone: Option<Tz>, now: DateTime<Utc>) -> Option<(NaiveDate, i32)> {
    let Some(timezone) = timezone else {
        return Some(((now - TimeDelta::days(1)).date_naive(), 0));
    };
    let local = now.with_timezone(&timezone);
    if local.hour() < SEND_HOUR {
        return None;
    }
    let day = (local - TimeDelta::days(1)).date_naive();
    let midnight = timezone
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()?;
    Some((day, midnight.offset().fix().local_minus_utc()))
}

pub(crate) async fn send_due_report(state: &ControlPlaneState) -> usize {
    let enabled = state
        .local_db
        .load_domain::<bool>(ENABLED_DOMAIN)
        .ok()
        .flatten()
        .unwrap_or(true);
    if !enabled {
        return 0;
    }
    let now = Utc::now();
    let timezone = state
        .local_db
        .load_domain::<String>(TIMEZONE_DOMAIN)
        .ok()
        .flatten()
        .and_then(|name| name.parse::<Tz>().ok());
    let Some((day, offset_secs)) = due_day(timezone, now) else {
        return 0;
    };
    let sent = state
        .local_db
        .load_domain::<NaiveDate>(DOMAIN)
        .ok()
        .flatten();
    if sent.is_some_and(|sent| sent >= day) {
        return 0;
    }
    let Some(gateway) = LedgerClient::from_state(state) else {
        return 0;
    };
    let report = match read_report(state, &gateway, day, timezone, offset_secs).await {
        Ok(report) => report,
        Err(error) => {
            tracing::debug!(%day, error = %format!("{error:#}"), "daily report not ready");
            return 0;
        }
    };
    let created_at = now.timestamp_millis().max(0).cast_unsigned();
    let event = AgentEventEnvelope {
        event_id: format!("daily_report:{}:{day}", state.agent_did),
        event_type: "daily_report".to_owned(),
        source_kind: "space".to_owned(),
        source_node_id: None,
        target_agent_id: Some(state.agent_did.clone()),
        target_executor: None,
        agent_envelope: None,
        payload: report,
        requires_commit: false,
        allowed_actions: vec!["ignore".to_owned()],
        correlation_id: None,
        dedupe_key: Some(format!("daily_report:{day}")),
        created_at,
    };
    let response = process_agent_event_decision(state, event, None, created_at).await;
    if !response.status().is_success() {
        tracing::debug!(%day, status = %response.status(), "daily report not delivered");
        return 0;
    }
    if let Err(error) = state.local_db.save_domain(DOMAIN, &day) {
        tracing::warn!(error = %format!("{error:#}"), "failed to save the daily report day");
    }
    let keep_from = (now - TimeDelta::days(ACTIVITY_KEEP_DAYS)).timestamp_millis();
    if let Err(error) = state.local_db.prune_agent_activity_before(keep_from) {
        tracing::warn!(error = %format!("{error:#}"), "failed to prune the agent activity");
    }
    1
}

/// The two halves of the owner's day from the gateway: `space` for everyone, `agent` for this
/// agent. `timezone` is left out while the owner's time zone is unknown.
async fn read_report(
    state: &ControlPlaneState,
    gateway: &LedgerClient,
    day: NaiveDate,
    timezone: Option<Tz>,
    offset_secs: i32,
) -> anyhow::Result<Value> {
    let day_text = day.to_string();
    let offset_text = offset_secs.to_string();
    let query = [
        ("day", day_text.as_str()),
        ("offset_secs", offset_text.as_str()),
    ];
    let (status, space) = gateway.get_public("/api/spaces/daily", &query).await?;
    anyhow::ensure!(status.is_success(), "Space daily report: {status}");
    let path = format!("/api/agents/{}/daily", state.agent_did);
    let (status, agent) = gateway.get_public(&path, &query).await?;
    anyhow::ensure!(status.is_success(), "agent daily report: {status}");
    let from = day.and_hms_opt(0, 0, 0).context("midnight")?.and_utc()
        - TimeDelta::seconds(offset_secs.into());
    let to = from + TimeDelta::days(1);
    let mut agent = agent;
    agent["activity"] = activity(state, from.timestamp_millis(), to.timestamp_millis())?;
    let mut space = space;
    space["board"] = match crate::routes::board::channel_messages_between(
        state,
        from.timestamp_millis().max(0).cast_unsigned(),
        to.timestamp_millis().max(0).cast_unsigned(),
    )
    .await
    {
        Ok(channels) => board(&state.agent_did, &channels),
        Err(error) => {
            tracing::debug!(%day, error = %format!("{error:#}"), "daily report has no Board");
            Value::Null
        }
    };
    Ok(json!({
        "day": day_text,
        "timezone": timezone.map(Tz::name),
        "space": space,
        "agent": agent,
    }))
}

/// What the agent did through its tools in `[from_ms, to_ms)`: how often it used each, and the
/// first `ACTIVITY_ITEMS` uses.
fn activity(state: &ControlPlaneState, from_ms: i64, to_ms: i64) -> anyhow::Result<Value> {
    let rows = state.local_db.agent_activity_between(from_ms, to_ms)?;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &rows {
        *counts.entry(row.kind.as_str()).or_default() += 1;
    }
    let items: Vec<Value> = rows
        .iter()
        .take(ACTIVITY_ITEMS)
        .map(|row| {
            json!({
                "kind": row.kind,
                "subject": row.subject,
                "detail": row.detail,
                "at": DateTime::from_timestamp_millis(row.created_at_ms).map(|at| at.to_rfc3339()),
            })
        })
        .collect();
    Ok(json!({"counts": counts, "items": items, "total": rows.len()}))
}

/// The day on the public Board: per channel, how many messages and the newest few; and every
/// message that mentions this agent, up to `BOARD_MENTIONS`.
fn board(agent_did: &str, channels: &[(&'static str, Vec<Value>)]) -> Value {
    let preview = |message: &Value| {
        let text = message["content"]
            .as_str()
            .map_or_else(|| message["content"].to_string(), ToOwned::to_owned);
        json!({
            "message_id": message["message_id"],
            "category": message["category"],
            "from": message["author_display_name"],
            "from_agent_did": message["author_agent_did"],
            "text": text.chars().take(BOARD_PREVIEW_CHARS).collect::<String>(),
            "at": message["created_at"]
                .as_i64()
                .and_then(DateTime::from_timestamp_millis)
                .map(|at| at.to_rfc3339()),
        })
    };
    let mut mentions = Vec::new();
    let mut summary = serde_json::Map::new();
    for (category, messages) in channels {
        mentions.extend(
            messages
                .iter()
                .filter(|message| message["mention"] == agent_did)
                .map(preview),
        );
        summary.insert(
            (*category).to_owned(),
            json!({
                "messages": messages.len(),
                "latest": messages.iter().take(BOARD_LATEST).map(preview).collect::<Vec<_>>(),
            }),
        );
    }
    mentions.sort_by(|a, b| a["at"].as_str().cmp(&b["at"].as_str()));
    let mentioned = mentions.len();
    mentions.truncate(BOARD_MENTIONS);
    json!({"channels": summary, "mentions": mentions, "mentioned": mentioned})
}
