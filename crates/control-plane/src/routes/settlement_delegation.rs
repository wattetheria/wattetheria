use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

const SERVICE_NET_AGENT_PROVIDER: &str = "servicenet-agent";

pub(crate) fn normalize_publish_delegation(value: Option<Value>) -> Result<Option<Value>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !delegation_enabled(&value)? {
        return Ok(None);
    }

    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("settlement_delegation must be a JSON object"))?;
    let layer = required_string(object, "layer")?;
    if layer != "web2" && layer != "web3" {
        bail!("settlement_delegation layer `{layer}` is not supported");
    }
    let provider = required_string(object, "provider")?;
    if provider != SERVICE_NET_AGENT_PROVIDER {
        bail!("settlement_delegation provider `{provider}` is not supported yet");
    }
    let provider_agent_id = required_string(object, "provider_agent_id")?;
    let provider_agent_name =
        required_string_any(object, &["provider_agent_name", "provider_name"])?;
    let asset = required_string(object, "asset")?;
    let amount = required_string(object, "amount")?;
    let provider_receipt = required_object(object, "provider_receipt")?;
    let status = required_string(provider_receipt, "status")?;
    validate_funding_proof(object.get("funding_proof"))?;

    let mut normalized = object.clone();
    normalized.insert("enabled".to_owned(), Value::Bool(true));
    normalized.insert("layer".to_owned(), Value::String(layer));
    normalized.insert("provider".to_owned(), Value::String(provider));
    normalized.insert(
        "provider_agent_id".to_owned(),
        Value::String(provider_agent_id),
    );
    normalized.insert(
        "provider_agent_name".to_owned(),
        Value::String(provider_agent_name),
    );
    normalized.insert("asset".to_owned(), Value::String(asset));
    normalized.insert("amount".to_owned(), Value::String(amount));
    normalized.insert("status".to_owned(), Value::String(status));
    Ok(Some(Value::Object(normalized)))
}

pub(crate) fn payload_with_settlement_delegation(
    mut payload: Value,
    delegation: Option<&Value>,
) -> Value {
    let Some(delegation) = delegation else {
        return payload;
    };
    if !payload.is_object() {
        payload = json!({});
    }
    if let Some(object) = payload.as_object_mut() {
        object.insert("settlement_delegation".to_owned(), delegation.clone());
    }
    payload
}

pub(crate) fn settlement_delegation_from_payload(payload: &Value) -> Option<&Value> {
    payload
        .get("settlement_delegation")
        .filter(|value| value.is_object())
}

fn delegation_enabled(value: &Value) -> Result<bool> {
    match value.get("enabled") {
        Some(Value::Bool(enabled)) => Ok(*enabled),
        Some(_) => bail!("settlement_delegation enabled must be a boolean"),
        None => Ok(true),
    }
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String> {
    optional_string(object, key)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("settlement_delegation {key} is required"))
}

fn required_string_any(object: &Map<String, Value>, keys: &[&str]) -> Result<String> {
    for key in keys {
        if let Some(value) = optional_string(object, key) {
            return Ok(value);
        }
    }
    bail!("settlement_delegation {} is required", keys[0])
}

fn required_object<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Map<String, Value>> {
    object
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("settlement_delegation {key} must be a JSON object"))
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn validate_funding_proof(value: Option<&Value>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let object = value.as_object().ok_or_else(|| {
        anyhow::anyhow!("settlement_delegation funding_proof must be a JSON object")
    })?;
    let proof_type = optional_string(object, "type");
    if proof_type.as_deref() == Some("evm_tx") {
        let hash = required_string(object, "tx_hash")?;
        if !is_evm_transaction_hash(&hash) {
            bail!("settlement_delegation funding_proof tx_hash must be an EVM transaction hash");
        }
    }
    Ok(())
}

fn is_evm_transaction_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value
            .chars()
            .skip(2)
            .all(|character| character.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_servicenet_agent_delegation() {
        let normalized = normalize_publish_delegation(Some(json!({
            "enabled": true,
            "layer": "web3",
            "provider": "servicenet-agent",
            "provider_agent_id": "escrow-agent-1",
            "provider_name": "ServiceNet Escrow",
            "network": "base-sepolia",
            "asset": "USDC",
            "amount": "10000000",
            "funding_proof": {
                "type": "evm_tx",
                "tx_hash": "0x3333333333333333333333333333333333333333333333333333333333333333",
                "chain_id": 84532,
                "to": "0x1111111111111111111111111111111111111111"
            },
            "provider_receipt": {
                "receipt_id": "receipt-1",
                "status": "funded",
                "task_id": "provider-task-1"
            },
            "terms": {
                "summary": "Provider-defined escrow rules.",
                "url": "https://example.test/terms"
            }
        })))
        .unwrap()
        .unwrap();

        assert_eq!(normalized["provider"].as_str(), Some("servicenet-agent"));
        assert_eq!(
            normalized["provider_agent_id"].as_str(),
            Some("escrow-agent-1")
        );
        assert_eq!(
            normalized["provider_agent_name"].as_str(),
            Some("ServiceNet Escrow")
        );
        assert_eq!(normalized["network"].as_str(), Some("base-sepolia"));
        assert_eq!(normalized["status"].as_str(), Some("funded"));
        assert_eq!(
            normalized["provider_receipt"]["receipt_id"].as_str(),
            Some("receipt-1")
        );
    }

    #[test]
    fn rejects_missing_provider_receipt_status() {
        let error = normalize_publish_delegation(Some(json!({
            "enabled": true,
            "layer": "web3",
            "provider": "servicenet-agent",
            "provider_agent_id": "escrow-agent-1",
            "provider_agent_name": "ServiceNet Escrow",
            "asset": "USDC",
            "amount": "10000000",
            "provider_receipt": {}
        })))
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("settlement_delegation status is required")
        );
    }

    #[test]
    fn rejects_unsupported_provider() {
        let error = normalize_publish_delegation(Some(json!({
            "enabled": true,
            "layer": "web3",
            "provider": "other",
            "provider_agent_id": "escrow-agent-1",
            "provider_agent_name": "ServiceNet Escrow",
            "asset": "USDC",
            "amount": "10000000",
            "provider_receipt": {"status": "funded"}
        })))
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("provider `other` is not supported")
        );
    }
}
