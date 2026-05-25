//! Payment ledger for agent-to-agent transactions within the Wattetheria network.
//!
//! Tracks payment proposals, authorizations, settlements, and receipts
//! between agents using their local wallet payment accounts.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use watt_wallet::{
    SignatureBytes, evm_address_from_secp256k1_multibase_public_key,
    verify_secp256k1_with_multibase_public_key,
};

pub mod x402;

pub use x402::{
    PAYMENT_REQUIRED_HEADER, PAYMENT_RESPONSE_HEADER, PAYMENT_SIGNATURE_HEADER,
    X402PaymentRequired, X402PaymentRequirement, X402SettlementResponse,
    build_payment_signature_payload, decode_payment_required_header,
    decode_settlement_response_header, encode_payment_signature_header, select_payment_requirement,
    stablecoin_amount_from_base_units, stablecoin_amount_to_base_units,
    validate_x402_settlement_receipt,
};

#[derive(Debug, Serialize)]
struct PaymentAuthorizationPayload<'a> {
    payment_id: &'a str,
    sender_did: &'a str,
    recipient_did: &'a str,
    sender_public_id: &'a str,
    recipient_public_id: &'a str,
    remote_node_id: &'a str,
    amount: &'a str,
    currency: &'a str,
    rail: &'a str,
    layer: &'a SettlementLayer,
    network: &'a Option<String>,
    sender_address: &'a Option<String>,
    recipient_address: &'a Option<String>,
    mission_id: &'a Option<String>,
    task_id: &'a Option<String>,
    description: &'a Option<String>,
    expires_at: &'a Option<i64>,
}

/// The settlement rail used for the payment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SettlementLayer {
    Web2,
    #[default]
    Web3,
}

/// Status of a payment transaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    /// Payment has been proposed by the sender but not yet authorized.
    Proposed,
    /// Sender has authorized the payment with a cryptographic signature.
    Authorized,
    /// Payment has been submitted to the settlement rail.
    Submitted,
    /// Payment has been settled and confirmed.
    Settled,
    /// Payment was rejected by the receiver or failed settlement.
    Rejected,
    /// Payment expired before being completed.
    Expired,
    /// Payment was cancelled by the sender.
    Cancelled,
}

impl PaymentStatus {
    #[must_use]
    pub fn rank(&self) -> u8 {
        match self {
            Self::Proposed => 0,
            Self::Authorized => 1,
            Self::Submitted => 2,
            Self::Settled => 3,
            Self::Rejected => 4,
            Self::Expired => 5,
            Self::Cancelled => 6,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PaymentMessageKind {
    #[serde(rename = "payment_request")]
    Request,
    #[serde(rename = "payment_authorized")]
    Authorized,
    #[serde(rename = "payment_submitted")]
    Submitted,
    #[serde(rename = "payment_settled")]
    Settled,
    #[serde(rename = "payment_rejected")]
    Rejected,
    #[serde(rename = "payment_cancelled")]
    Cancelled,
}

impl PaymentMessageKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Request => "payment_request",
            Self::Authorized => "payment_authorized",
            Self::Submitted => "payment_submitted",
            Self::Settled => "payment_settled",
            Self::Rejected => "payment_rejected",
            Self::Cancelled => "payment_cancelled",
        }
    }

    #[must_use]
    pub fn expected_status(&self) -> PaymentStatus {
        match self {
            Self::Request => PaymentStatus::Proposed,
            Self::Authorized => PaymentStatus::Authorized,
            Self::Submitted => PaymentStatus::Submitted,
            Self::Settled => PaymentStatus::Settled,
            Self::Rejected => PaymentStatus::Rejected,
            Self::Cancelled => PaymentStatus::Cancelled,
        }
    }

    #[must_use]
    pub fn expected_actor_did<'a>(&self, payment: &'a PaymentTransaction) -> Option<&'a str> {
        match self {
            Self::Request | Self::Authorized | Self::Submitted | Self::Cancelled => {
                Some(&payment.sender_did)
            }
            Self::Rejected => Some(&payment.recipient_did),
            Self::Settled => None,
        }
    }
}

