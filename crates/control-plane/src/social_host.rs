use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::Duration;
use wattetheria_social::domain::agent_skills::AgentSkill;
use wattetheria_social::domain::identities::LocalIdentityContext;
use wattetheria_social::ports::local_identity_provider::LocalIdentityProvider;
use wattetheria_social::ports::repositories::{
    RemoteIdentityRepository, TransportBindingRepository,
};
use wattetheria_social::ports::transport_port::TransportPort;
use wattetheria_social::types::{SocialError, SocialResult};

use crate::routes::identity::resolve_identity_context;
use crate::state::ControlPlaneState;
use wattetheria_kernel::identities::{ControllerBinding, PublicIdentity};
use wattetheria_kernel::swarm_bridge::{
    SwarmAgentEnvelope, SwarmDirectMessageCommand, SwarmRelationshipAction,
    SwarmRelationshipActionCommand, SwarmSourceAgentCard,
};

const PUBLIC_SOURCE_AGENT_CARD_NODE_ID_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub(crate) struct SocialLocalContext {
    pub(crate) public_id: String,
    pub(crate) agent_id: String,
    pub(crate) display_name: Option<String>,
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
    transport_profile: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_agent_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_node_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_node_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capability: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent_card_hash: Option<&'a String>,
    message_json: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions_json: Option<&'a String>,
}

#[derive(Debug, serde::Serialize)]
struct SignedSourceAgentCardPayload<'a> {
    agent_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<&'a String>,
    card_hash: &'a str,
    issued_at: u64,
}

