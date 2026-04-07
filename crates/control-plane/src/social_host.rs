use async_trait::async_trait;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use wattetheria_social::domain::identities::LocalIdentityContext;
use wattetheria_social::ports::local_identity_provider::LocalIdentityProvider;
use wattetheria_social::ports::transport_port::TransportPort;
use wattetheria_social::types::{SocialError, SocialResult};

use crate::routes::identity::resolve_identity_context;
use crate::state::ControlPlaneState;
use wattetheria_kernel::identities::{ControllerBinding, PublicIdentity};
use wattetheria_kernel::swarm_bridge::{
    SwarmAgentEnvelope, SwarmDirectMessageCommand, SwarmRelationshipAction,
    SwarmRelationshipActionCommand,
};

#[derive(Debug, Clone)]
pub(crate) struct SocialLocalContext {
    pub(crate) public_id: String,
    pub(crate) agent_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SocialCounterpartTarget {
    pub(crate) counterpart_public_id: String,
    pub(crate) remote_node: String,
    pub(crate) target_agent: String,
}

#[derive(Debug, serde::Serialize)]
struct SignedAgentEnvelopePayload<'a> {
    protocol: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_agent_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capability: Option<&'a String>,
    message_json: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions_json: Option<&'a String>,
}

#[derive(Clone)]
pub struct WattetheriaLocalIdentityProvider {
    state: ControlPlaneState,
}

