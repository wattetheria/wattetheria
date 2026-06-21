use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client, Method, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

const DEFAULT_TIMEOUT_SEC: u64 = 120;
pub const MAX_SERVICENET_AGENT_NAME_CHARS: usize = 40;

pub fn validate_servicenet_agent_name(name: &str) -> Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("agent card `name` must not be empty");
    }
    if trimmed.chars().count() > MAX_SERVICENET_AGENT_NAME_CHARS {
        bail!("agent card `name` must be {MAX_SERVICENET_AGENT_NAME_CHARS} characters or less");
    }
    if trimmed.chars().any(char::is_control) {
        bail!("agent card `name` must not contain control characters");
    }
    Ok(())
}

pub fn normalize_service_address(raw: &str) -> Result<Option<String>> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Ok(None);
    }
    validate_service_address(&normalized)?;
    Ok(Some(normalized))
}

fn validate_service_address(service_address: &str) -> Result<()> {
    if service_address.len() > 128 {
        bail!("service_address must be 128 characters or less");
    }
    if service_address != service_address.to_ascii_lowercase() {
        bail!("service_address must be lowercase");
    }
    let (local, namespace) = service_address
        .split_once('@')
        .ok_or_else(|| anyhow!("service_address must use local@namespace format"))?;
    if namespace.contains('@') {
        bail!("service_address must contain exactly one @");
    }
    validate_service_address_label(local, "local")?;
    for segment in namespace.split('.') {
        validate_service_address_label(segment, "namespace")?;
    }
    Ok(())
}

#[must_use]
pub fn servicenet_payment_account_bindings(payment_account_binding: Option<&Value>) -> Vec<Value> {
    payment_account_binding
        .filter(|value| !value.is_null())
        .map(|value| vec![value.clone()])
        .unwrap_or_default()
}

#[must_use]
pub fn servicenet_agent_did_document(
    provider_did: &str,
    agent_id: &str,
    service_address: Option<&str>,
    payment_account_binding: Option<&Value>,
) -> Value {
    let document_id = payment_account_binding_agent_did(payment_account_binding)
        .unwrap_or(provider_did.to_owned());
    let mut aliases = Vec::new();
    if let Some(service_address) = service_address {
        aliases.push(Value::String(service_address.to_owned()));
    }
    let service_endpoint = service_address.map_or_else(
        || format!("wattetheria://servicenet/agents/{agent_id}"),
        |service_address| format!("wattetheria://servicenet/{service_address}"),
    );
    json!({
        "id": document_id,
        "alsoKnownAs": aliases,
        "service": [{
            "id": "#servicenet-agent",
            "type": "WattetheriaServiceNetAgent",
            "serviceEndpoint": service_endpoint,
        }],
        "payment_account_bindings": servicenet_payment_account_bindings(payment_account_binding),
    })
}

fn payment_account_binding_agent_did(payment_account_binding: Option<&Value>) -> Option<String> {
    let agent_did = payment_account_binding.and_then(|value| value.get("agent_did"))?;
    if let Some(agent_did) = agent_did.as_str() {
        return Some(agent_did.to_owned());
    }
    let method = agent_did.get("method").and_then(Value::as_str)?;
    let id = agent_did.get("id").and_then(Value::as_str)?;
    Some(format!("did:{method}:{id}"))
}

pub fn attach_servicenet_agent_did_document(
    agent_card: &mut Value,
    provider_did: &str,
    agent_id: &str,
    service_address: Option<&str>,
    payment_account_binding: Option<&Value>,
) {
    let payment_account_bindings = servicenet_payment_account_bindings(payment_account_binding);
    let did_document = servicenet_agent_did_document(
        provider_did,
        agent_id,
        service_address,
        payment_account_binding,
    );
    if let Some(object) = agent_card.as_object_mut() {
        object.insert(
            "payment_account_bindings".to_owned(),
            Value::Array(payment_account_bindings),
        );
        object.insert("didDocument".to_owned(), did_document);
    }
}

fn validate_service_address_label(label: &str, field: &str) -> Result<()> {
    if label.is_empty() {
        bail!("service_address {field} segment must not be empty");
    }
    if label.starts_with('-') || label.ends_with('-') {
        bail!("service_address {field} segment must not start or end with -");
    }
    if !label
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("service_address {field} segment must contain only lowercase letters, digits, and -");
    }
    Ok(())
}

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

    #[test]
    fn normalize_service_address_trims_and_lowercases() {
        assert_eq!(
            normalize_service_address("  Agent-Name@Wattetheria  ")
                .unwrap()
                .as_deref(),
            Some("agent-name@wattetheria")
        );
        assert_eq!(normalize_service_address("   ").unwrap(), None);
    }

    #[test]
    fn normalize_service_address_rejects_invalid_labels() {
        assert!(normalize_service_address("-bad@wattetheria").is_err());
        assert!(normalize_service_address("bad@wat..etheria").is_err());
        assert!(normalize_service_address("bad name@wattetheria").is_err());
    }

    #[test]
    fn servicenet_agent_did_document_uses_servicenet_address_not_runtime_endpoint() {
        let binding = json!({
            "agent_did": "did:key:z6MkWallet",
            "rail": "x402",
            "network": "base",
            "payment_address": "0x1111111111111111111111111111111111111111",
        });
        let document = servicenet_agent_did_document(
            "did:key:z6MkProvider",
            "agent-one",
            Some("dumpling@wattetheria"),
            Some(&binding),
        );

        assert_eq!(document["id"].as_str(), Some("did:key:z6MkWallet"));
        assert_eq!(
            document["alsoKnownAs"].as_array().unwrap(),
            &[json!("dumpling@wattetheria")]
        );
        assert_eq!(
            document["service"][0]["serviceEndpoint"].as_str(),
            Some("wattetheria://servicenet/dumpling@wattetheria")
        );
        assert_eq!(document["payment_account_bindings"][0], binding);
    }

    #[test]
    fn validate_servicenet_agent_name_rejects_unsafe_names() {
        assert!(validate_servicenet_agent_name("Console Agent").is_ok());
        assert!(validate_servicenet_agent_name("饺子 Agent").is_ok());
        assert!(validate_servicenet_agent_name("   ").is_err());
        assert!(validate_servicenet_agent_name("Bad\u{0007}Name").is_err());
        assert!(
            validate_servicenet_agent_name(&"名".repeat(MAX_SERVICENET_AGENT_NAME_CHARS + 1))
                .is_err()
        );
    }
}
