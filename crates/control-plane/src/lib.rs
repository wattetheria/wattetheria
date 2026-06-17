mod agent_attach;
mod auth;
mod autonomy;
mod diagnostics;
mod gateway_dispatch;
mod reliability_maintenance;
pub mod social_host;
mod swarm_sync;
pub mod routes {
    pub(crate) mod agent_events;
    pub(crate) mod civilization;
    pub(crate) mod client;
    pub(crate) mod client_api;
    pub(crate) mod client_swarm;
    pub(crate) mod console;
    pub(crate) mod core;
    pub(crate) mod diagnostics;
    pub(crate) mod game;
    pub(crate) mod governance;
    pub(crate) mod identity;
    pub(crate) mod mailbox;
    pub(crate) mod map;
    pub(crate) mod mcp;
    pub(crate) mod missions;
    pub(crate) mod network;
    pub(crate) mod organizations;
    pub(crate) mod payment_chain;
    pub(crate) mod payments;
    pub(crate) mod policy;
    pub(crate) mod reward_events;
    pub(crate) mod reward_view;
    pub(crate) mod runtime_config;
    pub(crate) mod servicenet;
    pub(crate) mod servicenet_publish;
    pub(crate) mod servicenet_published;
    pub(crate) mod settlement_delegation;
    pub(crate) mod supervision;
    pub(crate) mod topics;
}
mod state;

use anyhow::{Context, Result};
use axum::Router;
use axum::routing::{get, post, put};
use std::net::SocketAddr;

pub use autonomy::run_autonomy_tick_once;
pub use gateway_dispatch::{
    SignedGatewayNodeEvent, build_signed_node_event, push_signed_node_event, push_signed_snapshot,
};
pub use reliability_maintenance::{
    RELIABILITY_MAINTENANCE_BATCH_LIMIT, RELIABILITY_MAINTENANCE_INTERVAL_SEC,
    run_reliability_maintenance_tick_once, spawn_reliability_maintenance_task,
};
pub use routes::client_api::{
    SignedPublicClientSnapshot, build_signed_public_client_snapshot,
    push_signed_public_client_snapshot,
};
pub use state::{
    ClientExportQuery, ControlPlaneState, GatewayEventSequence, GeoSource, NodeGeoLocation,
    RateLimiter, StreamEvent,
};
pub use swarm_sync::{DEFAULT_WATTSWARM_SYNC_GRPC_PORT, spawn_wattswarm_sync_bridge};

pub fn app(state: ControlPlaneState) -> Router {
    Router::new()
        .merge(console_router())
        .merge(core_router())
        .merge(client_router())
        .merge(mcp_router())
        .merge(client_facing_router())
        .merge(network_router())
        .merge(game_router())
        .merge(map_router())
        .merge(civilization_router())
        .merge(governance_router())
        .merge(mailbox_router())
        .merge(payments_router())
        .merge(policy_router())
        .merge(servicenet_router())
        .with_state(state)
}

fn mcp_router() -> Router<ControlPlaneState> {
    Router::new().route("/mcp", post(routes::mcp::mcp))
}

fn network_router() -> Router<ControlPlaneState> {
    Router::new()
        .route("/v1/network/status", get(routes::network::network_status))
        .route("/v1/network/peers", get(routes::network::network_peers))
}

fn console_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/favicon.png",
            get(routes::console::supervision_favicon_png),
        )
        .route("/supervision", get(routes::console::supervision_console))
        .route(
            "/supervision/console",
            get(routes::console::supervision_console),
        )
}

fn client_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/civilization/identities",
            get(routes::client::public_identities),
        )
        .route(
            "/v1/supervision/identities",
            get(routes::client::public_identities),
        )
        .route(
            "/v1/supervision/home",
            get(routes::client::supervision_home),
        )
        .route("/v1/supervision/missions", get(routes::client::my_missions))
        .route(
            "/v1/wattetheria/missions/my",
            get(routes::client::my_missions),
        )
        .route(
            "/v1/supervision/governance",
            get(routes::client::my_governance),
        )
        .route("/v1/governance/my", get(routes::client::my_governance))
        .route(
            "/v1/catalog/bootstrap",
            get(routes::client::bootstrap_catalog),
        )
        .route(
            "/v1/organizations/my",
            get(routes::organizations::my_organizations),
        )
}

