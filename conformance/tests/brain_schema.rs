// Validates brain output schemas used by humanized reports and proposals.

use serde_json::json;
use wattetheria_conformance::validate;

#[test]
fn human_report_schema_accepts_valid_payload() {
    let payload = json!({
        "title": "Night Shift Brief",
        "summary": "Events processed normally.",
        "highlights": ["tasks settled: 2"],
        "risk_level": "low",
        "recommended_actions": ["publish summary"]
    });

    validate("human_report.json", &payload).unwrap();
}

#[test]
fn action_proposal_schema_accepts_valid_payload() {
    let payload = json!({
        "action": "task.run_demo_market",
        "required_caps": ["p2p.publish"],
        "estimated_cost": 1,
        "risk_level": "medium",
        "rationale": "Maintain throughput"
    });

    validate("action_proposal.json", &payload).unwrap();
}

#[test]
fn skill_call_plan_schema_accepts_valid_payload() {
    let payload = json!({
        "skill_id": "echo-skill",
        "input": {"intent": "summarize_recent_report"},
        "required_caps": ["model.invoke"],
        "estimated_cost": 1,
        "risk_level": "low",
        "rationale": "Low-risk helper call"
    });

    validate("skill_call_plan.json", &payload).unwrap();
}
