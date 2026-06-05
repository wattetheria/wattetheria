use chrono::Utc;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use wattetheria_kernel::civilization::identities::{ControllerBinding, PublicIdentity};
use wattetheria_kernel::economy::{
    ContributionEventLog, EconomicPolicy, WalletBalanceState, WalletBoundBalance,
    wallet_bound_balance_from_rewards,
};
use wattetheria_kernel::local_db;
use wattetheria_kernel::wallet_identity::{active_payment_account, open_local_wallet};

use crate::state::ControlPlaneState;

fn load_economic_policy(state: &ControlPlaneState) -> anyhow::Result<EconomicPolicy> {
    state
        .local_db
        .load_domain_or_default(local_db::domain::ECONOMIC_POLICY)
}

pub(crate) async fn wallet_bound_balance_for_identity(
    state: &ControlPlaneState,
    controller_id: &str,
    public_id: Option<&str>,
) -> anyhow::Result<WalletBoundBalance> {
    Ok(
        persist_wallet_balance_for_identity(state, controller_id, public_id)
            .await?
            .balance(),
    )
}

pub(crate) async fn persist_wallet_balance_for_identity(
    state: &ControlPlaneState,
    controller_id: &str,
    public_id: Option<&str>,
) -> anyhow::Result<wattetheria_kernel::economy::WalletBalanceRecord> {
    let policy = load_economic_policy(state)?;
    let missions = state.mission_board.lock().await;
    let contribution_events = load_contribution_event_log(state)?;
    let balance = wallet_bound_balance_from_rewards(
        &policy,
        &missions,
        &contribution_events,
        controller_id,
        public_id,
    );
    drop(missions);
    let mut balance_state: WalletBalanceState = state
        .local_db
        .load_domain_or_default(local_db::domain::WATT_BALANCE_STATE)?;
    let record = balance_state.upsert(controller_id, public_id, &balance, Utc::now().timestamp());
    state
        .local_db
        .save_domain(local_db::domain::WATT_BALANCE_STATE, &balance_state)?;
    Ok(record)
}

pub(crate) async fn refresh_known_wallet_balances(state: &ControlPlaneState) -> anyhow::Result<()> {
    let subjects = wallet_balance_subjects(state).await;
    let policy = load_economic_policy(state)?;
    let missions = state.mission_board.lock().await;
    let contribution_events = load_contribution_event_log(state)?;
    let mut balance_state: WalletBalanceState = state
        .local_db
        .load_domain_or_default(local_db::domain::WATT_BALANCE_STATE)?;
    let updated_at = Utc::now().timestamp();
    for (controller_id, public_id) in subjects {
        let balance = wallet_bound_balance_from_rewards(
            &policy,
            &missions,
            &contribution_events,
            &controller_id,
            public_id.as_deref(),
        );
        balance_state.upsert(&controller_id, public_id.as_deref(), &balance, updated_at);
    }
    drop(missions);
    state
        .local_db
        .save_domain(local_db::domain::WATT_BALANCE_STATE, &balance_state)
}

fn load_contribution_event_log(state: &ControlPlaneState) -> anyhow::Result<ContributionEventLog> {
    state
        .local_db
        .load_domain_or_default(local_db::domain::CONTRIBUTION_EVENT_LOG)
}

async fn wallet_balance_subjects(state: &ControlPlaneState) -> Vec<(String, Option<String>)> {
    let binding_by_public_id = state
        .controller_binding_registry
        .lock()
        .await
        .list()
        .into_iter()
        .map(|binding| (binding.public_id.clone(), binding))
        .collect::<BTreeMap<_, _>>();
    let public_identities = state.public_identity_registry.lock().await.list();
    if public_identities.is_empty() {
        return vec![(state.agent_did.clone(), Some(state.agent_did.clone()))];
    }
    public_identities
        .into_iter()
        .map(|identity| {
            let controller_id = controller_id_for_identity(
                &identity,
                binding_by_public_id.get(&identity.public_id),
            );
            (controller_id, Some(identity.public_id))
        })
        .collect()
}

fn controller_id_for_identity(
    identity: &PublicIdentity,
    binding: Option<&ControllerBinding>,
) -> String {
    binding
        .and_then(|binding| binding.controller_node_id.clone())
        .or_else(|| identity.agent_did.clone())
        .unwrap_or_else(|| identity.public_id.clone())
}

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

pub(crate) fn wallet_identities_payload(state: &ControlPlaneState) -> Value {
    let wallet_dir = state.data_dir.join(".watt-wallet");
    if !wallet_dir.join("metadata.json").exists() || !wallet_dir.join("keystore.json").exists() {
        return json!([]);
    }
    match open_local_wallet(&state.data_dir) {
        Ok(wallet_state) => json!(
            wallet_state
                .profile
                .identities
                .iter()
                .map(|identity| {
                    json!({
                        "identity_id": identity.identity_id,
                        "did": identity.did.to_string(),
                        "algorithm": identity.algorithm,
                        "purposes": identity.purposes,
                        "status": identity.status,
                        "label": identity.label,
                        "created_at_ms": identity.created_at_ms,
                        "rotated_from": identity.rotated_from,
                        "active": wallet_state.profile.active_identity_id.as_deref()
                            == Some(identity.identity_id.as_str()),
                    })
                })
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
            let active_identity = wallet_state.profile.active_identity();
            let active_account = wallet_state.profile.active_payment_account();
            let can_sign = active_account.is_some_and(|account| account.key_handle.is_some());
            let status = match (active_identity, active_account, can_sign) {
                (Some(_), Some(_), true) => "ready",
                (Some(_), Some(_), false) => "watch_only",
                (Some(_), None, _) => "missing_payment_account",
                _ => "missing_identity",
            };
            json!({
                "status": status,
                "proof_available": can_sign,
                "agent_did": active_identity.map(|identity| identity.did.to_string()),
                "payment_address": active_account.and_then(|account| account.address.clone()),
                "rail": active_account.map(|account| account.rail.clone()),
                "network": active_account.and_then(|account| account.network.clone()),
                "custody": if can_sign { "local_generated" } else { "watch_only" },
                "receive_only": active_account.is_some_and(|account| account.key_handle.is_none()),
                "can_sign": can_sign,
                "capabilities": active_account.map_or_else(Vec::new, |account| account.capabilities.clone()),
                "agent_proof_algorithm": if active_identity.is_some() { "ed25519-binding" } else { "" },
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
