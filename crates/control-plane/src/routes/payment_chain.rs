use anyhow::{Context, Result, bail};
use k256::ecdsa::SigningKey;
use reqwest::Client;
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};
use std::path::Path;
use watt_wallet::{KeyStore, PaymentAccountKind};
use wattetheria_kernel::payments::{
    PaymentTransaction, SettlementLayer, validate_x402_settlement_receipt,
};
use wattetheria_kernel::wallet_identity::open_local_wallet;

const ERC20_TRANSFER_SELECTOR: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
const BASE_RPC_URL: &str = "https://mainnet.base.org";
const BASE_SEPOLIA_RPC_URL: &str = "https://sepolia.base.org";
const BASE_USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const BASE_USDT: &str = "0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2";
const BASE_SEPOLIA_USDC: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
const ERC20_TRANSFER_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
#[cfg(test)]
static TEST_BASE_SEPOLIA_RPC_URL: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

struct ChainConfig {
    chain_id: u64,
    network: &'static str,
    rpc_url: String,
    asset: String,
}

struct Erc20TransferRequest<'a> {
    rpc_url: &'a str,
    chain_id: u64,
    network: &'a str,
    secret: [u8; 32],
    from: &'a str,
    token: &'a str,
    to: &'a str,
    amount: u128,
}

struct EvmReceiptExpectations<'a> {
    tx_hash: &'a str,
    token: &'a str,
    from: &'a str,
    to: &'a str,
    amount: u128,
}

#[cfg(test)]
pub(crate) struct TestBaseSepoliaRpcUrlGuard {
    previous: Option<String>,
}

#[cfg(test)]
impl Drop for TestBaseSepoliaRpcUrlGuard {
    fn drop(&mut self) {
        let mut override_url = TEST_BASE_SEPOLIA_RPC_URL.lock().expect("test rpc url lock");
        *override_url = self.previous.take();
    }
}

#[cfg(test)]
pub(crate) fn set_test_base_sepolia_rpc_url(value: String) -> TestBaseSepoliaRpcUrlGuard {
    let mut override_url = TEST_BASE_SEPOLIA_RPC_URL.lock().expect("test rpc url lock");
    let previous = override_url.replace(value);
    TestBaseSepoliaRpcUrlGuard { previous }
}

pub(crate) async fn submit_x402_erc20_payment(
    data_dir: &Path,
    payment: &PaymentTransaction,
) -> Result<Option<Value>> {
    if !payment.rail.eq_ignore_ascii_case("x402") || !matches!(payment.layer, SettlementLayer::Web3)
    {
        return Ok(None);
    }

    let config = resolve_chain_config(payment)?;
    let sender_address = required_payment_address(payment.sender_address.as_deref(), "sender")?;
    let recipient_address =
        required_payment_address(payment.recipient_address.as_deref(), "recipient")?;
    let amount = payment
        .amount
        .parse::<u128>()
        .context("parse payment amount as token base units")?;
    if amount == 0 {
        bail!("payment amount must be greater than zero");
    }

    let secret = export_payment_secret(data_dir, payment, sender_address)?;
    submit_erc20_transfer(Erc20TransferRequest {
        rpc_url: &config.rpc_url,
        chain_id: config.chain_id,
        network: config.network,
        secret,
        from: sender_address,
        token: &config.asset,
        to: recipient_address,
        amount,
    })
    .await
    .map(Some)
}

pub(crate) async fn verify_x402_erc20_settlement_receipt(
    payment: &PaymentTransaction,
    receipt: &Value,
) -> Result<()> {
    if !payment.rail.eq_ignore_ascii_case("x402") || !matches!(payment.layer, SettlementLayer::Web3)
    {
        return Ok(());
    }

    validate_x402_settlement_receipt(payment, receipt)?;
    let config = resolve_chain_config(payment)?;
    let tx_hash = required_receipt_string(receipt, &["transaction", "tx_hash", "txHash"])
        .context("read x402 settlement transaction hash")?;
    if !is_evm_transaction_hash(tx_hash) {
        bail!("x402 settlement transaction must be an EVM transaction hash");
    }
    let sender_address = required_payment_address(payment.sender_address.as_deref(), "sender")?;
    let recipient_address =
        required_payment_address(payment.recipient_address.as_deref(), "recipient")?;
    let amount = payment
        .amount
        .parse::<u128>()
        .context("parse payment amount as token base units")?;
    verify_erc20_transfer_receipt(
        &config,
        EvmReceiptExpectations {
            tx_hash,
            token: &config.asset,
            from: sender_address,
            to: recipient_address,
            amount,
        },
    )
    .await
}

