use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use wattetheria_social::domain::agent_skills::AgentSkill;
use wattetheria_social::types::SocialError;

use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;

const MAX_SKILL_ID_LEN: usize = 80;
const MAX_SKILL_NAME_LEN: usize = 80;
const MAX_SKILL_DESCRIPTION_LEN: usize = 500;
const MAX_TAGS: usize = 12;
const MAX_TAG_LEN: usize = 40;

#[derive(Debug, Deserialize)]
pub(crate) struct UpsertAgentSkillRequest {
    skill_id: Option<String>,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "default_visible")]
    visible: bool,
    sort_order: Option<i64>,
}

fn default_visible() -> bool {
    true
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
        .into_response()
}

fn social_internal_error(error: &SocialError) -> Response {
    internal_error(&anyhow::anyhow!(error.to_string()))
}

pub(crate) async fn list_agent_skills(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let _auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    match state.social_store.list_agent_skills() {
        Ok(items) => Json(json!({"ok": true, "items": items})).into_response(),
        Err(error) => social_internal_error(&error),
    }
}

pub(crate) async fn upsert_agent_skill(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<UpsertAgentSkillRequest>,
) -> Response {
    let _auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let existing = match state.social_store.list_agent_skills() {
        Ok(items) => items,
        Err(error) => return social_internal_error(&error),
    };
    let skill = match normalize_agent_skill(body, &existing) {
        Ok(skill) => skill,
        Err(error) => return bad_request(error),
    };
    if let Err(error) = state.social_store.upsert_agent_skill(&skill) {
        return social_internal_error(&error);
    }
    Json(json!({"ok": true, "item": skill})).into_response()
}

fn normalize_agent_skill(
    body: UpsertAgentSkillRequest,
    existing: &[AgentSkill],
) -> Result<AgentSkill, String> {
    let name = normalized_non_empty(&body.name, "name", MAX_SKILL_NAME_LEN)?;
    let skill_id = body
        .skill_id
        .map(|value| normalize_skill_id(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| normalize_skill_id(&name));
    if skill_id.is_empty() {
        return Err("skill_id is required".to_string());
    }
    if skill_id.len() > MAX_SKILL_ID_LEN {
        return Err(format!(
            "skill_id must be at most {MAX_SKILL_ID_LEN} characters"
        ));
    }
    let description = body.description.trim().to_string();
    if description.len() > MAX_SKILL_DESCRIPTION_LEN {
        return Err(format!(
            "description must be at most {MAX_SKILL_DESCRIPTION_LEN} characters"
        ));
    }
    let mut tags = Vec::new();
    for tag in body.tags {
        let tag = tag.trim().to_string();
        if tag.is_empty() || tags.iter().any(|existing| existing == &tag) {
            continue;
        }
        if tag.len() > MAX_TAG_LEN {
            return Err(format!("tags must be at most {MAX_TAG_LEN} characters"));
        }
        tags.push(tag);
    }
    if tags.len() > MAX_TAGS {
        return Err(format!("at most {MAX_TAGS} tags are allowed"));
    }
    let now = Utc::now().timestamp_millis();
    let previous = existing.iter().find(|skill| skill.skill_id == skill_id);
    let sort_order = body
        .sort_order
        .or_else(|| previous.map(|skill| skill.sort_order))
        .unwrap_or_else(|| next_sort_order(existing));
    Ok(AgentSkill {
        skill_id,
        name,
        description,
        tags,
        visible: body.visible,
        source: previous.map_or_else(|| "manual".to_string(), |skill| skill.source.clone()),
        sort_order,
        created_at: previous.map_or(now, |skill| skill.created_at),
        updated_at: now,
    })
}

fn normalized_non_empty(value: &str, field: &str, max_len: usize) -> Result<String, String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(format!("{field} is required"));
    }
    if value.len() > max_len {
        return Err(format!("{field} must be at most {max_len} characters"));
    }
    Ok(value)
}

fn normalize_skill_id(value: &str) -> String {
    let mut normalized = String::new();
    let mut last_dash = false;
    for ch in value.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch);
            last_dash = false;
        } else if !last_dash {
            normalized.push('-');
            last_dash = true;
        }
    }
    normalized.trim_matches('-').to_string()
}

fn next_sort_order(existing: &[AgentSkill]) -> i64 {
    existing
        .iter()
        .map(|skill| skill.sort_order)
        .max()
        .unwrap_or(0)
        + 10
}
