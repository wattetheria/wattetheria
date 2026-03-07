//! MCP HTTP adapter, registry, and call/budget enforcement.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use jsonschema::validator_for;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: Option<Value>,
    #[serde(default)]
    pub output_schema: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub tools_allowlist: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_sec: u64,
    #[serde(default = "default_budget")]
    pub budget_per_minute: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpRegistryState {
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone)]
pub struct McpRegistry {
    path: PathBuf,
    state: McpRegistryState,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct McpUsageState {
    windows: BTreeMap<String, UsageWindow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsageWindow {
    minute: i64,
    count: u32,
}

fn default_timeout() -> u64 {
    15
}

fn default_budget() -> u32 {
    60
}

impl McpRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create mcp registry directory")?;
        }

        let state = if path.exists() {
            let raw = fs::read_to_string(&path).context("read mcp registry")?;
            if raw.trim().is_empty() {
                McpRegistryState::default()
            } else {
                serde_json::from_str(&raw).context("parse mcp registry")?
            }
        } else {
            McpRegistryState::default()
        };

        let registry = Self { path, state };
        registry.persist()?;
        Ok(registry)
    }

    pub fn add_server(&mut self, mut server: McpServerConfig) -> Result<McpServerConfig> {
        if server.name.trim().is_empty() {
            bail!("mcp server name cannot be empty");
        }
        if !server.url.starts_with("http://") && !server.url.starts_with("https://") {
            bail!("mcp server url must start with http:// or https://");
        }
        if server.timeout_sec == 0 {
            server.timeout_sec = default_timeout();
        }
        if server.budget_per_minute == 0 {
            server.budget_per_minute = default_budget();
        }

        self.state.servers.retain(|item| item.name != server.name);
        self.state.servers.push(server.clone());
        self.persist()?;
        Ok(server)
    }

    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<McpServerConfig> {
        let server = self
            .state
            .servers
            .iter_mut()
            .find(|server| server.name == name)
            .context("mcp server not found")?;
        server.enabled = enabled;
        let updated = server.clone();
        self.persist()?;
        Ok(updated)
    }

    pub fn get(&self, name: &str) -> Result<McpServerConfig> {
        self.state
            .servers
            .iter()
            .find(|server| server.name == name)
            .cloned()
            .context("mcp server not found")
    }

    #[must_use]
    pub fn list(&self) -> Vec<McpServerConfig> {
        let mut list = self.state.servers.clone();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    fn persist(&self) -> Result<()> {
        fs::write(&self.path, serde_json::to_string_pretty(&self.state)?)
            .context("write mcp registry")
    }
}

pub async fn list_tools(config: &McpServerConfig) -> Result<Vec<McpToolDescriptor>> {
    ensure_enabled(config)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(config.timeout_sec))
        .build()
        .context("build mcp client")?;

    let response = client
        .get(format!("{}/tools", config.url.trim_end_matches('/')))
        .send()
        .await
        .context("request mcp tools")?;

    if !response.status().is_success() {
        bail!("mcp tools endpoint returned {}", response.status());
    }

    let tools: Vec<McpToolDescriptor> =
        response.json().await.context("parse mcp tools response")?;
    Ok(filter_allowlist(config, tools))
}