fn client_facing_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/client/network/status",
            get(routes::client_api::client_network_status),
        )
        .route("/v1/client/peers", get(routes::client_api::client_peers))
        .route("/v1/client/self", get(routes::client_api::client_self))
        .route(
            "/v1/client/rpc-logs",
            get(routes::client_api::client_rpc_logs),
        )
        .route(
            "/v1/client/diagnostics",
            get(routes::diagnostics::client_diagnostics),
        )
        .route(
            "/v1/client/wattswarm-diagnostics",
            get(routes::diagnostics::client_wattswarm_diagnostics),
        )
        .route("/v1/client/tasks", get(routes::client_api::client_tasks))
        .route(
            "/v1/wattetheria/client/task-activity",
            get(routes::client_swarm::client_task_activity),
        )
        .route(
            "/v1/wattetheria/social/nearby",
            get(routes::client_api::list_nearby),
        )
        .route(
            "/v1/wattetheria/social/friend-requests",
            get(routes::civilization::list_friend_requests),
        )
        .route(
            "/v1/client/friend-requests",
            get(routes::civilization::list_friend_requests),
        )
        .route(
            "/v1/wattetheria/social/sent-friend-requests",
            get(routes::civilization::list_sent_friend_requests),
        )
        .route(
            "/v1/wattetheria/social/friend-requests/{request_id}",
            get(routes::civilization::get_friend_request),
        )
        .route(
            "/v1/wattetheria/social/friend-requests/{request_id}/accept",
            post(routes::civilization::accept_friend_request),
        )
        .route(
            "/v1/wattetheria/social/friend-requests/{request_id}/reject",
            post(routes::civilization::reject_friend_request),
        )
        .route(
            "/v1/client/organizations",
            get(routes::client_api::client_organizations),
        )
        .route("/v1/client/hives", get(routes::client_swarm::client_hives))
        .route(
            "/v1/client/hives/messages",
            get(routes::client_swarm::client_topic_messages),
        )
        .route(
            "/v1/client/conversations",
            get(routes::client_swarm::client_conversations),
        )
        .route(
            "/v1/client/conversations/messages",
            get(routes::civilization::list_agent_dm_messages),
        )
        .route(
            "/v1/client/friends",
            get(routes::client_swarm::client_friends),
        )
        .route(
            "/v1/client/friends/messages",
            get(routes::civilization::list_agent_dm_messages),
        )
        .route(
            "/v1/client/leaderboard",
            get(routes::client_api::client_leaderboard),
        )
        .route(
            "/v1/wattetheria/client/export",
            get(routes::client_api::client_export),
        )
        .route("/v1/client/export", get(routes::client_api::client_export))
}

fn game_router() -> Router<ControlPlaneState> {
    Router::new()
        .route("/v1/game/catalog", get(routes::game::game_catalog))
        .route("/v1/supervision/status", get(routes::game::game_status))
        .route("/v1/game/status", get(routes::game::game_status))
        .route(
            "/v1/supervision/bootstrap",
            get(routes::game::game_bootstrap),
        )
        .route("/v1/game/bootstrap", get(routes::game::game_bootstrap))
        .route(
            "/v1/game/mission-pack",
            get(routes::game::game_mission_pack),
        )
        .route(
            "/v1/game/starter-missions",
            get(routes::game::game_starter_missions),
        )
        .route(
            "/v1/game/starter-missions/bootstrap",
            post(routes::game::bootstrap_starter_missions_route),
        )
        .route(
            "/v1/game/mission-pack/bootstrap",
            post(routes::game::bootstrap_mission_pack_route),
        )
}

