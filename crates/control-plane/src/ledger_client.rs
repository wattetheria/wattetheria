//! Node-side client of the gateway Watt ledger.
//!
//! The gateway ledger is the only writer of Watt. A node never computes balances; it reads
//! them here and asks the ledger to lock, release or refund mission bounties, signing each
//! spend with its own `did:key` so the ledger can check the payer.

use anyhow::{Context, Result, anyhow};
use reqwest::{Client, StatusCode};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use tracing::warn;
use wattetheria_kernel::network::agent_registration::load_active_membership_credential;

use crate::state::ControlPlaneState;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const INGEST_SUFFIXES: [&str; 2] = ["/api/ingest/snapshot", "/api/ingest/event"];

/// Why the ledger refused or could not perform a movement.
#[derive(Debug)]
pub enum LedgerError {
    /// The payer does not hold enough Watt (HTTP 402).
    InsufficientBalance(String),
    /// The escrow or account does not exist (HTTP 404).
    NotFound(String),
    /// The ledger rejected the request for another reason (4xx/5xx).
    Rejected(StatusCode, String),
    /// The gateway could not be reached.
    Unavailable(anyhow::Error),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientBalance(message) | Self::NotFound(message) => f.write_str(message),
            Self::Rejected(status, message) => {
                write!(f, "ledger rejected request ({status}): {message}")
            }
            Self::Unavailable(error) => write!(f, "ledger unavailable: {error:#}"),
        }
    }
}

impl std::error::Error for LedgerError {}

/// A Watt balance as the ledger reported it, or zero when no ledger could be asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WattBalance {
    pub balance: i64,
    pub available: bool,
}

impl WattBalance {
    const UNAVAILABLE: Self = Self {
        balance: 0,
        available: false,
    };
}

/// Display-name snapshots sent with an escrow so the ledger can show who paid whom and why.
#[derive(Debug, Clone, Copy, Default)]
pub struct EscrowLabels<'a> {
    pub payer_label: Option<&'a str>,
    pub payee_label: Option<&'a str>,
    pub subject: Option<&'a str>,
}

#[derive(Clone)]
pub struct LedgerClient {
    client: Client,
    base_url: String,
}

impl LedgerClient {
    /// The first configured gateway hosts the ledger. `None` when the node runs without a
    /// gateway (local development, tests): Watt is then unavailable rather than invented.
    #[must_use]
    pub fn from_state(state: &ControlPlaneState) -> Option<Self> {
        let gateway_url = state.gateway_urls.first()?;
        Self::new(gateway_url).ok()
    }

