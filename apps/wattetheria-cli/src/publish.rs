//! Client-side helpers for publishing agents to a watt-servicenet node.
//!
//! Everything here is a thin layer over `watt-wallet`, `watt-did`, and
//! `reqwest`. The CLI is the only caller; tests live alongside the
//! corresponding command runners in `main.rs`.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;
use wattetheria_kernel::servicenet::validate_servicenet_agent_name;
use wattetheria_kernel::wallet_identity::{LocalWalletState, open_local_wallet};

/// Minimal client for a watt-servicenet node. Only covers the routes the
/// CLI uses today.
pub(crate) struct ServicenetClient {
    base_url: String,
    http: Client,
}

impl ServicenetClient {
    pub(crate) fn new(base_url: &str) -> Self {
        let base_url = base_url.trim_end_matches('/').to_owned();
        Self {
            base_url,
            http: Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub(crate) async fn create_ownership_challenge(
        &self,
        identity_did: &str,
        operation: &str,
    ) -> Result<OwnershipChallenge> {
        let body = json!({
            "provider_did": identity_did,
            "operation": operation,
        });
        let response = self
            .http
            .post(self.url("/v1/providers/ownership-challenges"))
            .json(&body)
            .send()
            .await
            .context("request ownership challenge")?;
        let status = response.status();
        let bytes = response.bytes().await.context("read challenge body")?;
        if !status.is_success() {
            bail!(
                "ownership challenge request returned {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            );
        }
        serde_json::from_slice(&bytes).context("parse ownership challenge")
    }

    pub(crate) async fn register_provider(
        &self,
        provider_id: &str,
        identity_did: &str,
        display_name: Option<&str>,
        challenge_id: Uuid,
        signature_b64: &str,
    ) -> Result<Value> {
        let body = json!({
            "provider_id": provider_id,
            "provider_did": identity_did,
            "display_name": display_name,
            "ownership_challenge_id": challenge_id,
            "ownership_signature": signature_b64,
        });
        let response = self
            .http
            .post(self.url("/v1/providers/register"))
            .json(&body)
            .send()
            .await
            .context("request provider register")?;
        let status = response.status();
        let bytes = response.bytes().await.context("read register body")?;
        if !status.is_success() {
            bail!(
                "provider register returned {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            );
        }
        serde_json::from_slice(&bytes).context("parse provider record")
    }

    pub(crate) async fn submit_agent(&self, request: Value) -> Result<Value> {
        let response = self
            .http
            .post(self.url("/v1/agent-submissions"))
            .json(&request)
            .send()
            .await
            .context("submit agent")?;
        let status = response.status();
        let bytes = response.bytes().await.context("read submit body")?;
        if !status.is_success() {
            bail!(
                "agent submission returned {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            );
        }
        serde_json::from_slice(&bytes).context("parse submission record")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OwnershipChallenge {
    pub(crate) challenge_id: Uuid,
    pub(crate) challenge: String,
    pub(crate) provider_id: String,
    pub(crate) provider_did: String,
}

/// L1 + L4 validation of an A2A agent card.
///
/// Mirrors the spec in `docs/PUBLISH_FLOW_DESIGN.md`. The server side
/// re-validates, but failing here gives the developer immediate feedback.
pub(crate) fn validate_agent_card(card: &Value) -> Result<()> {
    let object = card
        .as_object()
        .ok_or_else(|| anyhow!("agent card must be a JSON object"))?;

    for field in [
        "name",
        "description",
        "url",
        "preferredTransport",
        "protocolVersion",
        "scope",
        "origin",
        "domain",
        "cost",
        "currency",
        "supportsTask",
        "skills",
        "securitySchemes",
        "security",
    ] {
        if !object.contains_key(field) {
            bail!("agent card is missing required field `{field}`");
        }
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("agent card `name` must be a string"))?;
    validate_servicenet_agent_name(name)?;

    let url = object
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("agent card `url` must be a string"))?;
    if !url.starts_with("https://") {
        bail!("agent card `url` must be https:// (got `{url}`)");
    }

    let transport = object
        .get("preferredTransport")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("agent card `preferredTransport` must be a string"))?;
    if transport != "JSONRPC" {
        bail!("agent card `preferredTransport` must be `JSONRPC` (got `{transport}`)");
    }

    validate_scope_origin_domain(object)?;

    let cost = object
        .get("cost")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("agent card `cost` must be a non-negative integer"))?;
    if cost > u64::from(u32::MAX) {
        bail!(
            "agent card `cost` must be a non-negative integer up to {}",
            u32::MAX
        );
    }
    validate_agent_card_currency(object)?;

    if object
        .get("supportsTask")
        .and_then(Value::as_bool)
        .is_none()
    {
        bail!("agent card `supportsTask` must be a boolean");
    }

    let skills = object
        .get("skills")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("agent card `skills` must be an array"))?;
    if skills.is_empty() {
        bail!("agent card `skills` must list at least one skill");
    }
    for (index, skill) in skills.iter().enumerate() {
        let skill_obj = skill
            .as_object()
            .ok_or_else(|| anyhow!("skill[{index}] must be an object"))?;
        let name = skill_obj
            .get("name")
            .and_then(Value::as_str)
            .map_or("", str::trim);
        if name.is_empty() {
            bail!("skill[{index}] is missing required field `name`");
        }
    }

    let card_text = serde_json::to_string(card).unwrap_or_default();
    if card_text.contains("sk-") || card_text.contains("BEGIN PRIVATE KEY") {
        bail!("agent card appears to contain a secret; remove it before publishing");
    }

    Ok(())
}

fn validate_agent_card_currency(object: &serde_json::Map<String, Value>) -> Result<()> {
    let currency = object
        .get("currency")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("agent card `currency` must be a string"))?;
    if !matches!(currency, "USDC" | "USDT") {
        bail!("agent card `currency` must be `USDC` or `USDT` (got `{currency}`)");
    }
    Ok(())
}

fn validate_scope_origin_domain(object: &serde_json::Map<String, Value>) -> Result<()> {
    let scope = object
        .get("scope")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("agent card `scope` must be a string"))?;
    if !matches!(scope, "real_world" | "wattetheria_native") {
        bail!("agent card `scope` must be `real_world` or `wattetheria_native` (got `{scope}`)");
    }

    let origin = object
        .get("origin")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("agent card `origin` must be a string"))?;
    validate_origin_for_scope(scope, origin)?;

