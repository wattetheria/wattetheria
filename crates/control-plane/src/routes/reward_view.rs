use chrono::Utc;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use wattetheria_kernel::civilization::identities::{ControllerBinding, PublicIdentity};
use wattetheria_kernel::economy::{
    EconomicPolicy, WalletBalanceState, WalletBoundBalance, wallet_bound_balance_from_missions,
};
use wattetheria_kernel::local_db;
use wattetheria_kernel::wallet_identity::active_payment_account;

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
    let balance = wallet_bound_balance_from_missions(&policy, &missions, controller_id, public_id);
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
    let mut balance_state: WalletBalanceState = state
        .local_db
        .load_domain_or_default(local_db::domain::WATT_BALANCE_STATE)?;
    let updated_at = Utc::now().timestamp();
    for (controller_id, public_id) in subjects {
        let balance = wallet_bound_balance_from_missions(
            &policy,
            &missions,
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
        Ok(account) => json!({
            "account_id": account.account_id,
            "rail": account.rail,
            "network": account.network,
            "address": account.address,
            "kind": account.kind,
            "layer": account.layer,
        }),
        Err(_) => Value::Null,
    }
}
