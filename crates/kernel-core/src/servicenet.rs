use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use reqwest::{Client, Method, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;
use watt_did::{Did, DidKey, DidKeyPublicKey};

mod direct;
mod publication;
mod publisher_state;

pub use publication::{
    PreparedServiceAgentPublication, ServiceAgentPublicationInput,
    prepare_service_agent_publication, service_agent_card_requires_auth,
    submit_service_agent_publication,
};
pub use publisher_state::{
    CustomizedAgentProtocol, ServiceAgentExecution, ServiceNetConnectionMode,
    ServiceNetPublisherRegistration, ServiceNetPublisherState,
    find_servicenet_publisher_registration, load_servicenet_publisher_state,
    rollback_servicenet_publisher_registration, save_servicenet_publisher_state,
    stage_servicenet_publisher_registration, upsert_servicenet_publisher_registration,
};

const DEFAULT_TIMEOUT_SEC: u64 = 120;
const SERVICE_IDENTITY_CACHE_TTL: Duration = Duration::from_mins(5);
const SERVICE_IDENTITY_CACHE_MAX_ENTRIES: usize = 4_096;
const SERVICE_RESPONSE_NONCE_CACHE_TTL: Duration = Duration::from_mins(5);
const SERVICE_RESPONSE_NONCE_CACHE_MAX_ENTRIES: usize = 262_144;
const SERVICE_RESPONSE_SIGNATURE_PROTOCOL: &str = "wattetheria.servicenet.response.v1";
const SERVICE_RESPONSE_MAX_CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;
const SERVICE_AGENT_GET_TASK_DEFAULT_HISTORY_LENGTH: u32 = 10;
pub const SERVICENET_A2A_V1_PROTOCOL: &str = "a2a_v1";
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

pub fn attach_service_agent_payment_binding(
    agent_card: &mut Value,
    payment_account_binding: Option<&Value>,
) {
    let payment_account_bindings = servicenet_payment_account_bindings(payment_account_binding);
    attach_servicenet_payment_discovery_bindings(agent_card, &payment_account_bindings);
}

fn attach_servicenet_payment_discovery_bindings(
    agent_card: &mut Value,
    payment_account_bindings: &[Value],
) {
    if payment_account_bindings.is_empty() {
        return;
    }
    let Some(extensions) = agent_card
        .get_mut("capabilities")
        .and_then(|capabilities| capabilities.get_mut("extensions"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    for extension in extensions {
        if !extension_has_pay_to_accept(extension) {
            continue;
        }
        let Some(params) = extension.get_mut("params").and_then(Value::as_object_mut) else {
            continue;
        };
        params.insert(
            "payment_account_bindings".to_owned(),
            Value::Array(payment_account_bindings.to_vec()),
        );
    }
}

fn extension_has_pay_to_accept(extension: &Value) -> bool {
    extension
        .get("params")
        .and_then(|params| params.get("accepts"))
        .and_then(Value::as_array)
        .is_some_and(|accepts| {
            accepts.iter().any(|accept| {
                accept
                    .get("payTo")
                    .and_then(Value::as_str)
                    .is_some_and(|pay_to| !pay_to.trim().is_empty())
            })
        })
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_signature: Option<ServiceNetServiceAgentSignature>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceNetServiceAgentSignature {
    pub protocol: String,
    pub service_did: String,
    pub agent_id: String,
    pub verification_method: String,
    pub request_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_nonce: Option<String>,
    pub result_digest: String,
    pub nonce: String,
    pub issued_at_ms: u64,
    pub signature: String,
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
    service_identity_cache: Arc<Mutex<HashMap<String, CachedServiceIdentity>>>,
    service_response_nonce_cache: Arc<Mutex<HashMap<String, Instant>>>,
}

#[derive(Debug, Clone)]
struct CachedServiceIdentity {
    record: Value,
    expires_at: Instant,
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

    fn local(error: impl std::fmt::Display) -> Self {
        Self {
            status: None,
            message: error.to_string(),
        }
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
            service_identity_cache: Arc::new(Mutex::new(HashMap::new())),
            service_response_nonce_cache: Arc::new(Mutex::new(HashMap::new())),
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
        let record = self
            .request_json(
                Method::GET,
                self.endpoint(&["v1", "agents", agent_id])
                    .map_err(|error| Self::client_error(&error))?,
                Option::<&Value>::None,
            )
            .await?;
        self.cache_service_identity(agent_id, &record)
            .map_err(|error| Self::client_error(&error))?;
        Ok(record)
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
        let identity = self.resolve_service_identity(agent_id).await?;
        if direct::uses_wattetheria_direct(&identity) {
            return direct::invoke_agent(self, agent_id, request, &identity).await;
        }
        let response = self
            .request_json(
                Method::POST,
                self.endpoint(&["v1", "agents", agent_id, "invoke"])
                    .map_err(|error| Self::client_error(&error))?,
                Some(request),
            )
            .await?;
        let expected_request_digest = request
            .agent_envelope
            .as_ref()
            .and_then(|envelope| envelope.pointer("/extensions/request_digest"))
            .and_then(Value::as_str);
        let expected_request_nonce = request
            .agent_envelope
            .as_ref()
            .and_then(|envelope| envelope.pointer("/extensions/nonce"))
            .and_then(Value::as_str);
        verify_service_agent_invoke_response(
            &identity,
            &response,
            expected_request_digest,
            expected_request_nonce,
            true,
        )
        .map_err(|error| Self::client_error(&error))?;
        self.record_service_response_nonce(&response)
            .map_err(|error| Self::client_error(&error))?;
        Ok(response)
    }

    pub async fn invoke_agent_async(
        &self,
        agent_id: &str,
        request: &ServiceNetInvokeRequest,
    ) -> std::result::Result<ServiceNetInvokeResponse, ServiceNetClientError> {
        let identity = self.resolve_service_identity(agent_id).await?;
        if direct::uses_wattetheria_direct(&identity) {
            return Err(Self::client_error(&anyhow!(
                "wattetheria_direct agents do not support ServiceNet invoke-async; use synchronous direct invocation"
            )));
        }
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
        let identity = self.resolve_service_identity(agent_id).await?;
        if direct::uses_wattetheria_direct(&identity) {
            return Err(Self::client_error(&anyhow!(
                "wattetheria_direct agents do not support ServiceNet task polling"
            )));
        }
        let expected_request_digest = service_agent_task_request_digest(task_id, request)
            .map_err(|error| Self::client_error(&error))?;
        let response = self
            .request_json(
                Method::POST,
                self.endpoint(&["v1", "agents", agent_id, "tasks", task_id, "get"])
                    .map_err(|error| Self::client_error(&error))?,
                Some(request),
            )
            .await?;
        verify_service_agent_invoke_response(
            &identity,
            &response,
            Some(&expected_request_digest),
            None,
            true,
        )
        .map_err(|error| Self::client_error(&error))?;
        self.record_service_response_nonce(&response)
            .map_err(|error| Self::client_error(&error))?;
        Ok(response)
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

    async fn resolve_service_identity(
        &self,
        agent_id: &str,
    ) -> std::result::Result<Value, ServiceNetClientError> {
        let now = Instant::now();
        if let Some(record) = self
            .service_identity_cache
            .lock()
            .map_err(|_| Self::client_error(&anyhow!("ServiceNet identity cache lock poisoned")))?
            .get(agent_id)
            .filter(|cached| cached.expires_at > now)
            .map(|cached| cached.record.clone())
        {
            return Ok(record);
        }
        self.get_agent(agent_id).await
    }

    fn cache_service_identity(&self, agent_id: &str, record: &Value) -> Result<()> {
        let now = Instant::now();
        let mut cache = self
            .service_identity_cache
            .lock()
            .map_err(|_| anyhow!("ServiceNet identity cache lock poisoned"))?;
        cache.retain(|_, cached| cached.expires_at > now);
        if cache.len() >= SERVICE_IDENTITY_CACHE_MAX_ENTRIES && !cache.contains_key(agent_id) {
            let oldest = cache
                .iter()
                .min_by_key(|(_, cached)| cached.expires_at)
                .map(|(agent_id, _)| agent_id.clone());
            if let Some(oldest) = oldest {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            agent_id.to_owned(),
            CachedServiceIdentity {
                record: record.clone(),
                expires_at: now + SERVICE_IDENTITY_CACHE_TTL,
            },
        );
        Ok(())
    }

    fn record_service_response_nonce(&self, response: &ServiceNetInvokeResponse) -> Result<()> {
        let signature = response
            .service_signature
            .as_ref()
            .context("ServiceNet response is missing the Service Agent signature")?;
        let now = Instant::now();
        let key = format!("{}:{}", signature.service_did, signature.nonce);
        let mut cache = self
            .service_response_nonce_cache
            .lock()
            .map_err(|_| anyhow!("ServiceNet response nonce cache lock poisoned"))?;
        cache.retain(|_, expires_at| *expires_at > now);
        if cache.contains_key(&key) {
            bail!("ServiceNet response nonce has already been used; refusing replay");
        }
        if cache.len() >= SERVICE_RESPONSE_NONCE_CACHE_MAX_ENTRIES {
            bail!(
                "ServiceNet response nonce cache is at capacity; retry through another client instance"
            );
        }
        cache.insert(key, now + SERVICE_RESPONSE_NONCE_CACHE_TTL);
        Ok(())
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

fn service_agent_task_request_digest(
    task_id: &str,
    request: &ServiceNetGetAgentTaskRequest,
) -> Result<String> {
    let params = build_service_agent_get_task_signature_params(task_id, request.history_length);
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(
            serde_jcs::to_vec(&params).context("canonicalize Service Agent task request")?
        )
    ))
}

fn build_service_agent_get_task_signature_params(
    task_id: &str,
    history_length: Option<u32>,
) -> Value {
    json!({
        "id": task_id,
        "historyLength": history_length
            .unwrap_or(SERVICE_AGENT_GET_TASK_DEFAULT_HISTORY_LENGTH),
    })
}

pub fn verify_service_agent_invoke_response(
    published_agent: &Value,
    response: &ServiceNetInvokeResponse,
    expected_request_digest: Option<&str>,
    expected_request_nonce: Option<&str>,
    signature_required: bool,
) -> Result<()> {
    let Some(service_signature) = response.service_signature.as_ref() else {
        if signature_required {
            bail!("ServiceNet response is missing the Service Agent signature");
        }
        return Ok(());
    };
    if service_signature.protocol != SERVICE_RESPONSE_SIGNATURE_PROTOCOL {
        bail!("ServiceNet response uses an unsupported Service Agent signature protocol");
    }
    let published_agent_id = published_agent
        .get("agent_id")
        .and_then(Value::as_str)
        .context("published Service Agent is missing agent_id")?;
    if response.agent_id != published_agent_id || service_signature.agent_id != published_agent_id {
        bail!("ServiceNet response agent_id does not match the published Service Agent");
    }
    let published_service_did = published_agent
        .get("service_did")
        .and_then(Value::as_str)
        .context("published Service Agent is missing service_did")?;
    if service_signature.service_did != published_service_did {
        bail!("ServiceNet response DID does not match the published Service Agent");
    }
    if let Some(expected_request_digest) = expected_request_digest
        && service_signature.request_digest != expected_request_digest
    {
        bail!("ServiceNet response request digest does not match the invocation");
    }
    if service_signature.request_nonce.as_deref() != expected_request_nonce {
        bail!("ServiceNet response request nonce does not match the invocation");
    }
    if service_signature.nonce.trim().is_empty() {
        bail!("ServiceNet response signature nonce is missing");
    }
    let issued_at_ms =
        i64::try_from(service_signature.issued_at_ms).context("invalid response timestamp")?;
    if (chrono::Utc::now().timestamp_millis() - issued_at_ms).abs()
        > SERVICE_RESPONSE_MAX_CLOCK_SKEW_MS
    {
        bail!("ServiceNet response signature timestamp is outside the accepted window");
    }
    let result = unsigned_service_agent_result(&response.raw);
    let result_digest = format!(
        "sha256:{:x}",
        Sha256::digest(serde_jcs::to_vec(&result).context("canonicalize Service Agent result")?)
    );
    if service_signature.result_digest != result_digest {
        bail!("ServiceNet response result digest does not match its signed value");
    }

    let public_key = service_agent_response_public_key(published_service_did, service_signature)?;
    let signature_bytes = STANDARD
        .decode(&service_signature.signature)
        .context("decode Service Agent signature")?;
    let signature_bytes: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| anyhow!("Service Agent signature must contain 64 bytes"))?;
    let payload = json!({
        "protocol": service_signature.protocol,
        "service_did": service_signature.service_did,
        "agent_id": service_signature.agent_id,
        "verification_method": service_signature.verification_method,
        "request_digest": service_signature.request_digest,
        "request_nonce": service_signature.request_nonce,
        "result_digest": service_signature.result_digest,
        "nonce": service_signature.nonce,
        "issued_at_ms": service_signature.issued_at_ms,
    });
    let payload =
        serde_jcs::to_vec(&payload).context("canonicalize Service Agent signature payload")?;
    VerifyingKey::from_bytes(&public_key)
        .context("parse Service Agent public key")?
        .verify(&payload, &Signature::from_bytes(&signature_bytes))
        .context("verify Service Agent response signature")?;
    Ok(())
}

fn unsigned_service_agent_result(raw_response: &Value) -> Value {
    let mut result = raw_response.get("result").cloned().unwrap_or(Value::Null);
    for payload_name in ["task", "message"] {
        let Some(payload) = result.get_mut(payload_name).and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(metadata) = payload.get_mut("metadata").and_then(Value::as_object_mut) else {
            continue;
        };
        metadata.remove("wattetheriaServiceAgentSignature");
        if metadata.is_empty() {
            payload.remove("metadata");
        }
    }
    result
}

fn service_agent_response_public_key(
    published_service_did: &str,
    service_signature: &ServiceNetServiceAgentSignature,
) -> Result<[u8; 32]> {
    let did = Did::parse(published_service_did).context("parse Service Agent did:key")?;
    let did_key = DidKey::from_did(did).context("Service Agent identity must use did:key")?;
    let expected_verification_method = format!("{}#{}", did_key.did, did_key.public_key_multibase);
    if service_signature.verification_method != expected_verification_method {
        bail!("Service Agent signature references the wrong did:key verification method");
    }
    match did_key
        .decode_public_key()
        .context("decode Service Agent did:key public key")?
    {
        DidKeyPublicKey::Ed25519(public_key) => Ok(public_key),
        _ => bail!("Service Agent did:key must use Ed25519"),
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
    fn attach_service_agent_payment_binding_updates_existing_pay_to_extension() {
        let binding = json!({
            "agent_did": "did:key:z6MkWallet",
            "rail": "x402",
            "network": "base",
            "payment_address": "0x1111111111111111111111111111111111111111",
        });
        let mut agent_card = json!({
            "name": "Agent",
            "capabilities": {
                "extensions": [
                    {
                        "uri": "https://github.com/google-a2a/a2a-x402/v0.1",
                        "params": {
                            "accepts": [
                                {
                                    "network": "base",
                                    "payTo": "0x0000000000000000000000000000000000000000"
                                }
                            ]
                        }
                    }
                ]
            }
        });

        attach_service_agent_payment_binding(&mut agent_card, Some(&binding));

        assert!(agent_card.get("payment_account_bindings").is_none());
        assert!(agent_card.get("didDocument").is_none());
        assert_eq!(
            agent_card["capabilities"]["extensions"][0]["params"]["payment_account_bindings"][0],
            binding
        );
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

    #[test]
    fn get_task_signature_params_match_protocol_vector() {
        let params = build_service_agent_get_task_signature_params("task-123", None);
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(serde_jcs::to_vec(&params).unwrap())
        );
        assert_eq!(
            digest,
            "sha256:58ed7275f0789912a6589b6103ba2a5a21a84ac396f7e93a86d504df0cac401a"
        );
    }

    #[test]
    fn unsigned_service_agent_result_removes_only_embedded_signature_metadata() {
        let raw = json!({
            "result": {
                "task": {
                    "id": "task-1",
                    "metadata": {
                        "trace_id": "trace-1",
                        "wattetheriaServiceAgentSignature": "signed-json"
                    }
                }
            }
        });

        assert_eq!(
            unsigned_service_agent_result(&raw),
            json!({
                "task": {
                    "id": "task-1",
                    "metadata": {"trace_id": "trace-1"}
                }
            })
        );
    }

    #[test]
    fn client_response_nonce_cache_rejects_replay() {
        let client = ServiceNetClient::new("http://127.0.0.1:1").unwrap();
        let response = ServiceNetInvokeResponse {
            agent_id: "ride".to_owned(),
            status: "completed".to_owned(),
            receipt_id: None,
            task_id: None,
            context_id: None,
            message: None,
            output: None,
            settlement: None,
            payment_receipt: None,
            service_signature: Some(ServiceNetServiceAgentSignature {
                protocol: SERVICE_RESPONSE_SIGNATURE_PROTOCOL.to_owned(),
                service_did:
                    "did:key:z6Mkg5K92URgXhcuTfqt9jntq75JgPKgaQj36ougEQ3PrDXM".to_owned(),
                agent_id: "ride".to_owned(),
                verification_method:
                    "did:key:z6Mkg5K92URgXhcuTfqt9jntq75JgPKgaQj36ougEQ3PrDXM#z6Mkg5K92URgXhcuTfqt9jntq75JgPKgaQj36ougEQ3PrDXM"
                        .to_owned(),
                request_digest: "sha256:request".to_owned(),
                request_nonce: Some("request-nonce".to_owned()),
                result_digest: "sha256:result".to_owned(),
                nonce: "response-nonce".to_owned(),
                issued_at_ms: 1,
                signature: "unused".to_owned(),
            }),
            raw: Value::Null,
        };

        client.record_service_response_nonce(&response).unwrap();
        assert!(
            client
                .record_service_response_nonce(&response)
                .unwrap_err()
                .to_string()
                .contains("refusing replay")
        );
    }
}
