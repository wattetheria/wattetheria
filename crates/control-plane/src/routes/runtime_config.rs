use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::{Json, response::Response};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;
use wattetheria_kernel::brain::AgentRuntimeAdapter;

const DEFAULT_OPENAI_COMPATIBLE_API_KEY_ENV: &str = "WATTETHERIA_BRAIN_API_KEY";
const ENV_BRAIN_API_KEY_ENV: &str = "WATTETHERIA_BRAIN_API_KEY_ENV";
const ENV_BRAIN_API_KEY: &str = "WATTETHERIA_BRAIN_API_KEY";
const ENV_BRAIN_RUNTIME_ADAPTER: &str = "WATTETHERIA_BRAIN_RUNTIME_ADAPTER";
const ENV_BRAIN_SESSION_HEADER_NAME: &str = "WATTETHERIA_BRAIN_SESSION_HEADER_NAME";
const DEFAULT_RUNTIME_BASE_URL: &str = "http://host.docker.internal:8642/v1";

fn supported_runtime_adapters() -> Value {
    serde_json::to_value(AgentRuntimeAdapter::supported_metadata()).unwrap_or_else(|_| json!([]))
}

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
            runtime_adapter,
            ..
        } => {
            let adapter = AgentRuntimeAdapter::infer(base_url, model, runtime_adapter.as_ref());
            format!("adapter={} model={model} url={base_url}", adapter.key())
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

fn env_line_value(lines: &[String], key: &str) -> Option<String> {
    lines.iter().find_map(|line| {
        let (existing_key, value) = line.split_once('=')?;
        if existing_key.trim() == key {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

fn brain_api_key_present(
    env_path: &Path,
    config: &wattetheria_kernel::brain::BrainProviderConfig,
) -> bool {
    let wattetheria_kernel::brain::BrainProviderConfig::OpenaiCompatible { api_key_env, .. } =
        config
    else {
        return false;
    };
    let key_name = api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_OPENAI_COMPATIBLE_API_KEY_ENV);
    if std::env::var(key_name)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return true;
    }
    let Ok(raw) = fs::read_to_string(env_path) else {
        return false;
    };
    let lines = raw.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
    env_line_value(&lines, key_name).is_some_and(|value| !value.trim().is_empty())
}

fn normalize_brain_config_request(mut body: Value) -> anyhow::Result<(Value, Option<String>)> {
    let api_key = body
        .as_object_mut()
        .and_then(|object| object.remove("api_key"))
        .and_then(|value| value.as_str().map(str::trim).map(str::to_string))
        .filter(|value| !value.is_empty());
    if body
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "openai-compatible")
    {
        let object = body
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("brain config body must be a JSON object"))?;
        object.insert(
            "api_key_env".to_string(),
            Value::String(DEFAULT_OPENAI_COMPATIBLE_API_KEY_ENV.to_string()),
        );
        if let Some(adapter_value) = object.remove("adapter") {
            let adapter = adapter_value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("adapter must be a string"))?;
            let session_header = object
                .get("session_header_name")
                .or_else(|| object.get("custom_session_header_name"))
                .and_then(Value::as_str);
            object.insert(
                "runtime_adapter".to_string(),
                serde_json::to_value(AgentRuntimeAdapter::from_key(adapter, session_header)?)?,
            );
        }
        object.remove("session_header_name");
        object.remove("custom_session_header_name");
    }
    Ok((body, api_key))
}

fn persist_brain_config_to_env(
    env_path: &Path,
    config: &wattetheria_kernel::brain::BrainProviderConfig,
    api_key: Option<&str>,
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
    let (provider_kind, base_url, model, api_key_env, runtime_adapter, session_header_name) =
        match config {
            wattetheria_kernel::brain::BrainProviderConfig::Rules => (
                "rules".to_string(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ),
            wattetheria_kernel::brain::BrainProviderConfig::Ollama { base_url, model } => (
                "ollama".to_string(),
                base_url.clone(),
                model.clone(),
                String::new(),
                String::new(),
                String::new(),
            ),
            wattetheria_kernel::brain::BrainProviderConfig::OpenaiCompatible {
                base_url,
                model,
                api_key_env,
                runtime_adapter,
            } => (
                "openai-compatible".to_string(),
                base_url.clone(),
                model.clone(),
                api_key_env.clone().unwrap_or_default(),
                AgentRuntimeAdapter::infer(base_url, model, runtime_adapter.as_ref())
                    .key()
                    .to_string(),
                runtime_adapter
                    .as_ref()
                    .map(AgentRuntimeAdapter::session_header_name)
                    .unwrap_or_default()
                    .to_string(),
            ),
        };
    upsert_env_line(
        &mut lines,
        "WATTETHERIA_BRAIN_PROVIDER_KIND",
        &provider_kind,
    );
    upsert_env_line(&mut lines, "WATTETHERIA_BRAIN_BASE_URL", &base_url);
    upsert_env_line(&mut lines, "WATTETHERIA_BRAIN_MODEL", &model);
    upsert_env_line(&mut lines, ENV_BRAIN_API_KEY_ENV, &api_key_env);
    upsert_env_line(&mut lines, ENV_BRAIN_RUNTIME_ADAPTER, &runtime_adapter);
    upsert_env_line(
        &mut lines,
        ENV_BRAIN_SESSION_HEADER_NAME,
        &session_header_name,
    );
    if let Some(api_key) = api_key.map(str::trim).filter(|value| !value.is_empty()) {
        upsert_env_line(&mut lines, ENV_BRAIN_API_KEY, api_key);
    }
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
    let has_api_key = brain_api_key_present(&env_path, &config);
    let (runtime_adapter, session_header_name) = match &config {
        wattetheria_kernel::brain::BrainProviderConfig::OpenaiCompatible {
            base_url,
            model,
            runtime_adapter,
            ..
        } => {
            let adapter = AgentRuntimeAdapter::infer(base_url, model, runtime_adapter.as_ref());
            (
                Some(adapter.key().to_string()),
                Some(adapter.session_header_name().to_string()),
            )
        }
        _ => (None, None),
    };
    Json(json!({
        "ok": true,
        "config": config,
        "label": label,
        "env_path": env_path.display().to_string(),
        "has_api_key": has_api_key,
        "runtime_adapter": runtime_adapter,
        "session_header_name": session_header_name,
        "default_runtime_base_url": DEFAULT_RUNTIME_BASE_URL,
        "supported_runtime_adapters": supported_runtime_adapters(),
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
    let (body, api_key) = match normalize_brain_config_request(body) {
        Ok(normalized) => normalized,
        Err(error) => return bad_request(format!("invalid brain config: {error}")),
    };
    let config: wattetheria_kernel::brain::BrainProviderConfig = match serde_json::from_value(body)
    {
        Ok(config) => config,
        Err(error) => return bad_request(format!("invalid brain config: {error}")),
    };
    let engine = wattetheria_kernel::brain::BrainEngine::from_config(&config);
    let label = brain_provider_label(&config);
    let env_path = deploy_env_path(&state.data_dir);

    if let Err(error) = persist_brain_config_to_env(&env_path, &config, api_key.as_deref()) {
        return internal_error(&error);
    }
    if let Err(error) = persist_brain_config_to_json(&state.data_dir, &config) {
        return internal_error(&error);
    }

    let has_api_key = brain_api_key_present(&env_path, &config);
    *state.brain_config.write().await = config;
    *state.brain_engine.write().await = engine;

    Json(json!({
        "ok": true,
        "status": "updated",
        "label": label,
        "env_path": env_path.display().to_string(),
        "has_api_key": has_api_key,
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
