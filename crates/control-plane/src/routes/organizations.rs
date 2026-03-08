use anyhow::bail;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::json;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::missions::{MissionPublisherKind, MissionStatus};
use wattetheria_kernel::civilization::organizations::OrganizationRole;
use wattetheria_kernel::civilization::organizations::{
    OrganizationAutonomyTrack, OrganizationCreateSpec, OrganizationMembership,
    OrganizationPermission, OrganizationProfile, OrganizationProposalCreateSpec,
    OrganizationProposalStatus, OrganizationRegistry, OrganizationSubnetCharterApplication,
    compute_autonomy_track,
};
use wattetheria_kernel::governance::GovernanceEngine;
use wattetheria_kernel::missions::MissionBoard;

use crate::auth::{authorize, internal_error};
use crate::routes::identity::{
    identity_context_response, public_memory_payload, resolve_identity_context,
};
use crate::state::StreamEvent;
use crate::state::{
    ControlPlaneState, MyOrganizationsQuery, OrganizationCharterApplicationBody,
    OrganizationCreateBody, OrganizationMemberBody, OrganizationMissionPublishBody,
    OrganizationProposalCreateBody, OrganizationProposalFinalizeBody, OrganizationProposalVoteBody,
    OrganizationProposalsQuery, OrganizationTreasuryBody, OrganizationsQuery,
};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OrganizationView {
    pub organization: OrganizationProfile,
    pub membership: OrganizationMembership,
    pub permissions: Vec<OrganizationPermission>,
    pub autonomy_track: OrganizationAutonomyTrack,
    pub active_member_count: usize,
    pub open_mission_count: usize,
    pub settled_mission_count: usize,
    pub home_subnet_governed: bool,
    pub subnet_readiness: String,
    pub governance_summary: OrganizationGovernanceSummary,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OrganizationGovernanceSummary {
    pub open_proposals_count: usize,
    pub accepted_proposals_count: usize,
    pub charter_application_count: usize,
    pub latest_charter_application: Option<OrganizationSubnetCharterApplication>,
    pub can_submit_charter_application: bool,
}

pub(crate) fn build_organization_views(
    organizations: &OrganizationRegistry,
    missions: &MissionBoard,
    governance: &GovernanceEngine,
    public_id: &str,
) -> Vec<OrganizationView> {
    organizations
        .organizations_for_public(public_id)
        .into_iter()
        .map(|(organization, membership)| {
            let members = organizations.memberships(&organization.organization_id);
            let active_member_count = members.iter().filter(|member| member.active).count();
            let permissions =
                organizations.permissions_for_public(&organization.organization_id, public_id);
            let open_mission_count = organization_missions(
                missions,
                &organization.organization_id,
                Some(&MissionStatus::Open),
            )
            .count();
            let settled_mission_count = organization_missions(
                missions,
                &organization.organization_id,
                Some(&MissionStatus::Settled),
            )
            .count();
            let home_subnet_governed = organization
                .home_subnet_id
                .as_deref()
                .is_some_and(|subnet_id| governance.planet(subnet_id).is_some());
            let autonomy_track = compute_autonomy_track(
                &organization,
                active_member_count,
                open_mission_count,
                settled_mission_count,
                home_subnet_governed,
            );
            let proposals = organizations.list_proposals(Some(&organization.organization_id));
            let charter_applications =
                organizations.list_subnet_charter_applications(Some(&organization.organization_id));
            let subnet_readiness = autonomy_track.current_status.clone();

            OrganizationView {
                organization,
                membership,
                permissions,
                autonomy_track: autonomy_track.clone(),
                active_member_count,
                open_mission_count,
                settled_mission_count,
                home_subnet_governed,
                subnet_readiness,
                governance_summary: OrganizationGovernanceSummary {
                    open_proposals_count: proposals
                        .iter()
                        .filter(|proposal| proposal.status == OrganizationProposalStatus::Open)
                        .count(),
                    accepted_proposals_count: proposals
                        .iter()
                        .filter(|proposal| proposal.status == OrganizationProposalStatus::Accepted)
                        .count(),
                    charter_application_count: charter_applications.len(),
                    latest_charter_application: charter_applications.into_iter().last(),
                    can_submit_charter_application: autonomy_track.eligible_for_subnet_charter,
                },
            }
        })
        .collect()
}

fn organization_missions<'a>(
    missions: &'a MissionBoard,
    organization_id: &str,
    status: Option<&'a MissionStatus>,
) -> impl Iterator<Item = wattetheria_kernel::civilization::missions::CivilMission> + 'a {
    let organization_id = organization_id.to_string();
    missions.list(status).into_iter().filter(move |mission| {
        mission.publisher_kind
            == wattetheria_kernel::civilization::missions::MissionPublisherKind::Organization
            && mission.publisher == organization_id
    })
}

