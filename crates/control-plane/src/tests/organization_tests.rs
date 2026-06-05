use super::*;

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn organization_endpoints_and_views_work() {
    let (_dir, app, token, _, _state) = build_test_app(80);
    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();

    let captain = bootstrap_broker_identity(app.clone(), &token, agent_did).await;
    let echo_identity = Identity::new_random();
    let echo_did = echo_identity.agent_did.clone();
    let echo = scoped_id("quartermaster-echo", &echo_did);
    let _ = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/bootstrap-identity",
        json!({
            "public_id": echo,
            "display_name": "Quartermaster Echo",
            "agent_did": echo_did,
            "faction": "freeport",
            "role": "operator",
            "strategy": "balanced",
            "home_subnet_id": "planet-test",
            "home_zone_id": "frontier-belt",
            "controller_kind": "external_runtime",
            "controller_ref": "external-echo",
            "controller_node_id": echo_did,
            "ownership_scope": "external"
        }),
    )
    .await;
    let voss_identity = Identity::new_random();
    let voss_did = voss_identity.agent_did.clone();
    let voss = scoped_id("scout-voss", &voss_did);
    let _ = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/bootstrap-identity",
        json!({
            "public_id": voss,
            "display_name": "Scout Voss",
            "agent_did": voss_did,
            "faction": "freeport",
            "role": "enforcer",
            "strategy": "balanced",
            "home_subnet_id": "planet-test",
            "home_zone_id": "frontier-belt",
            "controller_kind": "external_runtime",
            "controller_ref": "external-voss",
            "controller_node_id": voss_did,
            "ownership_scope": "external"
        }),
    )
    .await;
    let created_org = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations",
        json!({
            "public_id": captain,
            "organization_id": "aurora-consortium",
            "name": "Aurora Consortium",
            "kind": "consortium",
            "summary": "Coordinates frontier logistics and trade corridors.",
            "faction_alignment": "freeport",
            "home_subnet_id": "planet-test",
            "home_zone_id": "frontier-belt"
        }),
    )
    .await;
    assert_eq!(
        created_org["organization"]["organization_id"].as_str(),
        Some("aurora-consortium")
    );

    let member_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/members",
        json!({
            "organization_id": "aurora-consortium",
            "actor_public_id": captain,
            "public_id": echo,
            "role": "officer",
            "title": "Quartermaster"
        }),
    )
    .await;
    assert_eq!(
        member_json["membership"]["public_id"].as_str(),
        Some(echo.as_str())
    );

    let funded_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/treasury/fund",
        json!({
            "organization_id": "aurora-consortium",
            "actor_public_id": captain,
            "amount_watt": 60,
            "reason": "seed frontier treasury"
        }),
    )
    .await;
    assert_eq!(
        funded_json["organization"]["treasury_watt"].as_i64(),
        Some(60)
    );

    let spent_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/treasury/spend",
        json!({
            "organization_id": "aurora-consortium",
            "actor_public_id": captain,
            "amount_watt": 15,
            "reason": "fund escort contract"
        }),
    )
    .await;
    assert_eq!(
        spent_json["organization"]["treasury_watt"].as_i64(),
        Some(45)
    );

    let forbidden_member_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/members",
        json!({
            "organization_id": "aurora-consortium",
            "actor_public_id": echo,
            "public_id": voss,
            "role": "member",
            "title": "Scout"
        }),
    )
    .await;
    assert_eq!(
        forbidden_member_json["error"].as_str(),
        Some("officer role does not grant ManageMembers")
    );

    let scout_member_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/members",
        json!({
            "organization_id": "aurora-consortium",
            "actor_public_id": captain,
            "public_id": voss,
            "role": "member",
            "title": "Scout"
        }),
    )
    .await;
    assert_eq!(
        scout_member_json["membership"]["public_id"].as_str(),
        Some(voss.as_str())
    );

    let published_mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/missions",
        json!({
            "organization_id": "aurora-consortium",
            "actor_public_id": echo,
            "title": "Staff the frontier exchange",
            "description": "Coordinate organization members around the exchange lane.",
            "domain": "trade",
            "subnet_id": "planet-test",
            "zone_id": "frontier-belt",
            "required_role": "broker",
            "required_faction": "freeport",
            "treasury_commit_watt": 5,
            "reward": {
                "agent_watt": 30,
                "reputation": 3,
                "capacity": 2,
                "treasury_share_watt": 4
            },
            "payload": {
                "organization_id": "aurora-consortium"
            }
        }),
    )
    .await;
    assert_eq!(
        published_mission["mission"]["publisher_kind"].as_str(),
        Some("organization")
    );
    assert_eq!(
        published_mission["organization"]["treasury_watt"].as_i64(),
        Some(40)
    );
    complete_and_settle_mission(
        app.clone(),
        &token,
        &published_mission["mission"]["mission_id"],
        agent_did,
    )
    .await;

    let second_org_mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/missions",
        json!({
            "organization_id": "aurora-consortium",
            "actor_public_id": captain,
            "title": "Audit the frontier exchange",
            "description": "Verify the route books before expansion.",
            "domain": "power",
            "subnet_id": "planet-test",
            "zone_id": "frontier-belt",
            "required_role": "broker",
            "required_faction": "freeport",
            "treasury_commit_watt": 0,
            "reward": {
                "agent_watt": 20,
                "reputation": 2,
                "capacity": 1,
                "treasury_share_watt": 3
            },
            "payload": {
                "organization_id": "aurora-consortium"
            }
        }),
    )
    .await;
    complete_and_settle_mission(
        app.clone(),
        &token,
        &second_org_mission["mission"]["mission_id"],
        agent_did,
    )
    .await;

    let proposal_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/proposals",
        json!({
            "organization_id": "aurora-consortium",
            "actor_public_id": captain,
            "kind": "subnet_charter",
            "title": "Charter Aurora Reach",
            "summary": "Request a dedicated subnet for consortium traffic and governance.",
            "proposed_subnet_id": "planet-aurora",
            "proposed_subnet_name": "Aurora Reach"
        }),
    )
    .await;
    let proposal_id = proposal_json["proposal"]["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();

    let founder_vote = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/proposals/vote",
        json!({
            "proposal_id": proposal_id.clone(),
            "actor_public_id": captain,
            "approve": true
        }),
    )
    .await;
    assert_eq!(
        founder_vote["proposal"]["votes_for"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let scout_vote = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/proposals/vote",
        json!({
            "proposal_id": proposal_id.clone(),
            "actor_public_id": voss,
            "approve": true
        }),
    )
    .await;
    assert_eq!(
        scout_vote["proposal"]["votes_for"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let finalized_proposal = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/proposals/finalize",
        json!({
            "proposal_id": proposal_id.clone(),
            "actor_public_id": echo,
            "min_votes_for": 2
        }),
    )
    .await;
    assert_eq!(
        finalized_proposal["proposal"]["status"].as_str(),
        Some("accepted")
    );

    let charter_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/charters",
        json!({
            "proposal_id": proposal_json["proposal"]["proposal_id"],
            "actor_public_id": captain
        }),
    )
    .await;
    assert_eq!(
        charter_json["charter_application"]["status"].as_str(),
        Some("pending_governance")
    );
    assert_eq!(
        charter_json["charter_application"]["sponsor_controller_id"].as_str(),
        Some(agent_did)
    );

    let organizations_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/civilization/organizations?public_id={captain}"),
    )
    .await;
    assert_eq!(
        organizations_json["organizations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        organizations_json["organizations"][0]["active_member_count"].as_u64(),
        Some(3)
    );
    assert_eq!(
        organizations_json["organizations"][0]["organization"]["treasury_watt"].as_i64(),
        Some(40)
    );
    assert_eq!(
        organizations_json["organizations"][0]["open_mission_count"].as_u64(),
        Some(0)
    );
    assert_eq!(
        organizations_json["organizations"][0]["settled_mission_count"].as_u64(),
        Some(2)
    );
    assert_eq!(
        organizations_json["organizations"][0]["subnet_readiness"].as_str(),
        Some("subnet-ready")
    );
    assert_eq!(
        organizations_json["organizations"][0]["permissions"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        organizations_json["organizations"][0]["autonomy_track"]["current_status"].as_str(),
        Some("subnet-ready")
    );
    assert_eq!(
        organizations_json["organizations"][0]["autonomy_track"]["eligible_for_subnet_charter"]
            .as_bool(),
        Some(true)
    );
    assert!(
        organizations_json["organizations"][0]["autonomy_track"]["gates"]
            .as_array()
            .unwrap()
            .len()
            >= 5
    );
    assert!(
        organizations_json["organizations"][0]["autonomy_track"]["next_action"]
            .as_str()
            .is_some()
    );
    assert_eq!(
        organizations_json["organizations"][0]["governance_summary"]["accepted_proposals_count"]
            .as_u64(),
        Some(1)
    );
    assert_eq!(
        organizations_json["organizations"][0]["governance_summary"]["charter_application_count"]
            .as_u64(),
        Some(1)
    );
    assert_eq!(
        organizations_json["organizations"][0]["governance_summary"]["latest_charter_application"]
            ["proposed_subnet_id"]
            .as_str(),
        Some("planet-aurora")
    );

    let governance_json = authed_get_json(
            app.clone(),
            &token,
            &format!("/v1/civilization/organizations/proposals?organization_id=aurora-consortium&public_id={captain}"),
        )
        .await;
    assert_eq!(governance_json["proposals"].as_array().unwrap().len(), 1);
    assert_eq!(
        governance_json["charter_applications"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let my_organizations_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/organizations/my?public_id={captain}"),
    )
    .await;
    assert_eq!(
        my_organizations_json["organizations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let officer_orgs_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/organizations/my?public_id={echo}"),
    )
    .await;
    assert_eq!(
        officer_orgs_json["organizations"][0]["permissions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(
        officer_orgs_json["organizations"][0]["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|permission| permission.as_str() != Some("manage_members"))
    );
    assert!(
        officer_orgs_json["organizations"][0]["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|permission| permission.as_str() == Some("manage_governance"))
    );

    let governance_my_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/governance/my?public_id={captain}"),
    )
    .await;
    assert_eq!(
        governance_my_json["organizations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        governance_my_json["charter_applications"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let supervision_home_json = authed_get_json(
        app,
        &token,
        &format!("/v1/supervision/home?public_id={captain}"),
    )
    .await;
    assert_eq!(
        supervision_home_json["organizations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        supervision_home_json["game"]["organizations"][0]["organization"]["organization_id"]
            .as_str(),
        Some("aurora-consortium")
    );
    assert!(
            supervision_home_json["game"]["organizations"][0]["autonomy_track"]["eligible_for_subnet_charter"]
                .as_bool()
                == Some(true)
        );
}

#[tokio::test]
async fn supervision_console_page_serves_canonical_surface() {
    let (_dir, app, _token, _, _state) = build_test_app(20);
    let (status, body) = request_text(
        app,
        axum::http::Request::builder()
            .uri("/supervision")
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Wattetheria Node Console"));
    assert!(body.contains("/v1/civilization/identities"));
    assert!(body.contains("/v1/client/export"));
    assert!(body.contains("/v1/client/friend-requests"));
    assert!(body.contains("/v1/client/friends/messages"));
    assert!(!body.contains("/v1/wattetheria/client/export"));
    assert!(!body.contains("/v1/wattetheria/social/friend-requests"));
    assert!(!body.contains("/v1/wattetheria/social/agent-dm/messages"));
    assert!(body.contains("node_limit"));
    assert!(body.contains("Nodes"));
    assert!(!body.contains("id=\"nodes-list\""));
    assert!(!body.contains("renderNodes(payload)"));
    assert!(body.contains("identityRecordPublicIdentity"));
    assert!(body.contains("isAgentIdentityRecord"));
    assert!(body.contains("identityRecordControllerBinding"));
    assert!(body.contains("identityProtectionBadges"));
    assert!(body.contains("Self-certifying public_id"));
    assert!(body.contains("Controller Binding"));
    assert!(body.contains("Public Identity"));
    assert!(body.contains("identity-detail-grid"));
    assert!(body.contains("selectPreferredIdentity"));
    assert!(body.contains("identitiesByPublicId.has(savedPublicId)"));
    assert!(body.contains("firstPublicId"));
    assert!(
        body.contains("publicId.startsWith(&quot;agent-&quot;)")
            || body.contains("publicId.startsWith(\"agent-\")")
    );
    assert!(body.contains("record?.identity"));
    assert!(body.contains("Friend Requests"));
    assert!(body.contains("DM Messages"));
    assert!(body.contains("Expires"));
    assert!(body.contains("id=\"overview-nearby\""));
    assert!(body.contains("Overview nearby agents"));
    assert!(body.contains("overview-nearby-count"));
    assert!(body.contains("safeArray(payload.nodes).concat(safeArray(payload.peers))"));
    assert!(body.contains("kind: \"node\""));
    assert!(body.contains("diagnosticContextSummary"));
    assert!(body.contains("diagnosticNodeId"));
    assert!(body.contains("network connection established"));
    assert!(body.contains("remote_addr"));
    assert!(body.contains("WATT Balance"));
    assert!(body.contains("Wallet Identity"));
    assert!(body.contains("Payment Accounts"));
    assert!(body.contains("DID Payment Binding"));
    assert!(body.contains("wallet_identities"));
    assert!(body.contains("payment_account_binding"));
    assert!(body.contains("walletActiveIdentity"));
    assert!(body.contains("proof ready"));
    assert!(body.contains("Agent Payment Account"));
    assert!(body.contains("Create Agent Wallet"));
    assert!(body.contains("id=\"web3-wallet-network\""));
    assert!(body.contains("value: \"base\""));
    assert!(body.contains("value: \"base-sepolia\""));
    assert!(body.contains("0x036CbD53842c5426634e7929541eC2318f3dCF7e"));
    assert!(body.contains("networkChainId"));
    assert!(body.contains("stablecoinRpcUrls"));
    assert!(body.contains("https://sepolia.base.org"));
    assert!(body.contains("rpcCall("));
    assert!(body.contains("stablecoinTokensFor(selectedNetwork)"));
    assert!(body.contains("/v1/wallet/payment-account/create"));
    assert!(body.contains("Web2 Payments"));
    assert!(body.contains("Agent Runtime"));
    assert!(!body.contains("Deployment env file"));
    assert!(!body.contains("/var/lib/wattetheria/deploy/.env"));
    assert!(body.contains("Wattetheria Node Logs"));
    assert!(body.contains("/v1/client/diagnostics"));
    assert!(body.contains("/v1/client/wattswarm-diagnostics"));
    assert!(body.contains("diagnostic-search"));
    assert!(body.contains("data-log-mode=\"wattswarm\""));
    assert!(body.contains("exportDiagnostics"));
    assert!(body.contains("Open Swarm Console"));
    assert!(body.contains("id=\"open-swarm-console\""));
    assert!(body.contains(":7788"));
    assert!(body.contains("box-shadow: var(--shadow-sm);"));
    assert!(body.contains("bootstrapControlToken"));
    assert!(body.contains("Auto-loaded for this local node"));
    assert!(body.contains("normalizeToken"));
    assert!(body.contains("wallet_bound_agent_did"));
    assert!(body.contains("public_topic_messages"));
}