    pub fn new(gateway_url: &str) -> Result<Self> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("build ledger http client")?;
        Ok(Self {
            client,
            base_url: ledger_base_url(gateway_url),
        })
    }

    /// Root URL of the gateway that hosts the ledger.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn balance(&self, account_id: &str) -> Result<i64> {
        self.account(account_id)
            .await?
            .get("balance")
            .and_then(Value::as_i64)
            .context("ledger balance response has no balance")
    }

    /// The ledger's account record: `balance`, `locked_in_escrow`, `exists`.
    pub async fn account(&self, account_id: &str) -> Result<Value> {
        let response = self
            .client
            .get(format!(
                "{}/api/ledger/accounts/{account_id}",
                self.base_url
            ))
            .send()
            .await
            .context("query ledger account")?
            .error_for_status()
            .context("ledger account returned error status")?;
        response.json().await.context("decode ledger account")
    }

    /// One page of the account's entries, newest first, as the ledger enriches them
    /// (`direction`, `counterparty_*`, `subject`, `escrow_id`) plus `next_before_seq`.
    pub async fn entries(
        &self,
        account_id: &str,
        limit: Option<i64>,
        before_seq: Option<i64>,
    ) -> Result<Value> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        if let Some(before_seq) = before_seq {
            query.push(("before_seq", before_seq.to_string()));
        }
        let response = self
            .client
            .get(format!(
                "{}/api/ledger/accounts/{account_id}/entries",
                self.base_url
            ))
            .query(&query)
            .send()
            .await
            .context("query ledger entries")?
            .error_for_status()
            .context("ledger entries returned error status")?;
        response.json().await.context("decode ledger entries")
    }

    /// Escrows the account pays or is paid from, optionally only those in `status`.
    pub async fn escrows(
        &self,
        account_id: &str,
        status: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<Value>> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(status) = status {
            query.push(("status", status.to_owned()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let response = self
            .client
            .get(format!(
                "{}/api/ledger/accounts/{account_id}/escrows",
                self.base_url
            ))
            .query(&query)
            .send()
            .await
            .context("query ledger escrows")?
            .error_for_status()
            .context("ledger escrows returned error status")?;
        let body: Value = response.json().await.context("decode ledger escrows")?;
        Ok(body
            .get("escrows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub async fn balances(&self, account_ids: &[String]) -> Result<BTreeMap<String, i64>> {
        if account_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let response = self
            .client
            .post(format!("{}/api/ledger/balances", self.base_url))
            .json(&json!({ "account_ids": account_ids }))
            .send()
            .await
            .context("query ledger balances")?
            .error_for_status()
            .context("ledger balances returned error status")?;
        let body: Value = response.json().await.context("decode ledger balances")?;
        let balances = body
            .get("balances")
            .and_then(Value::as_object)
            .context("ledger balances response has no balances")?;
        Ok(balances
            .iter()
            .map(|(id, value)| (id.clone(), value.as_i64().unwrap_or(0)))
            .collect())
    }

    /// Presents the Agent's membership Credential; the ledger grants the welcome bonus once.
    pub async fn claim_welcome(&self, credential: &impl Serialize) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}/api/ledger/welcome", self.base_url))
            .json(credential)
            .send()
            .await
            .context("claim ledger welcome bonus")?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(anyhow!(
                "ledger welcome claim rejected ({status}): {}",
                error_message(&body)
            ));
        }
        Ok(body)
    }

    /// Reads one escrow; `None` when the ledger holds no escrow under that id.
    pub async fn escrow(&self, escrow_id: &str) -> std::result::Result<Option<Value>, LedgerError> {
        let response = self
            .client
            .get(format!("{}/api/ledger/escrows/{escrow_id}", self.base_url))
            .send()
            .await
            .map_err(|error| LedgerError::Unavailable(error.into()))?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        match status {
            status if status.is_success() => Ok(Some(body)),
            StatusCode::NOT_FOUND => Ok(None),
            status => Err(LedgerError::Rejected(status, error_message(&body))),
        }
    }

    pub async fn lock_escrow(
        &self,
        state: &ControlPlaneState,
        escrow_id: &str,
        amount: i64,
        reference: &str,
        labels: EscrowLabels<'_>,
    ) -> std::result::Result<Value, LedgerError> {
        let payload = json!({
            "escrow_id": escrow_id,
            "payer": state.identity.agent_did,
            "amount": amount,
            "reference": reference,
            "payer_label": labels.payer_label,
            "subject": labels.subject,
            "issued_at_ms": chrono::Utc::now().timestamp_millis(),
        });
        self.post_signed(state, "/api/ledger/escrows", payload)
            .await
    }

    pub async fn release_escrow(
        &self,
        state: &ControlPlaneState,
        escrow_id: &str,
        payee: &str,
        payee_label: Option<&str>,
    ) -> std::result::Result<Value, LedgerError> {
        let payload = json!({
            "escrow_id": escrow_id,
            "payer": state.identity.agent_did,
            "payee": payee,
            "payee_label": payee_label,
            "issued_at_ms": chrono::Utc::now().timestamp_millis(),
        });
        self.post_signed(
            state,
            &format!("/api/ledger/escrows/{escrow_id}/release"),
            payload,
        )
        .await
    }

    pub async fn refund_escrow(
        &self,
        state: &ControlPlaneState,
        escrow_id: &str,
    ) -> std::result::Result<Value, LedgerError> {
        let payload = json!({
            "escrow_id": escrow_id,
            "payer": state.identity.agent_did,
            "issued_at_ms": chrono::Utc::now().timestamp_millis(),
        });
        self.post_signed(
            state,
            &format!("/api/ledger/escrows/{escrow_id}/refund"),
            payload,
        )
        .await
    }

    /// Publishes a custom skin on the gateway, paying the publication fee from this agent's
    /// account in the same transaction. The response carries `skin`, `charged` and `fee_watt`.
    pub async fn publish_skin(
        &self,
        state: &ControlPlaneState,
        name: Option<&str>,
        png_base64: &str,
    ) -> std::result::Result<Value, LedgerError> {
        let payload = json!({
            "uploader": state.identity.agent_did,
            "name": name,
            "png_base64": png_base64,
            "issued_at_ms": chrono::Utc::now().timestamp_millis(),
        });
        self.post_signed(state, "/api/skins", payload).await
    }

    /// Skins an agent has published, with the publication fee the gateway charges.
    /// Public URL of a published skin's PNG on the gateway.
    #[must_use]
    pub fn skin_image_url(&self, skin_id: &str) -> String {
        format!("{}/api/skins/{skin_id}", self.base_url)
    }

    /// The public gallery of every published skin, newest first.
    pub async fn skin_gallery(&self, limit: u32, offset: u32) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/api/skins", self.base_url))
            .query(&[("limit", limit), ("offset", offset)])
            .send()
            .await
            .context("query skin gallery")?
            .error_for_status()
            .context("skin gallery returned error status")?;
        response.json().await.context("decode skin gallery")
    }

    pub async fn skins_owned_by(&self, owner: &str) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/api/skins", self.base_url))
            .query(&[("owner", owner)])
            .send()
            .await
            .context("query owned skins")?
            .error_for_status()
            .context("owned skins returned error status")?;
        response.json().await.context("decode owned skins")
    }

    /// Has `owner` published `skin_id` (and so may wear it)?
    pub async fn owns_skin(&self, owner: &str, skin_id: &str) -> Result<bool> {
        let published = self.skins_owned_by(owner).await?;
        Ok(published["skins"]
            .as_array()
            .is_some_and(|skins| skins.iter().any(|skin| skin["skin_id"] == skin_id)))
    }

    async fn post_signed(
        &self,
        state: &ControlPlaneState,
        path: &str,
        payload: Value,
    ) -> std::result::Result<Value, LedgerError> {
        let signature = state
            .sign_payload(&payload)
            .map_err(LedgerError::Unavailable)?;
        let response = self
            .client
            .post(format!("{}{path}", self.base_url))
            .json(&json!({
                "payload": payload,
                "public_key": state.identity.public_key,
                "signature": signature,
            }))
            .send()
            .await
            .map_err(|error| LedgerError::Unavailable(error.into()))?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        match status {
            status if status.is_success() => Ok(body),
            StatusCode::PAYMENT_REQUIRED => {
                Err(LedgerError::InsufficientBalance(error_message(&body)))
            }
            StatusCode::NOT_FOUND => Err(LedgerError::NotFound(error_message(&body))),
            status => Err(LedgerError::Rejected(status, error_message(&body))),
        }
    }
}

