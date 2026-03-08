use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error};
use crate::autonomy::build_operator_briefing;
use crate::state::{
    CharacterBootstrapBody, CitizenProfileBody, CitizenProfileQuery, ControlPlaneState,
    ControllerBindingBody, ControllerBindingQuery, EmergencyQuery, GalaxyEventBody,
    GalaxyEventsQuery, GalaxyGenerateBody, MetricsQuery, NightShiftQuery, PublicIdentityBody,
    PublicIdentityQuery, StreamEvent,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::emergency::{evaluate_emergencies, generate_system_galaxy_events};
use wattetheria_kernel::identities::{
    ControllerBinding, ControllerKind, OwnershipScope, PublicIdentity,
};
use wattetheria_kernel::metrics::compute_scores;
use wattetheria_kernel::profiles::CitizenProfile;

#[derive(Debug, Clone, Serialize)]
struct PublicMemoryOwnerView {
    #[serde(rename = "public_id")]
    public: Option<String>,
    #[serde(rename = "controller_id")]
    controller: String,
    #[serde(rename = "legacy_agent_id")]
    legacy_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct IdentityContextView {
    public_identity: Option<PublicIdentity>,
    controller_binding: Option<ControllerBinding>,
    profile: Option<CitizenProfile>,
    public_memory_owner: PublicMemoryOwnerView,
}

#[derive(Debug, Clone, Serialize)]
struct PublicMemoryEnvelope {
    public_memory: PublicMemoryOwnerView,
    scope: String,
    record: Value,
}

#[derive(Debug, Clone)]
struct BootstrapCharacterPlan {
    controller_kind: ControllerKind,
    controller_ref: String,
    controller_node_id: Option<String>,
    ownership_scope: OwnershipScope,
    legacy_agent_id: Option<String>,
    active: bool,
}

pub(crate) async fn identity_context_value(
    state: &ControlPlaneState,
    public_id: Option<&str>,
    agent_id: Option<&str>,
) -> Value {
    serde_json::to_value(resolve_identity_context(state, public_id, agent_id).await)
        .unwrap_or(Value::Null)
}

async fn resolve_identity_context(
    state: &ControlPlaneState,
    public_id: Option<&str>,
    agent_id: Option<&str>,
) -> IdentityContextView {
    let current_public_id = {
        let registry = state.controller_binding_registry.lock().await;
        registry
            .active_for_controller(&state.agent_id)
            .map(|binding| binding.public_id)
    };
    let public_identity = {
        let registry = state.public_identity_registry.lock().await;
        if let Some(public_id) = public_id {
            registry.get(public_id)
        } else if let Some(agent_id) = agent_id {
            registry.active_for_legacy_agent(agent_id)
        } else {
            current_public_id
                .as_deref()
                .and_then(|current_public_id| registry.get(current_public_id))
                .or_else(|| registry.active_for_legacy_agent(&state.agent_id))
        }
    };
    let controller_binding = {
        let registry = state.controller_binding_registry.lock().await;
        public_identity
            .as_ref()
            .and_then(|identity| registry.get(&identity.public_id))
            .or_else(|| public_id.and_then(|public_id| registry.get(public_id)))
            .or_else(|| {
                agent_id.and_then(|controller_id| registry.active_for_controller(controller_id))
            })
            .or_else(|| registry.active_for_controller(&state.agent_id))
    };
    let profile_agent_id = public_identity
        .as_ref()
        .and_then(|identity| identity.legacy_agent_id.clone())
        .or_else(|| agent_id.map(ToOwned::to_owned))
        .or_else(|| {
            controller_binding
                .as_ref()
                .and_then(|binding| binding.controller_node_id.clone())
        })
        .unwrap_or_else(|| state.agent_id.clone());
    let profile = state
        .citizen_registry
        .lock()
        .await
        .profile(&profile_agent_id);
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
            .unwrap_or(profile_agent_id.clone()),
        legacy_agent: public_identity
            .as_ref()
            .and_then(|identity| identity.legacy_agent_id.clone())
            .or_else(|| profile.as_ref().map(|profile| profile.agent_id.clone())),
    };

    IdentityContextView {
        public_identity,
        controller_binding,
        profile,
        public_memory_owner,
    }
}

fn identity_context_response(context: &IdentityContextView) -> Value {
    json!({
        "public_identity": context.public_identity,
        "controller_binding": context.controller_binding,
        "profile": context.profile,
        "public_memory_owner": context.public_memory_owner,
    })
}

fn public_memory_payload(context: &IdentityContextView, scope: &str, record: Value) -> Value {
    serde_json::to_value(PublicMemoryEnvelope {
        public_memory: context.public_memory_owner.clone(),
        scope: scope.to_string(),
        record,
    })
    .unwrap_or(Value::Null)
}

fn plan_bootstrap_character(
    body: &CharacterBootstrapBody,
    state_agent_id: &str,
) -> Result<BootstrapCharacterPlan, String> {
    let controller_kind = body
        .controller_kind
        .clone()
        .unwrap_or(ControllerKind::LocalWattswarm);
    let controller_node_id = match controller_kind {
        ControllerKind::LocalWattswarm => Some(
            body.controller_node_id
                .clone()
                .unwrap_or_else(|| state_agent_id.to_string()),
        ),
        ControllerKind::ExternalRuntime => body.controller_node_id.clone(),
    };
    let legacy_agent_id = body
        .legacy_agent_id
        .clone()
        .or_else(|| controller_node_id.clone());
    if matches!(controller_kind, ControllerKind::ExternalRuntime) && legacy_agent_id.is_none() {
        return Err("external_runtime requires legacy_agent_id or controller_node_id".to_string());
    }
    let ownership_scope = body
        .ownership_scope
        .clone()
        .unwrap_or(match controller_kind {
            ControllerKind::LocalWattswarm => OwnershipScope::Local,
            ControllerKind::ExternalRuntime => OwnershipScope::External,
        });
    let controller_ref = body.controller_ref.clone().unwrap_or_else(|| {
        match controller_kind {
            ControllerKind::LocalWattswarm => "local-default",
            ControllerKind::ExternalRuntime => "external-runtime",
        }
        .to_string()
    });

    Ok(BootstrapCharacterPlan {
        controller_kind,
        controller_ref,
        controller_node_id,
        ownership_scope,
        legacy_agent_id,
        active: body.active.unwrap_or(true),
    })
}

async fn persist_bootstrap_character(
    state: &ControlPlaneState,
    body: &CharacterBootstrapBody,
    plan: &BootstrapCharacterPlan,
) -> Result<(), anyhow::Error> {
    {
        let mut registry = state.public_identity_registry.lock().await;
        registry.upsert(
            &body.public_id,
            body.display_name.clone(),
            plan.legacy_agent_id.clone(),
            plan.active,
        );
        registry.persist(&state.public_identity_registry_state_path)?;
    }
    {
        let mut registry = state.controller_binding_registry.lock().await;
        registry.upsert(
            &body.public_id,
            plan.controller_kind.clone(),
            plan.controller_ref.clone(),
            plan.controller_node_id.clone(),
            plan.ownership_scope.clone(),
            plan.active,
        );
        registry.persist(&state.controller_binding_registry_state_path)?;
    }
    {
        let profile_agent_id = plan
            .legacy_agent_id
            .clone()
            .unwrap_or_else(|| state.agent_id.clone());
        let mut registry = state.citizen_registry.lock().await;
        registry.set_profile(
            &profile_agent_id,
            body.faction.clone(),
            body.role.clone(),
            body.strategy.clone(),
            body.home_subnet_id.clone(),
            body.home_zone_id.clone(),
        );
        registry.persist(&state.citizen_registry_state_path)?;
    }

    Ok(())
}

pub(crate) async fn citizen_profile(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<CitizenProfileQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(
        &state,
        query.public_id.as_deref(),
        query.agent_id.as_deref(),
    )
    .await;
    let subject = context
        .public_memory_owner
        .public
        .clone()
        .unwrap_or_else(|| context.public_memory_owner.controller.clone());

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.profile.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(subject),
        capability: None,
        reason: None,
        duration_ms: None,
        details: None,
    });

    Json(identity_context_response(&context)).into_response()
}

