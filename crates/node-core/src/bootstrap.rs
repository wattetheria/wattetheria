use crate::cli::Cli;
use anyhow::{Context, Result, bail};
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;
use wattetheria_kernel::brain::BrainProviderConfig;

pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .without_time()
        .init();
}

pub fn parse_control_bind(value: &str) -> Result<SocketAddr> {
    value
        .parse()
        .with_context(|| format!("parse control plane bind address: {value}"))
}

pub fn resolve_brain_config(cli: &Cli) -> Result<BrainProviderConfig> {
    match cli.brain_provider_kind.as_str() {
        "rules" => Ok(BrainProviderConfig::Rules),
        "ollama" => Ok(BrainProviderConfig::Ollama {
            base_url: cli.brain_base_url.clone(),
            model: cli.brain_model.clone(),
        }),
        "openai-compatible" => Ok(BrainProviderConfig::OpenaiCompatible {
            base_url: cli.brain_base_url.clone(),
            model: cli.brain_model.clone(),
            api_key_env: cli.brain_api_key_env.clone(),
        }),
        other => {
            bail!("unsupported --brain-provider-kind: {other} (use rules|ollama|openai-compatible)")
        }
    }
}

pub fn load_or_create_control_token(path: PathBuf) -> Result<String> {
    if path.exists() {
        let token = std::fs::read_to_string(&path).context("read control token")?;
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let token = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create token directory")?;
    }
    std::fs::write(path, &token).context("write control token")?;
    Ok(token)
}
