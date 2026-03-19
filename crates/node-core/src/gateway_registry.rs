use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::collections::{BTreeSet, VecDeque};
use std::time::Duration;

const DISCOVERY_ROLE: &str = "ingest";

#[derive(Debug, Clone, Deserialize)]
pub struct BootstrapRegistryEntry {
    pub registry_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveredGatewayEntry {
    pub gateway: GatewayRegistryEntry,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayRegistryEntry {
    pub base_url: String,
    pub roles: Vec<String>,
    pub allows_public_ingest: bool,
}

#[derive(Debug, Clone)]
pub struct GatewayRegistryClient {
    client: Client,
}

impl GatewayRegistryClient {
    pub fn new(timeout_secs: u64) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .context("build gateway registry client")?;
        Ok(Self { client })
    }

    pub async fn fetch_bootstrap_registries(&self, registry_url: &str) -> Result<Vec<String>> {
        let entries = self
            .client
            .get(normalized_registry_bootstrap_url(registry_url))
            .send()
            .await
            .context("request gateway bootstrap registries")?
            .error_for_status()
            .context("gateway bootstrap registries returned error status")?
            .json::<Vec<BootstrapRegistryEntry>>()
            .await
            .context("parse gateway bootstrap registries")?;
        Ok(entries
            .into_iter()
            .map(|entry| normalized_registry_base_url(&entry.registry_url))
            .collect())
    }

    pub async fn fetch_discovered_gateways(&self, registry_url: &str) -> Result<Vec<String>> {
        let entries = self
            .client
            .get(normalized_registry_discovery_url(registry_url))
            .query(&[("role", DISCOVERY_ROLE)])
            .send()
            .await
            .context("request discovered gateways")?
            .error_for_status()
            .context("gateway discovery returned error status")?
            .json::<Vec<DiscoveredGatewayEntry>>()
            .await
            .context("parse discovered gateways")?;
        Ok(entries
            .into_iter()
            .filter(|entry| {
                entry.gateway.allows_public_ingest
                    && entry
                        .gateway
                        .roles
                        .iter()
                        .any(|role| role == DISCOVERY_ROLE)
            })
            .map(|entry| entry.gateway.base_url)
            .collect())
    }
}

pub async fn discover_gateways(
    client: &GatewayRegistryClient,
    seed_registry_urls: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut registry_set = BTreeSet::new();
    let mut registry_queue = VecDeque::new();
    for registry_url in seed_registry_urls {
        let normalized = normalized_registry_base_url(registry_url);
        if registry_set.insert(normalized.clone()) {
            registry_queue.push_back(normalized);
        }
    }

    while let Some(registry_url) = registry_queue.pop_front() {
        let Ok(bootstrap_registries) = client.fetch_bootstrap_registries(&registry_url).await
        else {
            continue;
        };
        for bootstrap_url in bootstrap_registries {
            if registry_set.insert(bootstrap_url.clone()) {
                registry_queue.push_back(bootstrap_url);
            }
        }
    }

    let registry_urls = registry_set.into_iter().collect::<Vec<_>>();
    let mut gateway_urls = BTreeSet::new();
    for registry_url in &registry_urls {
        if let Ok(discovered) = client.fetch_discovered_gateways(registry_url).await {
            for gateway_url in discovered {
                gateway_urls.insert(gateway_url.trim_end_matches('/').to_string());
            }
        }
    }
    (registry_urls, gateway_urls.into_iter().collect())
}

fn normalized_registry_base_url(registry_url: &str) -> String {
    let trimmed = registry_url.trim_end_matches('/');
    for suffix in [
        "/api/registry/gateways/register",
        "/api/registry/gateways",
        "/api/registry/bootstrap",
        "/api/registry/discovery",
    ] {
        if let Some(prefix) = trimmed.strip_suffix(suffix) {
            return prefix.trim_end_matches('/').to_string();
        }
    }
    trimmed.to_string()
}

fn normalized_registry_bootstrap_url(registry_url: &str) -> String {
    format!(
        "{}/api/registry/bootstrap",
        normalized_registry_base_url(registry_url)
    )
}

fn normalized_registry_discovery_url(registry_url: &str) -> String {
    format!(
        "{}/api/registry/discovery",
        normalized_registry_base_url(registry_url)
    )
}

#[cfg(test)]
mod tests {
    use super::{
        GatewayRegistryClient, discover_gateways, normalized_registry_base_url,
        normalized_registry_bootstrap_url, normalized_registry_discovery_url,
    };
    use axum::{Json, Router, routing::get};
    use serde_json::json;
    use tokio::net::TcpListener;

