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
use watt_did::{PaymentAccountBindingProof, PaymentAccountCustody};
use watt_wallet::{
    PaymentAccountBindingProofOptions, PaymentAccountSigner, build_payment_account_binding_proof,
};
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
        "skills",
        "securitySchemes",
        "security",
    ] {
        if !object.contains_key(field) {
            bail!("agent card is missing required field `{field}`");
        }
    }

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
        bail!("agent card `preferredTransport` must be `JSONRPC` (got `{transport}`)",);
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
        for field in ["id", "name", "description"] {
            let value = skill_obj
                .get(field)
                .and_then(Value::as_str)
                .map_or("", str::trim);
            if value.is_empty() {
                bail!("skill[{index}] is missing required field `{field}`");
            }
        }
    }

    let card_text = serde_json::to_string(card).unwrap_or_default();
    if card_text.contains("sk-") || card_text.contains("BEGIN PRIVATE KEY") {
        bail!("agent card appears to contain a secret; remove it before publishing");
    }

    Ok(())
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

/// Build a `PaymentAccountBindingProof` for the wallet's currently active
/// spending payment account, signed by the active agent identity.
///
/// Returns `Ok(None)` only when the active payment account is watch-only or
/// otherwise unable to sign — in that case the publisher must explicitly
/// pass a `--skip-binding-proof` flag to publish without one.
pub(crate) fn build_active_payment_account_binding(
    wallet: &LocalWalletState,
    issued_at_ms: u64,
    nonce: String,
) -> Result<PaymentAccountBindingProof> {
    let identity = wallet
        .wallet
        .active_identity(&wallet.profile)
        .context("resolve active identity")?;
    let identity_key_info = wallet
        .wallet
        .active_identity_key_info(&wallet.profile)
        .context("read active identity key info")?;
    let payment_account = wallet
        .wallet
        .active_payment_account(&wallet.profile)
        .context("resolve active payment account")?
        .clone();
    let key_handle = payment_account.key_handle.clone().ok_or_else(|| {
        anyhow!(
            "active payment account `{}` is watch-only and cannot sign a binding proof",
            payment_account.account_id
        )
    })?;
    let payment_key_info = wallet
        .wallet
        .active_payment_account_key_info(&wallet.profile)
        .context("read active payment account key info")?;

    let options = PaymentAccountBindingProofOptions {
        agent_did: identity.did.clone(),
        agent_key_handle: &identity.key_handle,
        agent_public_key_multibase: identity_key_info.public_key_multibase.clone(),
        rail: payment_account.rail.clone(),
        network: payment_account.network.clone(),
        custody: PaymentAccountCustody::LocalGenerated,
        receive_only: false,
        can_sign: true,
        capabilities: payment_account.capabilities.clone(),
        issued_at_ms,
        expires_at_ms: None,
        nonce: Some(nonce),
        payment_signer: Some(PaymentAccountSigner {
            key_handle: &key_handle,
            public_key_multibase: payment_key_info.public_key_multibase.clone(),
        }),
        watch_only_payment_address: None,
    };
    build_payment_account_binding_proof(wallet.wallet.keystore(), options)
        .context("build payment account binding proof")
}

/// Sign a payload with the active wallet identity and base64-encode it.
pub(crate) fn sign_with_identity_b64(wallet: &LocalWalletState, payload: &[u8]) -> Result<String> {
    let signature = wallet
        .wallet
        .sign_with_active_identity(&wallet.profile, payload)
        .context("sign with active identity")?;
    Ok(STANDARD.encode(signature.0))
}
