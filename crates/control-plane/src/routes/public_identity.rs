use anyhow::Context;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};

use crate::auth::{authorize, internal_error};
use crate::routes::identity::{
    identity_context_response, public_memory_payload, resolve_identity_context,
};
use crate::state::{
    BootstrapIdentityBody, CitizenProfileBody, ControlPlaneState, ControllerBindingBody,
    ControllerBindingQuery, PublicIdentityBody, PublicIdentityDisplayNameBody, PublicIdentityQuery,
    StreamEvent,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::identities::{
    ControllerBinding, ControllerKind, OwnershipScope, PublicIdentity, normalize_display_name,
};
use wattetheria_kernel::map::state::resolve_anchor_position;
use wattetheria_kernel::profiles::{Faction, RolePath, StrategyProfile};

#[derive(Debug, Clone)]
struct BootstrapIdentityPlan {
    public_id: String,
    controller_kind: ControllerKind,
    controller_ref: String,
    controller_node_id: Option<String>,
    ownership_scope: OwnershipScope,
    agent_did: Option<String>,
    active: bool,
    faction: Faction,
    role: RolePath,
    strategy: StrategyProfile,
}

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn slugify_public_id(seed: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_separator = false;
    for ch in seed.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator {
            slug.push('-');
            previous_was_separator = true;
        }
    }
    slug.trim_matches('-').to_string()
}

async fn resolve_bootstrap_public_id(
    state: &ControlPlaneState,
    body: &BootstrapIdentityBody,
    agent_did: Option<&str>,
) -> Result<String, String> {
    let fingerprint = agent_did
        .map(wattetheria_kernel::identity::fingerprint_from_did_key)
        .transpose()
        .map_err(|error| format!("fingerprint derivation failed: {error:#}"))?;

    if let Some(public_id) = normalize_optional_text(body.public_id.as_deref()) {
        if let Some(ref fp) = fingerprint {
            let embedded = wattetheria_kernel::identity::extract_public_id_fingerprint(&public_id);
            let is_did = wattetheria_kernel::identity::is_did_key_public_id(&public_id);
            if !is_did && (embedded != Some(fp.as_str())) {
                return Err(format!(
                    "provided public_id '{public_id}' must end with '.{fp}' for this agent"
                ));
            }
        }
        return Ok(public_id);
    }

    {
        let registry = state.public_identity_registry.lock().await;
        if let Some(agent_did) = agent_did
            && let Some(identity) = registry
                .list()
                .into_iter()
                .filter(|identity| {
                    identity.active
                        && identity.agent_did.as_deref() == Some(agent_did)
                        && identity.public_id != agent_did
                })
                .max_by_key(|identity| (identity.updated_at, identity.created_at))
        {
            return Ok(identity.public_id);
        }
    }

    let slug = slugify_public_id(&body.display_name);
    let fallback = agent_did
        .map(slugify_public_id)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "agent".to_string());
    let slug = if slug.is_empty() { fallback } else { slug };

    let Some(fp) = fingerprint else {
        return Ok(slug);
    };

    let scoped = wattetheria_kernel::identity::build_scoped_public_id(&slug, &fp);
    let registry = state.public_identity_registry.lock().await;
    if registry.get(&scoped).is_none() {
        return Ok(scoped);
    }

    let mut suffix = 2_u32;
    loop {
        let candidate =
            wattetheria_kernel::identity::build_scoped_public_id(&format!("{slug}-{suffix}"), &fp);
        if registry.get(&candidate).is_none() {
            return Ok(candidate);
        }
        suffix += 1;
    }
}

