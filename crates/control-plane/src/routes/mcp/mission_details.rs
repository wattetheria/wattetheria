//! `get_mission`: missions read in full, found by title or by id.
//!
//! The gateway keeps every mission. A search by id reads that one mission; a search by title
//! looks through the missions this agent takes part in, the open ones and the most recent of
//! every node. Each mission found states its scope, domain, region, publishing time and status
//! in the same fields.

use super::{numeric_argument, required_string, tool_error, tool_success};
use crate::routes::gateway_missions::{self, MissionResult};
use crate::state::ControlPlaneState;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;
const RECENT_LIMIT: &str = "200";

/// Where a mission record may keep a fact about the mission: on itself, in the gateway's
/// `place` (a record the node could not read) or inside what its publisher announced.
const FACT_HOLDERS: [&[&str]; 3] = [&[], &["place"], &["task_contract", "inputs"]];

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

/// States the same facts on every mission in the answer, however its record lays them out:
/// `scope`, `domain`, `region` (the mission's coordinates), `published_at` and `status` (the
/// gateway's). A fact the record does not give is `null`, never guessed.
pub(super) fn state_common_facts(record: &mut Value) {
    let scope = text_fact(record, &["scope"]);
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
    let status = text_fact(record, &["gateway_status", "status"]);
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
    title: String,
    record: Value,
}

/// The missions a title search looks through: the ones this agent takes part in, the open ones
/// and the most recent of every node.
async fn searchable_records(state: &ControlPlaneState) -> MissionResult<Vec<Value>> {
    let mut records = gateway_missions::view_records(state).await?;
    let recent = gateway_missions::list(state, &[("limit", RECENT_LIMIT)]).await?;
    let mut seen = records
        .iter()
        .filter_map(|record| record["mission_id"].as_str().map(ToOwned::to_owned))
        .collect::<BTreeSet<_>>();
    records.extend(recent.into_iter().filter(|record| {
        record["mission_id"]
            .as_str()
            .is_some_and(|id| seen.insert(id.to_owned()))
    }));
    Ok(records)
}

async fn candidate_records(
    state: &ControlPlaneState,
    wanted: &Wanted,
) -> MissionResult<Vec<Value>> {
    match &wanted.mission_id {
        Some(mission_id) => Ok(gateway_missions::fetch(state, mission_id)
            .await?
            .into_iter()
            .collect()),
        None => searchable_records(state).await,
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

    let records = match candidate_records(state, &wanted).await {
        Ok(records) => records,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    let mut found = records
        .into_iter()
        .filter_map(|record| {
            let mission_id = record["mission_id"].as_str()?.to_owned();
            let title = record["title"].as_str().unwrap_or_default().to_owned();
            wanted
                .matches(&mission_id, &title)
                .then_some(Found { title, record })
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|mission| !wanted.is_exact(&mission.title));
    let known_count = found.len();
    found.truncate(limit);

    let missions = found
        .into_iter()
        .map(|mission| {
            let mut record = gateway_missions::mission_json_or_record(&mission.record);
            state_common_facts(&mut record);
            record
        })
        .collect::<Vec<_>>();

    tool_success(&json!({
        "count": missions.len(),
        "known_count": known_count,
        "has_more": known_count > missions.len(),
        "missions": missions,
    }))
}
