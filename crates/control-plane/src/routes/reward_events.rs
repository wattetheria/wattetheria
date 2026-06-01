use anyhow::Context;
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wattetheria_kernel::economy::{
    ContributionEvent, ContributionEventLog, WalletBalanceState, ranking_compute, ranking_prestige,
    ranking_score, ranking_score_tenths,
};
use wattetheria_kernel::local_db;

use crate::routes::identity::IdentityContextView;
use crate::routes::reward_view::refresh_known_wallet_balances;
use crate::state::{ControlPlaneState, StreamEvent};

pub(crate) struct ContributionEventArgs<'a> {
    pub(crate) action_type: &'a str,
    pub(crate) source_id: &'a str,
    pub(crate) controller_id: &'a str,
    pub(crate) public_id: Option<&'a str>,
    pub(crate) agent_identity: Option<&'a str>,
    pub(crate) receipt: Value,
}

pub(crate) async fn record_contribution_event(
    state: &ControlPlaneState,
    args: ContributionEventArgs<'_>,
) -> anyhow::Result<ContributionEvent> {
    let occurred_at = Utc::now().timestamp();
    let event = ContributionEvent {
        event_id: contribution_event_id(&args, occurred_at)?,
        action_type: args.action_type.to_string(),
        source_id: args.source_id.to_string(),
        controller_id: args.controller_id.to_string(),
        public_id: args.public_id.map(str::to_string),
        agent_identity: args.agent_identity.map(str::to_string),
        occurred_at,
        receipt: args.receipt,
    };
    let mut log: ContributionEventLog = state
        .local_db
        .load_domain_or_default(local_db::domain::CONTRIBUTION_EVENT_LOG)?;
    let inserted = log.append(event.clone());
    if inserted {
        state
            .local_db
            .save_domain(local_db::domain::CONTRIBUTION_EVENT_LOG, &log)?;
        state.append_signed_event(
            "CONTRIBUTION_REWARD_EVENT",
            json!({
                "event": event,
                "authority": "local-control-plane",
                "gateway_authoritative": false,
            }),
        )?;
        refresh_known_wallet_balances(state).await?;
        publish_ranking_update(state, &event)?;
    }
    Ok(event)
}

pub(crate) fn contribution_actor<'a>(
    state: &'a ControlPlaneState,
    context: &'a IdentityContextView,
) -> (&'a str, Option<&'a str>, Option<&'a str>) {
    let controller_id = context.public_memory_owner.controller.as_str();
    let public_id = context.public_memory_owner.public.as_deref();
    let agent_identity = context
        .public_identity
        .as_ref()
        .map(|identity| identity.display_name.as_str())
        .or(Some(state.agent_did.as_str()));
    (controller_id, public_id, agent_identity)
}

pub(crate) fn message_action_type(reply_to_message_id: Option<&str>, base: &str) -> &'static str {
    if base == "hive" {
        if reply_to_message_id.is_some() {
            "hive.message.reply"
        } else {
            "hive.message.post"
        }
    } else if reply_to_message_id.is_some() {
        "topic.message.reply"
    } else {
        "topic.message.post"
    }
}

fn contribution_event_id(
    args: &ContributionEventArgs<'_>,
    _occurred_at: i64,
) -> anyhow::Result<String> {
    let canonical = serde_json::to_string(&json!({
        "action_type": args.action_type,
        "source_id": args.source_id,
        "controller_id": args.controller_id,
        "public_id": args.public_id,
    }))
    .context("serialize contribution event id input")?;
    Ok(format!(
        "reward:{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    ))
}

fn publish_ranking_update(
    state: &ControlPlaneState,
    event: &ContributionEvent,
) -> anyhow::Result<()> {
    let reward_balances: WalletBalanceState = state
        .local_db
        .load_domain_or_default(local_db::domain::WATT_BALANCE_STATE)?;
    let Some(balance) = reward_balances.get(&event.controller_id, event.public_id.as_deref())
    else {
        return Ok(());
    };
    let balance_stats = balance.balance().stats();
    let public_id = event.public_id.as_deref().unwrap_or(&event.controller_id);
    let display_name = event.agent_identity.as_deref().unwrap_or(public_id);
    let compute = ranking_compute(&balance_stats);
    let prestige = ranking_prestige(&balance_stats);
    let payload = json!({
        "agent_did": public_id,
        "agent_identity": display_name,
        "public_id": public_id,
        "display_name": display_name,
        "score": ranking_score(&balance_stats),
        "score_tenths": ranking_score_tenths(&balance_stats),
        "score_formula": "watts*0.1+compute*10+prestige*100",
        "watt_balance": balance_stats.watt,
        "compute": compute,
        "compute_score": compute,
        "tasks_completed": 0,
        "prestige": prestige,
        "prestige_level": prestige,
        "reputation": balance_stats.reputation,
        "capacity": balance_stats.capacity,
        "reward_event_id": event.event_id,
        "reward_action_type": event.action_type,
        "updated_at": event.occurred_at,
    });
    let _ = state.stream_tx.send(StreamEvent {
        kind: "ranking.updated".to_string(),
        timestamp: Utc::now().timestamp(),
        payload,
    });
    Ok(())
}