fn resolve_chain_config(payment: &PaymentTransaction) -> Result<ChainConfig> {
    let network = payment
        .network
        .as_deref()
        .map(canonical_network)
        .ok_or_else(|| anyhow::anyhow!("x402 chain payment requires network"))?;
    let (chain_id, rpc_url) = match network.as_str() {
        "base" => (8453, chain_rpc_url("base", BASE_RPC_URL)),
        "base-sepolia" => (84532, chain_rpc_url("base-sepolia", BASE_SEPOLIA_RPC_URL)),
        _ => bail!("x402 chain payment does not support network {network}"),
    };
    let asset = metadata_asset(payment).or_else(|| default_asset(&network, &payment.currency));
    let Some(asset) = asset else {
        bail!(
            "x402 chain payment has no ERC20 asset configured for {} on {network}",
            payment.currency
        );
    };
    required_payment_address(Some(&asset), "asset")?;
    Ok(ChainConfig {
        chain_id,
        network: match network.as_str() {
            "base" => "eip155:8453",
            "base-sepolia" => "eip155:84532",
            _ => unreachable!("unsupported network checked above"),
        },
        rpc_url,
        asset,
    })
}

fn chain_rpc_url(network: &str, default_url: &str) -> String {
    let _ = network;
    #[cfg(test)]
    if network == "base-sepolia"
        && let Some(value) = TEST_BASE_SEPOLIA_RPC_URL
            .lock()
            .expect("test rpc url lock")
            .clone()
    {
        return value;
    }
    default_url.to_owned()
}

