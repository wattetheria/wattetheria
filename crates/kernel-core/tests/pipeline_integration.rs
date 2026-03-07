//! End-to-end integration test across tasking, summary, governance, and mailbox.

use serde_json::json;
use tempfile::tempdir;
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::governance::{GovernanceEngine, PlanetCreationRequest};
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::mailbox::CrossSubnetMailbox;
use wattetheria_kernel::night_shift::generate_night_shift_report;
use wattetheria_kernel::summary::build_signed_summary;
use wattetheria_kernel::task_engine::TaskEngine;
use wattetheria_kernel::types::{Reward, Sla, VerificationMode, VerificationSpec};

#[test]
fn full_pipeline_runs() {
    let temp = tempdir().unwrap();
    let event_log = EventLog::new(temp.path().join("events.jsonl")).unwrap();
    let identity = Identity::new_random();
    let mut engine = TaskEngine::new(event_log.clone(), identity.clone());

    let task = engine
        .publish_task(
            "market.match",
            "T0",
            json!({
                "buy_orders": [{"id":"b1", "price":120, "qty":3}],
                "sell_orders": [{"id":"s1", "price":100, "qty":3}]
            }),
            VerificationSpec {
                mode: VerificationMode::Deterministic,
                witnesses: None,
            },
            Reward {
                watt: 10,
                reputation: 2,
                capacity: 4,
            },
            Sla { timeout_sec: 120 },
        )
        .unwrap();

    engine
        .claim_task(&task.task_id, &identity.agent_id)
        .unwrap();
    let result = engine.execute_task(&task.task_id).unwrap();
    engine
        .submit_task_result(&task.task_id, &result, &identity.agent_id)
        .unwrap();
    assert!(engine.verify_task(&task.task_id).unwrap());
    let ledger = engine.settle_task(&task.task_id).unwrap();

    let events = event_log.get_all().unwrap();
    assert!(event_log.verify_chain().unwrap().0);

    let now = chrono::Utc::now().timestamp();
    let report = generate_night_shift_report(&events, now - 3600, now);
    assert_eq!(report.totals.completed_tasks, 1);

    let summary =
        build_signed_summary(&identity, Some("planet-a".to_string()), &ledger, &events).unwrap();
    assert_eq!(summary.watt, 10);

    let mut gov = GovernanceEngine::default();
    gov.issue_license(&identity.agent_id, &identity.agent_id, "proof", 7);
    gov.lock_bond(&identity.agent_id, 100, 30);
    let signer1 = Identity::new_random();
    let signer2 = Identity::new_random();
    let created_at = chrono::Utc::now().timestamp();
    let approvals = vec![
        GovernanceEngine::sign_genesis(
            "planet-a",
            "Planet A",
            &identity.agent_id,
            created_at,
            &signer1,
        )
        .unwrap(),
        GovernanceEngine::sign_genesis(
            "planet-a",
            "Planet A",
            &identity.agent_id,
            created_at,
            &signer2,
        )
        .unwrap(),
    ];
    let request = PlanetCreationRequest {
        subnet_id: "planet-a".to_string(),
        name: "Planet A".to_string(),
        creator: identity.agent_id.clone(),
        created_at,
        tax_rate: 0.05,
        min_bond: 50,
        min_approvals: 2,
    };
    let planet = gov.create_planet(&request, &approvals).unwrap();
    assert_eq!(planet.subnet_id, "planet-a");

    let mut mailbox = CrossSubnetMailbox::default();
    let receiver = Identity::new_random();
    let msg = mailbox
        .enqueue_signed(
            &identity,
            &receiver.agent_id,
            "planet-a",
            "planet-b",
            json!({"kind":"mail"}),
        )
        .unwrap();
    assert!(CrossSubnetMailbox::verify_message(&msg).unwrap());
}