fn core_router() -> Router<ControlPlaneState> {
    Router::new()
        .route("/agent-events", post(routes::agent_events::callback))
        .route("/v1/health", get(routes::core::health))
        .route("/v1/state", get(routes::core::state_view))
        .route("/v1/events", get(routes::core::events))
        .route("/v1/events/export", get(routes::core::events_export))
        .route("/v1/night-shift", get(routes::core::night_shift))
        .route(
            "/v1/night-shift/summary",
            get(routes::core::night_shift_summary),
        )
        .route(
            "/v1/night-shift/narrative",
            get(routes::core::night_shift_narrative),
        )
        .route("/v1/actions", post(routes::core::actions))
        .route(
            "/v1/agent-actions/commit",
            post(routes::core::agent_action_commit),
        )
        .route("/v1/brain/doctor", post(routes::core::brain_doctor))
        .route(
            "/v1/brain/config",
            get(routes::runtime_config::brain_config_get),
        )
        .route(
            "/v1/brain/config",
            put(routes::runtime_config::brain_config_put),
        )
        .route(
            "/v1/brain/propose-actions",
            get(routes::core::brain_propose_actions),
        )
        .route(
            "/v1/agent/attach/status",
            get(routes::core::agent_attach_status),
        )
        .route("/v1/autonomy/tick", post(routes::core::autonomy_tick))
        .route("/v1/audit", get(routes::core::audit_recent))
        .route("/v1/stream", get(routes::core::stream))
}

fn servicenet_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/wattetheria/servicenet/agents",
            get(routes::servicenet::list_agents),
        )
        .route(
            "/v1/wattetheria/servicenet/agent-card-template",
            get(routes::servicenet_publish::agent_card_template),
        )
        .route(
            "/v1/wattetheria/servicenet/publish",
            post(routes::servicenet_publish::publish_agent),
        )
        .route(
            "/v1/wattetheria/servicenet/published-agents",
            get(routes::servicenet_published::published_agents),
        )
        .route(
            "/v1/wattetheria/servicenet/agents/{agent_id}",
            get(routes::servicenet::get_agent),
        )
        .route(
            "/v1/wattetheria/servicenet/agents/{agent_id}/unpublish",
            post(routes::servicenet_publish::unpublish_agent),
        )
        .route(
            "/v1/wattetheria/servicenet/agents/{agent_id}/invoke",
            post(routes::servicenet::invoke_agent),
        )
        .route(
            "/v1/wattetheria/servicenet/agents/{agent_id}/invoke-async",
            post(routes::servicenet::invoke_agent_async),
        )
        .route(
            "/v1/wattetheria/servicenet/agents/{agent_id}/tasks/{task_id}/get",
            post(routes::servicenet::get_agent_task),
        )
        .route(
            "/v1/wattetheria/servicenet/receipts/{receipt_id}",
            get(routes::servicenet::get_receipt),
        )
}

fn map_router() -> Router<ControlPlaneState> {
    Router::new()
        .route("/v1/galaxy/map", get(routes::map::galaxy_map))
        .route("/v1/galaxy/maps", get(routes::map::galaxy_maps))
        .route(
            "/v1/galaxy/travel/state",
            get(routes::map::galaxy_travel_state),
        )
        .route(
            "/v1/galaxy/travel/options",
            get(routes::map::galaxy_travel_options),
        )
        .route(
            "/v1/galaxy/travel/plan",
            get(routes::map::galaxy_travel_plan),
        )
        .route(
            "/v1/galaxy/travel/depart",
            post(routes::map::galaxy_travel_depart),
        )
        .route(
            "/v1/galaxy/travel/arrive",
            post(routes::map::galaxy_travel_arrive),
        )
}

