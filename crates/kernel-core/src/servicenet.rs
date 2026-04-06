use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, Method, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;

const DEFAULT_TIMEOUT_SEC: u64 = 15;

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
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServiceNetItemsResponse {
    items: Vec<Value>,
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

    pub async fn list_agents(&self) -> std::result::Result<Vec<Value>, ServiceNetClientError> {
        let response: ServiceNetItemsResponse = self
            .request_json(
                Method::GET,
                self.endpoint(&["v1", "agents"])
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
