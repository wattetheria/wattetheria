use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::{Value, json};

use crate::state::{ControlPlaneState, StreamEvent};
use wattetheria_kernel::brain::ActionProposal;
use wattetheria_kernel::capabilities::TrustLevel;
use wattetheria_kernel::galaxy_task::GalaxyTaskIntent;
use wattetheria_kernel::night_shift::generate_night_shift_report;
use wattetheria_kernel::policy_engine::{CapabilityRequest, DecisionKind};

pub(crate) async fn run_demo_market_task(state: &ControlPlaneState) -> Result<Value> {
    let task = state
        .swarm_bridge
        .run_galaxy_task(&state.agent_id, GalaxyTaskIntent::demo_market_match())
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

pub(crate) async fn build_brain_state(
    state: &ControlPlaneState,
    enable_skill_planner: bool,
) -> Result<Value> {
    let events = state.event_log.get_all()?;
    let pending_policy_requests = state.policy_engine.lock().await.list_pending().len();
    let agent_view = state.swarm_bridge.agent_view(&state.agent_id).await?;
    let latest_report_digest = events
        .last()
        .map_or_else(|| "no-events".to_string(), |event| event.hash.clone());

    Ok(json!({
        "events": events.len(),
        "pending_policy_requests": pending_policy_requests,
        "skill_planner_enabled": enable_skill_planner,
        "latest_report_digest": latest_report_digest,
        "agent_stats": agent_view.stats,
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
            subject: format!("autonomy:{}", state.agent_id),
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

pub async fn run_autonomy_tick_once(
    state: &ControlPlaneState,
    hours: i64,
    enable_skill_planner: bool,
) -> Result<Value> {
    let brain_state = build_brain_state(state, enable_skill_planner).await?;
    let proposals = state.brain_engine.propose_actions(&brain_state).await?;
    let plans = state.brain_engine.plan_skill_calls(&brain_state).await?;
    let report = load_night_shift_report(state, hours)?;
    let human_report = state.brain_engine.humanize_night_shift(&report).await?;

    let mut executed = Vec::new();
    for proposal in &proposals {
        let (allowed, capability_checks) = check_action_capabilities(state, proposal).await?;
        if !allowed {
            executed.push(json!({
                "action": proposal.action,
                "status": "blocked_by_policy",
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
        "skill_planner_enabled": enable_skill_planner,
        "human_report": human_report,
        "proposals": proposals,
        "skill_plans": plans,
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