pub(crate) async fn public_identity(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<PublicIdentityQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(&state, query.public_id.as_deref(), None).await;
    let subject = context
        .public_memory_owner
        .public
        .clone()
        .unwrap_or_else(|| state.agent_id.clone());

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.public_identity.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(subject),
        capability: None,
        reason: None,
        duration_ms: None,
        details: None,
    });

    Json(identity_context_response(&context)).into_response()
}

pub(crate) async fn public_identity_upsert(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<PublicIdentityBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let mut registry = state.public_identity_registry.lock().await;
    let identity = registry.upsert(
        &body.public_id,
        body.display_name,
        body.legacy_agent_id,
        body.active.unwrap_or(true),
    );
    if let Err(error) = registry.persist(&state.public_identity_registry_state_path) {
        return internal_error(&error);
    }
    drop(registry);

    public_identity_updated(&state, &auth, identity).await
}

pub(crate) async fn controller_binding(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ControllerBindingQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(&state, query.public_id.as_deref(), None).await;
    let subject = context
        .public_memory_owner
        .public
        .clone()
        .unwrap_or_else(|| state.agent_id.clone());

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.controller_binding.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(subject),
        capability: None,
        reason: None,
        duration_ms: None,
        details: None,
    });

    Json(identity_context_response(&context)).into_response()
}