    let domain = object
        .get("domain")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("agent card `domain` must be a string"))?;
    if !domain_allowed_for_scope(scope, domain) {
        bail!(
            "agent card `domain` is not a supported ServiceNet UI domain for `{scope}` scope (got `{domain}`)"
        );
    }

    Ok(())
}

fn validate_origin_for_scope(scope: &str, origin: &str) -> Result<()> {
    match scope {
        "real_world" if !matches!(origin, "established_service" | "custom_built") => {
            bail!(
                "agent card `origin` must be `established_service` or `custom_built` for `real_world` scope (got `{origin}`)"
            );
        }
        "wattetheria_native" if origin != "native_published" => {
            bail!(
                "agent card `origin` must be `native_published` for `wattetheria_native` scope (got `{origin}`)"
            );
        }
        _ => Ok(()),
    }
}

fn domain_allowed_for_scope(scope: &str, domain: &str) -> bool {
    match scope {
        "real_world" => matches!(
            domain,
            "GENERAL"
                | "TRANSPORTATION"
                | "FOOD"
                | "CLOTHING"
                | "HOUSING"
                | "PAYMENTS"
                | "COMMERCE"
                | "MEDIA"
                | "HEALTH"
                | "EDUCATION"
                | "TRAVEL"
        ),
        "wattetheria_native" => matches!(
            domain,
            "GENERAL"
                | "GOVERNANCE"
                | "PRODUCTION"
                | "TRADING"
                | "AUTOMATION"
                | "SECURITY"
                | "EXPLORATION"
                | "DISCOVERY"
                | "SERVICENET"
        ),
        _ => false,
    }
}