#[must_use]
pub fn source_payment_account_binding_required(
    kind: &PaymentMessageKind,
    payment: &PaymentTransaction,
    source_agent_did: &str,
) -> bool {
    if source_agent_did != payment.sender_did {
        return false;
    }
    matches!(
        kind,
        PaymentMessageKind::Authorized
            | PaymentMessageKind::Submitted
            | PaymentMessageKind::Settled
    ) || matches!(kind, PaymentMessageKind::Request)
        && payment
            .sender_address
            .as_deref()
            .is_some_and(|address| !address.trim().is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaymentAgentMessage {
    pub kind: PaymentMessageKind,
    pub payment: PaymentTransaction,
    pub emitted_at: i64,
}

/// A payment transaction between two agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaymentTransaction {
    pub payment_id: String,
    /// DID of the agent sending the payment.
    pub sender_did: String,
    /// DID of the agent receiving the payment.
    pub recipient_did: String,
    /// Public identity of the local sender agent.
    pub sender_public_id: String,
    /// Public identity of the remote recipient agent.
    pub recipient_public_id: String,
    /// Remote node id hosting the counterpart agent.
    pub remote_node_id: String,
    /// Payment amount in the smallest unit (e.g., wei for EVM).
    pub amount: String,
    /// Human-readable currency or token identifier (e.g., "WATT", "ETH", "USDC").
    pub currency: String,
    /// Settlement rail identifier (e.g., "x402").
    pub rail: String,
    /// Settlement layer (web2/web3).
    pub layer: SettlementLayer,
    /// Network identifier (e.g., "base-sepolia", "mainnet").
    pub network: Option<String>,
    /// Sender's payment account address.
    pub sender_address: Option<String>,
    /// Recipient's payment account address.
    pub recipient_address: Option<String>,
    /// Optional reference to the mission that triggered this payment.
    pub mission_id: Option<String>,
    /// Optional reference to the task that triggered this payment.
    pub task_id: Option<String>,
    /// Human-readable description of the payment purpose.
    pub description: Option<String>,
    /// Additional metadata as JSON.
    pub metadata: Option<Value>,
    /// Current status of the payment.
    pub status: PaymentStatus,
    /// Cryptographic authorization signature from the sender.
    pub authorization_signature: Option<String>,
    /// Public key used to verify the authorization signature.
    pub authorization_public_key: Option<String>,
    /// Settlement receipt from the rail provider.
    pub settlement_receipt: Option<Value>,
    /// Reason for rejection, if applicable.
    pub reject_reason: Option<String>,
    /// Timestamp when the payment was proposed.
    pub proposed_at: i64,
    /// Timestamp when the payment was authorized.
    pub authorized_at: Option<i64>,
    /// Timestamp when the payment was settled.
    pub settled_at: Option<i64>,
    /// Timestamp when the payment expires (if applicable).
    pub expires_at: Option<i64>,
}

impl PaymentTransaction {
    #[must_use]
    pub fn agent_message(&self, kind: PaymentMessageKind, emitted_at: i64) -> PaymentAgentMessage {
        PaymentAgentMessage {
            kind,
            payment: self.clone(),
            emitted_at,
        }
    }
}

pub fn authorization_payload_bytes(transaction: &PaymentTransaction) -> Result<Vec<u8>> {
    let payload = PaymentAuthorizationPayload {
        payment_id: &transaction.payment_id,
        sender_did: &transaction.sender_did,
        recipient_did: &transaction.recipient_did,
        sender_public_id: &transaction.sender_public_id,
        recipient_public_id: &transaction.recipient_public_id,
        remote_node_id: &transaction.remote_node_id,
        amount: &transaction.amount,
        currency: &transaction.currency,
        rail: &transaction.rail,
        layer: &transaction.layer,
        network: &transaction.network,
        sender_address: &transaction.sender_address,
        recipient_address: &transaction.recipient_address,
        mission_id: &transaction.mission_id,
        task_id: &transaction.task_id,
        description: &transaction.description,
        expires_at: &transaction.expires_at,
    };
    serde_json::to_vec(&payload).context("serialize payment authorization payload")
}

pub fn verify_payment_authorization_signature(transaction: &PaymentTransaction) -> Result<()> {
    let signature = transaction
        .authorization_signature
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("payment authorization signature is required"))?;
    let public_key = transaction
        .authorization_public_key
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("payment authorization public key is required"))?;
    let signature = SignatureBytes(
        STANDARD
            .decode(signature)
            .context("decode payment authorization signature")?,
    );
    verify_secp256k1_with_multibase_public_key(
        public_key,
        &authorization_payload_bytes(transaction)?,
        &signature,
    )
    .map_err(anyhow::Error::from)
    .context("verify payment authorization signature")?;
    let sender_address = transaction
        .sender_address
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("payment authorization sender address is required"))?;
    let derived_address =
        evm_address_from_secp256k1_multibase_public_key(public_key).map_err(anyhow::Error::from)?;
    if !derived_address.eq_ignore_ascii_case(sender_address) {
        bail!("payment authorization public key does not match sender address");
    }
    Ok(())
}

pub fn validate_agent_payment_message(
    message: &PaymentAgentMessage,
    source_agent_did: &str,
) -> Result<()> {
    let payment = &message.payment;
    if payment.status != message.kind.expected_status() {
        bail!(
            "payment message kind {} does not match payment status {:?}",
            message.kind.as_str(),
            payment.status
        );
    }

    if let Some(expected) = message.kind.expected_actor_did(payment) {
        if expected != source_agent_did {
            bail!(
                "payment message kind {} must be sent by {}",
                message.kind.as_str(),
                expected
            );
        }
    } else if source_agent_did != payment.sender_did && source_agent_did != payment.recipient_did {
        bail!("payment settlement message must be sent by a payment participant");
    }

    if matches!(
        message.kind,
        PaymentMessageKind::Authorized
            | PaymentMessageKind::Submitted
            | PaymentMessageKind::Settled
    ) {
        verify_payment_authorization_signature(payment)?;
    }
    Ok(())
}

