use anyhow::Result;
use serde_json::{Value, json};

use crate::social_host::{
    SignedAgentEnvelopeArgs, build_signed_agent_envelope_for_nodes, public_agent_id,
    resolve_social_local_context,
};
use crate::state::ControlPlaneState;

pub(crate) async fn servicenet_invoke_agent_envelope(
    state: &ControlPlaneState,
    agent_id: &str,
    body: &Value,
) -> Result<Value> {
    let source_node_id = state.swarm_bridge.local_node_id().await.ok();
    let local = resolve_social_local_context(state, None).await;
    let envelope = build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: local.agent_id,
            source_public_id: public_agent_id(&local.public_id),
            source_display_name: local.display_name,
            target_agent_id: Some(agent_id.to_owned()),
            source_node_id,
            target_node_id: None,
            capability: "servicenet.agents.invoke".to_owned(),
            message: servicenet_invoke_envelope_message(body),
            extensions: Some(json!({
                "caller_public_id": local.public_id,
            })),
        },
    )?;
    Ok(serde_json::to_value(envelope)?)
}

fn servicenet_invoke_envelope_message(body: &Value) -> Value {
    let mut message = body.clone();
    if let Some(object) = message.as_object_mut() {
        object.remove("auth_token");
        object.remove("auth_context_id");
        object.remove("agent_envelope");
    }
    message
}