fn metadata_asset(payment: &PaymentTransaction) -> Option<String> {
    payment
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("x402_accept"))
        .and_then(|accept| accept.get("asset"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn default_asset(network: &str, currency: &str) -> Option<String> {
    let currency = currency.trim().to_ascii_uppercase();
    match (network, currency.as_str()) {
        ("base", "USDC") => Some(BASE_USDC.to_owned()),
        ("base", "USDT") => Some(BASE_USDT.to_owned()),
        ("base-sepolia", "USDC") => Some(BASE_SEPOLIA_USDC.to_owned()),
        _ => None,
    }
}

fn export_payment_secret(
    data_dir: &Path,
    payment: &PaymentTransaction,
    sender_address: &str,
) -> Result<[u8; 32]> {
    let wallet_state = open_local_wallet(data_dir).context("open local wallet")?;
    let payment_network = payment.network.as_deref().map(canonical_network);
    let account = wallet_state
        .profile
        .payment_accounts
        .iter()
        .find(|account| {
            account.kind == PaymentAccountKind::Web3Evm
                && account.key_handle.is_some()
                && account
                    .address
                    .as_deref()
                    .is_some_and(|address| address.eq_ignore_ascii_case(sender_address))
                && account.rail.eq_ignore_ascii_case(&payment.rail)
                && account.network.as_deref().map(canonical_network) == payment_network
        })
        .ok_or_else(|| anyhow::anyhow!("local signing payment account not found for sender"))?;
    let key_handle = account
        .key_handle
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("payment account is watch-only"))?;
    wallet_state
        .wallet
        .keystore()
        .export_secp256k1_secret(key_handle)
        .context("export payment account secret")
}

async fn submit_erc20_transfer(request: Erc20TransferRequest<'_>) -> Result<Value> {
    let client = Client::new();
    let rpc_chain_id = rpc_hex_u64(&client, request.rpc_url, "eth_chainId", json!([])).await?;
    if rpc_chain_id != request.chain_id {
        bail!(
            "RPC chain id {rpc_chain_id} does not match payment network chain id {}",
            request.chain_id
        );
    }
    let nonce = rpc_hex_u64(
        &client,
        request.rpc_url,
        "eth_getTransactionCount",
        json!([request.from, "pending"]),
    )
    .await?;
    let gas_price = rpc_hex_u128(&client, request.rpc_url, "eth_gasPrice", json!([])).await?;
    let data = erc20_transfer_calldata(request.to, request.amount)?;
    let gas_limit = rpc_hex_u64(
        &client,
        request.rpc_url,
        "eth_estimateGas",
        json!([{
            "from": request.from,
            "to": request.token,
            "value": "0x0",
            "data": format!("0x{}", hex::encode(&data)),
        }]),
    )
    .await?
    .saturating_add(20_000);
    let raw_tx = sign_legacy_erc20_transaction(
        request.chain_id,
        nonce,
        gas_price,
        gas_limit,
        request.token,
        &data,
        request.secret,
    )?;
    let tx_hash = rpc_string(
        &client,
        request.rpc_url,
        "eth_sendRawTransaction",
        json!([raw_tx]),
    )
    .await?;
    Ok(json!({
        "success": true,
        "payer": request.from,
        "transaction": tx_hash,
        "network": request.network,
        "amount": request.amount.to_string(),
        "payTo": request.to,
        "asset": request.token,
        "rail": "x402",
    }))
}

async fn verify_erc20_transfer_receipt(
    config: &ChainConfig,
    expected: EvmReceiptExpectations<'_>,
) -> Result<()> {
    let client = Client::new();
    let rpc_chain_id = rpc_hex_u64(&client, &config.rpc_url, "eth_chainId", json!([])).await?;
    if rpc_chain_id != config.chain_id {
        bail!(
            "RPC chain id {rpc_chain_id} does not match payment network chain id {}",
            config.chain_id
        );
    }
    let receipt = rpc_value(
        &client,
        &config.rpc_url,
        "eth_getTransactionReceipt",
        json!([expected.tx_hash]),
    )
    .await?;
    if receipt.is_null() {
        bail!("x402 settlement transaction was not found on chain");
    }
    if receipt.get("status").and_then(Value::as_str) != Some("0x1") {
        bail!("x402 settlement transaction did not succeed on chain");
    }
    if let Some(transaction_hash) = receipt.get("transactionHash").and_then(Value::as_str)
        && !transaction_hash.eq_ignore_ascii_case(expected.tx_hash)
    {
        bail!("x402 settlement receipt transactionHash does not match receipt transaction");
    }
    if let Some(token_address) = receipt.get("to").and_then(Value::as_str)
        && !token_address.eq_ignore_ascii_case(expected.token)
    {
        bail!("x402 settlement transaction target does not match payment asset");
    }
    if !receipt_has_matching_erc20_transfer(&receipt, &expected)? {
        bail!("x402 settlement receipt missing matching ERC20 Transfer event");
    }
    Ok(())
}

async fn rpc_hex_u64(client: &Client, rpc_url: &str, method: &str, params: Value) -> Result<u64> {
    let value = rpc_string(client, rpc_url, method, params).await?;
    parse_hex_u64(&value).with_context(|| format!("parse {method} response"))
}

async fn rpc_hex_u128(client: &Client, rpc_url: &str, method: &str, params: Value) -> Result<u128> {
    let value = rpc_string(client, rpc_url, method, params).await?;
    parse_hex_u128(&value).with_context(|| format!("parse {method} response"))
}

async fn rpc_value(client: &Client, rpc_url: &str, method: &str, params: Value) -> Result<Value> {
    let response = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .send()
        .await
        .with_context(|| format!("send {method} RPC request"))?;
    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .with_context(|| format!("decode {method} RPC response"))?;
    if !status.is_success() {
        bail!("{method} RPC request failed with HTTP {status}: {payload}");
    }
    if let Some(error) = payload.get("error") {
        bail!("{method} RPC returned error: {error}");
    }
    payload
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{method} RPC response missing result"))
}

async fn rpc_string(client: &Client, rpc_url: &str, method: &str, params: Value) -> Result<String> {
    rpc_value(client, rpc_url, method, params)
        .await?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("{method} RPC response missing string result"))
}