/// Validate that the endpoint is a usable runtime URL.
///
/// Rejects: non-https schemes, IP-literal hosts (the registry expects a real
/// DNS name so the cert validates and the listing is human-readable), loopback
/// hosts, and RFC1918 / link-local / private / reserved addresses that would
/// only resolve inside one network.
pub(crate) fn validate_endpoint(endpoint: &str) -> Result<()> {
    let url = url::Url::parse(endpoint)
        .map_err(|error| anyhow!("endpoint is not a valid URL: {error}"))?;
    if url.scheme() != "https" {
        bail!("endpoint must use https:// (got scheme `{}`)", url.scheme());
    }
    let host = url
        .host()
        .ok_or_else(|| anyhow!("endpoint must include a host"))?;
    match host {
        url::Host::Domain(name) => {
            if name.eq_ignore_ascii_case("localhost") {
                bail!("endpoint host must not be localhost");
            }
        }
        url::Host::Ipv4(addr) => {
            if !is_ipv4_publicly_routable(addr) {
                bail!(
                    "endpoint host {addr} is not a publicly routable IPv4 address; \
                     publish endpoints must use a real DNS name"
                );
            }
            bail!("endpoint host is an IPv4 literal ({addr}); use a DNS hostname instead");
        }
        url::Host::Ipv6(addr) => {
            bail!("endpoint host is an IPv6 literal ({addr}); use a DNS hostname instead");
        }
    }
    Ok(())
}

fn is_ipv4_publicly_routable(addr: std::net::Ipv4Addr) -> bool {
    !addr.is_loopback()
        && !addr.is_private()
        && !addr.is_link_local()
        && !addr.is_broadcast()
        && !addr.is_multicast()
        && !addr.is_unspecified()
        && !addr.is_documentation()
}

/// Open the wallet, run a closure that needs `&LocalWalletState`, and surface
/// errors with a friendlier wallet-not-initialised message.
pub(crate) fn open_wallet_or_explain(data_dir: &Path) -> Result<LocalWalletState> {
    open_local_wallet(data_dir).with_context(|| {
        format!(
            "no wallet found at `{}` — run `wattetheria identity init --data-dir {}` first",
            data_dir.display(),
            data_dir.display()
        )
    })
}

/// Sign a payload with the active wallet identity and base64-encode it.
pub(crate) fn sign_with_identity_b64(wallet: &LocalWalletState, payload: &[u8]) -> Result<String> {
    let signature = wallet
        .wallet
        .sign_with_active_identity(&wallet.profile, payload)
        .context("sign with active identity")?;
    Ok(STANDARD.encode(signature.0))
}

#[cfg(test)]
mod tests {
    use super::validate_agent_card;
    use serde_json::json;

    fn valid_card() -> serde_json::Value {
        json!({
            "name": "Alice",
            "description": "Alice Test",
            "url": "https://alice.example.com/",
            "preferredTransport": "JSONRPC",
            "protocolVersion": "1.0",
            "scope": "real_world",
            "origin": "custom_built",
            "domain": "GENERAL",
            "cost": 18,
            "currency": "USDC",
            "supportsTask": false,
            "skills": [
                {
                    "description": "Get weather",
                    "id": "",
                    "name": "Get Weather"
                }
            ],
            "securitySchemes": {
                "none": {
                    "type": "none"
                }
            },
            "security": [
                {
                    "none": []
                }
            ]
        })
    }

    #[test]
    fn agent_card_allows_empty_skill_id() {
        assert!(validate_agent_card(&valid_card()).is_ok());
    }

    #[test]
    fn agent_card_requires_skill_name() {
        let mut card = valid_card();
        card["skills"][0]["name"] = json!("");
        let error = validate_agent_card(&card).expect_err("empty skill name should fail");
        assert!(
            error
                .to_string()
                .contains("skill[0] is missing required field `name`")
        );
    }

    #[test]
    fn agent_card_rejects_unsafe_name() {
        let mut card = valid_card();
        card["name"] = json!("Bad\u{0007}Name");
        let error = validate_agent_card(&card).expect_err("unsafe name should fail");
        assert!(
            error
                .to_string()
                .contains("agent card `name` must not contain control characters")
        );
    }

