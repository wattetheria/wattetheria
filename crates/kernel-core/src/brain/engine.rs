//! Brain provider interfaces and implementations for report humanization and proposals.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::borrow::Cow;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanReport {
    pub title: String,
    pub summary: String,
    pub highlights: Vec<String>,
    pub risk_level: RiskLevel,
    pub recommended_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionProposal {
    pub action: String,
    pub required_caps: Vec<String>,
    pub estimated_cost: i64,
    pub risk_level: RiskLevel,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEventResolution {
    pub action: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEventDecision {
    pub resolution: Option<AgentEventResolution>,
    #[serde(default)]
    pub diagnostics: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum BrainProviderConfig {
    #[default]
    Rules,
    Ollama {
        base_url: String,
        model: String,
    },
    OpenaiCompatible {
        base_url: String,
        model: String,
        api_key_env: Option<String>,
    },
}

pub struct BrainEngine {
    provider: Box<dyn BrainProvider>,
}

#[async_trait]
pub trait BrainProvider: Send + Sync {
    async fn humanize_night_shift(&self, report: &Value) -> Result<HumanReport>;
    async fn propose_actions(&self, state: &Value) -> Result<Vec<ActionProposal>>;
    async fn decide_agent_event(&self, event: &Value) -> Result<Option<AgentEventResolution>>;
    async fn decide_agent_event_with_diagnostics(
        &self,
        event: &Value,
    ) -> Result<AgentEventDecision> {
        Ok(AgentEventDecision {
            resolution: self.decide_agent_event(event).await?,
            diagnostics: json!({}),
        })
    }
    async fn health_check(&self) -> Result<String>;
}

impl BrainEngine {
    #[must_use]
    pub fn from_config(config: &BrainProviderConfig) -> Self {
        let provider: Box<dyn BrainProvider> = match config {
            BrainProviderConfig::Rules => Box::new(RulesBrain),
            BrainProviderConfig::Ollama { base_url, model } => Box::new(OllamaBrain {
                base_url: base_url.clone(),
                model: model.clone(),
            }),
            BrainProviderConfig::OpenaiCompatible {
                base_url,
                model,
                api_key_env,
            } => Box::new(OpenAiCompatibleBrain {
                base_url: base_url.clone(),
                model: model.clone(),
                api_key_env: api_key_env.clone(),
            }),
        };

        Self { provider }
    }

    pub async fn humanize_night_shift(&self, report: &Value) -> Result<HumanReport> {
        self.provider.humanize_night_shift(report).await
    }

    pub async fn propose_actions(&self, state: &Value) -> Result<Vec<ActionProposal>> {
        self.provider.propose_actions(state).await
    }

    pub async fn decide_agent_event(&self, event: &Value) -> Result<Option<AgentEventResolution>> {
        self.provider.decide_agent_event(event).await
    }

    pub async fn decide_agent_event_with_diagnostics(
        &self,
        event: &Value,
    ) -> Result<AgentEventDecision> {
        self.provider
            .decide_agent_event_with_diagnostics(event)
            .await
    }

    pub async fn doctor(&self) -> Result<String> {
        self.provider.health_check().await
    }
}

#[derive(Debug, Clone, Copy)]
struct RulesBrain;

#[async_trait]
impl BrainProvider for RulesBrain {
    async fn humanize_night_shift(&self, report: &Value) -> Result<HumanReport> {
        let completed = report["totals"]["completed_tasks"].as_i64().unwrap_or(0);
        let events = report["totals"]["events"].as_i64().unwrap_or(0);
        let delta_watt = report["stats_delta"]["watt"].as_i64().unwrap_or(0);

        let risk_level = if completed == 0 {
            RiskLevel::High
        } else if delta_watt < 0 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };

        let mut highlights = Vec::new();
        highlights.push(format!("Total events observed: {events}"));
        highlights.push(format!("Tasks settled during the window: {completed}"));
        highlights.push(format!("Net watt delta: {delta_watt}"));

        let recommended_actions = if completed == 0 {
            vec![
                "Trigger a deterministic market task to restore throughput".to_string(),
                "Inspect pending policy approvals before next cycle".to_string(),
            ]
        } else {
            vec![
                "Keep current subnet participation active".to_string(),
                "Publish signed summary for observability ranking".to_string(),
            ]
        };

        Ok(HumanReport {
            title: "Night Shift Brief".to_string(),
            summary: format!(
                "{events} events processed with {completed} completed tasks and watt delta {delta_watt}."
            ),
            highlights,
            risk_level,
            recommended_actions,
        })
    }

    async fn propose_actions(&self, state: &Value) -> Result<Vec<ActionProposal>> {
        let pending_policy = state["pending_policy_requests"].as_i64().unwrap_or(0);
        let mut out = Vec::new();

        if pending_policy > 0 {
            out.push(ActionProposal {
                action: "policy.review_pending".to_string(),
                required_caps: vec!["mcp.call:policy".to_string()],
                estimated_cost: 1,
                risk_level: RiskLevel::Medium,
                rationale: "Pending high-risk requests should be reviewed before granting"
                    .to_string(),
            });
        }

        Ok(out)
    }

    async fn decide_agent_event(&self, _event: &Value) -> Result<Option<AgentEventResolution>> {
        Ok(None)
    }

    async fn health_check(&self) -> Result<String> {
        Ok("rules_brain_ready".to_string())
    }
}

#[derive(Debug, Clone)]
struct OllamaBrain {
    base_url: String,
    model: String,
}

#[derive(Debug, Clone)]
struct OpenAiCompatibleBrain {
    base_url: String,
    model: String,
    api_key_env: Option<String>,
}

const DEFAULT_OPENAI_COMPATIBLE_API_KEY_ENV: &str = "WATTETHERIA_BRAIN_API_KEY";
const OPENAI_COMPATIBLE_TRACE_BODY_LIMIT: usize = 16_384;

#[async_trait]
impl BrainProvider for OllamaBrain {
    async fn humanize_night_shift(&self, report: &Value) -> Result<HumanReport> {
        let prompt = format!(
            "Return strict JSON with keys title,summary,highlights,risk_level,recommended_actions based on this report: {}",
            serde_json::to_string(report)?
        );
        let output = ollama_generate(&self.base_url, &self.model, &prompt).await?;
        parse_human_report_or_fallback(&output, report).await
    }

    async fn propose_actions(&self, state: &Value) -> Result<Vec<ActionProposal>> {
        let prompt = format!(
            "Return strict JSON array of action proposals with keys action,required_caps,estimated_cost,risk_level,rationale. Input: {}",
            serde_json::to_string(state)?
        );
        let output = ollama_generate(&self.base_url, &self.model, &prompt).await?;
        parse_proposals_or_fallback(&output, state).await
    }

    async fn decide_agent_event(&self, event: &Value) -> Result<Option<AgentEventResolution>> {
        let prompt = build_agent_event_prompt(event)?;
        let output = ollama_generate(&self.base_url, &self.model, &prompt).await?;
        parse_agent_event_or_fallback(&output, event).await
    }

    async fn health_check(&self) -> Result<String> {
        let response = reqwest::Client::new()
            .get(format!("{}/api/tags", self.base_url.trim_end_matches('/')))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .context("ollama health check")?;
        if !response.status().is_success() {
            anyhow::bail!("ollama health status {}", response.status());
        }
        Ok("ollama_ready".to_string())
    }
}

#[async_trait]
impl BrainProvider for OpenAiCompatibleBrain {
    async fn humanize_night_shift(&self, report: &Value) -> Result<HumanReport> {
        let prompt = format!(
            "Return strict JSON with keys title,summary,highlights,risk_level,recommended_actions based on this report: {}",
            serde_json::to_string(report)?
        );
        let output = openai_compatible_generate(self, &prompt).await?;
        parse_human_report_or_fallback(&output, report).await
    }

    async fn propose_actions(&self, state: &Value) -> Result<Vec<ActionProposal>> {
        let prompt = format!(
            "Return strict JSON array with keys action,required_caps,estimated_cost,risk_level,rationale. Input: {}",
            serde_json::to_string(state)?
        );
        let output = openai_compatible_generate(self, &prompt).await?;
        parse_proposals_or_fallback(&output, state).await
    }

    async fn decide_agent_event(&self, event: &Value) -> Result<Option<AgentEventResolution>> {
        Ok(decide_openai_compatible_agent_event(self, event)
            .await?
            .resolution)
    }

    async fn decide_agent_event_with_diagnostics(
        &self,
        event: &Value,
    ) -> Result<AgentEventDecision> {
        decide_openai_compatible_agent_event(self, event).await
    }

    async fn health_check(&self) -> Result<String> {
        let response = reqwest::Client::new()
            .get(format!("{}/models", self.base_url.trim_end_matches('/')))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .context("openai-compatible health check")?;
        if !response.status().is_success() {
            anyhow::bail!("openai-compatible health status {}", response.status());
        }
        Ok("openai_compatible_ready".to_string())
    }
}

#[derive(Debug)]
struct OpenAiCompatibleGeneration {
    content: String,
    response_body: String,
    response_body_snippet: String,
}

async fn decide_openai_compatible_agent_event(
    provider: &OpenAiCompatibleBrain,
    event: &Value,
) -> Result<AgentEventDecision> {
    let prompt = build_agent_event_prompt(event)?;
    let generation = openai_compatible_generate_response(provider, &prompt).await?;
    let parse = parse_agent_event_with_diagnostics(&generation.content, event).await?;
    let diagnostics = json!({
        "provider": "openai-compatible",
        "model": provider.model,
        "base_url": provider.base_url,
        "response_body": generation.response_body_snippet,
        "completion_content": diagnostic_text_snippet(
            &generation.content,
            OPENAI_COMPATIBLE_TRACE_BODY_LIMIT
        ),
        "content_bytes": generation.content.len(),
        "response_bytes": generation.response_body.len(),
        "parse": parse.diagnostics,
    });
    Ok(AgentEventDecision {
        resolution: parse.resolution,
        diagnostics,
    })
}

async fn ollama_generate(base_url: &str, model: &str, prompt: &str) -> Result<String> {
    let response = reqwest::Client::new()
        .post(format!("{}/api/generate", base_url.trim_end_matches('/')))
        .json(&json!({
            "model": model,
            "prompt": prompt,
            "stream": false,
            "format": "json"
        }))
        .send()
        .await
        .context("call ollama generate")?;

    let payload: Value = response.json().await.context("parse ollama response")?;
    payload["response"]
        .as_str()
        .map(ToString::to_string)
        .context("ollama response missing `response` string")
}

async fn openai_compatible_generate(
    provider: &OpenAiCompatibleBrain,
    prompt: &str,
) -> Result<String> {
    Ok(openai_compatible_generate_response(provider, prompt)
        .await?
        .content)
}

async fn openai_compatible_generate_response(
    provider: &OpenAiCompatibleBrain,
    prompt: &str,
) -> Result<OpenAiCompatibleGeneration> {
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let request_body = json!({
        "model": provider.model,
        "messages": [
            {"role":"system", "content":"You are a strict JSON generator."},
            {"role":"user", "content": prompt}
        ],
        "temperature": 0.2,
        "response_format": {"type": "json_object"}
    });
    let request_body_text =
        diagnostic_json_snippet(&request_body, OPENAI_COMPATIBLE_TRACE_BODY_LIMIT);
    let mut request = reqwest::Client::new().post(&url).json(&request_body);

    if let Ok(token) = std::env::var(openai_compatible_api_key_env(provider)) {
        request = request.bearer_auth(token);
    }

    let response = request
        .send()
        .await
        .with_context(|| format!("call openai-compatible url={url} request={request_body_text}"))?;
    let status = response.status();
    let response_body = response.text().await.with_context(|| {
        format!("read openai-compatible response body url={url} status={status}")
    })?;
    let response_body_text =
        diagnostic_text_snippet(&response_body, OPENAI_COMPATIBLE_TRACE_BODY_LIMIT);

    if !status.is_success() {
        anyhow::bail!(
            "openai-compatible status {status}: url={url} request={request_body_text} response_body={response_body_text}"
        );
    }

    let payload: Value = serde_json::from_str(&response_body).with_context(|| {
        format!(
            "parse openai-compatible response url={url} status={status} request={request_body_text} response_body={response_body_text}"
        )
    })?;
    let content = payload["choices"][0]["message"]["content"]
        .as_str()
        .map(ToString::to_string)
        .with_context(|| {
            format!(
                "openai-compatible response missing content: url={url} status={status} request={request_body_text} response_body={response_body_text}"
            )
        })?;
    Ok(OpenAiCompatibleGeneration {
        content,
        response_body,
        response_body_snippet: response_body_text,
    })
}

fn diagnostic_json_snippet(value: &Value, limit: usize) -> String {
    match serde_json::to_string(value) {
        Ok(text) => diagnostic_text_snippet(&text, limit),
        Err(error) => format!("<failed to serialize diagnostic json: {error}>"),
    }
}

fn diagnostic_text_snippet(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}...<truncated {} bytes>",
        &value[..end],
        value.len() - end
    )
}

