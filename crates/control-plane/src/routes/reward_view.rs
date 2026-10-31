use serde_json::{Value, json};
use wattetheria_kernel::wallet_identity::{active_payment_account, open_local_wallet};

use crate::state::ControlPlaneState;

pub(crate) fn active_wallet_payment_account_payload(state: &ControlPlaneState) -> Value {
    let wallet_dir = state.data_dir.join(".watt-wallet");
    if !wallet_dir.join("metadata.json").exists() || !wallet_dir.join("keystore.json").exists() {
        return Value::Null;
    }
    match active_payment_account(&state.data_dir) {
        Ok(account) => payment_account_payload(&account),
        Err(_) => Value::Null,
    }
}

pub(crate) fn wallet_payment_accounts_payload(state: &ControlPlaneState) -> Value {
    let wallet_dir = state.data_dir.join(".watt-wallet");
    if !wallet_dir.join("metadata.json").exists() || !wallet_dir.join("keystore.json").exists() {
        return json!([]);
    }
    match open_local_wallet(&state.data_dir) {
        Ok(wallet_state) => json!(
            wallet_state
                .wallet
                .list_payment_accounts(&wallet_state.profile)
                .into_iter()
                .map(|account| payment_account_payload(&account))
                .collect::<Vec<_>>()
        ),
        Err(_) => json!([]),
    }
}

pub(crate) fn wallet_payment_binding_payload(state: &ControlPlaneState) -> Value {
    let wallet_dir = state.data_dir.join(".watt-wallet");
    if !wallet_dir.join("metadata.json").exists() || !wallet_dir.join("keystore.json").exists() {
        return Value::Null;
    }
    match open_local_wallet(&state.data_dir) {
        Ok(wallet_state) => {
            let active_account = wallet_state.profile.active_payment_account();
            let can_sign = active_account.is_some_and(|account| account.key_handle.is_some());
            let status = match (active_account, can_sign) {
                (Some(_), true) => "ready",
                (Some(_), false) => "watch_only",
                (None, _) => "missing_payment_account",
            };
            json!({
                "status": status,
                "proof_available": can_sign,
                "agent_did": state.agent_did,
                "payment_address": active_account.and_then(|account| account.address.clone()),
                "rail": active_account.map(|account| account.rail.clone()),
                "network": active_account.and_then(|account| account.network.clone()),
                "custody": if can_sign { "local_generated" } else { "watch_only" },
                "receive_only": active_account.is_some_and(|account| account.key_handle.is_none()),
                "can_sign": can_sign,
                "capabilities": active_account.map_or_else(Vec::new, |account| account.capabilities.clone()),
                "agent_proof_algorithm": "ed25519-binding",
                "payment_proof_algorithm": if can_sign { "secp256k1-binding" } else { "" },
            })
        }
        Err(_) => Value::Null,
    }
}

fn payment_account_payload(account: &watt_wallet::PaymentAccount) -> Value {
    let can_sign = account.key_handle.is_some();
    let receive_only = !can_sign;
    json!({
        "account_id": account.account_id,
        "rail": account.rail,
        "network": account.network,
        "address": account.address,
        "kind": account.kind,
        "layer": account.layer,
        "label": account.label,
        "status": account.status,
        "capabilities": account.capabilities,
        "created_at_ms": account.created_at_ms,
        "custody": if can_sign { "local_key" } else { "watch_only" },
        "can_sign": can_sign,
        "can_submit_payment": can_sign,
        "receive_only": receive_only,
    })
}
