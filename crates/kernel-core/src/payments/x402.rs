use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::PaymentTransaction;

pub const PAYMENT_REQUIRED_HEADER: &str = "PAYMENT-REQUIRED";
pub const PAYMENT_SIGNATURE_HEADER: &str = "PAYMENT-SIGNATURE";
pub const PAYMENT_RESPONSE_HEADER: &str = "PAYMENT-RESPONSE";
pub const X402_VERSION: u64 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct X402PaymentRequirement {
    pub scheme: String,
    pub network: String,
    pub asset: Option<String>,
    pub amount: String,
    #[serde(rename = "payTo")]
    pub pay_to: String,
    #[serde(rename = "maxTimeoutSeconds")]
    pub max_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub extra: Option<Value>,
    #[serde(skip)]
    raw: Value,
}

impl X402PaymentRequirement {
    #[must_use]
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct X402PaymentRequired {
    pub x402_version: Option<u64>,
    pub accepts: Vec<X402PaymentRequirement>,
    raw: Value,
}

impl X402PaymentRequired {
    #[must_use]
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct X402SettlementResponse {
    pub success: bool,
    pub payer: String,
    pub transaction: String,
    pub network: String,
    pub amount: String,
    #[serde(rename = "errorReason")]
    pub error_reason: Option<String>,
    #[serde(rename = "errorMessage")]
    pub error_message: Option<String>,
    #[serde(skip)]
    raw: Value,
}

impl X402SettlementResponse {
    #[must_use]
    pub fn receipt_value(&self) -> Value {
        self.raw.clone()
    }
}

pub fn decode_payment_required_header(header: &str) -> Result<X402PaymentRequired> {
    let value = decode_header_json(header, PAYMENT_REQUIRED_HEADER)?;
    parse_payment_required(value)
}

pub fn decode_settlement_response_header(header: &str) -> Result<X402SettlementResponse> {
    let value = decode_header_json(header, PAYMENT_RESPONSE_HEADER)?;
    parse_settlement_response(value)
}

pub fn encode_payment_signature_header(payment_payload: &Value) -> Result<String> {
    encode_header_json(payment_payload)
}

pub fn stablecoin_amount_to_base_units(amount: &str, currency: &str) -> Result<Option<String>> {
    let Some(decimals) = stablecoin_decimals(currency) else {
        return Ok(None);
    };
    decimal_amount_to_base_units(amount, decimals, currency).map(Some)
}

#[must_use]
pub fn stablecoin_amount_from_base_units(amount: &str, currency: &str) -> Option<String> {
    let decimals = stablecoin_decimals(currency)?;
    let base_units = amount.trim().parse::<u128>().ok()?;
    Some(format_base_units_as_decimal(base_units, decimals))
}

#[must_use]
pub fn build_payment_signature_payload(
    accepted: &X402PaymentRequirement,
    scheme_payload: &Value,
    resource: Option<&Value>,
) -> Value {
    let mut payload = json!({
        "x402Version": X402_VERSION,
        "accepted": accepted.raw(),
        "payload": scheme_payload,
    });
    if let Some(resource) = resource {
        payload["resource"] = resource.clone();
    }
    payload
}

#[must_use]
pub fn select_payment_requirement<'a>(
    payment_required: &'a X402PaymentRequired,
    network: Option<&str>,
    amount: Option<&str>,
    currency: Option<&str>,
) -> Option<&'a X402PaymentRequirement> {
    payment_required.accepts.iter().find(|requirement| {
        network.is_none_or(|network| {
            canonical_x402_network(&requirement.network) == canonical_x402_network(network)
        }) && amount.is_none_or(|amount| amounts_match(&requirement.amount, amount))
            && currency.is_none_or(|currency| requirement_currency_matches(requirement, currency))
    })
}