fn sign_legacy_erc20_transaction(
    chain_id: u64,
    nonce: u64,
    gas_price: u128,
    gas_limit: u64,
    token: &str,
    data: &[u8],
    secret: [u8; 32],
) -> Result<String> {
    let to = decode_evm_address(token)?;
    let unsigned = rlp_list(&[
        rlp_u64(nonce),
        rlp_u128(gas_price),
        rlp_u64(gas_limit),
        rlp_bytes(&to),
        rlp_u64(0),
        rlp_bytes(data),
        rlp_u64(chain_id),
        rlp_bytes(&[]),
        rlp_bytes(&[]),
    ]);
    let digest = Keccak256::digest(&unsigned);
    let signing_key =
        SigningKey::from_bytes((&secret).into()).context("load payment signing key")?;
    let (signature, recovery_id) = signing_key
        .sign_prehash_recoverable(&digest)
        .context("sign ethereum transaction")?;
    let signature_bytes = signature.to_bytes();
    let recovery_byte = u64::from(recovery_id.to_byte() & 1);
    let v = chain_id
        .checked_mul(2)
        .and_then(|value| value.checked_add(35 + recovery_byte))
        .ok_or_else(|| anyhow::anyhow!("ethereum signature v overflow"))?;
    let signed = rlp_list(&[
        rlp_u64(nonce),
        rlp_u128(gas_price),
        rlp_u64(gas_limit),
        rlp_bytes(&to),
        rlp_u64(0),
        rlp_bytes(data),
        rlp_u64(v),
        rlp_uint_be(&signature_bytes[..32]),
        rlp_uint_be(&signature_bytes[32..]),
    ]);
    Ok(format!("0x{}", hex::encode(signed)))
}

fn erc20_transfer_calldata(to: &str, amount: u128) -> Result<Vec<u8>> {
    let address = decode_evm_address(to)?;
    let mut data = Vec::with_capacity(68);
    data.extend_from_slice(&ERC20_TRANSFER_SELECTOR);
    data.extend_from_slice(&[0_u8; 12]);
    data.extend_from_slice(&address);
    let mut amount_bytes = [0_u8; 32];
    amount_bytes[16..].copy_from_slice(&amount.to_be_bytes());
    data.extend_from_slice(&amount_bytes);
    Ok(data)
}

fn decode_evm_address(address: &str) -> Result<[u8; 20]> {
    let address = required_payment_address(Some(address), "address")?;
    let bytes = hex::decode(&address[2..]).context("decode EVM address")?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid EVM address length"))
}

fn receipt_has_matching_erc20_transfer(
    receipt: &Value,
    expected: &EvmReceiptExpectations<'_>,
) -> Result<bool> {
    let Some(logs) = receipt.get("logs").and_then(Value::as_array) else {
        return Ok(false);
    };
    let expected_from = decode_evm_address(expected.from)?;
    let expected_to = decode_evm_address(expected.to)?;
    for log in logs {
        if !log
            .get("address")
            .and_then(Value::as_str)
            .is_some_and(|address| address.eq_ignore_ascii_case(expected.token))
        {
            continue;
        }
        let Some(topics) = log.get("topics").and_then(Value::as_array) else {
            continue;
        };
        if topics.len() < 3
            || topics[0]
                .as_str()
                .is_none_or(|topic| !topic.eq_ignore_ascii_case(ERC20_TRANSFER_TOPIC))
        {
            continue;
        }
        if !indexed_address_topic_matches(&topics[1], &expected_from)
            || !indexed_address_topic_matches(&topics[2], &expected_to)
        {
            continue;
        }
        if let Some(amount) = log.get("data").and_then(Value::as_str)
            && parse_hex_u128(amount)? == expected.amount
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn indexed_address_topic_matches(topic: &Value, expected: &[u8; 20]) -> bool {
    let Some(topic) = topic.as_str() else {
        return false;
    };
    let topic = topic.trim_start_matches("0x");
    topic.len() == 64
        && topic[..24].chars().all(|value| value == '0')
        && hex::decode(&topic[24..])
            .ok()
            .is_some_and(|address| address.as_slice() == expected)
}

fn required_payment_address<'a>(address: Option<&'a str>, name: &str) -> Result<&'a str> {
    let address =
        address.ok_or_else(|| anyhow::anyhow!("x402 chain payment requires {name} address"))?;
    if !is_evm_address(address) {
        bail!("invalid {name} EVM address");
    }
    Ok(address)
}

fn is_evm_address(address: &str) -> bool {
    address.len() == 42
        && address.starts_with("0x")
        && address
            .chars()
            .skip(2)
            .all(|value| value.is_ascii_hexdigit())
}