pub(crate) struct SignedAgentEnvelopeArgs {
    pub source_agent_id: String,
    pub source_public_id: Option<String>,
    pub source_display_name: Option<String>,
    pub target_agent_id: Option<String>,
    pub source_node_id: Option<String>,
    pub target_node_id: Option<String>,
    pub capability: String,
    pub message: Value,
    pub extensions: Option<Value>,
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
        let local_node_id = self.state.swarm_bridge.local_node_id().await.ok();
        let target_agent = counterpart.target_agent.clone();
        let target_node = counterpart.remote_node.clone();
        let agent_envelope = build_signed_agent_envelope_for_nodes(
            &self.state,
            SignedAgentEnvelopeArgs {
                source_agent_id: local.agent_id,
                source_public_id: public_agent_id(&local.public_id),
                source_display_name: local.display_name,
                target_agent_id: Some(target_agent),
                source_node_id: local_node_id,
                target_node_id: Some(target_node),
                capability: capability.to_string(),
                message,
                extensions: None,
            },
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
        let local_node_id = self.state.swarm_bridge.local_node_id().await.ok();
        let target_agent = counterpart.target_agent.clone();
        let target_node = counterpart.remote_node.clone();
        let agent_envelope = build_signed_agent_envelope_for_nodes(
            &self.state,
            SignedAgentEnvelopeArgs {
                source_agent_id: local.agent_id,
                source_public_id: public_agent_id(&local.public_id),
                source_display_name: local.display_name,
                target_agent_id: Some(target_agent),
                source_node_id: local_node_id,
                target_node_id: Some(target_node),
                capability: "social.dm.send".to_string(),
                message,
                extensions: None,
            },
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
    let public_id = context
        .public_identity
        .as_ref()
        .and_then(|identity| public_agent_id(&identity.public_id))
        .unwrap_or_default();
    let agent_id = context
        .public_identity
        .as_ref()
        .and_then(|identity| identity.agent_did.clone())
        .unwrap_or_else(|| state.agent_did.clone());
    let display_name = context
        .public_identity
        .as_ref()
        .map(|identity| identity.display_name.clone());
    SocialLocalContext {
        public_id,
        agent_id,
        display_name,
    }
}

pub(crate) async fn load_social_identity_maps(
    state: &ControlPlaneState,
) -> (
    BTreeMap<String, PublicIdentity>,
    BTreeMap<String, ControllerBinding>,
) {
    let mut identities = state
        .public_identity_registry
        .lock()
        .await
        .list()
        .into_iter()
        .map(|identity| (identity.public_id.clone(), identity))
        .collect::<BTreeMap<_, _>>();
    for identity in state
        .social_store
        .list_remote_identities()
        .unwrap_or_default()
    {
        if let Some(existing) = identities.get_mut(&identity.public_id) {
            existing.display_name = identity.display_name;
        } else {
            identities.insert(
                identity.public_id.clone(),
                PublicIdentity {
                    public_id: identity.public_id,
                    display_name: identity.display_name,
                    agent_did: Some(identity.agent_did),
                    active: identity.active,
                    created_at: identity.created_at,
                    updated_at: identity.updated_at,
                },
            );
        }
    }
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

pub(crate) async fn resolve_dm_counterpart_target(
    state: &ControlPlaneState,
    counterpart_public_id: &str,
) -> Result<SocialCounterpartTarget, String> {
    let counterpart_public_id = counterpart_public_id.trim();
    if counterpart_public_id.is_empty() {
        return Err("counterpart_public_id is required".to_string());
    }

    let binding = state
        .social_store
        .list_transport_bindings_for_public_id(counterpart_public_id)
        .map_err(|error| format!("query remote transport bindings: {error}"))?
        .into_iter()
        .find(|binding| {
            matches!(
                binding.transport_kind,
                wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm
            ) && !binding.transport_node_id.trim().is_empty()
        })
        .ok_or_else(|| format!("remote transport binding missing for {counterpart_public_id}"))?;

    let remote_identity = state
        .social_store
        .get_remote_identity(counterpart_public_id)
        .map_err(|error| format!("query remote identity: {error}"))?;
    let kernel_identity = {
        let (identities, _) = load_social_identity_maps(state).await;
        identities.get(counterpart_public_id).cloned()
    };
    let target_agent = remote_identity
        .as_ref()
        .filter(|identity| identity.active)
        .map(|identity| identity.agent_did.clone())
        .or(binding.agent_did.clone())
        .or_else(|| kernel_identity.and_then(|identity| identity.agent_did))
        .unwrap_or_else(|| counterpart_public_id.to_string());

    Ok(SocialCounterpartTarget {
        counterpart_public_id: counterpart_public_id.to_string(),
        remote_node: binding.transport_node_id,
        target_agent,
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

pub(crate) async fn resolve_social_counterpart_target_by_agent_did(
    state: &ControlPlaneState,
    target_agent_did: &str,
    counterpart_public_id_hint: Option<String>,
) -> Result<SocialCounterpartTarget, String> {
    let target_agent_did = target_agent_did.trim();
    if target_agent_did.is_empty() {
        return Err("target_agent_did is required".to_string());
    }

    let counterpart_public_id_hint = counterpart_public_id_hint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let (identities, bindings) = load_social_identity_maps(state).await;
    let identity = if let Some(counterpart_public_id) = counterpart_public_id_hint {
        let identity = identities.get(counterpart_public_id).ok_or_else(|| {
            format!("public identity missing for counterpart_public_id {counterpart_public_id}")
        })?;
        if !identity.active {
            return Err(format!(
                "public identity {counterpart_public_id} is not active"
            ));
        }
        if identity.agent_did.as_deref() != Some(target_agent_did) {
            return Err("counterpart_public_id does not match target_agent_did".to_string());
        }
        identity
    } else {
        identities
            .values()
            .find(|identity| {
                identity.active && identity.agent_did.as_deref() == Some(target_agent_did)
            })
            .ok_or_else(|| {
                "target_agent_did is not a known public identity; provide remote_node_id or counterpart_public_id"
                    .to_string()
            })?
    };

    let binding = bindings
        .get(&identity.public_id)
        .filter(|binding| binding.active)
        .ok_or_else(|| {
            format!(
                "active controller binding missing for {}",
                identity.public_id
            )
        })?;
    let remote_node_id = binding
        .controller_node_id
        .clone()
        .ok_or_else(|| format!("controller_node_id missing for {}", identity.public_id))?;
    Ok(SocialCounterpartTarget {
        counterpart_public_id: identity.public_id.clone(),
        remote_node: remote_node_id,
        target_agent: target_agent_did.to_string(),
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

pub(crate) fn build_signed_agent_envelope_with_optional_target(
    state: &ControlPlaneState,
    args: SignedAgentEnvelopeArgs,
) -> anyhow::Result<SwarmAgentEnvelope> {
    let protocol = "google_a2a".to_string();
    let transport_profile = Some("wattswarm_mesh".to_string());
    let message_json = serde_json::to_string(&args.message)?;
    let extensions_json = args
        .extensions
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let capability = Some(args.capability);
    let source_agent_id = Some(args.source_agent_id);
    let source_agent_card = source_agent_id
        .as_ref()
        .map(|agent_id| {
            build_source_agent_card(
                state,
                agent_id,
                args.source_public_id.as_deref(),
                args.source_display_name.as_deref(),
                args.source_node_id.as_ref(),
            )
        })
        .transpose()?;
    let unsigned = SignedAgentEnvelopePayload {
        protocol: &protocol,
        transport_profile: transport_profile.as_ref(),
        source_agent_id: source_agent_id.as_ref(),
        target_agent_id: args.target_agent_id.as_ref(),
        source_node_id: args.source_node_id.as_ref(),
        target_node_id: args.target_node_id.as_ref(),
        capability: capability.as_ref(),
        source_agent_card_hash: source_agent_card.as_ref().map(|card| &card.card_hash),
        message_json: &message_json,
        extensions_json: extensions_json.as_ref(),
    };
    let signature = state.sign_payload(&unsigned)?;
    Ok(SwarmAgentEnvelope {
        protocol,
        transport_profile,
        source_agent_id,
        target_agent_id: args.target_agent_id,
        source_node_id: args.source_node_id,
        target_node_id: args.target_node_id,
        capability,
        source_agent_card,
        message: args.message,
        extensions: args.extensions,
        signature: Some(signature),
    })
}

pub(crate) fn build_signed_agent_envelope_for_nodes(
    state: &ControlPlaneState,
    args: SignedAgentEnvelopeArgs,
) -> anyhow::Result<SwarmAgentEnvelope> {
    build_signed_agent_envelope_with_optional_target(state, args)
}

pub async fn public_source_agent_card(
    state: &ControlPlaneState,
) -> anyhow::Result<SwarmSourceAgentCard> {
    let local = resolve_social_local_context(state, None).await;
    let source_node_id = tokio::time::timeout(
        PUBLIC_SOURCE_AGENT_CARD_NODE_ID_TIMEOUT,
        state.swarm_bridge.local_node_id(),
    )
    .await
    .ok()
    .and_then(Result::ok);
    let card = build_source_agent_card(
        state,
        &local.agent_id,
        public_agent_id(&local.public_id).as_deref(),
        local.display_name.as_deref(),
        source_node_id.as_ref(),
    )?;
    Ok(card)
}

fn build_source_agent_card(
    state: &ControlPlaneState,
    agent_id: &str,
    public_id: Option<&str>,
    display_name: Option<&str>,
    node_id: Option<&String>,
) -> anyhow::Result<SwarmSourceAgentCard> {
    let issued_at = Utc::now().timestamp_millis().max(0).cast_unsigned();
    let display_suffix = agent_id
        .rsplit(':')
        .next()
        .unwrap_or(agent_id)
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    let card_display_name = display_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || format!("Wattetheria Agent {display_suffix}"),
            ToOwned::to_owned,
        );
    let skills = state
        .social_store
        .list_visible_agent_skills()
        .map_err(|error| anyhow::anyhow!(error))?
        .iter()
        .map(agent_skill_card_json)
        .collect::<Vec<_>>();
    let mut card = serde_json::json!({
        "protocolVersion": "1.0",
        "name": card_display_name,
        "description": "Wattetheria node agent participating through Wattswarm mesh.",
        "preferredTransport": "wattswarm_mesh",
        "defaultInputModes": ["application/json"],
        "defaultOutputModes": ["application/json"],
        "skills": skills,
        "capabilities": {
            "streaming": false,
            "pushNotifications": false,
            "stateTransitionHistory": true
        },
        "metadata": {
            "agent_id": agent_id,
            "node_id": node_id,
            "transport_profile": "wattswarm_mesh",
            "public_key": state.identity.public_key
        }
    });
    if let Some(public_id) = public_id.and_then(public_agent_id)
        && let Some(metadata) = card.get_mut("metadata").and_then(Value::as_object_mut)
    {
        metadata.insert("public_id".to_string(), Value::String(public_id));
    }
    if let Some(display_name) = display_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && let Some(metadata) = card.get_mut("metadata").and_then(Value::as_object_mut)
    {
        metadata.insert(
            "display_name".to_string(),
            Value::String(display_name.to_owned()),
        );
    }
    let card_hash = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_jcs::to_string(&card)?.as_bytes()))
    );
    let unsigned = SignedSourceAgentCardPayload {
        agent_id,
        node_id,
        card_hash: &card_hash,
        issued_at,
    };
    let signature = state.sign_payload(&unsigned)?;
    Ok(SwarmSourceAgentCard {
        agent_id: agent_id.to_owned(),
        node_id: node_id.cloned(),
        card_hash,
        issued_at,
        card,
        signature: Some(signature),
    })
}

fn agent_skill_card_json(skill: &AgentSkill) -> Value {
    serde_json::json!({
        "id": skill.skill_id,
        "name": skill.name,
        "description": skill.description,
        "tags": skill.tags
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

pub(crate) fn public_agent_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || is_did(value) || is_node_id(value) {
        return None;
    }
    Some(value.to_owned())
}

fn is_did(value: &str) -> bool {
    value
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("did:"))
}

fn is_node_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
