use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::{Value, json};

use crate::state::{ControlPlaneState, StreamEvent};
use wattetheria_kernel::brain::ActionProposal;
use wattetheria_kernel::capabilities::TrustLevel;
use wattetheria_kernel::emergency::evaluate_emergencies;
use wattetheria_kernel::galaxy_task::GalaxyTaskIntent;
use wattetheria_kernel::night_shift::generate_night_shift_report;
use wattetheria_kernel::policy_engine::{CapabilityRequest, DecisionKind};
use wattetheria_kernel::profiles::strategy_directive;

pub(crate) async fn run_demo_market_task(state: &ControlPlaneState) -> Result<Value> {
    let task = state
        .swarm_bridge
        .run_galaxy_task(&state.agent_did, GalaxyTaskIntent::demo_market_match())
        .await?;
    if task.terminal_state != "finalized" {
        bail!("demo task verification failed");
    }
    serde_json::to_value(task).context("serialize bridge demo task result")
}

pub(crate) fn load_night_shift_report(state: &ControlPlaneState, hours: i64) -> Result<Value> {
    let now = Utc::now().timestamp();
    let events = state.event_log.get_all()?;
    let report = generate_night_shift_report(&events, now - hours * 3600, now);
    serde_json::to_value(report).context("serialize night shift report")
}

pub(crate) async fn build_brain_state(state: &ControlPlaneState) -> Result<Value> {
    let events = state.event_log.get_all()?;
    let pending_policy_requests = state.policy_engine.lock().await.list_pending().len();
    let agent_view = state.swarm_bridge.agent_view(&state.agent_did).await?;
    let latest_report_digest = events
        .last()
        .map_or_else(|| "no-events".to_string(), |event| event.hash.clone());
    let missions = state.mission_board.lock().await;
    let profiles = state.citizen_registry.lock().await;
    let governance = state.governance_engine.lock().await;
    let galaxy = state.galaxy_state.lock().await;
    let profile = profiles.profile(&state.agent_did);
    let strategy = profile.as_ref().map_or_else(
        || strategy_directive(&wattetheria_kernel::profiles::StrategyProfile::Balanced),
        |entry| strategy_directive(&entry.strategy),
    );
    let emergencies =
        evaluate_emergencies(&state.agent_did, &profiles, &missions, &governance, &galaxy);

    Ok(json!({
        "events": events.len(),
        "pending_policy_requests": pending_policy_requests,
        "latest_report_digest": latest_report_digest,
        "agent_stats": agent_view.stats,
        "profile": profile,
        "strategy": strategy,
        "emergencies": emergencies,
    }))
}

pub(crate) async fn check_action_capabilities(
    state: &ControlPlaneState,
    proposal: &ActionProposal,
) -> Result<(bool, Vec<Value>)> {
    let mut policy = state.policy_engine.lock().await;
    let mut decisions = Vec::new();

    for capability in &proposal.required_caps {
        let decision = policy.evaluate(CapabilityRequest {
            request_id: String::new(),
            timestamp: 0,
            subject: format!("autonomy:{}", state.agent_did),
            trust: TrustLevel::Verified,
            capability: capability.clone(),
            reason: Some("autonomy.tick".to_string()),
            input_digest: None,
        })?;

        let allowed = decision.decision == DecisionKind::Allowed;
        decisions.push(json!({
            "capability": capability,
            "decision": decision,
            "allowed": allowed,
        }));

        if !allowed {
            return Ok((false, decisions));
        }
    }

    Ok((true, decisions))
}

pub async fn run_autonomy_tick_once(state: &ControlPlaneState, hours: i64) -> Result<Value> {
    let brain_state = build_brain_state(state).await?;
    let emergencies = brain_state["emergencies"]
        .as_array()
        .map_or(0_usize, std::vec::Vec::len);
    let proposals = state.brain_engine.propose_actions(&brain_state).await?;
    let report = load_night_shift_report(state, hours)?;
    let human_report = state.brain_engine.humanize_night_shift(&report).await?;
    let strategy = brain_state["strategy"].clone();
    let auto_action_budget = strategy["max_auto_actions"]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    let allow_high_risk = strategy["allow_high_risk"].as_bool().unwrap_or(false);

    let mut executed = Vec::new();
    for (index, proposal) in proposals.iter().enumerate() {
        if index >= auto_action_budget {
            executed.push(json!({
                "action": proposal.action,
                "status": "deferred_by_strategy_budget",
            }));
            continue;
        }
        let (allowed, capability_checks) = check_action_capabilities(state, proposal).await?;
        if !allowed {
            executed.push(json!({
                "action": proposal.action,
                "status": "blocked_by_policy",
                "capability_checks": capability_checks,
            }));
            continue;
        }
        if emergencies > 0 && !allow_high_risk {
            executed.push(json!({
                "action": proposal.action,
                "status": "deferred_for_human_recall",
                "capability_checks": capability_checks,
            }));
            continue;
        }

        let execution = match proposal.action.as_str() {
            "task.run_demo_market" => json!({
                "action": proposal.action,
                "status": "ok",
                "result": run_demo_market_task(state).await?,
                "capability_checks": capability_checks,
            }),
            _ => json!({
                "action": proposal.action,
                "status": "skipped_unsupported",
                "capability_checks": capability_checks,
            }),
        };
        executed.push(execution);
    }

    let payload = json!({
        "status": "ok",
        "hours": hours,
        "strategy": strategy,
        "emergencies": brain_state["emergencies"].clone(),
        "human_report": human_report,
        "proposals": proposals,
        "executed_actions": executed,
    });

    state
        .event_log
        .append_signed("AUTONOMY_TICK", payload.clone(), &state.identity)?;

    let _ = state.stream_tx.send(StreamEvent {
        kind: "autonomy.tick".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });

    Ok(payload)
}

pub(crate) async fn build_operator_briefing(
    state: &ControlPlaneState,
    hours: i64,
) -> Result<Value> {
    let report = load_night_shift_report(state, hours)?;
    let human_report = state.brain_engine.humanize_night_shift(&report).await?;
    let brain_state = build_brain_state(state).await?;
    Ok(json!({
        "hours": hours,
        "report": report,
        "human_report": human_report,
        "profile": brain_state["profile"].clone(),
        "strategy": brain_state["strategy"].clone(),
        "emergencies": brain_state["emergencies"].clone(),
    }))
}