/// A request to propose a new payment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposePaymentRequest {
    pub sender_public_id: String,
    pub remote_node_id: String,
    pub recipient_public_id: String,
    pub recipient_did: String,
    pub amount: String,
    pub currency: String,
    pub rail: String,
    pub layer: SettlementLayer,
    pub network: Option<String>,
    pub recipient_address: Option<String>,
    pub mission_id: Option<String>,
    pub task_id: Option<String>,
    pub description: Option<String>,
    pub metadata: Option<Value>,
    pub expires_at: Option<i64>,
}

/// A request to authorize a proposed payment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizePaymentRequest {
    pub payment_id: String,
    pub authorization_signature: String,
    pub authorization_public_key: Option<String>,
    pub sender_address: Option<String>,
}

/// A request to settle an authorized payment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlePaymentRequest {
    pub payment_id: String,
    pub settlement_receipt: Value,
}

/// A request to reject a payment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectPaymentRequest {
    pub payment_id: String,
    pub reject_reason: String,
}

/// Query parameters for listing payments.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaymentQuery {
    pub status: Option<PaymentStatus>,
    pub sender_did: Option<String>,
    pub recipient_did: Option<String>,
    pub sender_public_id: Option<String>,
    pub recipient_public_id: Option<String>,
    pub remote_node_id: Option<String>,
    pub mission_id: Option<String>,
    pub task_id: Option<String>,
    pub rail: Option<String>,
    pub since: Option<i64>,
    pub limit: Option<usize>,
}

/// Summary statistics for payments.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaymentSummary {
    pub total_proposed: usize,
    pub total_authorized: usize,
    pub total_settled: usize,
    pub total_rejected: usize,
    pub total_cancelled: usize,
    pub total_expired: usize,
    pub total_amount_settled: BTreeMap<String, String>,
}

/// In-memory payment ledger backed by JSON file persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaymentLedger {
    payments: BTreeMap<String, PaymentTransaction>,
}