pub(crate) async fn controller_binding_upsert(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ControllerBindingBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let mut registry = state.controller_binding_registry.lock().await;
    let binding = registry.upsert(
        &body.public_id,
        body.controller_kind,
        body.controller_ref,
        body.controller_node_id,
        body.ownership_scope,
        body.active.unwrap_or(true),
    );
    if let Err(error) = registry.persist(&state.controller_binding_registry_state_path) {
        return internal_error(&error);
    }
    drop(registry);

    controller_binding_updated(&state, &auth, binding).await
}

pub(crate) async fn citizen_profile_upsert(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<CitizenProfileBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let mut registry = state.citizen_registry.lock().await;
    let profile = registry.set_profile(
        &body.agent_id,
        body.faction,
        body.role,
        body.strategy,
        body.home_subnet_id,
        body.home_zone_id,
    );
    if let Err(error) = registry.persist(&state.citizen_registry_state_path) {
        return internal_error(&error);
    }
    drop(registry);

    let context = resolve_identity_context(&state, None, Some(&body.agent_id)).await;
    let payload = public_memory_payload(
        &context,
        "identity",
        serde_json::to_value(&profile).unwrap_or(Value::Null),
    );
    let _ = state.stream_tx.send(StreamEvent {
        kind: "civilization.profile.updated".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.event_log.append_signed(
        "CIVILIZATION_PROFILE_UPDATED",
        payload.clone(),
        &state.identity,
    );

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.profile.update".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.agent_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    Json(identity_context_response(&context)).into_response()
}

pub(crate) async fn bootstrap_character(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<CharacterBootstrapBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let plan = match plan_bootstrap_character(&body, &state.agent_id) {
        Ok(plan) => plan,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
        }
    };
    if let Err(error) = persist_bootstrap_character(&state, &body, &plan).await {
        return internal_error(&error);
    }
    let context = resolve_identity_context(&state, Some(&body.public_id), None).await;
    let payload = public_memory_payload(&context, "identity", identity_context_response(&context));
    let _ = state.stream_tx.send(StreamEvent {
        kind: "civilization.character.bootstrapped".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.event_log.append_signed(
        "CIVILIZATION_CHARACTER_BOOTSTRAPPED",
        payload.clone(),
        &state.identity,
    );
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.character.bootstrap".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.public_id),
        capability: Some("civilization.character.bootstrap".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    Json(identity_context_response(&context)).into_response()
}

async fn public_identity_updated(
    state: &ControlPlaneState,
    auth: &str,
    identity: PublicIdentity,
) -> Response {
    let context = resolve_identity_context(state, Some(&identity.public_id), None).await;
    let payload = public_memory_payload(
        &context,
        "identity",
        serde_json::to_value(&identity).unwrap_or(Value::Null),
    );
    let _ = state.stream_tx.send(StreamEvent {
        kind: "civilization.public_identity.updated".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.event_log.append_signed(
        "CIVILIZATION_PUBLIC_IDENTITY_UPDATED",
        payload.clone(),
        &state.identity,
    );
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.public_identity.update".to_string(),
        status: "ok".to_string(),
        actor: Some(auth.to_string()),
        subject: Some(identity.public_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });
    Json(identity_context_response(&context)).into_response()
}

async fn controller_binding_updated(
    state: &ControlPlaneState,
    auth: &str,
    binding: ControllerBinding,
) -> Response {
    let context = resolve_identity_context(state, Some(&binding.public_id), None).await;
    let payload = public_memory_payload(
        &context,
        "identity",
        serde_json::to_value(&binding).unwrap_or(Value::Null),
    );
    let _ = state.stream_tx.send(StreamEvent {
        kind: "civilization.controller_binding.updated".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.event_log.append_signed(
        "CIVILIZATION_CONTROLLER_BINDING_UPDATED",
        payload.clone(),
        &state.identity,
    );
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.controller_binding.update".to_string(),
        status: "ok".to_string(),
        actor: Some(auth.to_string()),
        subject: Some(binding.public_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });
    Json(identity_context_response(&context)).into_response()
}

pub(crate) async fn civilization_metrics(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<MetricsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(
        &state,
        query.public_id.as_deref(),
        query.agent_id.as_deref(),
    )
    .await;
    let agent_id = context.public_memory_owner.controller.clone();
    let agent_stats = match state.swarm_bridge.agent_view(&agent_id).await {
        Ok(view) => view.stats,
        Err(error) => return internal_error(&error),
    };
    let missions = state.mission_board.lock().await;
    let profiles = state.citizen_registry.lock().await;
    let governance = state.governance_engine.lock().await;
    let galaxy = state.galaxy_state.lock().await;
    let scores = compute_scores(
        &agent_id,
        &agent_stats,
        &missions,
        &profiles,
        &governance,
        &galaxy,
    );
    drop(galaxy);
    drop(governance);
    drop(profiles);
    drop(missions);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.metrics.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(
            context
                .public_memory_owner
                .public
                .clone()
                .unwrap_or_else(|| agent_id.clone()),
        ),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(serde_json::to_value(&scores).unwrap_or(Value::Null)),
    });

    Json(json!({
        "metrics": scores,
        "public_identity": context.public_identity,
        "controller_binding": context.controller_binding,
        "profile": context.profile,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
}

pub(crate) async fn galaxy_zones(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let zones = state.galaxy_state.lock().await.zones();
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "galaxy".to_string(),
        action: "galaxy.zones.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": zones.len()})),
    });
    Json(zones).into_response()
}

pub(crate) async fn galaxy_events(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<GalaxyEventsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let events = state
        .galaxy_state
        .lock()
        .await
        .events(query.zone_id.as_deref());
    let context = resolve_identity_context(&state, None, Some(&state.agent_id)).await;
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "galaxy".to_string(),
        action: "galaxy.events.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: query.zone_id,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": events.len()})),
    });
    Json(json!({
        "events": events,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
}

pub(crate) async fn galaxy_event_publish(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GalaxyEventBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let mut galaxy = state.galaxy_state.lock().await;
    let event = match galaxy.publish_event(
        body.category,
        &body.zone_id,
        &body.title,
        &body.description,
        body.severity,
        body.expires_at,
        body.tags,
    ) {
        Ok(event) => event,
        Err(error) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    if let Err(error) = galaxy.persist(&state.galaxy_state_path) {
        return internal_error(&error);
    }
    drop(galaxy);

    let context = resolve_identity_context(&state, None, Some(&state.agent_id)).await;
    let payload = public_memory_payload(
        &context,
        "galaxy",
        serde_json::to_value(&event).unwrap_or(Value::Null),
    );
    let _ = state.stream_tx.send(StreamEvent {
        kind: "galaxy.event.published".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ =
        state
            .event_log
            .append_signed("GALAXY_EVENT_PUBLISHED", payload.clone(), &state.identity);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "galaxy".to_string(),
        action: "galaxy.event.publish".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.zone_id),
        capability: Some("galaxy.event.publish".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    Json(json!({
        "event": event,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
}

pub(crate) async fn galaxy_event_generate(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<GalaxyGenerateBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let missions = state.mission_board.lock().await;
    let governance = state.governance_engine.lock().await;
    let mut galaxy = state.galaxy_state.lock().await;
    let generated = match generate_system_galaxy_events(
        &mut galaxy,
        &governance,
        &missions,
        body.max_events.unwrap_or(5).max(1),
    ) {
        Ok(events) => events,
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = galaxy.persist(&state.galaxy_state_path) {
        return internal_error(&error);
    }
    drop(galaxy);
    drop(governance);
    drop(missions);

    let context = resolve_identity_context(&state, None, Some(&state.agent_id)).await;
    let payload = public_memory_payload(
        &context,
        "galaxy",
        serde_json::to_value(&generated).unwrap_or(Value::Null),
    );
    let _ = state.stream_tx.send(StreamEvent {
        kind: "galaxy.events.generated".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ =
        state
            .event_log
            .append_signed("GALAXY_EVENTS_GENERATED", payload.clone(), &state.identity);
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "galaxy".to_string(),
        action: "galaxy.events.generate".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: Some("galaxy.events.generate".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    Json(json!({
        "events": generated,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
}

pub(crate) async fn civilization_emergencies(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<EmergencyQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(
        &state,
        query.public_id.as_deref(),
        query.agent_id.as_deref(),
    )
    .await;
    let agent_id = context.public_memory_owner.controller.clone();
    let missions = state.mission_board.lock().await;
    let profiles = state.citizen_registry.lock().await;
    let governance = state.governance_engine.lock().await;
    let galaxy = state.galaxy_state.lock().await;
    let emergencies = evaluate_emergencies(&agent_id, &profiles, &missions, &governance, &galaxy);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.emergencies.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(
            context
                .public_memory_owner
                .public
                .clone()
                .unwrap_or_else(|| agent_id.clone()),
        ),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": emergencies.len()})),
    });

    Json(json!({
        "emergencies": emergencies,
        "public_identity": context.public_identity,
        "controller_binding": context.controller_binding,
        "profile": context.profile,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
}

pub(crate) async fn civilization_briefing(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<NightShiftQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let hours = query.hours.unwrap_or(12).max(1);
    let briefing = match build_operator_briefing(&state, hours).await {
        Ok(briefing) => briefing,
        Err(error) => return internal_error(&error),
    };
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.briefing.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(state.agent_id.clone()),
        capability: None,
        reason: Some(format!("hours={hours}")),
        duration_ms: None,
        details: Some(json!({"emergencies": briefing["emergencies"]})),
    });
    let context = resolve_identity_context(&state, None, Some(&state.agent_id)).await;
    Json(json!({
        "briefing": briefing,
        "public_identity": context.public_identity,
        "controller_binding": context.controller_binding,
        "profile": context.profile,
        "public_memory_owner": context.public_memory_owner,
    }))
    .into_response()
}
