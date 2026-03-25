// Validates brain output schemas used by narrative reports and proposals.

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
        "action": "policy.review_pending",
        "required_caps": ["mcp.call:policy"],
        "estimated_cost": 1,
        "risk_level": "medium",
        "rationale": "Review pending high-risk requests"
    });

    validate("action_proposal.json", &payload).unwrap();
}