pub fn validate_x402_settlement_receipt(
    transaction: &PaymentTransaction,
    receipt: &Value,
) -> Result<()> {
    if !transaction.rail.eq_ignore_ascii_case("x402") {
        return Ok(());
    }
    if receipt_string(receipt, &["rail"]).is_some_and(|rail| !rail.eq_ignore_ascii_case("x402")) {
        bail!("x402 settlement receipt rail must be x402");
    }
    if receipt_bool(receipt, &["success"]) != Some(true) {
        bail!("x402 settlement receipt must report success=true");
    }
    let payer = required_receipt_string(receipt, &["payer"])?;
    let sender_address = transaction
        .sender_address
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("x402 settlement requires sender_address"))?;
    if !payer.eq_ignore_ascii_case(sender_address) {
        bail!("x402 settlement payer does not match sender_address");
    }

    let transaction_hash = required_receipt_string(receipt, &["transaction", "tx_hash", "txHash"])?;
    if !is_evm_transaction_hash(transaction_hash) {
        bail!("x402 settlement transaction must be an EVM transaction hash");
    }

    let receipt_amount = required_receipt_string(receipt, &["amount"])?;
    if !amounts_match(receipt_amount, &transaction.amount) {
        bail!("x402 settlement amount does not match payment amount");
    }

    let receipt_network = required_receipt_string(receipt, &["network"])?;
    if let Some(expected_network) = transaction.network.as_deref()
        && canonical_x402_network(receipt_network) != canonical_x402_network(expected_network)
    {
        bail!("x402 settlement network does not match payment network");
    }

    if let Some(pay_to) = receipt_string(receipt, &["payTo", "pay_to", "recipient", "to"])
        && let Some(expected_recipient) = transaction.recipient_address.as_deref()
        && !pay_to.eq_ignore_ascii_case(expected_recipient)
    {
        bail!("x402 settlement payTo does not match recipient_address");
    }

    Ok(())
}

fn decode_header_json(header: &str, header_name: &str) -> Result<Value> {
    let decoded = STANDARD
        .decode(header.trim())
        .with_context(|| format!("decode {header_name} header"))?;
    serde_json::from_slice(&decoded).with_context(|| format!("parse {header_name} JSON"))
}

fn encode_header_json(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value).context("serialize x402 header JSON")?;
    Ok(STANDARD.encode(bytes))
}

fn parse_payment_required(value: Value) -> Result<X402PaymentRequired> {
    let x402_version = value.get("x402Version").and_then(Value::as_u64);
    if let Some(version) = x402_version
        && version != X402_VERSION
    {
        bail!("unsupported x402 version {version}");
    }
    let requirements = value
        .get("accepts")
        .or_else(|| value.get("paymentRequirements"))
        .or_else(|| value.get("paymentRequirementsList"))
        .ok_or_else(|| anyhow::anyhow!("x402 payment required missing accepted requirements"))?;
    let accepts = match requirements {
        Value::Array(values) => values
            .iter()
            .cloned()
            .map(parse_payment_requirement)
            .collect::<Result<Vec<_>>>()?,
        Value::Object(_) => vec![parse_payment_requirement(requirements.clone())?],
        _ => bail!("x402 payment requirements must be an object or array"),
    };
    if accepts.is_empty() {
        bail!("x402 payment required contains no accepted requirements");
    }
    Ok(X402PaymentRequired {
        x402_version,
        accepts,
        raw: value,
    })
}

fn parse_payment_requirement(value: Value) -> Result<X402PaymentRequirement> {
    let requirement: X402PaymentRequirement =
        serde_json::from_value(value.clone()).context("parse x402 payment requirement")?;
    if requirement.scheme.trim().is_empty() {
        bail!("x402 payment requirement missing scheme");
    }
    if requirement.network.trim().is_empty() {
        bail!("x402 payment requirement missing network");
    }
    if requirement.amount.trim().is_empty() {
        bail!("x402 payment requirement missing amount");
    }
    if requirement.pay_to.trim().is_empty() {
        bail!("x402 payment requirement missing payTo");
    }
    Ok(X402PaymentRequirement {
        raw: value,
        ..requirement
    })
}