fn civilization_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/civilization/bootstrap-identity",
            post(routes::civilization::bootstrap_identity),
        )
        .route(
            "/v1/civilization/public-identity",
            get(routes::civilization::public_identity)
                .patch(routes::civilization::public_identity_display_name_patch)
                .post(routes::civilization::public_identity_upsert),
        )
        .route(
            "/v1/civilization/controller-binding",
            get(routes::civilization::controller_binding)
                .post(routes::civilization::controller_binding_upsert),
        )
        .route(
            "/v1/civilization/profile",
            get(routes::civilization::citizen_profile)
                .post(routes::civilization::citizen_profile_upsert),
        )
        .route(
            "/v1/wattetheria/social/friends",
            get(routes::civilization::list_relationships)
                .post(routes::civilization::upsert_relationship),
        )
        .route(
            "/v1/wattetheria/social/agent-friends",
            get(routes::civilization::list_agent_relationships)
                .post(routes::civilization::agent_relationship_action),
        )
        .route(
            "/v1/wattetheria/social/agent-dm/threads",
            get(routes::civilization::list_agent_dm_threads),
        )
        .route(
            "/v1/wattetheria/social/agent-dm/messages",
            get(routes::civilization::list_agent_dm_messages)
                .post(routes::civilization::send_agent_dm_message),
        )
        .merge(organization_civilization_router())
        .merge(hive_wattetheria_router())
        .route(
            "/v1/civilization/metrics",
            get(routes::civilization::civilization_metrics),
        )
        .route(
            "/v1/civilization/emergencies",
            get(routes::civilization::civilization_emergencies),
        )
        .route(
            "/v1/civilization/briefing",
            get(routes::civilization::civilization_briefing),
        )
        .route(
            "/v1/supervision/briefing",
            get(routes::civilization::supervision_briefing),
        )
        .route("/v1/galaxy/zones", get(routes::civilization::galaxy_zones))
        .route("/v1/world/zones", get(routes::civilization::galaxy_zones))
        .route(
            "/v1/galaxy/events",
            get(routes::civilization::galaxy_events)
                .post(routes::civilization::galaxy_event_publish),
        )
        .route(
            "/v1/world/events",
            get(routes::civilization::galaxy_events)
                .post(routes::civilization::galaxy_event_publish),
        )
        .route(
            "/v1/galaxy/events/generate",
            post(routes::civilization::galaxy_event_generate),
        )
        .route(
            "/v1/world/events/generate",
            post(routes::civilization::galaxy_event_generate),
        )
        .route(
            "/v1/wattetheria/missions",
            get(routes::missions::mission_list).post(routes::missions::mission_publish),
        )
        .route(
            "/v1/wattetheria/missions/{mission_id}",
            get(routes::missions::mission_get),
        )
        .route(
            "/v1/wattetheria/missions/{mission_id}/claim",
            post(routes::missions::mission_claim_by_id),
        )
        .route(
            "/v1/wattetheria/missions/{mission_id}/complete",
            post(routes::missions::mission_complete_by_id),
        )
        .route(
            "/v1/wattetheria/missions/{mission_id}/settle",
            post(routes::missions::mission_settle_by_id),
        )
}

fn payments_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/wallet/payment-account/bind-web3",
            post(routes::payments::bind_web3_payment_account),
        )
        .route(
            "/v1/wallet/payment-account/create",
            post(routes::payments::create_payment_account),
        )
        .route(
            "/v1/wattetheria/payments/agent-payments",
            get(routes::payments::list_agent_payments),
        )
        .route(
            "/v1/wattetheria/payments/agent-payments/propose",
            post(routes::payments::propose_agent_payment),
        )
        .route(
            "/v1/wattetheria/payments/agent-payments/{payment_id}",
            get(routes::payments::get_agent_payment),
        )
        .route(
            "/v1/wattetheria/payments/agent-payments/{payment_id}/authorize",
            post(routes::payments::authorize_agent_payment),
        )
        .route(
            "/v1/wattetheria/payments/agent-payments/{payment_id}/submit",
            post(routes::payments::submit_agent_payment),
        )
        .route(
            "/v1/wattetheria/payments/agent-payments/{payment_id}/settle",
            post(routes::payments::settle_agent_payment),
        )
        .route(
            "/v1/wattetheria/payments/agent-payments/{payment_id}/reject",
            post(routes::payments::reject_agent_payment),
        )
        .route(
            "/v1/wattetheria/payments/agent-payments/{payment_id}/cancel",
            post(routes::payments::cancel_agent_payment),
        )
}