impl WattetheriaLocalIdentityProvider {
    #[must_use]
    pub fn new(state: ControlPlaneState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl LocalIdentityProvider for WattetheriaLocalIdentityProvider {
    async fn active_identity(&self) -> SocialResult<LocalIdentityContext> {
        let registry = self.state.public_identity_registry.lock().await;
        let identity = registry
            .active_for_agent_did(&self.state.agent_did)
            .or_else(|| registry.list().into_iter().find(|identity| identity.active))
            .ok_or_else(|| SocialError::NotFound("active local identity missing".to_owned()))?;

        Ok(LocalIdentityContext {
            public_id: identity.public_id.clone(),
            agent_did: identity
                .agent_did
                .unwrap_or_else(|| self.state.agent_did.clone()),
            display_name: identity.display_name,
            description: None,
            capabilities: Vec::new(),
            skills: Vec::new(),
            active: identity.active,
            created_at: identity.created_at,
            updated_at: identity.updated_at,
        })
    }
}

#[derive(Clone)]
pub struct WattetheriaTransportAdapter {
    state: ControlPlaneState,
}

impl WattetheriaTransportAdapter {
    #[must_use]
    pub fn new(state: ControlPlaneState) -> Self {
        Self { state }
    }

    async fn send_relationship_action(
        &self,
        remote_node_id: &str,
        action: SwarmRelationshipAction,
        payload: &Value,
    ) -> SocialResult<()> {
        let local = resolve_social_local_context(&self.state, None).await;
        let counterpart = resolve_social_counterpart_target_by_remote_node(
            &self.state,
            remote_node_id,
            payload
                .get("counterpart_public_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        )
        .await?;
        let capability = capability_for_relationship_action(&action);
        let message = with_social_defaults(
            payload.clone(),
            [
                ("source_public_id", Value::String(local.public_id.clone())),
                (
                    "target_public_id",
                    Value::String(counterpart.counterpart_public_id.clone()),
                ),
                (
                    "action",
                    serde_json::to_value(&action).unwrap_or(Value::Null),
                ),
            ],
        );
        let agent_envelope = build_signed_agent_envelope(
            &self.state,
            local.agent_id,
            counterpart.target_agent,
            capability,
            message,
            None,
        )
        .map_err(|error| SocialError::Storage(format!("sign social envelope: {error:#}")))?;
        self.state
            .swarm_bridge
            .send_peer_relationship_action(SwarmRelationshipActionCommand {
                remote_node_id: counterpart.remote_node,
                action,
                agent_envelope,
            })
            .await
            .map(|_| ())
            .map_err(|error| SocialError::Storage(format!("send relationship action: {error:#}")))
    }
}

#[async_trait]
impl TransportPort for WattetheriaTransportAdapter {
    async fn send_friend_request(&self, remote_node_id: &str, payload: &Value) -> SocialResult<()> {
        self.send_relationship_action(remote_node_id, SwarmRelationshipAction::Request, payload)
            .await
    }

    async fn send_friend_decision(
        &self,
        remote_node_id: &str,
        payload: &Value,
    ) -> SocialResult<()> {
        let decision = payload
            .get("decision")
            .and_then(Value::as_str)
            .ok_or_else(|| SocialError::InvalidInput("decision is required".to_owned()))?;
        let action = match decision {
            "accept" => SwarmRelationshipAction::Accept,
            "reject" => SwarmRelationshipAction::Reject,
            "block" => SwarmRelationshipAction::Block,
            "cancel" => SwarmRelationshipAction::Cancel,
            "remove" => SwarmRelationshipAction::Remove,
            other => {
                return Err(SocialError::InvalidInput(format!(
                    "unsupported decision: {other}"
                )));
            }
        };
        self.send_relationship_action(remote_node_id, action, payload)
            .await
    }

    async fn send_direct_message(&self, remote_node_id: &str, payload: &Value) -> SocialResult<()> {
        let local = resolve_social_local_context(&self.state, None).await;
        let counterpart = resolve_social_counterpart_target_by_remote_node(
            &self.state,
            remote_node_id,
            payload
                .get("counterpart_public_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        )
        .await?;
        let content = payload
            .get("content")
            .cloned()
            .unwrap_or_else(|| payload.clone());
        let message = with_social_defaults(
            payload.clone(),
            [
                ("source_public_id", Value::String(local.public_id.clone())),
                (
                    "target_public_id",
                    Value::String(counterpart.counterpart_public_id.clone()),
                ),
                ("content", content.clone()),
            ],
        );
        let agent_envelope = build_signed_agent_envelope(
            &self.state,
            local.agent_id,
            counterpart.target_agent,
            "social.dm.send",
            message,
            None,
        )
        .map_err(|error| SocialError::Storage(format!("sign dm envelope: {error:#}")))?;
        self.state
            .swarm_bridge
            .send_peer_direct_message(SwarmDirectMessageCommand {
                remote_node_id: counterpart.remote_node,
                agent_envelope,
                content,
            })
            .await
            .map(|_| ())
            .map_err(|error| SocialError::Storage(format!("send direct message: {error:#}")))
    }
}

pub(crate) async fn resolve_social_local_context(
    state: &ControlPlaneState,
    public_id: Option<&str>,
) -> SocialLocalContext {
    let context = resolve_identity_context(state, public_id, None).await;
    let public_id = context.public_identity.as_ref().map_or_else(
        || context.public_memory_owner.controller.clone(),
        |identity| identity.public_id.clone(),
    );
    let agent_id = context
        .public_identity
        .as_ref()
        .and_then(|identity| identity.agent_did.clone())
        .unwrap_or_else(|| state.agent_did.clone());
    SocialLocalContext {
        public_id,
        agent_id,
    }
}

pub(crate) async fn load_social_identity_maps(
    state: &ControlPlaneState,
) -> (
    BTreeMap<String, PublicIdentity>,
    BTreeMap<String, ControllerBinding>,
) {
    let identities = state
        .public_identity_registry
        .lock()
        .await
        .list()
        .into_iter()
        .map(|identity| (identity.public_id.clone(), identity))
        .collect::<BTreeMap<_, _>>();
    let bindings = state
        .controller_binding_registry
        .lock()
        .await
        .list()
        .into_iter()
        .map(|binding| (binding.public_id.clone(), binding))
        .collect::<BTreeMap<_, _>>();
    (identities, bindings)
}

pub(crate) async fn resolve_social_counterpart_target(
    state: &ControlPlaneState,
    counterpart_public_id: &str,
) -> Result<SocialCounterpartTarget, String> {
    let counterpart_public_id = counterpart_public_id.trim();
    if counterpart_public_id.is_empty() {
        return Err("counterpart_public_id is required".to_string());
    }
    let (identities, bindings) = load_social_identity_maps(state).await;
    let identity = identities.get(counterpart_public_id).cloned();
    let binding = bindings
        .get(counterpart_public_id)
        .cloned()
        .ok_or_else(|| format!("controller binding missing for {counterpart_public_id}"))?;
    let remote_node_id = binding
        .controller_node_id
        .clone()
        .ok_or_else(|| format!("controller_node_id missing for {counterpart_public_id}"))?;
    let target_agent_id = identity
        .as_ref()
        .and_then(|entry| entry.agent_did.clone())
        .unwrap_or_else(|| counterpart_public_id.to_string());
    Ok(SocialCounterpartTarget {
        counterpart_public_id: counterpart_public_id.to_string(),
        remote_node: remote_node_id,
        target_agent: target_agent_id,
    })
}

pub(crate) async fn resolve_social_counterpart_target_by_remote_node(
    state: &ControlPlaneState,
    remote_node_id: &str,
    counterpart_public_id_hint: Option<String>,
) -> SocialResult<SocialCounterpartTarget> {
    if remote_node_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "remote_node_id is required".to_owned(),
        ));
    }
    let (identities, bindings) = load_social_identity_maps(state).await;
    let counterpart_public_id = counterpart_public_id_hint
        .or_else(|| counterpart_public_id_for_remote_node(&bindings, remote_node_id));
    let counterpart_public_id = counterpart_public_id.unwrap_or_else(|| remote_node_id.to_owned());
    let target_agent = identities
        .get(&counterpart_public_id)
        .and_then(|identity| identity.agent_did.clone())
        .unwrap_or_else(|| counterpart_public_id.clone());
    Ok(SocialCounterpartTarget {
        counterpart_public_id,
        remote_node: remote_node_id.to_owned(),
        target_agent,
    })
}

pub(crate) fn counterpart_public_id_for_remote_node(
    bindings: &BTreeMap<String, ControllerBinding>,
    remote_node_id: &str,
) -> Option<String> {
    bindings
        .values()
        .find(|binding| {
            binding.active && binding.controller_node_id.as_deref() == Some(remote_node_id)
        })
        .map(|binding| binding.public_id.clone())
}

pub(crate) fn build_signed_agent_envelope(
    state: &ControlPlaneState,
    source_agent_id: String,
    target_agent_id: String,
    capability: &str,
    message: Value,
    extensions: Option<Value>,
) -> anyhow::Result<SwarmAgentEnvelope> {
    let protocol = "google_a2a".to_string();
    let message_json = serde_json::to_string(&message)?;
    let extensions_json = extensions.as_ref().map(serde_json::to_string).transpose()?;
    let capability = Some(capability.to_string());
    let source_agent_id = Some(source_agent_id);
    let target_agent_id = Some(target_agent_id);
    let unsigned = SignedAgentEnvelopePayload {
        protocol: &protocol,
        source_agent_id: source_agent_id.as_ref(),
        target_agent_id: target_agent_id.as_ref(),
        capability: capability.as_ref(),
        message_json: &message_json,
        extensions_json: extensions_json.as_ref(),
    };
    let signature = state.sign_payload(&unsigned)?;
    Ok(SwarmAgentEnvelope {
        protocol,
        source_agent_id,
        target_agent_id,
        capability,
        message,
        extensions,
        signature: Some(signature),
    })
}

pub(crate) fn capability_for_relationship_action(action: &SwarmRelationshipAction) -> &'static str {
    match action {
        SwarmRelationshipAction::Request => "social.friend.request",
        SwarmRelationshipAction::Accept => "social.friend.accept",
        SwarmRelationshipAction::Reject => "social.friend.reject",
        SwarmRelationshipAction::Cancel => "social.friend.cancel",
        SwarmRelationshipAction::Remove => "social.friend.remove",
        SwarmRelationshipAction::Block => "social.friend.block",
        SwarmRelationshipAction::Unblock => "social.friend.unblock",
    }
}

pub(crate) fn with_social_defaults<const N: usize>(
    payload: Value,
    defaults: [(&str, Value); N],
) -> Value {
    let mut object = match payload {
        Value::Object(object) => object,
        value => {
            let mut object = Map::new();
            object.insert("payload".to_owned(), value);
            object
        }
    };
    for (key, value) in defaults {
        object.entry(key.to_owned()).or_insert(value);
    }
    Value::Object(object)
}