    #[test]
    fn agent_card_rejects_overlong_name() {
        let mut card = valid_card();
        card["name"] = json!("名".repeat(41));
        let error = validate_agent_card(&card).expect_err("overlong name should fail");
        assert!(
            error
                .to_string()
                .contains("agent card `name` must be 40 characters or less")
        );
    }

    #[test]
    fn agent_card_allows_empty_skill_description() {
        let mut card = valid_card();
        card["skills"][0]["description"] = json!("");
        assert!(validate_agent_card(&card).is_ok());
    }

    #[test]
    fn agent_card_allows_missing_skill_description() {
        let mut card = valid_card();
        card["skills"][0]
            .as_object_mut()
            .unwrap()
            .remove("description");
        assert!(validate_agent_card(&card).is_ok());
    }

    #[test]
    fn agent_card_allows_wattetheria_native_scope() {
        let mut card = valid_card();
        card["scope"] = json!("wattetheria_native");
        card["origin"] = json!("native_published");
        card["domain"] = json!("SERVICENET");
        assert!(validate_agent_card(&card).is_ok());
    }

    #[test]
    fn agent_card_requires_known_real_world_origin() {
        let mut card = valid_card();
        card["origin"] = json!("native_published");
        let error = validate_agent_card(&card).expect_err("native origin should fail");
        assert!(
            error.to_string().contains(
                "agent card `origin` must be `established_service` or `custom_built` for `real_world` scope"
            )
        );
    }

    #[test]
    fn agent_card_requires_native_origin_for_native_scope() {
        let mut card = valid_card();
        card["scope"] = json!("wattetheria_native");
        card["origin"] = json!("custom_built");
        card["domain"] = json!("SERVICENET");
        let error = validate_agent_card(&card).expect_err("real-world origin should fail");
        assert!(error.to_string().contains(
            "agent card `origin` must be `native_published` for `wattetheria_native` scope"
        ));
    }

    #[test]
    fn agent_card_rejects_risk_level_as_domain() {
        let mut card = valid_card();
        card["domain"] = json!("LOW");
        let error = validate_agent_card(&card).expect_err("risk level should not be a domain");
        assert!(
            error
                .to_string()
                .contains("agent card `domain` is not a supported ServiceNet UI domain")
        );
    }

    #[test]
    fn agent_card_rejects_real_world_domain_for_native_scope() {
        let mut card = valid_card();
        card["scope"] = json!("wattetheria_native");
        card["origin"] = json!("native_published");
        card["domain"] = json!("TRANSPORTATION");
        let error = validate_agent_card(&card).expect_err("real-world domain should fail");
        assert!(error.to_string().contains("for `wattetheria_native` scope"));
    }

    #[test]
    fn agent_card_requires_integer_cost() {
        let mut card = valid_card();
        card["cost"] = json!("18");
        let error = validate_agent_card(&card).expect_err("string cost should fail");
        assert!(
            error
                .to_string()
                .contains("agent card `cost` must be a non-negative integer")
        );
    }

    #[test]
    fn agent_card_requires_string_currency() {
        let mut card = valid_card();
        card["currency"] = json!(18);
        let error = validate_agent_card(&card).expect_err("numeric currency should fail");
        assert!(
            error
                .to_string()
                .contains("agent card `currency` must be a string")
        );
    }

    #[test]
    fn agent_card_requires_supported_currency() {
        let mut card = valid_card();
        card["currency"] = json!("WATT");
        let error = validate_agent_card(&card).expect_err("unsupported currency should fail");
        assert!(
            error
                .to_string()
                .contains("agent card `currency` must be `USDC` or `USDT`")
        );
    }

    #[test]
    fn agent_card_requires_boolean_supports_task() {
        let mut card = valid_card();
        card["supportsTask"] = json!("false");
        let error = validate_agent_card(&card).expect_err("string supportsTask should fail");
        assert!(
            error
                .to_string()
                .contains("agent card `supportsTask` must be a boolean")
        );
    }
}
