//! `get_mission`: missions read in full, found by title or by id.
//!
//! Two places know missions: this node's board (the ones it published or took part in, with
//! everything about them) and the gateway's network market (the recent ones of every node, as
//! summaries). A mission on both is answered from the board. Each mission found also carries
//! what the ledger records for its Watt bounty, when it records anything, and states its scope,
//! domain, region, publishing time and status in the same fields whichever place it came from.

use super::{
    MAX_GATEWAY_TASK_WINDOW, fetch_gateway_tasks, gateway_task_string, is_gateway_mission,
    normalize_gateway_mission, normalized_gateway_tasks_url, numeric_argument, required_string,
    resolve_gateway_query_url, tool_error, tool_success,
};
use crate::ledger_client::LedgerClient;
use crate::routes::mission_trade;
use crate::state::ControlPlaneState;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;

const TITLE_PATHS: [&[&str]; 5] = [
    &["title"],
    &["summary", "title"],
    &["task_contract", "inputs", "title"],
    &["contract", "inputs", "title"],
    &["inputs", "title"],
];
const ID_PATHS: [&[&str]; 3] = [&["mission_id"], &["id"], &["task_id"]];

/// Where a mission record may keep a fact about the mission: on itself (the board, and market
/// records that state it outright) or inside what its publisher announced.
const FACT_HOLDERS: [&[&str]; 6] = [
    &[],
    &["summary"],
    &["task_contract", "inputs"],
    &["contract", "inputs"],
    &["inputs"],
    &["payload"],
];

/// The first of `keys` the record states, wherever it keeps it.
fn fact<'a>(record: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    FACT_HOLDERS.iter().find_map(|holder| {
        let holder = holder
            .iter()
            .try_fold(record, |value, key| value.get(*key))?;
        keys.iter()
            .find_map(|key| holder.get(*key).filter(|value| !value.is_null()))
    })
}