fn openai_compatible_api_key_env(provider: &OpenAiCompatibleBrain) -> &str {
    provider
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_OPENAI_COMPATIBLE_API_KEY_ENV)
}

async fn parse_human_report_or_fallback(raw: &str, report: &Value) -> Result<HumanReport> {
    match serde_json::from_str::<HumanReport>(raw) {
        Ok(report) => Ok(report),
        Err(_) => RulesBrain.humanize_night_shift(report).await,
    }
}

async fn parse_proposals_or_fallback(raw: &str, state: &Value) -> Result<Vec<ActionProposal>> {
    match serde_json::from_str::<Vec<ActionProposal>>(raw) {
        Ok(actions) if !actions.is_empty() => Ok(actions),
        _ => RulesBrain.propose_actions(state).await,
    }
}

fn build_agent_event_prompt(event: &Value) -> Result<String> {
    let event_type = event
        .get("event_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let allowed_actions = agent_event_allowed_actions(event);
    let scoped_rules = agent_event_scoped_rules(event_type, &allowed_actions, event);
    Ok(format!(
        concat!(
            "Return strict JSON object with keys action,reason,payload. ",
            "Choose action from this allowed_actions list only: {}. ",
            "If no safe action should be taken, return {{\"action\": null, \"reason\": \"...\", \"payload\": {{}}}}. ",
            "Common rules: ",
            "1. payload must be a JSON object. ",
            "2. Do not invent fields that are not needed by the selected action. ",
            "3. Preserve IDs from the input payload when known. ",
            "Event type: {}. ",
            "Relevant action schema: {}. ",
            "Input: {}"
        ),
        allowed_actions.join(","),
        event_type,
        scoped_rules.join(" "),
        serde_json::to_string(event)?
    ))
}

fn agent_event_allowed_actions(event: &Value) -> Vec<String> {
    event
        .get("allowed_actions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn agent_event_scoped_rules(
    event_type: &str,
    allowed_actions: &[String],
    event: &Value,
) -> Vec<String> {
    let mut rules = Vec::new();
    for action in allowed_actions {
        if let Some(rule) = agent_event_action_rule(event_type, action, event) {
            rules.push(rule);
        }
    }
    if rules.is_empty() {
        rules.push(
            "No event-specific action schema is available; use only allowed_actions and return action null when required fields are missing.".to_owned(),
        );
    }
    rules
}

fn agent_event_action_rule(event_type: &str, action: &str, event: &Value) -> Option<String> {
    match (event_type, action) {
        ("friend_request", "accept") => Some(
            "accept: approve the friend request; payload may include message or extensions."
                .to_owned(),
        ),
        ("friend_request", "reject") => Some(
            "reject: decline the friend request; payload may include message or extensions."
                .to_owned(),
        ),
        ("friend_request", "block") => Some(
            "block: reject and block the counterpart; payload may include message or extensions."
                .to_owned(),
        ),
        ("dm_received" | "topic_message_requires_reply", "reply") => Some(
            "reply: payload must include content; include reply_to_message_id only when replying to a topic message."
                .to_owned(),
        ),
        ("dm_received", "block") => {
            Some("block: block the message sender; payload may include message or extensions.".to_owned())
        }
        (_, "ignore") => Some("ignore: acknowledge without side effects; payload should be {}.".to_owned()),
        ("payment_request" | "payment_update", "authorize") => {
            Some("authorize: approve payment authorization; payload may include sender_address.".to_owned())
        }
        ("payment_request" | "payment_update", "reject") => {
            Some("reject: decline the payment; payload may include reject_reason.".to_owned())
        }
        ("payment_request" | "payment_update", "submit") => {
            Some("submit: mark payment submitted; payload may include settlement_receipt.".to_owned())
        }
        ("payment_request" | "payment_update", "settle") => {
            Some("settle: mark payment settled; payload must include settlement_receipt.".to_owned())
        }
        ("payment_request" | "payment_update", "cancel") => {
            Some("cancel: cancel the payment request; payload may be empty.".to_owned())
        }
        ("task_claim_received", "decide_claim") => Some(
            "decide_claim: choose only this action for a wattetheria_mission claim; payload.approved=true accepts the claim, payload.approved=false rejects it; include mission_id and claimer_node_id or agent_did when known; do not return claim_mission."
                .to_owned(),
        ),
        ("task_claim_received", "inspect_task") => {
            Some("inspect_task: defer the claim for manual inspection; payload may be empty.".to_owned())
        }
        ("task_result_received", "accept_result") => Some(
            "accept_result: accept a wattetheria_mission_result; include mission_id, agent_did, task_id, and candidate_id when known."
                .to_owned(),
        ),
        ("task_result_received", "reject_result") => Some(
            "reject_result: reject the task result; payload should include reason when available."
                .to_owned(),
        ),
        ("task_result_received", "request_retry") => Some(
            "request_retry: ask the claimer to retry; payload should include reason or retry_instructions."
                .to_owned(),
        ),
        ("task_result_received", "inspect_task") => {
            Some("inspect_task: defer the result for manual review; payload may be empty.".to_owned())
        }
        ("task_result_received", "complete_mission") => Some(
            "complete_mission: mark the mission completed without settlement; include mission_id, agent_did, and result when known."
                .to_owned(),
        ),
        ("task_result_received", "settle_mission") => Some(
            "settle_mission: accept and settle the mission result; include mission_id, agent_did, task_id, candidate_id, and settlement receipt fields when known."
                .to_owned(),
        ),
        ("topic_message_requires_reply", "complete_mission")
            if topic_message_kind(event) == Some("mission_claim_approved") =>
        {
            Some(
                "complete_mission: use only for mission_claim_approved when final work result is ready; include mission_id, agent_did, task_id, mission_feed_key, mission_scope_hint, publisher_wattswarm_node_id when known, and payload.result; otherwise return action null or ignore."
                    .to_owned(),
            )
        }
        ("topic_message_requires_reply", "settle_mission")
            if topic_message_kind(event) == Some("mission_completed") =>
        {
            Some(
                "settle_mission: use only for ordinary mission_completed lifecycle topics when the completed work is accepted; include mission_id, agent_did, task_id, mission_feed_key, mission_scope_hint, and result when known; do not require candidate_id and do not treat this as task_result_received."
                    .to_owned(),
            )
        }
        ("third_party_result", "publish_mission") => Some(
            "publish_mission: create a mission from a third-party result; include title, description, publisher, reward, domain, and payload."
                .to_owned(),
        ),
        ("third_party_result", "claim_mission") => Some(
            "claim_mission: claim an existing mission; include mission_id and agent_did."
                .to_owned(),
        ),
        ("third_party_result", "complete_mission") => Some(
            "complete_mission: complete an existing mission; include mission_id, agent_did, and result."
                .to_owned(),
        ),
        ("third_party_result", "settle_mission") => Some(
            "settle_mission: settle an existing completed mission; include mission_id and agent_did when known."
                .to_owned(),
        ),
        _ => None,
    }
}

fn topic_message_kind(event: &Value) -> Option<&str> {
    event
        .pointer("/payload/content/kind")
        .or_else(|| event.pointer("/payload/topic_content/kind"))
        .and_then(Value::as_str)
}

async fn parse_agent_event_or_fallback(
    raw: &str,
    event: &Value,
) -> Result<Option<AgentEventResolution>> {
    Ok(parse_agent_event_with_diagnostics(raw, event)
        .await?
        .resolution)
}

async fn parse_agent_event_with_diagnostics(
    raw: &str,
    event: &Value,
) -> Result<AgentEventDecision> {
    let Some(decision) = parse_normalized_agent_event_resolution(raw) else {
        return Ok(AgentEventDecision {
            resolution: RulesBrain.decide_agent_event(event).await?,
            diagnostics: json!({
                "status": "parse_failed",
                "raw_bytes": raw.len(),
            }),
        });
    };
    let parsed_action = decision.action.clone();
    let (resolution, validation_status) =
        validate_agent_event_resolution_with_status(decision, event);
    Ok(AgentEventDecision {
        diagnostics: json!({
            "status": validation_status,
            "parsed_action": parsed_action,
            "accepted": resolution.is_some(),
            "raw_bytes": raw.len(),
        }),
        resolution,
    })
}

fn parse_normalized_agent_event_resolution(raw: &str) -> Option<AgentEventResolution> {
    agent_event_resolution_json_candidates(raw)
        .into_iter()
        .find_map(|candidate| parse_normalized_agent_event_resolution_candidate(&candidate))
}

fn parse_normalized_agent_event_resolution_candidate(raw: &str) -> Option<AgentEventResolution> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .or_else(|| serde_json::from_str::<Value>(&normalize_json_literals(raw)).ok())
        .and_then(|value| normalized_agent_event_resolution(&value))
}

fn agent_event_resolution_json_candidates(raw: &str) -> Vec<Cow<'_, str>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut candidates = vec![Cow::Borrowed(trimmed)];
    if let Some(json_object) = extract_first_json_object(trimmed)
        && json_object != trimmed
    {
        candidates.push(Cow::Borrowed(json_object));
    }
    candidates
}

