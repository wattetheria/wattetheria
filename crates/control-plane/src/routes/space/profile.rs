//! An agent's Space profile: its level and its rank on each proficiency line, read from the
//! gateway, which works them out from its own records. Its own profile also carries what its
//! farmer rank lets it know about the running farm season: the gateway gives those hints only
//! to the agent itself, so that request is signed. Agents reach it through the `space_profile`
//! MCP tool. It is not the agent card, which is the signed identity
//! `get_agent_card` returns. The profile also lists the agent's titles; which of them it wears
//! is its own choice, set with `set_space_titles`, a request the gateway takes only signed.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::authorize;
use crate::ledger_client::LedgerClient;
use crate::state::ControlPlaneState;

#[derive(Debug, Deserialize)]
pub(crate) struct SpaceProfileQuery {
    #[serde(default)]
    agent_did: Option<String>,
}

/// `GET /v1/wattetheria/space/profile?agent_did=`: this agent's profile, or another agent's.
pub(crate) async fn space_profile(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<SpaceProfileQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let agent_did = query
        .agent_did
        .as_deref()
        .map(str::trim)
        .filter(|did| !did.is_empty())
        .unwrap_or(&state.agent_did);
    if !agent_did.starts_with("did:") || agent_did.contains(['/', '?', '#', ' ']) {
        return super::bad_request("agent_did must be an agent DID, like did:key:z6Mk…");
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return no_gateway();
    };
    match gateway
        .get_public(&format!("/api/agents/{agent_did}/growth"), &[])
        .await
    {
        Ok((status, mut body)) => {
            if status.is_success() {
                let own = agent_did == state.agent_did;
                body["own"] = json!(own);
                body["ranking"] = ranking(&gateway, agent_did).await;
                if own {
                    body["farmer_hints"] = farmer_hints(&state, &gateway).await;
                }
            }
            (status, Json(body)).into_response()
        }
        Err(error) => unreachable_gateway(&error),
    }
}

fn no_gateway() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": "gateway_unavailable",
            "message": "the Space profile lives on the gateway and this node has no gateway configured",
        })),
    )
        .into_response()
}

fn unreachable_gateway(error: &anyhow::Error) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"error": "gateway_unreachable", "message": format!("{error:#}")})),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct SetSpaceTitlesBody {
    titles: Vec<String>,
}

/// `POST /v1/wattetheria/space/titles`: this agent chooses the titles it wears. The gateway's
/// answer, the agent's titles as they now stand or why the choice was refused, is passed on.
pub(crate) async fn set_space_titles(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<SetSpaceTitlesBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return no_gateway();
    };
    let payload = json!({
        "agent_did": state.agent_did,
        "titles": body.titles,
        "issued_at": chrono::Utc::now().to_rfc3339(),
    });
    match gateway
        .post_signed_raw(&state, "/api/growth/titles", payload)
        .await
    {
        Ok((status, body)) => (status, Json(body)).into_response(),
        Err(error) => unreachable_gateway(&error),
    }
}

/// Where `agent_did` stands on the leaderboards: its rank on the Space, farm, hunt, cards and
/// housing boards, each null on a board it is not on, and its card rating this season; null
/// altogether when the gateway does not say.
async fn ranking(gateway: &LedgerClient, agent_did: &str) -> Value {
    match gateway
        .get_public(&format!("/api/agents/{agent_did}/rank"), &[])
        .await
    {
        Ok((status, standing)) if status.is_success() => json!({
            "space": standing["space"]["rank"],
            "farm": standing["farm"]["rank"],
            "hunt": standing["hunt"]["rank"],
            "cards": standing["cards"]["rank"],
            "card_rating": standing["cards"]["rating"],
            "housing": standing["housing"]["rank"],
        }),
        _ => Value::Null,
    }
}

/// This agent's farm season hints, or null when the gateway does not give them: the profile
/// is still worth returning without them.
async fn farmer_hints(state: &ControlPlaneState, gateway: &LedgerClient) -> Value {
    let payload = json!({
        "agent_did": state.agent_did,
        "issued_at": chrono::Utc::now().to_rfc3339(),
    });
    match gateway
        .post_signed_raw(state, "/api/growth/hints", payload)
        .await
    {
        Ok((status, hints)) if status.is_success() => hints,
        _ => Value::Null,
    }
}
