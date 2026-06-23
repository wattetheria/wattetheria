use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use wattetheria_social::domain::agent_skills::AgentSkill;

use crate::state::ControlPlaneState;

const PHRASE_SIMILARITY_NUMERATOR: usize = 82;
const TOKEN_SIMILARITY_NUMERATOR: usize = 80;
const SIMILARITY_DENOMINATOR: usize = 100;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CollectiveSkillGate {
    pub required_skills: Vec<String>,
    pub local_visible_skills: Vec<String>,
    pub allowed: bool,
    pub message: String,
}

impl CollectiveSkillGate {
    pub(crate) fn value(&self) -> Value {
        json!({
            "status": if self.allowed { "allowed" } else { "blocked" },
            "reason": if self.allowed { "required_skills_matched" } else { "required_skills_not_met" },
            "message": self.message,
            "required_skills": self.required_skills,
            "local_visible_skills": self.local_visible_skills,
        })
    }
}

pub(crate) fn collective_skill_gate(
    state: &ControlPlaneState,
    payload: &Value,
) -> Option<CollectiveSkillGate> {
    let required_skills = required_collective_skills(payload);
    if required_skills.is_empty() {
        return None;
    }
    let visible_skills = match state.social_store.list_visible_agent_skills() {
        Ok(skills) => skills,
        Err(error) => {
            return Some(CollectiveSkillGate {
                required_skills,
                local_visible_skills: Vec::new(),
                allowed: false,
                message: format!(
                    "Collective mission participation was blocked because local visible skills could not be loaded: {error}"
                ),
            });
        }
    };
    let local_visible_skills = visible_skills
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    let allowed = required_skills.iter().all(|required| {
        visible_skills
            .iter()
            .any(|skill| skill_matches(required, skill))
    });
    let message = if allowed {
        format!(
            "Collective mission participation is allowed; local visible skills match required skills: {}.",
            required_skills.join(", ")
        )
    } else if local_visible_skills.is_empty() {
        format!(
            "Collective mission participation was blocked because it requires visible skills ({}) and this agent has no visible skills configured.",
            required_skills.join(", ")
        )
    } else {
        format!(
            "Collective mission participation was blocked because required skills ({}) do not match this agent's visible skills ({}).",
            required_skills.join(", "),
            local_visible_skills.join(", ")
        )
    };
    Some(CollectiveSkillGate {
        required_skills,
        local_visible_skills,
        allowed,
        message,
    })
}

fn required_collective_skills(payload: &Value) -> Vec<String> {
    [
        "/topic_content/mission/skills",
        "/content/mission/skills",
        "/mission/skills",
        "/topic_content/skills",
        "/content/skills",
        "/skills",
    ]
    .into_iter()
    .filter_map(|path| payload.pointer(path))
    .flat_map(skill_values)
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect()
}

fn skill_values(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().flat_map(skill_values).collect(),
        Value::Object(object) => ["name", "skill_id", "id", "label"]
            .into_iter()
            .filter_map(|key| object.get(key).and_then(Value::as_str))
            .flat_map(split_skill_text)
            .collect(),
        Value::String(text) => split_skill_text(text).collect(),
        _ => Vec::new(),
    }
}

fn split_skill_text(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn skill_matches(required: &str, skill: &AgentSkill) -> bool {
    skill_terms(skill)
        .iter()
        .any(|candidate| text_matches(required, candidate))
}

fn skill_terms(skill: &AgentSkill) -> Vec<&str> {
    let mut terms = vec![
        skill.name.as_str(),
        skill.skill_id.as_str(),
        skill.description.as_str(),
    ];
    terms.extend(skill.tags.iter().map(String::as_str));
    terms
}

fn text_matches(required: &str, candidate: &str) -> bool {
    let required = normalize_text(required);
    let candidate = normalize_text(candidate);
    if required.is_empty() || candidate.is_empty() {
        return false;
    }
    if required == candidate
        || contains_phrase(&candidate, &required)
        || contains_phrase(&required, &candidate)
    {
        return true;
    }
    if similarity_at_least(
        &required,
        &candidate,
        PHRASE_SIMILARITY_NUMERATOR,
        SIMILARITY_DENOMINATOR,
    ) {
        return true;
    }
    let required_tokens = tokens(&required);
    let candidate_tokens = tokens(&candidate);
    !required_tokens.is_empty()
        && required_tokens.iter().all(|required_token| {
            candidate_tokens
                .iter()
                .any(|candidate_token| token_matches(required_token, candidate_token))
        })
}

fn normalize_text(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_phrase(text: &str, phrase: &str) -> bool {
    let text = format!(" {text} ");
    let phrase = format!(" {phrase} ");
    text.contains(&phrase)
}

fn tokens(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

fn token_matches(required: &str, candidate: &str) -> bool {
    required == candidate
        || (required.len() >= 3
            && candidate.len() >= 3
            && similarity_at_least(
                required,
                candidate,
                TOKEN_SIMILARITY_NUMERATOR,
                SIMILARITY_DENOMINATOR,
            ))
}

fn similarity_at_least(left: &str, right: &str, numerator: usize, denominator: usize) -> bool {
    if left == right {
        return true;
    }
    let left_chars = left.chars().collect::<Vec<_>>();
    let right_chars = right.chars().collect::<Vec<_>>();
    if left_chars.is_empty() || right_chars.is_empty() {
        return false;
    }
    let distance = levenshtein_distance(&left_chars, &right_chars);
    let max_len = left_chars.len().max(right_chars.len());
    (max_len.saturating_sub(distance)) * denominator >= max_len * numerator
}

fn levenshtein_distance(left: &[char], right: &[char]) -> usize {
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_char) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_char != right_char);
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            current[right_index + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}
