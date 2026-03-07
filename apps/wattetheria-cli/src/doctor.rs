use crate::config::{
    LocalConfig, can_write_storage, check_control_plane, fetch_server_timestamp, read_config,
    read_control_token,
};
use anyhow::{Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use wattetheria_kernel::brain::BrainEngine;
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::mcp::McpRegistry;

#[derive(Debug, Serialize)]
pub(crate) struct DoctorCheck {
    name: String,
    status: String,
    detail: String,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    data_dir: String,
    overall: String,
    checks: Vec<DoctorCheck>,
}

pub(crate) async fn run_doctor(
    data_dir: &Path,
    endpoint_override: Option<&str>,
    brain_check: bool,
) -> Result<()> {
    let config = read_config(data_dir).unwrap_or_default();
    let endpoint = endpoint_override.map_or_else(
        || config.control_plane_endpoint.clone(),
        ToString::to_string,
    );

    let mut checks = Vec::new();

    push_check(
        &mut checks,
        "identity",
        Identity::load(data_dir.join("identity.json")).is_ok(),
        "identity file and keypair are valid",
        "identity file missing or invalid",
    );
    append_signing_check(&mut checks, data_dir.join("identity.json"));
    append_network_config_check(&mut checks, &config);
    append_event_log_check(&mut checks, data_dir.join("events.jsonl"));

    push_check(
        &mut checks,
        "storage",
        can_write_storage(data_dir).is_ok(),
        "data directory writable",
        "cannot write to data directory",
    );

    let token = read_control_token(data_dir.join("control.token"));
    push_check(
        &mut checks,
        "control_token",
        token.is_ok(),
        "control token is available",
        "control token missing",
    );

    append_control_plane_checks(&mut checks, &endpoint, token).await;
    append_mcp_registry_check(&mut checks, data_dir);
    append_provider_checks(&mut checks, &config, brain_check).await;
    finalize_doctor_report(data_dir, checks)
}

fn append_mcp_registry_check(checks: &mut Vec<DoctorCheck>, data_dir: &Path) {
    let registry_path = data_dir.join("mcp/servers.json");
    match McpRegistry::load_or_new(registry_path) {
        Ok(registry) => {
            let total = registry.list().len();
            checks.push(DoctorCheck {
                name: "mcp_registry".to_string(),
                status: if total == 0 {
                    "warn".to_string()
                } else {
                    "ok".to_string()
                },
                detail: format!("configured MCP servers: {total}"),
            });
        }
        Err(error) => checks.push(DoctorCheck {
            name: "mcp_registry".to_string(),
            status: "fail".to_string(),
            detail: error.to_string(),
        }),
    }
}

fn append_signing_check(checks: &mut Vec<DoctorCheck>, identity_path: PathBuf) {
    match Identity::load(identity_path) {
        Ok(identity) => {
            let probe = serde_json::json!({"probe":"doctor_signing"});
            match wattetheria_kernel::signing::sign_payload(&probe, &identity).and_then(
                |signature| {
                    wattetheria_kernel::signing::verify_payload(
                        &probe,
                        &signature,
                        &identity.agent_id,
                    )
                },
            ) {
                Ok(true) => checks.push(DoctorCheck {
                    name: "signing".to_string(),
                    status: "ok".to_string(),
                    detail: "sign + verify roundtrip passed".to_string(),
                }),
                Ok(false) => checks.push(DoctorCheck {
                    name: "signing".to_string(),
                    status: "fail".to_string(),
                    detail: "signature verification returned false".to_string(),
                }),
                Err(error) => checks.push(DoctorCheck {
                    name: "signing".to_string(),
                    status: "fail".to_string(),
                    detail: error.to_string(),
                }),
            }
        }
        Err(error) => checks.push(DoctorCheck {
            name: "signing".to_string(),
            status: "fail".to_string(),
            detail: error.to_string(),
        }),
    }
}

fn append_network_config_check(checks: &mut Vec<DoctorCheck>, config: &LocalConfig) {
    let endpoint_ok = reqwest::Url::parse(&config.control_plane_endpoint).is_ok();
    checks.push(DoctorCheck {
        name: "network_endpoint".to_string(),
        status: if endpoint_ok {
            "ok".to_string()
        } else {
            "fail".to_string()
        },
        detail: if endpoint_ok {
            format!(
                "control plane endpoint is valid: {}",
                config.control_plane_endpoint
            )
        } else {
            format!(
                "invalid control plane endpoint: {}",
                config.control_plane_endpoint
            )
        },
    });

    let bind = config.control_plane_bind.trim();
    let status = if bind.starts_with("127.") || bind.starts_with("localhost") {
        "warn"
    } else {
        "ok"
    };
    let detail = if status == "warn" {
        format!("control plane bind is local-only ({bind}); NAT reachability is limited")
    } else {
        format!("control plane bind allows remote reachability checks ({bind})")
    };
    checks.push(DoctorCheck {
        name: "nat_reachability_hint".to_string(),
        status: status.to_string(),
        detail,
    });
}

fn append_event_log_check(checks: &mut Vec<DoctorCheck>, event_path: PathBuf) {
    match EventLog::new(event_path) {
        Ok(log) => match log.verify_chain() {
            Ok((true, _)) => checks.push(DoctorCheck {
                name: "event_log".to_string(),
                status: "ok".to_string(),
                detail: "hash chain verified".to_string(),
            }),
            Ok((false, reason)) => checks.push(DoctorCheck {
                name: "event_log".to_string(),
                status: "fail".to_string(),
                detail: reason.unwrap_or_else(|| "hash chain invalid".to_string()),
            }),
            Err(error) => checks.push(DoctorCheck {
                name: "event_log".to_string(),
                status: "fail".to_string(),
                detail: error.to_string(),
            }),
        },
        Err(error) => checks.push(DoctorCheck {
            name: "event_log".to_string(),
            status: "fail".to_string(),
            detail: error.to_string(),
        }),
    }
}

async fn append_control_plane_checks(
    checks: &mut Vec<DoctorCheck>,
    endpoint: &str,
    token: Result<String>,
) {
    match token {
        Ok(token) => {
            if let Err(error) = check_control_plane(endpoint, &token).await {
                checks.push(DoctorCheck {
                    name: "control_plane_health".to_string(),
                    status: "fail".to_string(),
                    detail: error.to_string(),
                });
                return;
            }

            checks.push(DoctorCheck {
                name: "control_plane_health".to_string(),
                status: "ok".to_string(),
                detail: format!("reachable at {endpoint}"),
            });

            if let Ok(server_ts) = fetch_server_timestamp(endpoint).await {
                let drift = (chrono::Utc::now().timestamp() - server_ts).abs();
                checks.push(DoctorCheck {
                    name: "time_drift".to_string(),
                    status: if drift <= 120 {
                        "ok".to_string()
                    } else {
                        "fail".to_string()
                    },
                    detail: format!("clock drift: {drift}s"),
                });
            }
        }
        Err(_) => checks.push(DoctorCheck {
            name: "control_plane_health".to_string(),
            status: "fail".to_string(),
            detail: "token unavailable, skipping health check".to_string(),
        }),
    }
}

async fn append_provider_checks(
    checks: &mut Vec<DoctorCheck>,
    config: &LocalConfig,
    brain_check: bool,
) {
    let provider_name = match &config.brain_provider {
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
    };

    checks.push(DoctorCheck {
        name: "brain_provider".to_string(),
        status: "ok".to_string(),
        detail: provider_name,
    });

    if brain_check {
        let engine = BrainEngine::from_config(&config.brain_provider);
        match engine.doctor().await {
            Ok(status) => checks.push(DoctorCheck {
                name: "brain_health".to_string(),
                status: "ok".to_string(),
                detail: status,
            }),
            Err(error) => checks.push(DoctorCheck {
                name: "brain_health".to_string(),
                status: "fail".to_string(),
                detail: error.to_string(),
            }),
        }
    }
}

fn finalize_doctor_report(data_dir: &Path, checks: Vec<DoctorCheck>) -> Result<()> {
    let has_fail = checks.iter().any(|check| check.status == "fail");
    let report = DoctorReport {
        data_dir: data_dir.display().to_string(),
        overall: if has_fail {
            "fail".to_string()
        } else {
            "ok".to_string()
        },
        checks,
    };

    println!("{}", serde_json::to_string_pretty(&report)?);

    if has_fail {
        bail!("doctor detected failing checks");
    }

    Ok(())
}

fn push_check(
    checks: &mut Vec<DoctorCheck>,
    name: &str,
    condition: bool,
    ok_detail: &str,
    fail_detail: &str,
) {
    checks.push(DoctorCheck {
        name: name.to_string(),
        status: if condition {
            "ok".to_string()
        } else {
            "fail".to_string()
        },
        detail: if condition {
            ok_detail.to_string()
        } else {
            fail_detail.to_string()
        },
    });
}