impl PaymentLedger {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create payment ledger directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read payment ledger")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse payment ledger")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create payment ledger directory")?;
        }
        let json = serde_json::to_string_pretty(self).context("serialize payment ledger")?;
        fs::write(path, json).context("write payment ledger")
    }

    /// Propose a new payment from the given sender.
    pub fn propose(
        &mut self,
        sender_did: &str,
        request: ProposePaymentRequest,
    ) -> Result<PaymentTransaction> {
        let payment_id = format!("payment-{}", uuid::Uuid::new_v4());
        let now = Utc::now().timestamp();

        let transaction = PaymentTransaction {
            payment_id: payment_id.clone(),
            sender_did: sender_did.to_owned(),
            recipient_did: request.recipient_did,
            sender_public_id: request.sender_public_id,
            recipient_public_id: request.recipient_public_id,
            remote_node_id: request.remote_node_id,
            amount: request.amount,
            currency: request.currency,
            rail: request.rail,
            layer: request.layer,
            network: request.network,
            sender_address: None,
            recipient_address: request.recipient_address,
            mission_id: request.mission_id,
            task_id: request.task_id,
            description: request.description,
            metadata: request.metadata,
            status: PaymentStatus::Proposed,
            authorization_signature: None,
            authorization_public_key: None,
            settlement_receipt: None,
            reject_reason: None,
            proposed_at: now,
            authorized_at: None,
            settled_at: None,
            expires_at: request.expires_at,
        };

        self.payments.insert(payment_id, transaction.clone());
        Ok(transaction)
    }

    /// Authorize a proposed payment with a cryptographic signature.
    pub fn authorize(&mut self, request: AuthorizePaymentRequest) -> Result<PaymentTransaction> {
        let transaction = self
            .payments
            .get_mut(&request.payment_id)
            .ok_or_else(|| anyhow::anyhow!("payment not found: {}", request.payment_id))?;

        if transaction.status != PaymentStatus::Proposed {
            bail!(
                "payment {} is not in proposed state (current: {:?})",
                request.payment_id,
                transaction.status
            );
        }

        // Check if the payment has expired
        if let Some(expires_at) = transaction.expires_at
            && Utc::now().timestamp() > expires_at
        {
            transaction.status = PaymentStatus::Expired;
            bail!("payment {} has expired", request.payment_id);
        }

        transaction.status = PaymentStatus::Authorized;
        transaction.authorization_signature = Some(request.authorization_signature);
        transaction.authorization_public_key = request.authorization_public_key;
        transaction.sender_address = request.sender_address;
        transaction.authorized_at = Some(Utc::now().timestamp());

        Ok(transaction.clone())
    }

    /// Mark an authorized payment as submitted to the settlement rail.
    pub fn submit(
        &mut self,
        payment_id: &str,
        settlement_receipt: Option<Value>,
    ) -> Result<PaymentTransaction> {
        let transaction = self
            .payments
            .get_mut(payment_id)
            .ok_or_else(|| anyhow::anyhow!("payment not found: {payment_id}"))?;

        if transaction.status != PaymentStatus::Authorized {
            bail!(
                "payment {payment_id} is not in authorized state (current: {:?})",
                transaction.status
            );
        }

        transaction.status = PaymentStatus::Submitted;
        transaction.settlement_receipt = settlement_receipt;
        Ok(transaction.clone())
    }

    /// Settle a submitted payment with a receipt from the settlement rail.
    pub fn settle(&mut self, request: SettlePaymentRequest) -> Result<PaymentTransaction> {
        let transaction = self
            .payments
            .get_mut(&request.payment_id)
            .ok_or_else(|| anyhow::anyhow!("payment not found: {}", request.payment_id))?;

        if !matches!(
            transaction.status,
            PaymentStatus::Submitted | PaymentStatus::Authorized
        ) {
            bail!(
                "payment {} is not in a settleable state (current: {:?})",
                request.payment_id,
                transaction.status
            );
        }

        if transaction.rail.eq_ignore_ascii_case("x402") {
            validate_x402_settlement_receipt(transaction, &request.settlement_receipt)?;
        }

        transaction.status = PaymentStatus::Settled;
        transaction.settlement_receipt = Some(request.settlement_receipt);
        transaction.settled_at = Some(Utc::now().timestamp());

        Ok(transaction.clone())
    }

    /// Reject a payment.
    pub fn reject(&mut self, request: RejectPaymentRequest) -> Result<PaymentTransaction> {
        let transaction = self
            .payments
            .get_mut(&request.payment_id)
            .ok_or_else(|| anyhow::anyhow!("payment not found: {}", request.payment_id))?;

        if matches!(
            transaction.status,
            PaymentStatus::Settled | PaymentStatus::Cancelled | PaymentStatus::Rejected
        ) {
            bail!(
                "payment {} cannot be rejected in state {:?}",
                request.payment_id,
                transaction.status
            );
        }

        transaction.status = PaymentStatus::Rejected;
        transaction.reject_reason = Some(request.reject_reason);

        Ok(transaction.clone())
    }

    /// Cancel a proposed or authorized payment.
    pub fn cancel(&mut self, payment_id: &str) -> Result<PaymentTransaction> {
        let transaction = self
            .payments
            .get_mut(payment_id)
            .ok_or_else(|| anyhow::anyhow!("payment not found: {payment_id}"))?;

        if !matches!(
            transaction.status,
            PaymentStatus::Proposed | PaymentStatus::Authorized
        ) {
            bail!(
                "payment {payment_id} cannot be cancelled in state {:?}",
                transaction.status
            );
        }

        transaction.status = PaymentStatus::Cancelled;
        Ok(transaction.clone())
    }

    /// Get a specific payment by ID.
    #[must_use]
    pub fn get(&self, payment_id: &str) -> Option<&PaymentTransaction> {
        self.payments.get(payment_id)
    }

    /// Query payments matching the given criteria.
    #[must_use]
    pub fn query(&self, query: &PaymentQuery) -> Vec<&PaymentTransaction> {
        let mut results: Vec<&PaymentTransaction> = self
            .payments
            .values()
            .filter(|payment| {
                if let Some(ref status) = query.status
                    && payment.status != *status
                {
                    return false;
                }
                if let Some(ref sender_did) = query.sender_did
                    && payment.sender_did != *sender_did
                {
                    return false;
                }
                if let Some(ref recipient_did) = query.recipient_did
                    && payment.recipient_did != *recipient_did
                {
                    return false;
                }
                if let Some(ref sender_public_id) = query.sender_public_id
                    && payment.sender_public_id != *sender_public_id
                {
                    return false;
                }
                if let Some(ref recipient_public_id) = query.recipient_public_id
                    && payment.recipient_public_id != *recipient_public_id
                {
                    return false;
                }
                if let Some(ref remote_node_id) = query.remote_node_id
                    && payment.remote_node_id != *remote_node_id
                {
                    return false;
                }
                if let Some(ref mission_id) = query.mission_id
                    && payment.mission_id.as_deref() != Some(mission_id.as_str())
                {
                    return false;
                }
                if let Some(ref task_id) = query.task_id
                    && payment.task_id.as_deref() != Some(task_id.as_str())
                {
                    return false;
                }
                if let Some(ref rail) = query.rail
                    && payment.rail != *rail
                {
                    return false;
                }
                if let Some(since) = query.since
                    && payment.proposed_at < since
                {
                    return false;
                }
                true
            })
            .collect();

        // Sort by proposed_at descending (newest first)
        results.sort_by_key(|payment| std::cmp::Reverse(payment.proposed_at));

        if let Some(limit) = query.limit {
            results.truncate(limit);
        }

        results
    }

    /// Compute summary statistics for all payments.
    #[must_use]
    pub fn summary(&self) -> PaymentSummary {
        let mut summary = PaymentSummary::default();

        for payment in self.payments.values() {
            match payment.status {
                PaymentStatus::Proposed => summary.total_proposed += 1,
                PaymentStatus::Authorized => summary.total_authorized += 1,
                PaymentStatus::Submitted => {}
                PaymentStatus::Settled => {
                    summary.total_settled += 1;
                    let entry = summary
                        .total_amount_settled
                        .entry(payment.currency.clone())
                        .or_insert_with(|| "0".to_owned());
                    if let Ok(current) = entry.parse::<u128>()
                        && let Ok(add) = payment.amount.parse::<u128>()
                    {
                        *entry = (current + add).to_string();
                    }
                }
                PaymentStatus::Rejected => summary.total_rejected += 1,
                PaymentStatus::Expired => summary.total_expired += 1,
                PaymentStatus::Cancelled => summary.total_cancelled += 1,
            }
        }

        summary
    }

    /// Get all payments (for serialization/export).
    #[must_use]
    pub fn all(&self) -> Vec<&PaymentTransaction> {
        self.payments.values().collect()
    }

    /// Count of payments in the ledger.
    #[must_use]
    pub fn len(&self) -> usize {
        self.payments.len()
    }

    /// Check if the ledger is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.payments.is_empty()
    }

    pub fn merge_remote_transaction(
        &mut self,
        transaction: PaymentTransaction,
    ) -> Result<(PaymentTransaction, bool)> {
        if let Some(existing) = self.payments.get_mut(&transaction.payment_id) {
            let changed = merge_transaction(existing, &transaction);
            Ok((existing.clone(), changed))
        } else {
            self.payments
                .insert(transaction.payment_id.clone(), transaction.clone());
            Ok((transaction, true))
        }
    }

    pub fn merge_remote_agent_message(
        &mut self,
        message: PaymentAgentMessage,
        source_agent_did: &str,
    ) -> Result<(PaymentTransaction, bool)> {
        validate_agent_payment_message(&message, source_agent_did)?;
        self.merge_remote_transaction(message.payment)
    }
}