pub(crate) async fn list_organizations(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<OrganizationsQuery>,
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
    let Some(public_id) = context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "public identity required"})),
        )
            .into_response();
    };
    let organizations = state.organization_registry.lock().await;
    let missions = state.mission_board.lock().await;
    let governance = state.governance_engine.lock().await;
    let views = build_organization_views(&organizations, &missions, &governance, &public_id);

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "organization".to_string(),
        action: "organization.list.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(public_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": views.len()})),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "organizations": views,
    }))
    .into_response()
}

pub(crate) async fn my_organizations(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<MyOrganizationsQuery>,
) -> Response {
    list_organizations(
        State(state),
        headers,
        Query(OrganizationsQuery {
            agent_id: query.agent_id,
            public_id: query.public_id,
        }),
    )
    .await
}

pub(crate) async fn create_organization(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<OrganizationCreateBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
    let Some(founder_public_id) = context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "public identity required"})),
        )
            .into_response();
    };

    let organization = {
        let mut organizations = state.organization_registry.lock().await;
        match organizations.create_organization(OrganizationCreateSpec {
            organization_id: body.organization_id,
            name: body.name,
            kind: body.kind,
            summary: body.summary,
            faction_alignment: body.faction_alignment,
            home_subnet_id: body.home_subnet_id,
            home_zone_id: body.home_zone_id,
            founder_public_id: founder_public_id.clone(),
        }) {
            Ok(organization) => {
                if let Err(error) = organizations.persist(&state.organization_registry_state_path) {
                    return internal_error(&error);
                }
                organization
            }
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        }
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "organization".to_string(),
        action: "organization.create".to_string(),
        status: "created".to_string(),
        actor: Some(auth),
        subject: Some(organization.organization_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"founder_public_id": founder_public_id})),
    });

    let founder_context = resolve_identity_context(&state, Some(&founder_public_id), None).await;
    let payload = public_memory_payload(
        &founder_context,
        "organization",
        json!({
            "organization": organization.clone(),
            "membership_role": "founder",
        }),
    );
    let _ = state.stream_tx.send(StreamEvent {
        kind: "organization.created".to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state
        .event_log
        .append_signed("ORGANIZATION_CREATED", payload, &state.identity);

    (
        StatusCode::CREATED,
        Json(json!({
            "organization": organization,
        })),
    )
        .into_response()
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn upsert_organization_member(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<OrganizationMemberBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    if state
        .public_identity_registry
        .lock()
        .await
        .get(&body.public_id)
        .is_none()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "public identity does not exist"})),
        )
            .into_response();
    }
    let actor_context =
        resolve_identity_context(&state, body.actor_public_id.as_deref(), None).await;
    let Some(actor_public_id) = actor_context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "acting public identity required"})),
        )
            .into_response();
    };
    let membership = {
        let mut organizations = state.organization_registry.lock().await;
        if let Err(error) = require_permission(
            &organizations,
            &body.organization_id,
            &actor_public_id,
            &OrganizationPermission::ManageMembers,
        ) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
        match organizations.upsert_membership(
            &body.organization_id,
            &body.public_id,
            body.role,
            body.title,
            body.active.unwrap_or(true),
        ) {
            Ok(membership) => {
                if let Err(error) = organizations.persist(&state.organization_registry_state_path) {
                    return internal_error(&error);
                }
                membership
            }
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        }
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "organization".to_string(),
        action: "organization.member.upsert".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(membership.organization_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "actor_public_id": actor_public_id,
            "public_id": membership.public_id,
            "role": membership.role,
            "active": membership.active,
        })),
    });

    let member_context = resolve_identity_context(&state, Some(&membership.public_id), None).await;
    let payload = public_memory_payload(
        &member_context,
        "organization",
        json!({
            "membership": membership.clone(),
        }),
    );
    let _ = state.stream_tx.send(StreamEvent {
        kind: "organization.member.updated".to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state
        .event_log
        .append_signed("ORGANIZATION_MEMBER_UPDATED", payload, &state.identity);

    Json(json!({
        "membership": membership,
    }))
    .into_response()
}

pub(crate) async fn fund_organization_treasury(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<OrganizationTreasuryBody>,
) -> Response {
    mutate_organization_treasury(state, headers, body, true).await
}

pub(crate) async fn spend_organization_treasury(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<OrganizationTreasuryBody>,
) -> Response {
    mutate_organization_treasury(state, headers, body, false).await
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn publish_organization_mission(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<OrganizationMissionPublishBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let actor_context =
        resolve_identity_context(&state, body.actor_public_id.as_deref(), None).await;
    let Some(actor_public_id) = actor_context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "acting public identity required"})),
        )
            .into_response();
    };

    let organization = {
        let mut organizations = state.organization_registry.lock().await;
        if let Err(error) = require_permission(
            &organizations,
            &body.organization_id,
            &actor_public_id,
            &OrganizationPermission::PublishMissions,
        ) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
        let treasury_commit_watt = body.treasury_commit_watt.unwrap_or(0);
        let organization = if treasury_commit_watt > 0 {
            match organizations.spend_treasury(&body.organization_id, treasury_commit_watt) {
                Ok(organization) => organization,
                Err(error) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": error.to_string()})),
                    )
                        .into_response();
                }
            }
        } else {
            match organizations.organization(&body.organization_id) {
                Some(organization) => organization,
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": "organization does not exist"})),
                    )
                        .into_response();
                }
            }
        };
        if let Err(error) = organizations.persist(&state.organization_registry_state_path) {
            return internal_error(&error);
        }
        organization
    };

    let mission = {
        let mut board = state.mission_board.lock().await;
        let mission = board.publish(
            &body.title,
            &body.description,
            &body.organization_id,
            MissionPublisherKind::Organization,
            body.domain,
            body.subnet_id,
            body.zone_id,
            body.required_role,
            body.required_faction,
            body.reward,
            json!({
                "organization_id": body.organization_id,
                "published_by_public_id": actor_public_id,
                "treasury_commit_watt": body.treasury_commit_watt.unwrap_or(0),
                "organization_mission": true,
                "payload": body.payload,
            }),
        );
        if let Err(error) = board.persist(&state.mission_board_state_path) {
            return internal_error(&error);
        }
        mission
    };

    let event_payload = public_memory_payload(
        &actor_context,
        "organization",
        json!({
            "organization": organization.clone(),
            "mission": mission.clone(),
            "published_by_public_id": actor_public_id,
        }),
    );
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "organization".to_string(),
        action: "organization.mission.publish".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(mission.mission_id.clone()),
        capability: Some("organization.mission.publish".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(event_payload.clone()),
    });
    let _ = state.stream_tx.send(StreamEvent {
        kind: "organization.mission.published".to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        payload: event_payload.clone(),
    });
    let _ = state.event_log.append_signed(
        "ORGANIZATION_MISSION_PUBLISHED",
        event_payload,
        &state.identity,
    );

    (
        StatusCode::CREATED,
        Json(json!({
            "organization": organization,
            "mission": mission,
        })),
    )
        .into_response()
}