fn parse_hex_u64(value: &str) -> Result<u64> {
    u64::from_str_radix(value.trim_start_matches("0x"), 16).context("parse hex u64")
}

fn parse_hex_u128(value: &str) -> Result<u128> {
    u128::from_str_radix(value.trim_start_matches("0x"), 16).context("parse hex u128")
}

fn required_receipt_string<'a>(receipt: &'a Value, keys: &[&str]) -> Result<&'a str> {
    keys.iter()
        .find_map(|key| receipt.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("x402 settlement receipt missing {}", keys[0]))
}

fn is_evm_transaction_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value
            .chars()
            .skip(2)
            .all(|character| character.is_ascii_hexdigit())
}

fn canonical_network(network: &str) -> String {
    match network.trim().to_ascii_lowercase().as_str() {
        "base" | "base-mainnet" | "eip155:8453" | "8453" => "base".to_owned(),
        "base-sepolia" | "base_sepolia" | "eip155:84532" | "84532" => "base-sepolia".to_owned(),
        other => other.to_owned(),
    }
}

fn rlp_u64(value: u64) -> Vec<u8> {
    rlp_u128(u128::from(value))
}

fn rlp_u128(value: u128) -> Vec<u8> {
    if value == 0 {
        return rlp_bytes(&[]);
    }
    let bytes = value.to_be_bytes();
    rlp_uint_be(&bytes)
}

fn rlp_uint_be(bytes: &[u8]) -> Vec<u8> {
    let first_non_zero = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len());
    if first_non_zero == bytes.len() {
        return rlp_bytes(&[]);
    }
    rlp_bytes(&bytes[first_non_zero..])
}

fn rlp_bytes(bytes: &[u8]) -> Vec<u8> {
    if bytes.len() == 1 && bytes[0] < 0x80 {
        return vec![bytes[0]];
    }
    if bytes.len() < 56 {
        let mut out = Vec::with_capacity(1 + bytes.len());
        out.push(0x80 + u8::try_from(bytes.len()).unwrap());
        out.extend_from_slice(bytes);
        return out;
    }
    let len_bytes = length_bytes(bytes.len());
    let mut out = Vec::with_capacity(1 + len_bytes.len() + bytes.len());
    out.push(0xb7 + u8::try_from(len_bytes.len()).unwrap());
    out.extend_from_slice(&len_bytes);
    out.extend_from_slice(bytes);
    out
}

fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let payload = items.concat();
    if payload.len() < 56 {
        let mut out = Vec::with_capacity(1 + payload.len());
        out.push(0xc0 + u8::try_from(payload.len()).unwrap());
        out.extend_from_slice(&payload);
        return out;
    }
    let len_bytes = length_bytes(payload.len());
    let mut out = Vec::with_capacity(1 + len_bytes.len() + payload.len());
    out.push(0xf7 + u8::try_from(len_bytes.len()).unwrap());
    out.extend_from_slice(&len_bytes);
    out.extend_from_slice(&payload);
    out
}