fn merge_transaction(current: &mut PaymentTransaction, incoming: &PaymentTransaction) -> bool {
    let original = current.clone();

    if incoming.status.rank() >= current.status.rank() {
        current.status = incoming.status.clone();
    }
    if current.sender_address.is_none() {
        current.sender_address.clone_from(&incoming.sender_address);
    }
    if current.recipient_address.is_none() {
        current
            .recipient_address
            .clone_from(&incoming.recipient_address);
    }
    if current.authorization_signature.is_none() {
        current
            .authorization_signature
            .clone_from(&incoming.authorization_signature);
    }
    if current.authorization_public_key.is_none() {
        current
            .authorization_public_key
            .clone_from(&incoming.authorization_public_key);
    }
    if current.settlement_receipt.is_none() {
        current
            .settlement_receipt
            .clone_from(&incoming.settlement_receipt);
    }
    if current.reject_reason.is_none() {
        current.reject_reason.clone_from(&incoming.reject_reason);
    }
    current.authorized_at = current.authorized_at.or(incoming.authorized_at);
    current.settled_at = current.settled_at.or(incoming.settled_at);
    current.expires_at = current.expires_at.or(incoming.expires_at);

    *current != original
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::json;
    use watt_wallet::{InMemoryKeyStore, KeyStore};

    const TEST_TX_HASH: &str = "0x89c91c789e57059b17285e7ba1716a1f5ff4c5dace0ea5a5135f26158d0421b9";

    fn test_ledger() -> PaymentLedger {
        PaymentLedger::default()
    }

    fn sample_proposal(_sender_did: &str, recipient_did: &str) -> ProposePaymentRequest {
        ProposePaymentRequest {
            sender_public_id: "local-public".to_owned(),
            remote_node_id: "12D3KooRemotePeer".to_owned(),
            recipient_public_id: "remote-public".to_owned(),
            recipient_did: recipient_did.to_owned(),
            amount: "1000000000000000000".to_owned(), // 1 token in wei
            currency: "WATT".to_owned(),
            rail: "x402".to_owned(),
            layer: SettlementLayer::Web3,
            network: Some("base-sepolia".to_owned()),
            recipient_address: Some("0xrecipient123".to_owned()),
            mission_id: None,
            task_id: None,
            description: Some("Test payment".to_owned()),
            metadata: None,
            expires_at: Some(Utc::now().timestamp() + 3600),
        }
    }

    fn sign_authorization(payment: &mut PaymentTransaction) {
        let mut keystore = InMemoryKeyStore::new();
        let key_info = keystore.generate_secp256k1().unwrap();
        payment.sender_address = key_info.derived_address.clone();
        let payload = authorization_payload_bytes(payment).unwrap();
        let signature = keystore.sign_bytes(&key_info.key_handle, &payload).unwrap();
        payment.authorization_signature = Some(STANDARD.encode(signature.0));
        payment.authorization_public_key = Some(key_info.public_key_multibase);
    }

    fn authorize_for_settlement(
        ledger: &mut PaymentLedger,
        payment: &PaymentTransaction,
    ) -> PaymentTransaction {
        let mut signed = payment.clone();
        sign_authorization(&mut signed);
        ledger
            .authorize(AuthorizePaymentRequest {
                payment_id: signed.payment_id,
                authorization_signature: signed.authorization_signature.unwrap(),
                authorization_public_key: signed.authorization_public_key,
                sender_address: signed.sender_address,
            })
            .unwrap()
    }

    fn x402_success_receipt(sender_address: &str, amount: &str, network: &str) -> Value {
        json!({
            "success": true,
            "payer": sender_address,
            "transaction": TEST_TX_HASH,
            "network": network,
            "amount": amount
        })
    }

    #[test]
    fn propose_payment_creates_transaction_in_proposed_state() {
        let mut ledger = test_ledger();
        let proposal = sample_proposal("did:key:sender", "did:key:recipient");
        let payment = ledger.propose("did:key:sender", proposal).unwrap();

        assert_eq!(payment.sender_did, "did:key:sender");
        assert_eq!(payment.recipient_did, "did:key:recipient");
        assert_eq!(payment.sender_public_id, "local-public");
        assert_eq!(payment.recipient_public_id, "remote-public");
        assert_eq!(payment.status, PaymentStatus::Proposed);
        assert_eq!(payment.rail, "x402");
        assert!(payment.authorization_signature.is_none());
        assert!(payment.settlement_receipt.is_none());
    }

    #[test]
    fn authorize_payment_transitions_to_authorized() {
        let mut ledger = test_ledger();
        let proposal = sample_proposal("did:key:sender", "did:key:recipient");
        let proposed = ledger.propose("did:key:sender", proposal).unwrap();

        let authorized = ledger
            .authorize(AuthorizePaymentRequest {
                payment_id: proposed.payment_id,
                authorization_signature: "sig-ed25519-abc123".to_owned(),
                authorization_public_key: Some("zpubkey".to_owned()),
                sender_address: Some("0xsender456".to_owned()),
            })
            .unwrap();

        assert_eq!(authorized.status, PaymentStatus::Authorized);
        assert_eq!(
            authorized.authorization_signature,
            Some("sig-ed25519-abc123".to_owned())
        );
        assert_eq!(
            authorized.authorization_public_key,
            Some("zpubkey".to_owned())
        );
        assert_eq!(authorized.sender_address, Some("0xsender456".to_owned()));
        assert!(authorized.authorized_at.is_some());
    }

    #[test]
    fn submit_payment_can_store_settlement_receipt() {
        let mut ledger = test_ledger();
        let proposal = sample_proposal("did:key:sender", "did:key:recipient");
        let proposed = ledger.propose("did:key:sender", proposal).unwrap();
        let authorized = authorize_for_settlement(&mut ledger, &proposed);
        let receipt = x402_success_receipt(
            authorized.sender_address.as_deref().unwrap(),
            &authorized.amount,
            "eip155:84532",
        );

        let submitted = ledger
            .submit(&proposed.payment_id, Some(receipt.clone()))
            .unwrap();

        assert_eq!(submitted.status, PaymentStatus::Submitted);
        assert_eq!(submitted.settlement_receipt, Some(receipt));
        assert!(submitted.settled_at.is_none());
    }

    #[test]
    fn settle_payment_transitions_to_settled() {
        let mut ledger = test_ledger();
        let proposal = sample_proposal("did:key:sender", "did:key:recipient");
        let proposed = ledger.propose("did:key:sender", proposal).unwrap();
        let authorized = authorize_for_settlement(&mut ledger, &proposed);

        let settled = ledger
            .settle(SettlePaymentRequest {
                payment_id: proposed.payment_id,
                settlement_receipt: x402_success_receipt(
                    authorized.sender_address.as_deref().unwrap(),
                    &authorized.amount,
                    "eip155:84532",
                ),
            })
            .unwrap();

        assert_eq!(settled.status, PaymentStatus::Settled);
        assert!(settled.settlement_receipt.is_some());
        assert!(settled.settled_at.is_some());
    }

    #[test]
    fn settle_x402_rejects_failed_receipt() {
        let mut ledger = test_ledger();
        let proposed = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();
        let authorized = authorize_for_settlement(&mut ledger, &proposed);

        let error = ledger
            .settle(SettlePaymentRequest {
                payment_id: proposed.payment_id,
                settlement_receipt: json!({
                    "success": false,
                    "payer": authorized.sender_address.as_deref().unwrap(),
                    "transaction": TEST_TX_HASH,
                    "network": "base-sepolia",
                    "amount": authorized.amount
                }),
            })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("receipt must report success=true")
        );
    }

    #[test]
    fn settle_x402_rejects_mismatched_receipt_fields() {
        let mut ledger = test_ledger();
        let proposed = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();
        let authorized = authorize_for_settlement(&mut ledger, &proposed);

        let error = ledger
            .settle(SettlePaymentRequest {
                payment_id: proposed.payment_id,
                settlement_receipt: x402_success_receipt(
                    authorized.sender_address.as_deref().unwrap(),
                    "42",
                    "base",
                ),
            })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("amount does not match payment amount")
        );
    }

    #[test]
    fn validate_x402_settlement_receipt_accepts_pay_to_when_matching() {
        let mut ledger = test_ledger();
        let mut proposal = sample_proposal("did:key:a", "did:key:b");
        proposal.recipient_address = Some("0x0000000000000000000000000000000000000001".to_owned());
        let proposed = ledger.propose("did:key:a", proposal).unwrap();
        let authorized = authorize_for_settlement(&mut ledger, &proposed);
        let mut receipt = x402_success_receipt(
            authorized.sender_address.as_deref().unwrap(),
            &authorized.amount,
            "84532",
        );
        receipt["payTo"] = json!("0x0000000000000000000000000000000000000001");

        validate_x402_settlement_receipt(&authorized, &receipt).unwrap();
    }

    #[test]
    fn reject_payment_transitions_to_rejected() {
        let mut ledger = test_ledger();
        let proposal = sample_proposal("did:key:sender", "did:key:recipient");
        let proposed = ledger.propose("did:key:sender", proposal).unwrap();

        let rejected = ledger
            .reject(RejectPaymentRequest {
                payment_id: proposed.payment_id,
                reject_reason: "insufficient funds".to_owned(),
            })
            .unwrap();

        assert_eq!(rejected.status, PaymentStatus::Rejected);
        assert_eq!(
            rejected.reject_reason,
            Some("insufficient funds".to_owned())
        );
    }

    #[test]
    fn cancel_payment_transitions_to_cancelled() {
        let mut ledger = test_ledger();
        let proposal = sample_proposal("did:key:sender", "did:key:recipient");
        let proposed = ledger.propose("did:key:sender", proposal).unwrap();

        let cancelled = ledger.cancel(&proposed.payment_id).unwrap();
        assert_eq!(cancelled.status, PaymentStatus::Cancelled);
    }

    #[test]
    fn cannot_authorize_non_proposed_payment() {
        let mut ledger = test_ledger();
        let proposal = sample_proposal("did:key:sender", "did:key:recipient");
        let proposed = ledger.propose("did:key:sender", proposal).unwrap();

        // First authorize
        ledger
            .authorize(AuthorizePaymentRequest {
                payment_id: proposed.payment_id.clone(),
                authorization_signature: "sig".to_owned(),
                authorization_public_key: None,
                sender_address: None,
            })
            .unwrap();

        // Try to authorize again
        let result = ledger.authorize(AuthorizePaymentRequest {
            payment_id: proposed.payment_id,
            authorization_signature: "sig2".to_owned(),
            authorization_public_key: None,
            sender_address: None,
        });

        assert!(result.is_err());
    }

    #[test]
    fn cannot_reject_settled_payment() {
        let mut ledger = test_ledger();
        let proposal = sample_proposal("did:key:sender", "did:key:recipient");
        let proposed = ledger.propose("did:key:sender", proposal).unwrap();
        let authorized = authorize_for_settlement(&mut ledger, &proposed);

        ledger
            .settle(SettlePaymentRequest {
                payment_id: proposed.payment_id.clone(),
                settlement_receipt: x402_success_receipt(
                    authorized.sender_address.as_deref().unwrap(),
                    &authorized.amount,
                    "base-sepolia",
                ),
            })
            .unwrap();

        let result = ledger.reject(RejectPaymentRequest {
            payment_id: proposed.payment_id,
            reject_reason: "too late".to_owned(),
        });

        assert!(result.is_err());
    }

    #[test]
    fn query_filters_by_status() {
        let mut ledger = test_ledger();

        let p1 = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();
        let p2 = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:c"))
            .unwrap();

        ledger
            .authorize(AuthorizePaymentRequest {
                payment_id: p1.payment_id,
                authorization_signature: "sig".to_owned(),
                authorization_public_key: None,
                sender_address: None,
            })
            .unwrap();

        let proposed = ledger.query(&PaymentQuery {
            status: Some(PaymentStatus::Proposed),
            ..PaymentQuery::default()
        });
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].payment_id, p2.payment_id);

        let authorized = ledger.query(&PaymentQuery {
            status: Some(PaymentStatus::Authorized),
            ..PaymentQuery::default()
        });
        assert_eq!(authorized.len(), 1);
    }

    #[test]
    fn query_filters_by_sender_did() {
        let mut ledger = test_ledger();

        ledger
            .propose(
                "did:key:alice",
                sample_proposal("did:key:alice", "did:key:bob"),
            )
            .unwrap();
        ledger
            .propose(
                "did:key:charlie",
                sample_proposal("did:key:charlie", "did:key:bob"),
            )
            .unwrap();

        let alice_payments = ledger.query(&PaymentQuery {
            sender_did: Some("did:key:alice".to_owned()),
            ..PaymentQuery::default()
        });
        assert_eq!(alice_payments.len(), 1);
        assert_eq!(alice_payments[0].sender_did, "did:key:alice");
    }

    #[test]
    fn summary_counts_statuses() {
        let mut ledger = test_ledger();

        let p1 = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();
        let _p2 = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:c"))
            .unwrap();
        let authorized = authorize_for_settlement(&mut ledger, &p1);

        ledger
            .settle(SettlePaymentRequest {
                payment_id: p1.payment_id,
                settlement_receipt: x402_success_receipt(
                    authorized.sender_address.as_deref().unwrap(),
                    &authorized.amount,
                    "base-sepolia",
                ),
            })
            .unwrap();

        let summary = ledger.summary();
        assert_eq!(summary.total_proposed, 1);
        assert_eq!(summary.total_settled, 1);
        assert_eq!(
            summary.total_amount_settled.get("WATT"),
            Some(&"1000000000000000000".to_owned())
        );
    }

    #[test]
    fn ledger_round_trips_json() {
        let mut ledger = test_ledger();
        ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();

        let json = serde_json::to_string_pretty(&ledger).unwrap();
        let deserialized: PaymentLedger = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.len(), 1);
    }

    #[test]
    fn ledger_persists_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payments.json");

        let mut ledger = PaymentLedger::load_or_new(&path).unwrap();
        let payment = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();
        ledger.persist(&path).unwrap();

        let loaded = PaymentLedger::load_or_new(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.get(&payment.payment_id).unwrap().sender_did,
            "did:key:a"
        );
    }

    #[test]
    fn merge_remote_transaction_advances_status_without_losing_fields() {
        let mut ledger = test_ledger();
        let mut proposed = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();
        proposed.status = PaymentStatus::Authorized;
        proposed.sender_address = Some("0xsender".to_owned());
        proposed.authorization_signature = Some("sig".to_owned());
        proposed.authorization_public_key = Some("zpub".to_owned());

        let (merged, changed) = ledger.merge_remote_transaction(proposed).unwrap();
        assert!(changed);
        assert_eq!(merged.status, PaymentStatus::Authorized);
        assert_eq!(merged.sender_address.as_deref(), Some("0xsender"));
        assert_eq!(merged.authorization_public_key.as_deref(), Some("zpub"));
    }

    #[test]
    fn validate_agent_payment_message_requires_expected_sender_for_request() {
        let mut ledger = test_ledger();
        let proposed = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();
        let message = proposed.agent_message(PaymentMessageKind::Request, 1);

        assert!(validate_agent_payment_message(&message, "did:key:a").is_ok());
        let error = validate_agent_payment_message(&message, "did:key:b").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("payment_request must be sent by did:key:a")
        );
    }

    #[test]
    fn source_payment_account_binding_required_only_for_sender_payment_account_claims() {
        let mut ledger = test_ledger();
        let mut payment = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();

        assert!(!source_payment_account_binding_required(
            &PaymentMessageKind::Request,
            &payment,
            "did:key:a"
        ));

        payment.sender_address = Some("0x0000000000000000000000000000000000000001".to_owned());
        assert!(source_payment_account_binding_required(
            &PaymentMessageKind::Request,
            &payment,
            "did:key:a"
        ));
        assert!(source_payment_account_binding_required(
            &PaymentMessageKind::Authorized,
            &payment,
            "did:key:a"
        ));
        assert!(!source_payment_account_binding_required(
            &PaymentMessageKind::Rejected,
            &payment,
            "did:key:b"
        ));
    }

    #[test]
    fn validate_agent_payment_message_verifies_authorization_signature() {
        let mut ledger = test_ledger();
        let mut payment = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();
        payment.status = PaymentStatus::Authorized;
        sign_authorization(&mut payment);
        let message = payment.agent_message(PaymentMessageKind::Authorized, 1);

        validate_agent_payment_message(&message, "did:key:a").unwrap();
    }

    #[test]
    fn validate_agent_payment_message_rejects_tampered_authorization_payload() {
        let mut ledger = test_ledger();
        let mut payment = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();
        payment.status = PaymentStatus::Authorized;
        sign_authorization(&mut payment);
        payment.amount = "2".to_owned();
        let message = payment.agent_message(PaymentMessageKind::Authorized, 1);

        let error = validate_agent_payment_message(&message, "did:key:a").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("verify payment authorization signature")
        );
    }

    #[test]
    fn validate_agent_payment_message_rejects_sender_address_mismatch() {
        let mut ledger = test_ledger();
        let mut payment = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();
        payment.status = PaymentStatus::Authorized;
        payment.sender_address = Some("0x0000000000000000000000000000000000000000".to_owned());
        let mut keystore = InMemoryKeyStore::new();
        let key_info = keystore.generate_secp256k1().unwrap();
        let payload = authorization_payload_bytes(&payment).unwrap();
        let signature = keystore.sign_bytes(&key_info.key_handle, &payload).unwrap();
        payment.authorization_signature = Some(STANDARD.encode(signature.0));
        payment.authorization_public_key = Some(key_info.public_key_multibase);
        let message = payment.agent_message(PaymentMessageKind::Authorized, 1);

        let error = validate_agent_payment_message(&message, "did:key:a").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("public key does not match sender address")
        );
    }

    #[test]
    fn merge_remote_agent_message_validates_actor_before_merge() {
        let mut ledger = test_ledger();
        let payment = ledger
            .propose("did:key:a", sample_proposal("did:key:a", "did:key:b"))
            .unwrap();
        let message = payment.agent_message(PaymentMessageKind::Request, 1);

        let error = ledger
            .merge_remote_agent_message(message, "did:key:b")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("payment_request must be sent by did:key:a")
        );
    }
}