pub(crate) async fn list_organization_governance(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<OrganizationProposalsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(&state, query.public_id.as_deref(), None).await;
    let Some(public_id) = context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "public identity required"})),
        )
            .into_response();
    };
    let organizations = state.organization_registry.lock().await;
    if organizations
        .membership_for_public(&query.organization_id, &public_id)
        .is_none()
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "organization membership required"})),
        )
            .into_response();
    }
    let proposals = organizations.list_proposals(Some(&query.organization_id));
    let applications = organizations.list_subnet_charter_applications(Some(&query.organization_id));

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "organization".to_string(),
        action: "organization.governance.list".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(query.organization_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "proposal_count": proposals.len(),
            "charter_application_count": applications.len(),
        })),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "proposals": proposals,
        "charter_applications": applications,
    }))
    .into_response()
}

pub(crate) async fn create_organization_proposal(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<OrganizationProposalCreateBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let actor_context =
        resolve_identity_context(&state, body.actor_public_id.as_deref(), None).await;
    let Some(actor_public_id) = actor_context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "acting public identity required"})),
        )
            .into_response();
    };
    let proposal = {
        let mut organizations = state.organization_registry.lock().await;
        if let Err(error) = require_permission(
            &organizations,
            &body.organization_id,
            &actor_public_id,
            &OrganizationPermission::ManageGovernance,
        ) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
        match organizations.create_proposal(OrganizationProposalCreateSpec {
            organization_id: body.organization_id,
            kind: body.kind,
            title: body.title,
            summary: body.summary,
            proposed_subnet_id: body.proposed_subnet_id,
            proposed_subnet_name: body.proposed_subnet_name,
            created_by_public_id: actor_public_id.clone(),
        }) {
            Ok(proposal) => {
                if let Err(error) = organizations.persist(&state.organization_registry_state_path) {
                    return internal_error(&error);
                }
                proposal
            }
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        }
    };

    let payload = public_memory_payload(
        &actor_context,
        "organization",
        json!({
            "proposal": proposal.clone(),
            "actor_public_id": actor_public_id,
        }),
    );
    emit_organization_event(
        &state,
        auth,
        "organization.proposal.create",
        "organization.proposal.created",
        "ORGANIZATION_PROPOSAL_CREATED",
        Some(proposal.organization_id.clone()),
        payload,
    );

    (StatusCode::CREATED, Json(json!({"proposal": proposal}))).into_response()
}