    #[test]
    fn normalizes_registry_base_urls() {
        assert_eq!(
            normalized_registry_base_url("https://gw.example"),
            "https://gw.example"
        );
        assert_eq!(
            normalized_registry_base_url("https://gw.example/api/registry/bootstrap"),
            "https://gw.example"
        );
        assert_eq!(
            normalized_registry_base_url("https://gw.example/api/registry/discovery"),
            "https://gw.example"
        );
        assert_eq!(
            normalized_registry_base_url("https://gw.example/api/registry/gateways"),
            "https://gw.example"
        );
        assert_eq!(
            normalized_registry_base_url("https://gw.example/api/registry/gateways/register"),
            "https://gw.example"
        );
    }

    #[test]
    fn normalizes_registry_endpoint_urls() {
        assert_eq!(
            normalized_registry_bootstrap_url("https://gw.example"),
            "https://gw.example/api/registry/bootstrap"
        );
        assert_eq!(
            normalized_registry_bootstrap_url("https://gw.example/api/registry/discovery"),
            "https://gw.example/api/registry/bootstrap"
        );
        assert_eq!(
            normalized_registry_discovery_url("https://gw.example"),
            "https://gw.example/api/registry/discovery"
        );
        assert_eq!(
            normalized_registry_discovery_url("https://gw.example/api/registry/gateways/register"),
            "https://gw.example/api/registry/discovery"
        );
    }

    #[tokio::test]
    async fn discovers_gateways_from_bootstrap_registries() {
        async fn empty_bootstrap() -> Json<serde_json::Value> {
            Json(json!([]))
        }

        async fn empty_discovery() -> Json<serde_json::Value> {
            Json(json!([]))
        }

        async fn discovered_gateways() -> Json<serde_json::Value> {
            Json(json!([{
                "source_registry_url": "http://127.0.0.1:42082/api/registry/gateways",
                "gateway": {
                    "gateway_id": "gw-remote-1",
                    "display_name": "Remote Gateway",
                    "base_url": "https://gw-remote-1.example",
                    "public_key": "pub-1",
                    "region": "ap-southeast",
                    "operator_id": "operator-1",
                    "roles": ["ingest", "query"],
                    "supported_endpoints": ["/api/network/status"],
                    "federation_peers": [],
                    "allows_public_ingest": true,
                    "manifest": {
                        "generated_at": 1,
                        "gateway_id": "gw-remote-1",
                        "display_name": "Remote Gateway",
                        "base_url": "https://gw-remote-1.example",
                        "public_key": "pub-1",
                        "region": "ap-southeast",
                        "operator_id": "operator-1",
                        "roles": ["ingest", "query"],
                        "supported_endpoints": ["/api/network/status"],
                        "federation_peers": [],
                        "allows_public_ingest": true
                    },
                    "manifest_signature": "sig-1",
                    "status": "approved",
                    "discovery_tier": "verified",
                    "review_reason": null,
                    "reviewed_at": null,
                    "reviewed_by": null,
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z"
                }
            }]))
        }

        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let app = Router::new()
                .route("/api/registry/bootstrap", get(empty_bootstrap))
                .route("/api/registry/discovery", get(discovered_gateways));
            axum::serve(upstream_listener, app).await.unwrap();
        });

        let seed_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let seed_addr = seed_listener.local_addr().unwrap();
        let upstream_registry_url = format!("http://{upstream_addr}");
        tokio::spawn(async move {
            let app = Router::new()
                .route(
                    "/api/registry/bootstrap",
                    get(move || {
                        let upstream_registry_url = upstream_registry_url.clone();
                        async move { Json(json!([{ "registry_url": upstream_registry_url }])) }
                    }),
                )
                .route("/api/registry/discovery", get(empty_discovery));
            axum::serve(seed_listener, app).await.unwrap();
        });

        let client = GatewayRegistryClient::new(5).unwrap();
        let (registries, gateways) =
            discover_gateways(&client, &[format!("http://{seed_addr}")]).await;

        assert_eq!(registries.len(), 2);
        assert!(
            registries
                .iter()
                .any(|url| url == &format!("http://{seed_addr}"))
        );
        assert!(
            registries
                .iter()
                .any(|url| url == &format!("http://{upstream_addr}"))
        );
        assert_eq!(gateways, vec!["https://gw-remote-1.example".to_string()]);
    }
}
