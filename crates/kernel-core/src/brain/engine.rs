//! Brain provider interfaces and implementations for report humanization and proposals.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
        let prompt = build_agent_event_prompt(event)?;
        let output = openai_compatible_generate(self, &prompt).await?;
        parse_agent_event_or_fallback(&output, event).await
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
    let mut request = reqwest::Client::new()
        .post(format!(
            "{}/chat/completions",
            provider.base_url.trim_end_matches('/')
        ))
        .json(&json!({
            "model": provider.model,
            "messages": [
                {"role":"system", "content":"You are a strict JSON generator."},
                {"role":"user", "content": prompt}
            ],
            "temperature": 0.2,
            "response_format": {"type": "json_object"}
        }));

    if let Ok(token) = std::env::var(openai_compatible_api_key_env(provider)) {
        request = request.bearer_auth(token);
    }

    let response = request.send().await.context("call openai-compatible")?;
    let payload: Value = response
        .json()
        .await
        .context("parse openai-compatible response")?;
    payload["choices"][0]["message"]["content"]
        .as_str()
        .map(ToString::to_string)
        .context("openai-compatible response missing content")
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
    Ok(format!(
        concat!(
            "Return strict JSON object with keys action,reason,payload. ",
            "Choose action from allowed_actions only. ",
            "If no safe action should be taken, return {{\"action\": null, \"reason\": \"...\", \"payload\": {{}}}}. ",
            "Rules: ",
            "1. payload must be a JSON object. ",
            "2. For action=reply on dm_received or topic_message_requires_reply, payload must include key content. ",
            "3. For payment reject, payload may include reject_reason. ",
            "4. For payment authorize, payload may include sender_address. ",
            "5. For mission transition actions, include mission_id when known. ",
            "6. For task_claim_received on a wattetheria_mission, choose claim_mission to accept the claim and include mission_id plus agent_did when known. ",
            "7. For task_result_received on a wattetheria_mission_result, choose settle_mission to accept and settle the result, or complete_mission to mark it completed without settlement; include mission_id and agent_did when known. ",
            "Input: {}"
        ),
        serde_json::to_string(event)?
    ))
}

async fn parse_agent_event_or_fallback(
    raw: &str,
    event: &Value,
) -> Result<Option<AgentEventResolution>> {
    match serde_json::from_str::<AgentEventResolution>(raw) {
        Ok(decision) => Ok(validate_agent_event_resolution(decision, event)),
        Err(_) => RulesBrain.decide_agent_event(event).await,
    }
}

fn validate_agent_event_resolution(
    mut decision: AgentEventResolution,
    event: &Value,
) -> Option<AgentEventResolution> {
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
        return None;
    }
    let action = decision
        .action
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if !allowed_actions.contains(&action) {
        return None;
    }
    if !decision.payload.is_object() {
        decision.payload = json!({});
    }
    Some(decision)
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
            "allowed_actions": ["inspect_task", "claim_mission"],
            "payload": {
                "task_inputs": {
                    "kind": "wattetheria_mission",
                    "mission_id": "mission-1"
                }
            }
        }))
        .unwrap();

        assert!(prompt.contains("task_claim_received"));
        assert!(prompt.contains("claim_mission"));
        assert!(prompt.contains("task_result_received"));
        assert!(prompt.contains("settle_mission"));
        assert!(prompt.contains("complete_mission"));
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