pub(crate) async fn vote_organization_proposal(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<OrganizationProposalVoteBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let actor_context =
        resolve_identity_context(&state, body.actor_public_id.as_deref(), None).await;
    let Some(actor_public_id) = actor_context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "acting public identity required"})),
        )
            .into_response();
    };
    let proposal = {
        let mut organizations = state.organization_registry.lock().await;
        match organizations.vote_proposal(&body.proposal_id, &actor_public_id, body.approve) {
            Ok(proposal) => {
                if let Err(error) = organizations.persist(&state.organization_registry_state_path) {
                    return internal_error(&error);
                }
                proposal
            }
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        }
    };

    let payload = public_memory_payload(
        &actor_context,
        "organization",
        json!({
            "proposal": proposal.clone(),
            "actor_public_id": actor_public_id,
            "approve": body.approve,
        }),
    );
    emit_organization_event(
        &state,
        auth,
        "organization.proposal.vote",
        "organization.proposal.voted",
        "ORGANIZATION_PROPOSAL_VOTED",
        Some(proposal.organization_id.clone()),
        payload,
    );

    Json(json!({"proposal": proposal})).into_response()
}

pub(crate) async fn finalize_organization_proposal(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<OrganizationProposalFinalizeBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let actor_context =
        resolve_identity_context(&state, body.actor_public_id.as_deref(), None).await;
    let Some(actor_public_id) = actor_context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "acting public identity required"})),
        )
            .into_response();
    };
    let proposal = {
        let mut organizations = state.organization_registry.lock().await;
        let existing = organizations
            .list_proposals(None)
            .into_iter()
            .find(|proposal| proposal.proposal_id == body.proposal_id);
        let Some(existing) = existing else {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "organization proposal not found"})),
            )
                .into_response();
        };
        if let Err(error) = require_permission(
            &organizations,
            &existing.organization_id,
            &actor_public_id,
            &OrganizationPermission::ManageGovernance,
        ) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
        match organizations.finalize_proposal(&body.proposal_id, body.min_votes_for.unwrap_or(2)) {
            Ok(proposal) => {
                if let Err(error) = organizations.persist(&state.organization_registry_state_path) {
                    return internal_error(&error);
                }
                proposal
            }
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        }
    };

    let payload = public_memory_payload(
        &actor_context,
        "organization",
        json!({
            "proposal": proposal.clone(),
            "actor_public_id": actor_public_id,
        }),
    );
    emit_organization_event(
        &state,
        auth,
        "organization.proposal.finalize",
        "organization.proposal.finalized",
        "ORGANIZATION_PROPOSAL_FINALIZED",
        Some(proposal.organization_id.clone()),
        payload,
    );

    Json(json!({"proposal": proposal})).into_response()
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn submit_subnet_charter_application(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<OrganizationCharterApplicationBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let actor_context =
        resolve_identity_context(&state, body.actor_public_id.as_deref(), None).await;
    let Some(actor_public_id) = actor_context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "acting public identity required"})),
        )
            .into_response();
    };
    let sponsor_controller_id = actor_context.public_memory_owner.controller.clone();
    let (organization_id, organization) = {
        let organizations = state.organization_registry.lock().await;
        let proposal = organizations
            .list_proposals(None)
            .into_iter()
            .find(|proposal| proposal.proposal_id == body.proposal_id);
        let Some(proposal) = proposal else {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "organization proposal not found"})),
            )
                .into_response();
        };
        if let Err(error) = require_permission(
            &organizations,
            &proposal.organization_id,
            &actor_public_id,
            &OrganizationPermission::ManageGovernance,
        ) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
        let Some(organization) = organizations.organization(&proposal.organization_id) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "organization does not exist"})),
            )
                .into_response();
        };
        (proposal.organization_id.clone(), organization)
    };
    let active_member_count = {
        let organizations = state.organization_registry.lock().await;
        organizations
            .memberships(&organization_id)
            .into_iter()
            .filter(|member| member.active)
            .count()
    };
    let (open_mission_count, settled_mission_count) = {
        let missions = state.mission_board.lock().await;
        (
            organization_missions(&missions, &organization_id, Some(&MissionStatus::Open)).count(),
            organization_missions(&missions, &organization_id, Some(&MissionStatus::Settled))
                .count(),
        )
    };
    let home_subnet_governed = {
        let governance = state.governance_engine.lock().await;
        organization
            .home_subnet_id
            .as_deref()
            .is_some_and(|subnet_id| governance.planet(subnet_id).is_some())
    };
    let application = {
        let mut organizations = state.organization_registry.lock().await;
        let readiness = compute_autonomy_track(
            &organization,
            active_member_count,
            open_mission_count,
            settled_mission_count,
            home_subnet_governed,
        );
        match organizations.create_subnet_charter_application(
            &body.proposal_id,
            &actor_public_id,
            &sponsor_controller_id,
            readiness,
        ) {
            Ok(application) => {
                if let Err(error) = organizations.persist(&state.organization_registry_state_path) {
                    return internal_error(&error);
                }
                application
            }
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        }
    };

    let payload = public_memory_payload(
        &actor_context,
        "organization",
        json!({
            "charter_application": application.clone(),
            "actor_public_id": actor_public_id,
        }),
    );
    emit_organization_event(
        &state,
        auth,
        "organization.charter.submit",
        "organization.charter.submitted",
        "ORGANIZATION_SUBNET_CHARTER_SUBMITTED",
        Some(application.organization_id.clone()),
        payload,
    );

    (
        StatusCode::CREATED,
        Json(json!({"charter_application": application})),
    )
        .into_response()
}