fn parse_settlement_response(value: Value) -> Result<X402SettlementResponse> {
    let response: X402SettlementResponse =
        serde_json::from_value(value.clone()).context("parse x402 settlement response")?;
    Ok(X402SettlementResponse {
        raw: value,
        ..response
    })
}

fn requirement_currency_matches(requirement: &X402PaymentRequirement, currency: &str) -> bool {
    if let Some(asset) = requirement.asset.as_deref()
        && asset.eq_ignore_ascii_case(currency)
    {
        return true;
    }
    requirement
        .extra
        .as_ref()
        .and_then(|extra| extra.get("name"))
        .and_then(Value::as_str)
        .is_some_and(|name| name.eq_ignore_ascii_case(currency))
}

fn receipt_bool<'a>(receipt: &'a Value, keys: &[&'a str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| receipt.get(*key).and_then(Value::as_bool))
}

fn receipt_string<'a>(receipt: &'a Value, keys: &[&'a str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| receipt.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn required_receipt_string<'a>(receipt: &'a Value, keys: &[&'a str]) -> Result<&'a str> {
    receipt_string(receipt, keys)
        .ok_or_else(|| anyhow::anyhow!("x402 settlement receipt missing {}", keys.join("/")))
}

fn is_evm_transaction_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value.chars().skip(2).all(|value| value.is_ascii_hexdigit())
}

fn amounts_match(receipt_amount: &str, expected_amount: &str) -> bool {
    match (
        receipt_amount.parse::<u128>(),
        expected_amount.parse::<u128>(),
    ) {
        (Ok(receipt), Ok(expected)) => receipt == expected,
        _ => receipt_amount == expected_amount,
    }
}

fn stablecoin_decimals(currency: &str) -> Option<u32> {
    match currency.trim().to_ascii_uppercase().as_str() {
        "USDC" | "USDT" => Some(6),
        _ => None,
    }
}

fn decimal_amount_to_base_units(amount: &str, decimals: u32, currency: &str) -> Result<String> {
    let currency = currency.trim().to_ascii_uppercase();
    let amount = amount.trim();
    if amount.is_empty() || amount.starts_with('-') {
        bail!("{currency} payment amount must be greater than zero");
    }
    let parts = amount.split('.').collect::<Vec<_>>();
    if parts.len() > 2 {
        bail!("{currency} payment amount is not a decimal value");
    }
    let whole = if parts[0].is_empty() { "0" } else { parts[0] };
    if !whole.chars().all(|value| value.is_ascii_digit()) {
        bail!("{currency} payment amount is not a decimal value");
    }
    let whole_units = whole
        .parse::<u128>()
        .map_err(|_| anyhow::anyhow!("{currency} payment amount is too large"))?;
    let scale = 10_u128
        .checked_pow(decimals)
        .ok_or_else(|| anyhow::anyhow!("{currency} payment decimals are too large"))?;
    let fractional_units = if let Some(fractional) = parts.get(1) {
        let fractional = fractional.trim_end_matches('0');
        if !fractional.chars().all(|value| value.is_ascii_digit()) {
            bail!("{currency} payment amount is not a decimal value");
        }
        let decimals_usize = usize::try_from(decimals)
            .map_err(|_| anyhow::anyhow!("{currency} payment decimals are too large"))?;
        if fractional.len() > decimals_usize {
            bail!("{currency} payment amount has more than {decimals} decimal places");
        }
        let padded = format!("{fractional:0<decimals_usize$}");
        padded
            .parse::<u128>()
            .map_err(|_| anyhow::anyhow!("{currency} payment amount is too large"))?
    } else {
        0
    };
    let base_units = whole_units
        .checked_mul(scale)
        .and_then(|value| value.checked_add(fractional_units))
        .ok_or_else(|| anyhow::anyhow!("{currency} payment amount is too large"))?;
    if base_units == 0 {
        bail!("{currency} payment amount must be greater than zero");
    }
    Ok(base_units.to_string())
}