/// Reads one account's Watt from the ledger; zero and `available: false` without a gateway.
pub async fn watt_balance(state: &ControlPlaneState, account_id: &str) -> WattBalance {
    let Some(ledger) = LedgerClient::from_state(state) else {
        return WattBalance::UNAVAILABLE;
    };
    match ledger.balance(account_id).await {
        Ok(balance) => WattBalance {
            balance,
            available: true,
        },
        Err(error) => {
            warn!("ledger balance lookup failed for {account_id}: {error:#}");
            WattBalance::UNAVAILABLE
        }
    }
}

/// Reads many accounts at once; missing or unreachable entries read as zero.
pub async fn watt_balances(
    state: &ControlPlaneState,
    account_ids: &[String],
) -> BTreeMap<String, i64> {
    let Some(ledger) = LedgerClient::from_state(state) else {
        return BTreeMap::new();
    };
    match ledger.balances(account_ids).await {
        Ok(balances) => balances,
        Err(error) => {
            warn!("ledger balances lookup failed: {error:#}");
            BTreeMap::new()
        }
    }
}

/// Presents this node's active membership Credential to the ledger so the Agent receives its
/// one-time welcome bonus. Safe to call on every start: the ledger grants once per DID.
pub async fn claim_welcome_bonus(state: &ControlPlaneState) -> Result<Option<Value>> {
    let Some(ledger) = LedgerClient::from_state(state) else {
        return Ok(None);
    };
    let Some(credential) =
        load_active_membership_credential(&state.local_db, &state.agent_did, None)?
    else {
        return Ok(None);
    };
    ledger.claim_welcome(&credential).await.map(Some)
}

fn ledger_base_url(gateway_url: &str) -> String {
    let mut base = gateway_url.trim_end_matches('/');
    for suffix in INGEST_SUFFIXES {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped;
        }
    }
    base.to_string()
}

fn error_message(body: &Value) -> String {
    body.get("error")
        .and_then(Value::as_str)
        .map_or_else(|| body.to_string(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::ledger_base_url;

    #[test]
    fn ledger_base_url_strips_ingest_paths_and_trailing_slashes() {
        assert_eq!(
            ledger_base_url("https://gw.example/api/ingest/snapshot"),
            "https://gw.example"
        );
        assert_eq!(
            ledger_base_url("https://gw.example/api/ingest/event/"),
            "https://gw.example"
        );
        assert_eq!(
            ledger_base_url("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080"
        );
    }
}