fn text_fact(record: &Value, keys: &[&str]) -> Option<String> {
    fact(record, keys)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

/// A time a record gives as Unix seconds, Unix milliseconds or text, as RFC 3339 text.
fn time_fact(record: &Value, keys: &[&str]) -> Option<String> {
    let value = fact(record, keys)?;
    if let Some(text) = value.as_str() {
        return Some(text.to_owned()).filter(|text| !text.trim().is_empty());
    }
    let number = value.as_i64()?;
    let seconds = if number >= 1_000_000_000_000 {
        number / 1000
    } else {
        number
    };
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// States the same facts on every mission in the answer, wherever its record came from and
/// however that source lays them out: `scope`, `domain`, `region` (the mission's coordinates),
/// `published_at` and `status`. A fact the record does not give is `null`, never guessed; the
/// scope also comes from the ledger's record of the bounty when only that states it.
pub(super) fn state_common_facts(record: &mut Value, bounty: Option<&Value>) {
    let scope = text_fact(record, &["scope"]).or_else(|| {
        bounty
            .and_then(|bounty| bounty["scope"].as_str())
            .map(ToOwned::to_owned)
    });
    let domain = text_fact(record, &["domain"]);
    let region = match (
        fact(record, &["lat", "latitude"]).and_then(Value::as_f64),
        fact(record, &["lng", "lon", "longitude"]).and_then(Value::as_f64),
    ) {
        (Some(lat), Some(lng)) => json!({
            "lat": lat,
            "lng": lng,
            "city": text_fact(record, &["city"]),
            "country": text_fact(record, &["country"]),
        }),
        _ => Value::Null,
    };
    let published_at = time_fact(record, &["published_at", "created_at"]);
    let status = text_fact(record, &["status", "terminal_state"]);
    let Some(object) = record.as_object_mut() else {
        return;
    };
    object.insert("scope".to_owned(), json!(scope));
    object.insert("domain".to_owned(), json!(domain));
    object.insert("region".to_owned(), region);
    object.insert("published_at".to_owned(), json!(published_at));
    object.insert("status".to_owned(), json!(status));
}

/// What the agent asked for: one id, or any part of a title, ignoring case.
struct Wanted {
    mission_id: Option<String>,
    title: Option<String>,
}

impl Wanted {
    fn matches(&self, mission_id: &str, title: &str) -> bool {
        match (&self.mission_id, &self.title) {
            (Some(wanted), _) => wanted == mission_id,
            (None, Some(wanted)) => title.to_lowercase().contains(wanted),
            (None, None) => false,
        }
    }

    /// Titles that are exactly what was asked for come before those that only contain it.
    fn is_exact(&self, title: &str) -> bool {
        self.title
            .as_ref()
            .is_some_and(|wanted| title.trim().to_lowercase() == *wanted)
    }
}

/// One mission found, before the answer is built.
struct Found {
    mission_id: String,
    title: String,
    on_this_node: bool,
    record: Value,
}

async fn network_missions(state: &ControlPlaneState) -> anyhow::Result<Vec<Value>> {
    let gateway_url = resolve_gateway_query_url(state)?;
    let endpoint = normalized_gateway_tasks_url(&gateway_url);
    Ok(fetch_gateway_tasks(&endpoint, MAX_GATEWAY_TASK_WINDOW)
        .await?
        .into_iter()
        .filter(is_gateway_mission)
        .collect())
}

/// The board's record of a mission, as it stands after the ledger's verdict on its bounty
/// (a trade the ledger already ended ends the mission here too).
async fn board_record(state: &ControlPlaneState, mission_id: &str) -> Option<Value> {
    mission_trade::end_returned_mission(state, mission_id).await;
    let mission = state.mission_board.lock().await.get(mission_id)?;
    serde_json::to_value(mission).ok()
}

/// What the ledger records for the mission's bounty: who it is reserved for, who holds it, the
/// items held and the deadlines. `None` when the bounty carries no such record.
async fn bounty_record(state: &ControlPlaneState, mission_id: &str) -> Option<Value> {
    let ledger = LedgerClient::from_state(state)?;
    match ledger.trade(mission_id).await {
        Ok((status, record)) if status.is_success() => Some(record),
        _ => None,
    }
}

pub(crate) async fn get_mission_result(state: &ControlPlaneState, arguments: &Value) -> Value {
    let wanted = Wanted {
        mission_id: required_string(arguments, "mission_id"),
        title: required_string(arguments, "title").map(|title| title.to_lowercase()),
    };
    if wanted.mission_id.is_none() && wanted.title.is_none() {
        return tool_error(&json!({"error": "get_mission needs title or mission_id"}));
    }
    let limit = numeric_argument(arguments, "limit")
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);

    let mut found = state
        .mission_board
        .lock()
        .await
        .list(None)
        .into_iter()
        .filter(|mission| wanted.matches(&mission.mission_id, &mission.title))
        .map(|mission| Found {
            mission_id: mission.mission_id,
            title: mission.title,
            on_this_node: true,
            record: Value::Null,
        })
        .collect::<Vec<_>>();
    let mut seen = found
        .iter()
        .map(|mission| mission.mission_id.clone())
        .collect::<BTreeSet<_>>();
    let network_error = match network_missions(state).await {
        Ok(tasks) => {
            for task in tasks {
                let Some(mission_id) = gateway_task_string(&task, &ID_PATHS) else {
                    continue;
                };
                let title = gateway_task_string(&task, &TITLE_PATHS).unwrap_or_default();
                if wanted.matches(&mission_id, &title) && seen.insert(mission_id.clone()) {
                    found.push(Found {
                        mission_id,
                        title,
                        on_this_node: false,
                        record: normalize_gateway_mission(task),
                    });
                }
            }
            None
        }
        Err(error) => Some(format!("{error:#}")),
    };
    found.sort_by_key(|mission| !wanted.is_exact(&mission.title));
    let known_count = found.len();
    found.truncate(limit);

    let mut missions = Vec::with_capacity(found.len());
    for mission in found {
        let record = if mission.on_this_node {
            board_record(state, &mission.mission_id).await
        } else {
            Some(mission.record)
        };
        let Some(mut record) = record else {
            continue;
        };
        let bounty = bounty_record(state, &mission.mission_id).await;
        state_common_facts(&mut record, bounty.as_ref());
        if let Some(object) = record.as_object_mut() {
            let source = if mission.on_this_node {
                "this_node"
            } else {
                "network"
            };
            object.insert("source".to_owned(), json!(source));
            if let Some(bounty) = bounty {
                object.insert("bounty".to_owned(), bounty);
            }
        }
        missions.push(record);
    }

    let mut answer = json!({
        "count": missions.len(),
        "known_count": known_count,
        "has_more": known_count > missions.len(),
        "missions": missions,
    });
    if let Some(error) = network_error {
        answer["network_error"] = json!(error);
    }
    tool_success(&answer)
}