fn hive_wattetheria_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/wattetheria/hives",
            get(routes::topics::list_hives).post(routes::topics::create_hive),
        )
        .route(
            "/v1/wattetheria/hives/{hive_id}/messages",
            get(routes::topics::hive_messages).post(routes::topics::post_hive_message),
        )
        .route(
            "/v1/wattetheria/hives/{hive_id}/subscribe",
            post(routes::topics::subscribe_hive),
        )
        .route(
            "/v1/wattetheria/hives/{hive_id}/unsubscribe",
            post(routes::topics::unsubscribe_hive),
        )
        .route(
            "/v1/wattetheria/hives/{hive_id}/invite",
            post(routes::topics::invite_private_hive_participant),
        )
}

fn organization_civilization_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/civilization/organizations",
            get(routes::organizations::list_organizations)
                .post(routes::organizations::create_organization),
        )
        .route(
            "/v1/civilization/organizations/members",
            post(routes::organizations::upsert_organization_member),
        )
        .route(
            "/v1/civilization/organizations/proposals",
            get(routes::organizations::list_organization_governance)
                .post(routes::organizations::create_organization_proposal),
        )
        .route(
            "/v1/civilization/organizations/proposals/vote",
            post(routes::organizations::vote_organization_proposal),
        )
        .route(
            "/v1/civilization/organizations/proposals/finalize",
            post(routes::organizations::finalize_organization_proposal),
        )
        .route(
            "/v1/civilization/organizations/charters",
            post(routes::organizations::submit_subnet_charter_application),
        )
        .route(
            "/v1/civilization/organizations/missions",
            post(routes::organizations::publish_organization_mission),
        )
        .route(
            "/v1/civilization/organizations/treasury/fund",
            post(routes::organizations::fund_organization_treasury),
        )
        .route(
            "/v1/civilization/organizations/treasury/spend",
            post(routes::organizations::spend_organization_treasury),
        )
}

fn governance_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/governance/planets",
            get(routes::governance::governance_planets),
        )
        .route(
            "/v1/governance/proposals",
            get(routes::governance::governance_proposals)
                .post(routes::governance::governance_create_proposal),
        )
        .route(
            "/v1/governance/proposals/vote",
            post(routes::governance::governance_vote_proposal),
        )
        .route(
            "/v1/governance/proposals/finalize",
            post(routes::governance::governance_finalize_proposal),
        )
        .route(
            "/v1/governance/treasury/fund",
            post(routes::governance::governance_fund_treasury),
        )
        .route(
            "/v1/governance/treasury/spend",
            post(routes::governance::governance_spend_treasury),
        )
        .route(
            "/v1/governance/stability",
            post(routes::governance::governance_adjust_stability),
        )
        .route(
            "/v1/governance/recall",
            post(routes::governance::governance_start_recall),
        )
        .route(
            "/v1/governance/recall/resolve",
            post(routes::governance::governance_resolve_recall),
        )
        .route(
            "/v1/governance/custody",
            post(routes::governance::governance_enter_custody),
        )
        .route(
            "/v1/governance/custody/release",
            post(routes::governance::governance_release_custody),
        )
        .route(
            "/v1/governance/takeover",
            post(routes::governance::governance_hostile_takeover),
        )
}

fn mailbox_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/wattetheria/mailbox/messages",
            get(routes::mailbox::mailbox_fetch).post(routes::mailbox::mailbox_send),
        )
        .route(
            "/v1/wattetheria/mailbox/ack",
            post(routes::mailbox::mailbox_ack),
        )
}

fn policy_router() -> Router<ControlPlaneState> {
    Router::new()
        .route("/v1/policy/check", post(routes::policy::policy_check))
        .route("/v1/policy/pending", get(routes::policy::policy_pending))
        .route("/v1/policy/approve", post(routes::policy::policy_approve))
        .route("/v1/policy/revoke", post(routes::policy::policy_revoke))
        .route("/v1/policy/grants", get(routes::policy::policy_grants))
}

pub async fn serve_control_plane(state: ControlPlaneState, bind: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind control plane on {bind}"))?;
    axum::serve(listener, app(state))
        .await
        .context("serve control plane")
}

#[cfg(test)]
mod tests;
