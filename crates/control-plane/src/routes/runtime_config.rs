use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::{Json, response::Response};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;

pub(crate) fn brain_provider_label(
    config: &wattetheria_kernel::brain::BrainProviderConfig,
) -> String {
    match config {
        wattetheria_kernel::brain::BrainProviderConfig::Rules => "rules".to_string(),
        wattetheria_kernel::brain::BrainProviderConfig::Ollama { base_url, model } => {
            format!("ollama model={model} url={base_url}")
        }
        wattetheria_kernel::brain::BrainProviderConfig::OpenaiCompatible {
            base_url,
            model,
            ..
        } => {
            format!("openai-compatible model={model} url={base_url}")
        }
    }
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
        .into_response()
}

fn deploy_env_path(data_dir: &Path) -> PathBuf {
    deploy_env_path_from_config(
        data_dir,
        std::env::var("WATTETHERIA_RUNTIME_ENV_FILE")
            .ok()
            .as_deref(),
    )
}

fn deploy_env_path_from_config(data_dir: &Path, configured: Option<&str>) -> PathBuf {
    let deploy_dir = data_dir.join("deploy");
    let configured = configured.map(str::trim).filter(|value| !value.is_empty());
    match configured {
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                deploy_dir.join(path)
            }
        }
        None => deploy_dir.join(".env"),
    }
}

fn upsert_env_line(lines: &mut Vec<String>, key: &str, value: &str) {
    let replacement = format!("{key}={value}");
    for line in lines.iter_mut() {
        if let Some((existing_key, _)) = line.split_once('=')
            && existing_key.trim() == key
        {
            *line = replacement;
            return;
        }
    }
    lines.push(replacement);
}

fn persist_brain_config_to_env(
    env_path: &Path,
    config: &wattetheria_kernel::brain::BrainProviderConfig,
) -> anyhow::Result<()> {
    if let Some(parent) = env_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut lines = if env_path.exists() {
        fs::read_to_string(env_path)?
            .lines()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let (provider_kind, base_url, model, api_key_env) = match config {
        wattetheria_kernel::brain::BrainProviderConfig::Rules => (
            "rules".to_string(),
            String::new(),
            String::new(),
            String::new(),
        ),
        wattetheria_kernel::brain::BrainProviderConfig::Ollama { base_url, model } => (
            "ollama".to_string(),
            base_url.clone(),
            model.clone(),
            String::new(),
        ),
        wattetheria_kernel::brain::BrainProviderConfig::OpenaiCompatible {
            base_url,
            model,
            api_key_env,
        } => (
            "openai-compatible".to_string(),
            base_url.clone(),
            model.clone(),
            api_key_env.clone().unwrap_or_default(),
        ),
    };
    upsert_env_line(
        &mut lines,
        "WATTETHERIA_BRAIN_PROVIDER_KIND",
        &provider_kind,
    );
    upsert_env_line(&mut lines, "WATTETHERIA_BRAIN_BASE_URL", &base_url);
    upsert_env_line(&mut lines, "WATTETHERIA_BRAIN_MODEL", &model);
    upsert_env_line(&mut lines, "WATTETHERIA_BRAIN_API_KEY_ENV", &api_key_env);
    let rendered = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    fs::write(env_path, rendered)?;
    Ok(())
}

fn persist_brain_config_to_json(
    data_dir: &Path,
    config: &wattetheria_kernel::brain::BrainProviderConfig,
) -> anyhow::Result<()> {
    let config_path = data_dir.join("config.json");
    let mut root = if config_path.exists() {
        serde_json::from_str::<Value>(&fs::read_to_string(&config_path)?)?
    } else {
        json!({})
    };
    root["brain_provider"] = serde_json::to_value(config)?;
    fs::write(config_path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

pub(crate) async fn brain_config_get(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let _auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let config = state.brain_config.read().await.clone();
    let label = brain_provider_label(&config);
    let env_path = deploy_env_path(&state.data_dir);
    Json(json!({
        "ok": true,
        "config": config,
        "label": label,
        "env_path": env_path.display().to_string(),
    }))
    .into_response()
}

pub(crate) async fn brain_config_put(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let _auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let config: wattetheria_kernel::brain::BrainProviderConfig = match serde_json::from_value(body)
    {
        Ok(config) => config,
        Err(error) => return bad_request(format!("invalid brain config: {error}")),
    };
    let engine = wattetheria_kernel::brain::BrainEngine::from_config(&config);
    let label = brain_provider_label(&config);
    let env_path = deploy_env_path(&state.data_dir);

    if let Err(error) = persist_brain_config_to_env(&env_path, &config) {
        return internal_error(&error);
    }
    if let Err(error) = persist_brain_config_to_json(&state.data_dir, &config) {
        return internal_error(&error);
    }

    *state.brain_config.write().await = config;
    *state.brain_engine.write().await = engine;

    Json(json!({
        "ok": true,
        "status": "updated",
        "label": label,
        "env_path": env_path.display().to_string(),
        "restart_required": true,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_env_path_defaults_to_data_dir_deploy_env() {
        assert_eq!(
            deploy_env_path_from_config(Path::new("/var/lib/wattetheria"), None),
            PathBuf::from("/var/lib/wattetheria/deploy/.env")
        );
    }

    #[test]
    fn deploy_env_path_preserves_absolute_runtime_env_file() {
        assert_eq!(
            deploy_env_path_from_config(
                Path::new("/var/lib/wattetheria"),
                Some("/var/lib/wattetheria-deploy/.env"),
            ),
            PathBuf::from("/var/lib/wattetheria-deploy/.env")
        );
    }
}
