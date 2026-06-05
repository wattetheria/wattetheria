use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, Method, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;

const DEFAULT_TIMEOUT_SEC: u64 = 120;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SettlementLayer {
    Web2,
    #[default]
    Web3,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SettlementRequest {
    #[serde(default)]
    pub layer: SettlementLayer,
    pub rail: String,
    #[serde(default)]
    pub request: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceNetInvokeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_context_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default)]
    pub confirm_risky: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_units: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<SettlementRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_envelope: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceNetGetAgentTaskRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_context_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceNetInvokeResponse {
    pub agent_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<SettlementRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_receipt: Option<Value>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServiceNetItemsResponse {
    items: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServiceNetListAgentsResponseBody {
    items: Vec<Value>,
    #[serde(default)]
    count: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    next_offset: Option<usize>,
    #[serde(default)]
    has_more: Option<bool>,
    #[serde(default)]
    known_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceNetOwnershipChallenge {
    pub challenge_id: Uuid,
    pub challenge: String,
    pub provider_id: String,
    pub provider_did: String,
}

#[derive(Debug, Clone)]
pub struct ServiceNetListAgentsResponse {
    pub items: Vec<Value>,
    pub count: usize,
    pub limit: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub has_more: bool,
    pub known_count: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ServiceNetClient {
    base_url: String,
    http_client: Client,
}

#[derive(Debug, Clone)]
pub struct ServiceNetClientError {
    status: Option<reqwest::StatusCode>,
    message: String,
}

impl ServiceNetClientError {
    #[must_use]
    pub fn status(&self) -> Option<reqwest::StatusCode> {
        self.status
    }
}

impl std::fmt::Display for ServiceNetClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for ServiceNetClientError {}

impl ServiceNetClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into().trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            bail!("servicenet base url cannot be empty");
        }
        let _ = Url::parse(&base_url).context("parse servicenet base url")?;
        let http_client = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SEC))
            .build()
            .context("build servicenet client")?;
        Ok(Self {
            base_url,
            http_client,
        })
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn list_agents(
        &self,
        limit: usize,
        offset: usize,
    ) -> std::result::Result<ServiceNetListAgentsResponse, ServiceNetClientError> {
        let mut url = self
            .endpoint(&["v1", "agents"])
            .map_err(|error| Self::client_error(&error))?;
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string())
            .append_pair("offset", &offset.to_string());
        let response: ServiceNetListAgentsResponseBody = self
            .request_json(Method::GET, url, Option::<&Value>::None)
            .await?;
        let count = response.count.unwrap_or(response.items.len());
        let limit = response.limit.unwrap_or(limit);
        let offset = response.offset.unwrap_or(offset);
        let next_offset = response.next_offset.or_else(|| {
            let next = offset.saturating_add(response.items.len());
            response.has_more.unwrap_or(false).then_some(next)
        });
        Ok(ServiceNetListAgentsResponse {
            items: response.items,
            count,
            limit,
            offset,
            next_offset,
            has_more: response.has_more.unwrap_or(next_offset.is_some()),
            known_count: response.known_count,
        })
    }

    pub async fn list_agent_health(
        &self,
    ) -> std::result::Result<Vec<Value>, ServiceNetClientError> {
        let response: ServiceNetItemsResponse = self
            .request_json(
                Method::GET,
                self.endpoint(&["v1", "health", "agents"])
                    .map_err(|error| Self::client_error(&error))?,
                Option::<&Value>::None,
            )
            .await?;
        Ok(response.items)
    }

    pub async fn list_agent_trust(&self) -> std::result::Result<Vec<Value>, ServiceNetClientError> {
        let response: ServiceNetItemsResponse = self
            .request_json(
                Method::GET,
                self.endpoint(&["v1", "trust", "agents"])
                    .map_err(|error| Self::client_error(&error))?,
                Option::<&Value>::None,
            )
            .await?;
        Ok(response.items)
    }

    pub async fn get_agent(
        &self,
        agent_id: &str,
    ) -> std::result::Result<Value, ServiceNetClientError> {
        self.request_json(
            Method::GET,
            self.endpoint(&["v1", "agents", agent_id])
                .map_err(|error| Self::client_error(&error))?,
            Option::<&Value>::None,
        )
        .await
    }

    pub async fn create_provider_ownership_challenge(
        &self,
        provider_did: &str,
        operation: &str,
    ) -> std::result::Result<ServiceNetOwnershipChallenge, ServiceNetClientError> {
        self.request_json(
            Method::POST,
            self.endpoint(&["v1", "providers", "ownership-challenges"])
                .map_err(|error| Self::client_error(&error))?,
            Some(&serde_json::json!({
                "provider_did": provider_did,
                "operation": operation,
            })),
        )
        .await
    }

    pub async fn register_provider(
        &self,
        request: &Value,
    ) -> std::result::Result<Value, ServiceNetClientError> {
        self.request_json(
            Method::POST,
            self.endpoint(&["v1", "providers", "register"])
                .map_err(|error| Self::client_error(&error))?,
            Some(request),
        )
        .await
    }

    pub async fn submit_agent(
        &self,
        request: &Value,
    ) -> std::result::Result<Value, ServiceNetClientError> {
        self.request_json(
            Method::POST,
            self.endpoint(&["v1", "agent-submissions"])
                .map_err(|error| Self::client_error(&error))?,
            Some(request),
        )
        .await
    }

    pub async fn unpublish_agent(
        &self,
        agent_id: &str,
        request: &Value,
    ) -> std::result::Result<Value, ServiceNetClientError> {
        self.request_json(
            Method::POST,
            self.endpoint(&["v1", "agents", agent_id, "unpublish"])
                .map_err(|error| Self::client_error(&error))?,
            Some(request),
        )
        .await
    }

    pub async fn invoke_agent(
        &self,
        agent_id: &str,
        request: &ServiceNetInvokeRequest,
    ) -> std::result::Result<ServiceNetInvokeResponse, ServiceNetClientError> {
        self.request_json(
            Method::POST,
            self.endpoint(&["v1", "agents", agent_id, "invoke"])
                .map_err(|error| Self::client_error(&error))?,
            Some(request),
        )
        .await
    }

    pub async fn invoke_agent_async(
        &self,
        agent_id: &str,
        request: &ServiceNetInvokeRequest,
    ) -> std::result::Result<ServiceNetInvokeResponse, ServiceNetClientError> {
        self.request_json(
            Method::POST,
            self.endpoint(&["v1", "agents", agent_id, "invoke-async"])
                .map_err(|error| Self::client_error(&error))?,
            Some(request),
        )
        .await
    }

    pub async fn get_agent_task(
        &self,
        agent_id: &str,
        task_id: &str,
        request: &ServiceNetGetAgentTaskRequest,
    ) -> std::result::Result<ServiceNetInvokeResponse, ServiceNetClientError> {
        self.request_json(
            Method::POST,
            self.endpoint(&["v1", "agents", agent_id, "tasks", task_id, "get"])
                .map_err(|error| Self::client_error(&error))?,
            Some(request),
        )
        .await
    }

    pub async fn get_receipt(
        &self,
        receipt_id: &Uuid,
    ) -> std::result::Result<Value, ServiceNetClientError> {
        self.request_json(
            Method::GET,
            self.endpoint(&["v1", "receipts", &receipt_id.to_string()])
                .map_err(|error| Self::client_error(&error))?,
            Option::<&Value>::None,
        )
        .await
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url> {
        let mut url = Url::parse(&self.base_url).context("parse servicenet base url")?;
        let mut path = url
            .path_segments_mut()
            .map_err(|()| anyhow!("servicenet base url cannot be a base without path segments"))?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
        drop(path);
        Ok(url)
    }

    fn client_error(error: &anyhow::Error) -> ServiceNetClientError {
        ServiceNetClientError {
            status: None,
            message: format!("{error:#}"),
        }
    }

    async fn request_json<T, B>(
        &self,
        method: Method,
        url: Url,
        body: Option<&B>,
    ) -> std::result::Result<T, ServiceNetClientError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let mut request = self.http_client.request(method, url.clone());
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("request servicenet {url}"))
            .map_err(|error| ServiceNetClientError {
                status: None,
                message: format!("{error:#}"),
            })?;
        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ServiceNetClientError {
                status: Some(status),
                message: format!("servicenet {url} returned {status}: {error_body}"),
            });
        }
        response
            .json::<T>()
            .await
            .with_context(|| format!("parse servicenet response {url}"))
            .map_err(|error| ServiceNetClientError {
                status: Some(status),
                message: format!("{error:#}"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invoke_request_round_trips_settlement() {
        let request = ServiceNetInvokeRequest {
            message: Some("buy flight".into()),
            input: json!({"route": "SYD-LAX"}),
            settlement: Some(SettlementRequest {
                layer: SettlementLayer::Web3,
                rail: "x402".into(),
                request: json!({
                    "protocol": "x402",
                    "payment_account_ref": "payment-account-123",
                }),
            }),
            ..ServiceNetInvokeRequest::default()
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["settlement"]["rail"].as_str(), Some("x402"));
        let round_tripped: ServiceNetInvokeRequest = serde_json::from_value(value).unwrap();
        assert_eq!(
            round_tripped
                .settlement
                .as_ref()
                .and_then(|value| value.request.get("payment_account_ref"))
                .and_then(Value::as_str),
            Some("payment-account-123")
        );
    }
}