async fn mutate_organization_treasury(
    state: ControlPlaneState,
    headers: HeaderMap,
    body: OrganizationTreasuryBody,
    fund: bool,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let actor_context =
        resolve_identity_context(&state, body.actor_public_id.as_deref(), None).await;
    let Some(actor_public_id) = actor_context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "acting public identity required"})),
        )
            .into_response();
    };
    let organization = {
        let mut organizations = state.organization_registry.lock().await;
        if let Err(error) =
            require_founder_or_officer(&organizations, &body.organization_id, &actor_public_id)
        {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
        let result = if fund {
            organizations.fund_treasury(&body.organization_id, body.amount_watt)
        } else {
            organizations.spend_treasury(&body.organization_id, body.amount_watt)
        };
        match result {
            Ok(organization) => {
                if let Err(error) = organizations.persist(&state.organization_registry_state_path) {
                    return internal_error(&error);
                }
                organization
            }
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        }
    };

    let action = if fund {
        "organization.treasury.fund"
    } else {
        "organization.treasury.spend"
    };
    let stream_kind = if fund {
        "organization.treasury.funded"
    } else {
        "organization.treasury.spent"
    };
    let event_type = if fund {
        "ORGANIZATION_TREASURY_FUNDED"
    } else {
        "ORGANIZATION_TREASURY_SPENT"
    };
    let payload = public_memory_payload(
        &actor_context,
        "organization",
        json!({
            "organization": organization.clone(),
            "amount_watt": body.amount_watt,
            "reason": body.reason,
            "actor_public_id": actor_public_id,
        }),
    );

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "organization".to_string(),
        action: action.to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(organization.organization_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });
    let _ = state.stream_tx.send(StreamEvent {
        kind: stream_kind.to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state
        .event_log
        .append_signed(event_type, payload, &state.identity);

    Json(json!({
        "organization": organization,
    }))
    .into_response()
}

fn emit_organization_event(
    state: &ControlPlaneState,
    auth: String,
    action: &str,
    stream_kind: &str,
    event_type: &str,
    subject: Option<String>,
    payload: serde_json::Value,
) {
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "organization".to_string(),
        action: action.to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });
    let _ = state.stream_tx.send(StreamEvent {
        kind: stream_kind.to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state
        .event_log
        .append_signed(event_type, payload, &state.identity);
}

pub(crate) fn require_founder_or_officer(
    organizations: &OrganizationRegistry,
    organization_id: &str,
    public_id: &str,
) -> anyhow::Result<()> {
    require_permission(
        organizations,
        organization_id,
        public_id,
        &OrganizationPermission::ManageTreasury,
    )
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn require_permission(
    organizations: &OrganizationRegistry,
    organization_id: &str,
    public_id: &str,
    permission: &OrganizationPermission,
) -> anyhow::Result<()> {
    if organizations.has_permission(organization_id, public_id, permission) {
        return Ok(());
    }
    let member = organizations
        .membership_for_public(organization_id, public_id)
        .ok_or_else(|| anyhow::anyhow!("organization membership required"))?;
    bail!(
        "{} role does not grant {:?}",
        match member.role {
            OrganizationRole::Founder => "founder",
            OrganizationRole::Officer => "officer",
            OrganizationRole::Member => "member",
        },
        permission
    )
}
