use serde::Serialize;
use serde_json::{Value, json};
use wattetheria_kernel::identities::{ControllerBinding, PublicIdentity};
use wattetheria_kernel::profiles::CitizenProfile;

use crate::state::ControlPlaneState;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PublicMemoryOwnerView {
    #[serde(rename = "public_id")]
    pub(crate) public: Option<String>,
    #[serde(rename = "controller_id")]
    pub(crate) controller: String,
    #[serde(rename = "agent_did")]
    pub(crate) agent_did: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct IdentityContextView {
    pub(crate) public_identity: Option<PublicIdentity>,
    pub(crate) controller_binding: Option<ControllerBinding>,
    pub(crate) profile: Option<CitizenProfile>,
    pub(crate) public_memory_owner: PublicMemoryOwnerView,
}

#[derive(Debug, Clone, Serialize)]
struct PublicMemoryEnvelope {
    public_memory: PublicMemoryOwnerView,
    scope: String,
    record: Value,
}

pub(crate) async fn identity_context_value(
    state: &ControlPlaneState,
    public_id: Option<&str>,
    agent_did: Option<&str>,
) -> Value {
    serde_json::to_value(resolve_identity_context(state, public_id, agent_did).await)
        .unwrap_or(Value::Null)
}

pub(crate) async fn resolve_identity_context(
    state: &ControlPlaneState,
    public_id: Option<&str>,
    agent_did: Option<&str>,
) -> IdentityContextView {
    let current_public_id = {
        let registry = state.controller_binding_registry.lock().await;
        registry
            .active_for_controller(&state.agent_did)
            .map(|binding| binding.public_id)
    };
    let public_identity = {
        let registry = state.public_identity_registry.lock().await;
        if let Some(public_id) = public_id {
            registry.get(public_id)
        } else if let Some(agent_did) = agent_did {
            registry.active_for_agent_did(agent_did)
        } else {
            current_public_id
                .as_deref()
                .and_then(|current_public_id| registry.get(current_public_id))
                .or_else(|| registry.active_for_agent_did(&state.agent_did))
        }
    };
    let controller_binding = {
        let registry = state.controller_binding_registry.lock().await;
        public_identity
            .as_ref()
            .and_then(|identity| registry.get(&identity.public_id))
            .or_else(|| public_id.and_then(|public_id| registry.get(public_id)))
            .or_else(|| {
                agent_did.and_then(|controller_id| registry.active_for_controller(controller_id))
            })
            .or_else(|| registry.active_for_controller(&state.agent_did))
    };
    let profile_agent_did = public_identity
        .as_ref()
        .and_then(|identity| identity.agent_did.clone())
        .or_else(|| agent_did.map(ToOwned::to_owned))
        .or_else(|| {
            controller_binding
                .as_ref()
                .and_then(|binding| binding.controller_node_id.clone())
        })
        .unwrap_or_else(|| state.agent_did.clone());
    let profile = state
        .citizen_registry
        .lock()
        .await
        .profile(&profile_agent_did);
    let public_memory_owner = PublicMemoryOwnerView {
        public: public_identity
            .as_ref()
            .map(|identity| identity.public_id.clone())
            .or_else(|| {
                controller_binding
                    .as_ref()
                    .map(|binding| binding.public_id.clone())
            }),
        controller: controller_binding
            .as_ref()
            .and_then(|binding| binding.controller_node_id.clone())
            .unwrap_or(profile_agent_did.clone()),
        agent_did: public_identity
            .as_ref()
            .and_then(|identity| identity.agent_did.clone())
            .or_else(|| profile.as_ref().map(|profile| profile.agent_did.clone())),
    };

    IdentityContextView {
        public_identity,
        controller_binding,
        profile,
        public_memory_owner,
    }
}

pub(crate) fn identity_context_response(context: &IdentityContextView) -> Value {
    json!({
        "public_identity": context.public_identity,
        "controller_binding": context.controller_binding,
        "profile": context.profile,
        "public_memory_owner": context.public_memory_owner,
    })
}

pub(crate) fn public_memory_payload(
    context: &IdentityContextView,
    scope: &str,
    record: Value,
) -> Value {
    serde_json::to_value(PublicMemoryEnvelope {
        public_memory: context.public_memory_owner.clone(),
        scope: scope.to_string(),
        record,
    })
    .unwrap_or(Value::Null)
}