fn format_base_units_as_decimal(base_units: u128, decimals: u32) -> String {
    let scale = 10_u128.checked_pow(decimals).unwrap_or(1);
    let whole = base_units / scale;
    let fractional = base_units % scale;
    if fractional == 0 {
        return whole.to_string();
    }
    let width = usize::try_from(decimals).unwrap_or(0);
    let fractional = format!("{fractional:0width$}")
        .trim_end_matches('0')
        .to_string();
    format!("{whole}.{fractional}")
}

fn canonical_x402_network(network: &str) -> String {
    match network.trim().to_ascii_lowercase().as_str() {
        "base" | "base-mainnet" | "eip155:8453" | "8453" => "eip155:8453".to_owned(),
        "base-sepolia" | "base_sepolia" | "eip155:84532" | "84532" => "eip155:84532".to_owned(),
        "ethereum" | "ethereum-mainnet" | "mainnet" | "eip155:1" | "1" => "eip155:1".to_owned(),
        "polygon" | "polygon-mainnet" | "eip155:137" | "137" => "eip155:137".to_owned(),
        "optimism" | "optimism-mainnet" | "eip155:10" | "10" => "eip155:10".to_owned(),
        "arbitrum" | "arbitrum-one" | "eip155:42161" | "42161" => "eip155:42161".to_owned(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payments::{PaymentStatus, SettlementLayer};

    const TEST_TX_HASH: &str = "0x89c91c789e57059b17285e7ba1716a1f5ff4c5dace0ea5a5135f26158d0421b9";

    fn encode(value: &Value) -> String {
        encode_header_json(value).unwrap()
    }

    fn test_transaction() -> PaymentTransaction {
        PaymentTransaction {
            payment_id: "payment-1".to_owned(),
            sender_did: "did:key:sender".to_owned(),
            recipient_did: "did:key:recipient".to_owned(),
            sender_public_id: "sender".to_owned(),
            recipient_public_id: "recipient".to_owned(),
            remote_node_id: "12D3Remote".to_owned(),
            amount: "2500000".to_owned(),
            currency: "USDC".to_owned(),
            rail: "x402".to_owned(),
            layer: SettlementLayer::Web3,
            network: Some("base-sepolia".to_owned()),
            sender_address: Some("0x742d35Cc6634C0532925a3b844Bc454e4438f44e".to_owned()),
            recipient_address: Some("0x122F8Fcaf2152420445Aa424E1D8C0306935B5c9".to_owned()),
            mission_id: None,
            task_id: None,
            description: None,
            metadata: None,
            status: PaymentStatus::Authorized,
            authorization_signature: None,
            authorization_public_key: None,
            settlement_receipt: None,
            reject_reason: None,
            proposed_at: 1,
            authorized_at: Some(2),
            settled_at: None,
            expires_at: None,
        }
    }

    #[test]
    fn stablecoin_amounts_convert_between_human_and_base_units() {
        assert_eq!(
            stablecoin_amount_to_base_units("1", "USDC").unwrap(),
            Some("1000000".to_owned())
        );
        assert_eq!(
            stablecoin_amount_to_base_units("0.5", "USDT").unwrap(),
            Some("500000".to_owned())
        );
        assert_eq!(
            stablecoin_amount_to_base_units("0.18", "USDC").unwrap(),
            Some("180000".to_owned())
        );
        assert_eq!(
            stablecoin_amount_from_base_units("1000000", "USDC"),
            Some("1".to_owned())
        );
        assert_eq!(
            stablecoin_amount_from_base_units("180000", "USDC"),
            Some("0.18".to_owned())
        );
    }

    #[test]
    fn stablecoin_amounts_reject_precision_below_token_base_unit() {
        let error = stablecoin_amount_to_base_units("0.0000001", "USDC").unwrap_err();
        assert!(error.to_string().contains("more than 6 decimal places"));
    }

    #[test]
    fn decode_payment_required_header_selects_matching_requirement() {
        let header = encode(&json!({
            "x402Version": 2,
            "accepts": [
                {
                    "scheme": "exact",
                    "network": "eip155:1",
                    "asset": "0xasset-mainnet",
                    "amount": "1",
                    "payTo": "0xmainnet",
                    "maxTimeoutSeconds": 60,
                    "extra": {"name": "USDC", "version": "2"}
                },
                {
                    "scheme": "exact",
                    "network": "eip155:84532",
                    "asset": "0xasset-base-sepolia",
                    "amount": "2500000",
                    "payTo": "0x122F8Fcaf2152420445Aa424E1D8C0306935B5c9",
                    "maxTimeoutSeconds": 60,
                    "extra": {"name": "USDC", "version": "2"}
                }
            ]
        }));

        let decoded = decode_payment_required_header(&header).unwrap();
        let selected = select_payment_requirement(
            &decoded,
            Some("base-sepolia"),
            Some("2500000"),
            Some("USDC"),
        )
        .unwrap();

        assert_eq!(decoded.x402_version, Some(2));
        assert_eq!(selected.network, "eip155:84532");
        assert_eq!(selected.amount, "2500000");
    }

    #[test]
    fn build_payment_signature_payload_encodes_standard_header_json() {
        let required = parse_payment_requirement(json!({
            "scheme": "exact",
            "network": "eip155:84532",
            "asset": "0xasset-base-sepolia",
            "amount": "2500000",
            "payTo": "0x122F8Fcaf2152420445Aa424E1D8C0306935B5c9",
            "maxTimeoutSeconds": 60,
            "extra": {"name": "USDC", "version": "2"}
        }))
        .unwrap();

        let payload = build_payment_signature_payload(
            &required,
            &json!({
                "signature": "0xsig",
                "authorization": {
                    "from": "0x742d35Cc6634C0532925a3b844Bc454e4438f44e",
                    "to": "0x122F8Fcaf2152420445Aa424E1D8C0306935B5c9",
                    "value": "2500000"
                }
            }),
            Some(&json!({"url": "https://api.example.com/paid"})),
        );
        let encoded = encode_payment_signature_header(&payload).unwrap();
        let decoded = decode_header_json(&encoded, PAYMENT_SIGNATURE_HEADER).unwrap();

        assert_eq!(decoded["x402Version"].as_u64(), Some(2));
        assert_eq!(
            decoded["accepted"]["network"].as_str(),
            Some("eip155:84532")
        );
        assert_eq!(decoded["payload"]["signature"].as_str(), Some("0xsig"));
    }

    #[test]
    fn decode_settlement_response_header_returns_receipt_value() {
        let header = encode(&json!({
            "success": true,
            "payer": "0x742d35Cc6634C0532925a3b844Bc454e4438f44e",
            "transaction": TEST_TX_HASH,
            "network": "base",
            "amount": "2500000"
        }));

        let response = decode_settlement_response_header(&header).unwrap();
        let receipt = response.receipt_value();

        assert!(response.success);
        assert_eq!(receipt["transaction"].as_str(), Some(TEST_TX_HASH));
    }

    #[test]
    fn validate_x402_settlement_receipt_accepts_matching_response() {
        let transaction = test_transaction();
        let receipt = json!({
            "success": true,
            "payer": transaction.sender_address.as_deref().unwrap(),
            "transaction": TEST_TX_HASH,
            "network": "84532",
            "amount": "2500000",
            "payTo": transaction.recipient_address.as_deref().unwrap()
        });

        validate_x402_settlement_receipt(&transaction, &receipt).unwrap();
    }

    #[test]
    fn validate_x402_settlement_receipt_rejects_failed_response() {
        let transaction = test_transaction();
        let error = validate_x402_settlement_receipt(
            &transaction,
            &json!({
                "success": false,
                "payer": transaction.sender_address.as_deref().unwrap(),
                "transaction": TEST_TX_HASH,
                "network": "base-sepolia",
                "amount": "2500000"
            }),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("receipt must report success=true")
        );
    }
}