fn plan_bootstrap_identity(
    body: &BootstrapIdentityBody,
    state_agent_did: &str,
) -> Result<BootstrapIdentityPlan, String> {
    let display_name = body.display_name.trim();
    normalize_display_name(display_name).map_err(|error| format!("{error:#}"))?;
    let controller_kind = body
        .controller_kind
        .clone()
        .unwrap_or(ControllerKind::LocalWattswarm);
    let controller_node_id = match controller_kind {
        ControllerKind::LocalWattswarm => Some(
            body.controller_node_id
                .clone()
                .unwrap_or_else(|| state_agent_did.to_string()),
        ),
        ControllerKind::ExternalRuntime => body.controller_node_id.clone(),
    };
    let agent_did = body
        .agent_did
        .clone()
        .or_else(|| controller_node_id.clone());
    if matches!(controller_kind, ControllerKind::ExternalRuntime) && agent_did.is_none() {
        return Err("external_runtime requires agent_did or controller_node_id".to_string());
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

    Ok(BootstrapIdentityPlan {
        public_id: String::new(),
        controller_kind,
        controller_ref,
        controller_node_id,
        ownership_scope,
        agent_did,
        active: body.active.unwrap_or(true),
        faction: body.faction.clone().unwrap_or(Faction::Freeport),
        role: body.role.clone().unwrap_or(RolePath::Broker),
        strategy: body.strategy.clone().unwrap_or(StrategyProfile::Balanced),
    })
}

async fn persist_bootstrap_identity(
    state: &ControlPlaneState,
    body: &BootstrapIdentityBody,
    plan: &BootstrapIdentityPlan,
) -> Result<(), anyhow::Error> {
    {
        let mut registry = state.public_identity_registry.lock().await;
        registry.upsert(
            &plan.public_id,
            body.display_name.trim().to_string(),
            plan.agent_did.clone(),
            plan.active,
        )?;
        state.local_db.save_domain(
            wattetheria_kernel::local_db::domain::PUBLIC_IDENTITY_REGISTRY,
            &*registry,
        )?;
    }
    {
        let mut registry = state.controller_binding_registry.lock().await;
        registry.upsert(
            &plan.public_id,
            plan.controller_kind.clone(),
            plan.controller_ref.clone(),
            plan.controller_node_id.clone(),
            plan.ownership_scope.clone(),
            plan.active,
        );
        state.local_db.save_domain(
            wattetheria_kernel::local_db::domain::CONTROLLER_BINDING_REGISTRY,
            &*registry,
        )?;
    }
    {
        let profile_agent_did = plan
            .agent_did
            .clone()
            .unwrap_or_else(|| state.agent_did.clone());
        let mut registry = state.citizen_registry.lock().await;
        registry.set_profile(
            &profile_agent_did,
            plan.faction.clone(),
            plan.role.clone(),
            plan.strategy.clone(),
            body.home_subnet_id.clone(),
            body.home_zone_id.clone(),
        );
        state.local_db.save_domain(
            wattetheria_kernel::local_db::domain::CITIZEN_REGISTRY,
            &*registry,
        )?;
    }
    {
        let map = state
            .galaxy_map_registry
            .lock()
            .await
            .get("genesis-base")
            .context("default galaxy map missing")?;
        let Some(position) = resolve_anchor_position(
            &map,
            body.home_subnet_id.as_deref(),
            body.home_zone_id.as_deref(),
        ) else {
            anyhow::bail!("unable to resolve travel anchor");
        };
        let mut registry = state.travel_state_registry.lock().await;
        registry.set_position(
            &plan.public_id,
            plan.controller_node_id
                .as_deref()
                .unwrap_or(&state.agent_did),
            position,
        );
        state.local_db.save_domain(
            wattetheria_kernel::local_db::domain::TRAVEL_STATE_REGISTRY,
            &*registry,
        )?;
    }

    Ok(())
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
    let _ = state.append_signed_event("CIVILIZATION_PUBLIC_IDENTITY_UPDATED", payload.clone());
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
    let _ = state.append_signed_event("CIVILIZATION_CONTROLLER_BINDING_UPDATED", payload.clone());
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
        .unwrap_or_else(|| state.agent_did.clone());

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
    let identity = match registry.upsert(
        &body.public_id,
        body.display_name,
        body.agent_did,
        body.active.unwrap_or(true),
    ) {
        Ok(identity) => identity,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("{error:#}")})),
            )
                .into_response();
        }
    };
    if let Err(error) = state.local_db.save_domain(
        wattetheria_kernel::local_db::domain::PUBLIC_IDENTITY_REGISTRY,
        &*registry,
    ) {
        return internal_error(&error);
    }
    drop(registry);

    public_identity_updated(&state, &auth, identity).await
}

pub(crate) async fn public_identity_display_name_patch(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<PublicIdentityDisplayNameBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let public_id = body.public_id.trim();
    if public_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "public_id is required"})),
        )
            .into_response();
    }
    let mut registry = state.public_identity_registry.lock().await;
    let identity = match registry.update_display_name(public_id, &body.display_name) {
        Ok(identity) => identity,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("{error:#}")})),
            )
                .into_response();
        }
    };
    if let Err(error) = state.local_db.save_domain(
        wattetheria_kernel::local_db::domain::PUBLIC_IDENTITY_REGISTRY,
        &*registry,
    ) {
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
        .unwrap_or_else(|| state.agent_did.clone());

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
    if let Err(error) = state.local_db.save_domain(
        wattetheria_kernel::local_db::domain::CONTROLLER_BINDING_REGISTRY,
        &*registry,
    ) {
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
        &body.agent_did,
        body.faction,
        body.role,
        body.strategy,
        body.home_subnet_id,
        body.home_zone_id,
    );
    if let Err(error) = state.local_db.save_domain(
        wattetheria_kernel::local_db::domain::CITIZEN_REGISTRY,
        &*registry,
    ) {
        return internal_error(&error);
    }
    drop(registry);

    let context = resolve_identity_context(&state, None, Some(&body.agent_did)).await;
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
    let _ = state.append_signed_event("CIVILIZATION_PROFILE_UPDATED", payload.clone());

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.profile.update".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(body.agent_did),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    Json(identity_context_response(&context)).into_response()
}

pub(crate) async fn bootstrap_identity(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<BootstrapIdentityBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let mut plan = match plan_bootstrap_identity(&body, &state.agent_did) {
        Ok(plan) => plan,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
        }
    };
    plan.public_id =
        match resolve_bootstrap_public_id(&state, &body, plan.agent_did.as_deref()).await {
            Ok(id) => id,
            Err(error) => {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
            }
        };
    if let Err(error) = persist_bootstrap_identity(&state, &body, &plan).await {
        return internal_error(&error);
    }
    let context = resolve_identity_context(&state, Some(&plan.public_id), None).await;
    let payload = public_memory_payload(&context, "identity", identity_context_response(&context));
    let _ = state.stream_tx.send(StreamEvent {
        kind: "civilization.identity.bootstrapped".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event("CIVILIZATION_IDENTITY_BOOTSTRAPPED", payload.clone());
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.identity.bootstrap".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(plan.public_id),
        capability: Some("civilization.identity.bootstrap".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    Json(identity_context_response(&context)).into_response()
}
