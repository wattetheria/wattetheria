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
pub struct SkillCallPlan {
    pub skill_id: String,
    pub input: Value,
    pub required_caps: Vec<String>,
    pub estimated_cost: i64,
    pub risk_level: RiskLevel,
    pub rationale: String,
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
    async fn plan_skill_calls(&self, state: &Value) -> Result<Vec<SkillCallPlan>>;
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

    pub async fn plan_skill_calls(&self, state: &Value) -> Result<Vec<SkillCallPlan>> {
        self.provider.plan_skill_calls(state).await
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
        let mut out = vec![ActionProposal {
            action: "task.run_demo_market".to_string(),
            required_caps: vec!["p2p.publish".to_string()],
            estimated_cost: 1,
            risk_level: RiskLevel::Low,
            rationale: "Maintains deterministic throughput and updates ledger stats".to_string(),
        }];

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

    async fn plan_skill_calls(&self, state: &Value) -> Result<Vec<SkillCallPlan>> {
        // Keep planner disabled by default. Callers must opt in explicitly.
        if !state["skill_planner_enabled"].as_bool().unwrap_or(false) {
            return Ok(vec![]);
        }

        let report_hint = state["latest_report_digest"]
            .as_str()
            .unwrap_or("no-report-digest");

        Ok(vec![SkillCallPlan {
            skill_id: "echo-skill".to_string(),
            input: json!({
                "intent": "summarize_recent_report",
                "report_digest": report_hint
            }),
            required_caps: vec!["model.invoke".to_string()],
            estimated_cost: 1,
            risk_level: RiskLevel::Low,
            rationale: "Use a low-risk skill path to produce deterministic helper output"
                .to_string(),
        }])
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

    async fn plan_skill_calls(&self, state: &Value) -> Result<Vec<SkillCallPlan>> {
        let prompt = format!(
            "Return strict JSON array of skill call plans with keys skill_id,input,required_caps,estimated_cost,risk_level,rationale. Input: {}",
            serde_json::to_string(state)?
        );
        let output = ollama_generate(&self.base_url, &self.model, &prompt).await?;
        parse_skill_call_plans_or_fallback(&output, state).await
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

    async fn plan_skill_calls(&self, state: &Value) -> Result<Vec<SkillCallPlan>> {
        let prompt = format!(
            "Return strict JSON array with keys skill_id,input,required_caps,estimated_cost,risk_level,rationale. Input: {}",
            serde_json::to_string(state)?
        );
        let output = openai_compatible_generate(self, &prompt).await?;
        parse_skill_call_plans_or_fallback(&output, state).await
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

    if let Some(env_name) = &provider.api_key_env
        && let Ok(token) = std::env::var(env_name)
    {
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

async fn parse_skill_call_plans_or_fallback(
    raw: &str,
    state: &Value,
) -> Result<Vec<SkillCallPlan>> {
    match serde_json::from_str::<Vec<SkillCallPlan>>(raw) {
        Ok(plans) => Ok(plans),
        _ => RulesBrain.plan_skill_calls(state).await,
    }
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

        let plans_disabled = engine
            .plan_skill_calls(&json!({"skill_planner_enabled": false}))
            .await
            .unwrap();
        assert!(plans_disabled.is_empty());

        let plans_enabled = engine
            .plan_skill_calls(&json!({"skill_planner_enabled": true}))
            .await
            .unwrap();
        assert_eq!(plans_enabled.len(), 1);

        let health = engine.doctor().await.unwrap();
        assert_eq!(health, "rules_brain_ready");
    }
}