fn length_bytes(length: usize) -> Vec<u8> {
    let bytes = length.to_be_bytes();
    let first_non_zero = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len() - 1);
    bytes[first_non_zero..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    const TEST_TX_HASH: &str = "0x89c91c789e57059b17285e7ba1716a1f5ff4c5dace0ea5a5135f26158d0421b9";
    const TEST_SENDER: &str = "0x1111111111111111111111111111111111111111";
    const TEST_RECIPIENT: &str = "0x2222222222222222222222222222222222222222";

    #[test]
    fn erc20_transfer_calldata_encodes_recipient_and_amount() {
        let data = erc20_transfer_calldata("0x0000000000000000000000000000000000000001", 1_500_000)
            .unwrap();

        assert_eq!(&data[..4], &ERC20_TRANSFER_SELECTOR);
        assert_eq!(
            hex::encode(&data[4..36]),
            "0000000000000000000000000000000000000000000000000000000000000001"
        );
        assert_eq!(
            hex::encode(&data[36..68]),
            "000000000000000000000000000000000000000000000000000000000016e360"
        );
    }

    #[tokio::test]
    async fn submit_erc20_transfer_posts_signed_transaction_and_returns_receipt() {
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = Router::new()
            .route("/", post(mock_rpc))
            .with_state(calls.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let secret = hex_secret("4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318");

        let receipt = submit_erc20_transfer(Erc20TransferRequest {
            rpc_url: &format!("http://{addr}"),
            chain_id: 84532,
            network: "eip155:84532",
            secret,
            from: "0x1111111111111111111111111111111111111111",
            token: BASE_SEPOLIA_USDC,
            to: "0x2222222222222222222222222222222222222222",
            amount: 2_500_000,
        })
        .await
        .unwrap();

        assert_eq!(receipt["success"].as_bool(), Some(true));
        assert_eq!(receipt["transaction"].as_str(), Some(TEST_TX_HASH));
        assert_eq!(receipt["network"].as_str(), Some("eip155:84532"));
        let calls = calls.lock().await;
        assert_eq!(calls.len(), 5);
        assert_eq!(calls[4]["method"].as_str(), Some("eth_sendRawTransaction"));
        let raw = calls[4]["params"][0].as_str().unwrap();
        assert!(raw.starts_with("0x"));
        assert!(raw.len() > 200);
        server.abort();
    }

    #[tokio::test]
    async fn verify_erc20_transfer_receipt_accepts_matching_onchain_transfer() {
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = Router::new()
            .route("/", post(mock_rpc))
            .with_state(calls.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        verify_erc20_transfer_receipt(
            &ChainConfig {
                chain_id: 84532,
                network: "eip155:84532",
                rpc_url: format!("http://{addr}"),
                asset: BASE_SEPOLIA_USDC.to_owned(),
            },
            EvmReceiptExpectations {
                tx_hash: TEST_TX_HASH,
                token: BASE_SEPOLIA_USDC,
                from: TEST_SENDER,
                to: TEST_RECIPIENT,
                amount: 2_500_000,
            },
        )
        .await
        .unwrap();

        server.abort();
    }

    #[tokio::test]
    async fn verify_erc20_transfer_receipt_rejects_mismatched_transfer() {
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = Router::new()
            .route("/", post(mock_rpc))
            .with_state(calls.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let error = verify_erc20_transfer_receipt(
            &ChainConfig {
                chain_id: 84532,
                network: "eip155:84532",
                rpc_url: format!("http://{addr}"),
                asset: BASE_SEPOLIA_USDC.to_owned(),
            },
            EvmReceiptExpectations {
                tx_hash: TEST_TX_HASH,
                token: BASE_SEPOLIA_USDC,
                from: TEST_SENDER,
                to: TEST_RECIPIENT,
                amount: 1,
            },
        )
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("missing matching ERC20 Transfer event")
        );
        server.abort();
    }

    async fn mock_rpc(
        State(calls): State<Arc<Mutex<Vec<Value>>>>,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        calls.lock().await.push(payload.clone());
        let result = match payload["method"].as_str().unwrap() {
            "eth_chainId" => json!("0x14a34"),
            "eth_getTransactionCount" => json!("0x0"),
            "eth_gasPrice" => json!("0x3b9aca00"),
            "eth_estimateGas" => json!("0x186a0"),
            "eth_sendRawTransaction" => json!(TEST_TX_HASH),
            "eth_getTransactionReceipt" => json!(matching_transaction_receipt()),
            method => json!({"unexpected": method}),
        };
        Json(json!({
            "jsonrpc": "2.0",
            "id": payload["id"].clone(),
            "result": result,
        }))
    }

    fn hex_secret(value: &str) -> [u8; 32] {
        hex::decode(value).unwrap().try_into().unwrap()
    }

    fn matching_transaction_receipt() -> Value {
        json!({
            "transactionHash": TEST_TX_HASH,
            "status": "0x1",
            "to": BASE_SEPOLIA_USDC,
            "logs": [{
                "address": BASE_SEPOLIA_USDC,
                "topics": [
                    ERC20_TRANSFER_TOPIC,
                    indexed_address_topic(TEST_SENDER),
                    indexed_address_topic(TEST_RECIPIENT)
                ],
                "data": u256_hex(2_500_000)
            }]
        })
    }

    fn indexed_address_topic(address: &str) -> String {
        format!("0x{}{}", "0".repeat(24), address.trim_start_matches("0x"))
    }

    fn u256_hex(value: u128) -> String {
        format!("0x{value:064x}")
    }
}
