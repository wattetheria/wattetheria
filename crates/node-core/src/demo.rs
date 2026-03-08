use anyhow::{Context, Result};
use serde_json::json;
use std::sync::Arc;
use tracing::info;
use wattetheria_kernel::galaxy_task::GalaxyTaskIntent;
use wattetheria_kernel::governance::{
    GovernanceEngine, PlanetConstitutionTemplate, PlanetCreationRequest,
};
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::signing::sign_payload;
use wattetheria_kernel::swarm_bridge::SwarmBridge;
use wattetheria_kernel::types::ActionEnvelope;
use wattetheria_p2p_runtime::P2PNode;

pub async fn run_demo_task(
    swarm_bridge: &Arc<dyn SwarmBridge>,
    p2p: &mut P2PNode,
    identity: &Identity,
) -> Result<()> {
    let task = swarm_bridge
        .run_galaxy_task(&identity.agent_id, GalaxyTaskIntent::demo_market_match())
        .await?;
    let task_id = task.task_id.clone();
    info!(task_id = %task_id, terminal_state = %task.terminal_state, "demo task settled");

    let action = ActionEnvelope {
        r#type: "ACTION".to_string(),
        version: "0.1".to_string(),
        action: "TASK_RESULT".to_string(),
        action_id: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        sender: identity.agent_id.clone(),
        recipient: None,
        payload: serde_json::to_value(&task).context("serialize demo swarm task for gossip")?,
        signature: sign_payload(&json!({"kind":"TASK_RESULT","task_id":task_id}), identity)?,
    };
    p2p.publish_json(&action)?;
    Ok(())
}

pub fn ignite_demo_planet(governance: &mut GovernanceEngine, identity: &Identity) -> Result<()> {
    let signer_a = Identity::new_random();
    let signer_b = Identity::new_random();
    governance.issue_license(&identity.agent_id, &identity.agent_id, "task-proof", 7);
    governance.lock_bond(&identity.agent_id, 100, 30);
    let created_at = chrono::Utc::now().timestamp();
    let approvals = vec![
        GovernanceEngine::sign_genesis(
            "planet-main",
            "Planet Main",
            &identity.agent_id,
            created_at,
            &signer_a,
        )?,
        GovernanceEngine::sign_genesis(
            "planet-main",
            "Planet Main",
            &identity.agent_id,
            created_at,
            &signer_b,
        )?,
    ];
    let request = PlanetCreationRequest {
        subnet_id: "planet-main".to_string(),
        name: "Planet Main".to_string(),
        creator: identity.agent_id.clone(),
        created_at,
        tax_rate: 0.05,
        constitution_template: PlanetConstitutionTemplate::CorporateCharter,
        min_bond: 50,
        min_approvals: 2,
    };
    let planet = governance.create_planet(&request, &approvals)?;
    info!(subnet = %planet.subnet_id, "demo planet created");
    Ok(())
}