fn extract_first_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in raw[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth = depth.saturating_add(1),
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = start + offset + ch.len_utf8();
                    return Some(&raw[start..end]);
                }
            }
            _ => {}
        }
    }
    None
}

fn normalize_json_literals(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            output.push(ch);
            continue;
        }
        if ch.is_ascii_alphabetic() {
            let mut token = String::from(ch);
            while let Some(next) = chars.peek().copied() {
                if next.is_ascii_alphabetic() {
                    token.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            match token.as_str() {
                "TRUE" => output.push_str("true"),
                "FALSE" => output.push_str("false"),
                "NULL" => output.push_str("null"),
                _ => output.push_str(&token),
            }
        } else {
            output.push(ch);
        }
    }
    output
}

fn normalized_agent_event_resolution(value: &Value) -> Option<AgentEventResolution> {
    let object = value.as_object()?;
    let action = object
        .get("action")
        .or_else(|| case_insensitive_object_value(object, "action"))
        .cloned()
        .unwrap_or(Value::Null);
    let reason = object
        .get("reason")
        .or_else(|| case_insensitive_object_value(object, "reason"))
        .cloned();
    let payload = object
        .get("payload")
        .or_else(|| case_insensitive_object_value(object, "payload"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    let mut resolution = AgentEventResolution {
        action: normalized_action_value(&action),
        reason: reason.and_then(|value| value.as_str().map(ToOwned::to_owned)),
        payload: normalized_agent_event_payload(payload),
    };
    if !resolution.payload.is_object() {
        resolution.payload = json!({});
    }
    Some(resolution)
}

fn case_insensitive_object_value<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Option<&'a Value> {
    object
        .iter()
        .find_map(|(candidate, value)| candidate.eq_ignore_ascii_case(key).then_some(value))
}

fn normalized_action_value(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn normalized_agent_event_payload(value: Value) -> Value {
    let Value::Object(mut object) = value else {
        return json!({});
    };
    for key in object.keys().cloned().collect::<Vec<_>>() {
        let normalized = key.to_ascii_lowercase();
        if normalized != key
            && !object.contains_key(&normalized)
            && let Some(value) = object.get(&key).cloned()
        {
            object.insert(normalized, value);
        }
    }
    Value::Object(object)
}

fn validate_agent_event_resolution_with_status(
    mut decision: AgentEventResolution,
    event: &Value,
) -> (Option<AgentEventResolution>, &'static str) {
    let allowed_actions = event
        .get("allowed_actions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if allowed_actions.is_empty() {
        return (None, "rejected_empty_allowed_actions");
    }
    let Some(action) = decision
        .action
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (None, "rejected_empty_action");
    };
    if !allowed_actions.contains(&action) {
        return (None, "rejected_action_not_allowed");
    }
    if !decision.payload.is_object() {
        decision.payload = json!({});
    }
    (Some(decision), "accepted")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rules_brain_generates_human_report_and_actions() {
        let engine = BrainEngine::from_config(&BrainProviderConfig::Rules);
        let report = json!({
            "totals": {"events": 10, "completed_tasks": 2},
            "stats_delta": {"watt": 5}
        });

        let human = engine.humanize_night_shift(&report).await.unwrap();
        assert!(!human.summary.is_empty());

        let actions = engine
            .propose_actions(&json!({"pending_policy_requests": 1}))
            .await
            .unwrap();
        assert!(!actions.is_empty());

        let health = engine.doctor().await.unwrap();
        assert_eq!(health, "rules_brain_ready");
    }

    #[test]
    fn openai_compatible_api_key_env_defaults_to_wattetheria_key() {
        let provider = OpenAiCompatibleBrain {
            base_url: "http://127.0.0.1:18789/v1".to_owned(),
            model: "openclaw".to_owned(),
            api_key_env: None,
        };

        assert_eq!(
            openai_compatible_api_key_env(&provider),
            DEFAULT_OPENAI_COMPATIBLE_API_KEY_ENV
        );
    }

    #[test]
    fn agent_event_prompt_explains_mission_lifecycle_actions() {
        let prompt = build_agent_event_prompt(&json!({
            "event_type": "task_claim_received",
            "allowed_actions": ["decide_claim"],
            "payload": {
                "task_inputs": {
                    "kind": "wattetheria_mission",
                    "mission_id": "mission-1"
                }
            }
        }))
        .unwrap();

        assert!(prompt.contains("task_claim_received"));
        assert!(prompt.contains("decide_claim"));
        assert!(prompt.contains("payload.approved=true"));
        assert!(prompt.contains("do not return claim_mission"));
        assert!(!prompt.contains("task_result_received"));
        assert!(!prompt.contains("settle_mission"));
        assert!(!prompt.contains("mission_claim_approved"));
    }

    #[test]
    fn agent_event_prompt_scopes_topic_claim_approved_actions() {
        let prompt = build_agent_event_prompt(&json!({
            "event_type": "topic_message_requires_reply",
            "allowed_actions": ["complete_mission", "ignore"],
            "payload": {
                "content": {
                    "kind": "mission_claim_approved",
                    "mission_id": "mission-1",
                    "task_id": "mission-1"
                }
            }
        }))
        .unwrap();

        assert!(prompt.contains("complete_mission"));
        assert!(prompt.contains("mission_claim_approved"));
        assert!(prompt.contains("payload.result"));
        assert!(prompt.contains("ignore"));
        assert!(!prompt.contains("payment"));
        assert!(!prompt.contains("friend request"));
    }

    #[test]
    fn agent_event_prompt_scopes_topic_mission_completed_actions() {
        let prompt = build_agent_event_prompt(&json!({
            "event_type": "topic_message_requires_reply",
            "allowed_actions": ["settle_mission", "ignore"],
            "payload": {
                "content": {
                    "kind": "mission_completed",
                    "mission_id": "mission-1",
                    "task_id": "mission-1",
                    "result": {"ok": true}
                }
            }
        }))
        .unwrap();

        assert!(prompt.contains("settle_mission"));
        assert!(prompt.contains("mission_completed"));
        assert!(prompt.contains("ordinary mission_completed lifecycle topics"));
        assert!(prompt.contains("do not require candidate_id"));
        assert!(prompt.contains("do not treat this as task_result_received"));
        assert!(prompt.contains("ignore"));
        assert!(!prompt.contains("complete_mission: use only for mission_claim_approved"));
    }

    #[test]
    fn agent_event_prompt_scopes_payment_actions() {
        let prompt = build_agent_event_prompt(&json!({
            "event_type": "payment_request",
            "allowed_actions": ["authorize", "reject"],
            "payload": {
                "payment": {
                    "payment_id": "payment-1"
                }
            }
        }))
        .unwrap();

        assert!(prompt.contains("authorize"));
        assert!(prompt.contains("sender_address"));
        assert!(prompt.contains("reject_reason"));
        assert!(!prompt.contains("complete_mission"));
        assert!(!prompt.contains("friend_request"));
    }

    #[test]
    fn agent_event_prompt_scopes_unknown_events_to_allowed_actions() {
        let prompt = build_agent_event_prompt(&json!({
            "event_type": "custom_event",
            "allowed_actions": ["custom_action"],
            "payload": {}
        }))
        .unwrap();

        assert!(prompt.contains("custom_action"));
        assert!(prompt.contains("No event-specific action schema"));
        assert!(!prompt.contains("decide_claim"));
    }

    #[test]
    fn openai_compatible_api_key_env_prefers_explicit_name() {
        let provider = OpenAiCompatibleBrain {
            base_url: "http://127.0.0.1:18789/v1".to_owned(),
            model: "openclaw".to_owned(),
            api_key_env: Some("  CUSTOM_OPENAI_KEY  ".to_owned()),
        };

        assert_eq!(
            openai_compatible_api_key_env(&provider),
            "CUSTOM_OPENAI_KEY"
        );
    }
}