pub async fn call_tool(
    config: &McpServerConfig,
    usage_path: impl AsRef<Path>,
    tool: &str,
    input: Value,
) -> Result<Value> {
    ensure_enabled(config)?;
    enforce_allowlist(config, tool)?;
    enforce_budget(usage_path, &config.name, config.budget_per_minute)?;

    let tools = list_tools(config).await?;
    let descriptor = tools
        .iter()
        .find(|candidate| candidate.name == tool)
        .cloned()
        .context("mcp tool not found")?;

    if let Some(schema) = &descriptor.input_schema {
        let compiled = validator_for(schema).context("compile mcp input schema")?;
        if let Err(error) = compiled.validate(&input) {
            bail!("mcp input schema validation failed: {error}");
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(config.timeout_sec))
        .build()
        .context("build mcp client")?;

    let response = client
        .post(format!("{}/call", config.url.trim_end_matches('/')))
        .json(&json!({"tool": tool, "input": input}))
        .send()
        .await
        .context("request mcp call")?;

    if !response.status().is_success() {
        bail!("mcp call endpoint returned {}", response.status());
    }

    let payload: Value = response.json().await.context("parse mcp call response")?;
    let output = payload["output"].clone();

    if let Some(schema) = &descriptor.output_schema {
        let compiled = validator_for(schema).context("compile mcp output schema")?;
        if let Err(error) = compiled.validate(&output) {
            bail!("mcp output schema validation failed: {error}");
        }
    }

    Ok(output)
}

fn ensure_enabled(config: &McpServerConfig) -> Result<()> {
    if !config.enabled {
        bail!("mcp server {} is disabled", config.name);
    }
    Ok(())
}

fn filter_allowlist(
    config: &McpServerConfig,
    tools: Vec<McpToolDescriptor>,
) -> Vec<McpToolDescriptor> {
    if config.tools_allowlist.is_empty() {
        return tools;
    }
    tools
        .into_iter()
        .filter(|tool| config.tools_allowlist.contains(&tool.name))
        .collect()
}

fn enforce_allowlist(config: &McpServerConfig, tool: &str) -> Result<()> {
    if !config.tools_allowlist.is_empty() && !config.tools_allowlist.iter().any(|name| name == tool)
    {
        bail!("tool {tool} is not in allowlist");
    }
    Ok(())
}

fn enforce_budget(
    usage_path: impl AsRef<Path>,
    server: &str,
    budget_per_minute: u32,
) -> Result<()> {
    let mut state = load_usage_state(usage_path.as_ref())?;
    let current_minute = Utc::now().timestamp() / 60;

    let entry = state
        .windows
        .entry(server.to_string())
        .or_insert(UsageWindow {
            minute: current_minute,
            count: 0,
        });

    if entry.minute != current_minute {
        entry.minute = current_minute;
        entry.count = 0;
    }

    if entry.count >= budget_per_minute {
        bail!("mcp budget exceeded for {server}");
    }

    entry.count += 1;
    save_usage_state(usage_path.as_ref(), &state)
}

fn load_usage_state(path: &Path) -> Result<McpUsageState> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create mcp usage directory")?;
    }

    if !path.exists() {
        return Ok(McpUsageState::default());
    }

    let raw = fs::read_to_string(path).context("read mcp usage state")?;
    if raw.trim().is_empty() {
        return Ok(McpUsageState::default());
    }

    serde_json::from_str(&raw).context("parse mcp usage state")
}

fn save_usage_state(path: &Path, state: &McpUsageState) -> Result<()> {
    fs::write(path, serde_json::to_string_pretty(state)?).context("write mcp usage state")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::routing::{get, post};
    use axum::{Router, extract::State};
    use serde_json::json;
    use std::sync::Arc;

    #[derive(Clone)]
    struct MockState;

    async fn tools(_: State<Arc<MockState>>) -> Json<Vec<McpToolDescriptor>> {
        Json(vec![McpToolDescriptor {
            name: "news.read".to_string(),
            description: "read-only".to_string(),
            input_schema: Some(json!({
                "type": "object",
                "required": ["query"],
                "properties": {"query": {"type": "string"}}
            })),
            output_schema: Some(json!({
                "type": "object",
                "required": ["headline"],
                "properties": {"headline": {"type": "string"}}
            })),
        }])
    }

    async fn call(Json(payload): Json<Value>) -> Json<Value> {
        let tool = payload["tool"].as_str().unwrap_or("unknown");
        Json(json!({"output": {"headline": format!("ok:{tool}")}}))
    }

    #[tokio::test]
    async fn mcp_registry_and_call_path_work() {
        let dir = tempfile::tempdir().unwrap();

        let state = Arc::new(MockState);
        let app = Router::new()
            .route("/tools", get(tools))
            .route("/call", post(call))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut registry = McpRegistry::load_or_new(dir.path().join("mcp/servers.json")).unwrap();
        registry
            .add_server(McpServerConfig {
                name: "local".to_string(),
                url: format!("http://{addr}"),
                enabled: true,
                tools_allowlist: vec!["news.read".to_string()],
                timeout_sec: 3,
                budget_per_minute: 2,
            })
            .unwrap();

        let config = registry.get("local").unwrap();
        let tools = list_tools(&config).await.unwrap();
        assert_eq!(tools.len(), 1);

        let result = call_tool(
            &config,
            dir.path().join("mcp/usage.json"),
            "news.read",
            json!({"query":"btc"}),
        )
        .await
        .unwrap();
        assert_eq!(result["headline"], "ok:news.read");

        let second = call_tool(
            &config,
            dir.path().join("mcp/usage.json"),
            "news.read",
            json!({"query":"eth"}),
        )
        .await;
        assert!(second.is_ok());

        let third = call_tool(
            &config,
            dir.path().join("mcp/usage.json"),
            "news.read",
            json!({"query":"sol"}),
        )
        .await;
        assert!(third.is_err());

        handle.abort();
    }
}
