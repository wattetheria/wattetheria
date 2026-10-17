use super::*;

struct FailingOncePermissionBridge {
    attempts: std::sync::atomic::AtomicUsize,
}

struct SpaceRegistrationBridge {
    anchor: NetworkCredentialTrustAnchor,
    updates: Mutex<Vec<wattetheria_kernel::network_agent_registration::NetworkPermissionUpdate>>,
}

#[async_trait::async_trait]
impl SwarmBridge for SpaceRegistrationBridge {
    async fn agent_view(&self, agent_did: &str) -> anyhow::Result<SwarmAgentView> {
        Ok(SwarmAgentView {
            agent_did: agent_did.to_owned(),
            stats: AgentStats::default(),
        })
    }
    async fn current_network_id(&self) -> anyhow::Result<String> {
        Ok(self.anchor.network_id.clone())
    }
    async fn local_node_id(&self) -> anyhow::Result<String> {
        Ok("registration-transport-node".to_owned())
    }
    async fn network_credential_trust_anchor(
        &self,
    ) -> anyhow::Result<NetworkCredentialTrustAnchor> {
        Ok(self.anchor.clone())
    }
    async fn update_network_permission(
        &self,
        update: wattetheria_kernel::network_agent_registration::NetworkPermissionUpdate,
    ) -> anyhow::Result<()> {
        self.updates.lock().await.push(update);
        Ok(())
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn network_registration_approval_spawns_space_without_restart_and_does_not_reset_replay() {
    let (_dir, _app, token, _policy, mut state) = build_test_app(100);
    let stored = state
        .local_db
        .load_network_agent_credential(&state.agent_did, None)
        .unwrap()
        .unwrap();
    let bridge = Arc::new(SpaceRegistrationBridge {
        anchor: serde_json::from_str(stored.trust_anchor_json.as_ref().unwrap()).unwrap(),
        updates: Mutex::new(Vec::new()),
    });
    state.swarm_bridge = bridge.clone();
    state.local_db = Arc::new(wattetheria_kernel::local_db::LocalDb::open_in_memory().unwrap());
    let request = crate::routes::network::build_registration_request(&state)
        .await
        .unwrap();
    let mut events = state.stream_tx.subscribe();
    let pending = json!({"request":request,"status":"pending"});
    assert_eq!(
        crate::routes::network::apply_registry_registration_record(&state, &pending)
            .await
            .unwrap()
            .status,
        "pending"
    );
    let pending_registry = state.own_space_presence.lock().await;
    assert_eq!(
        pending_registry.get(
            &test_network_id(&state),
            state
                .space_catalog
                .as_ref()
                .unwrap()
                .current("the-ville")
                .unwrap()
        ),
        None
    );
    drop(pending_registry);
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let approved = json!({"request":request,"status":"approved","credential":serde_json::from_str::<Value>(&stored.credential_json).unwrap()});
    assert_eq!(
        crate::routes::network::apply_registry_registration_record(&state, &approved)
            .await
            .unwrap()
            .status,
        "approved"
    );
    assert!(state.own_space_presence.lock().await.active_space.is_none());
    assert!(state.own_space_presence.lock().await.networks.is_empty());
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let entered = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/enter",
        json!({}),
    )
    .await;
    assert_eq!(entered["ok"], true);
    let spawned = state
        .own_space_presence
        .lock()
        .await
        .get(
            &test_network_id(&state),
            state
                .space_catalog
                .as_ref()
                .unwrap()
                .current("the-ville")
                .unwrap(),
        )
        .cloned()
        .unwrap();
    assert_eq!(spawned.record.seq, 1);
    assert_eq!(spawned.record.destination_place_id, "johnson-park");
    let event = events.recv().await.unwrap();
    assert_eq!(event.kind, "space.presence.updated");
    assert_eq!(event.payload, serde_json::to_value(&spawned).unwrap());
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"hobbs-cafe/cafe"}),
    )
    .await;
    assert_eq!(moved["presence"]["record"]["seq"], 2);
    let moved: wattetheria_gateway_contract::SignedSpacePresence =
        serde_json::from_value(moved["presence"].clone()).unwrap();
    events.recv().await.unwrap();
    crate::routes::network::apply_registry_registration_record(&state, &approved)
        .await
        .unwrap();
    let latest = state
        .own_space_presence
        .lock()
        .await
        .get(
            &test_network_id(&state),
            state
                .space_catalog
                .as_ref()
                .unwrap()
                .current("the-ville")
                .unwrap(),
        )
        .cloned()
        .unwrap();
    assert_eq!(latest, moved);
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let persisted = crate::OwnSpacePresence::load(
        &state.local_db,
        state.space_catalog.as_deref(),
        &state.agent_did,
    )
    .unwrap();
    assert!(persisted.active_space.is_none());
    assert_eq!(
        persisted.get(
            &test_network_id(&state),
            state
                .space_catalog
                .as_ref()
                .unwrap()
                .current("the-ville")
                .unwrap()
        ),
        Some(&moved)
    );
    assert_eq!(bridge.updates.lock().await.len(), 2);
}

#[tokio::test]
async fn each_new_registration_attempt_has_independent_identifiers() {
    let (_dir, _app, _token, _policy, state) = build_test_app(100);

    let first = crate::routes::network::build_registration_request(&state)
        .await
        .expect("first registration request");
    let second = crate::routes::network::build_registration_request(&state)
        .await
        .expect("second registration request");

    assert_ne!(first.request_id, second.request_id);
    assert_ne!(first.nonce, second.nonce);
    assert_eq!(first.network_id, second.network_id);
    assert_eq!(first.agent_did, second.agent_did);
    assert!(first.agent_card.is_some());
    assert!(
        first
            .agent_card_hash
            .as_deref()
            .is_some_and(|hash| hash.starts_with("sha256:"))
    );
    assert_eq!(first.agent_card, second.agent_card);
    assert_eq!(first.agent_card_hash, second.agent_card_hash);
    wattetheria_kernel::network_agent_registration::request_signature_is_valid(&first)
        .expect("first request signature");
    wattetheria_kernel::network_agent_registration::request_signature_is_valid(&second)
        .expect("second request signature");
}

#[async_trait::async_trait]
impl SwarmBridge for FailingOncePermissionBridge {
    async fn agent_view(&self, agent_did: &str) -> anyhow::Result<SwarmAgentView> {
        Ok(SwarmAgentView {
            agent_did: agent_did.to_owned(),
            stats: AgentStats::default(),
        })
    }

    async fn update_network_permission(
        &self,
        _update: wattetheria_kernel::network_agent_registration::NetworkPermissionUpdate,
    ) -> anyhow::Result<()> {
        let attempt = self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if attempt == 0 {
            anyhow::bail!("temporary permission callback failure");
        }
        Ok(())
    }
}

#[tokio::test]
async fn network_permission_endpoint_reads_local_registration_tables() {
    let (_dir, app, token, _policy, state) = build_test_app(100);

    let active = authed_get_json(app.clone(), &token, "/v1/wattetheria/network-permission").await;
    assert_eq!(active["ok"], true);
    assert_eq!(active["active"], true);
    assert_eq!(
        active["checkpoint"]["agent_did"].as_str(),
        Some(state.agent_did.as_str())
    );
    assert_eq!(active["checkpoint"]["permission_status"], "active");
    assert_eq!(active["checkpoint"]["network_status"], "running");
    assert_eq!(
        active["checkpoint"]["credential_id"],
        "test-network-credential"
    );
    assert!(
        active["checkpoint"]["credential_hash"]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:"))
    );

    let mut checkpoint = state
        .local_db
        .load_network_permission_checkpoint(&state.agent_did, None, None)
        .unwrap()
        .unwrap();
    checkpoint.permission_status = "pending".to_owned();
    state
        .local_db
        .upsert_network_permission_checkpoint(&checkpoint)
        .unwrap();

    let pending = authed_get_json(app, &token, "/v1/wattetheria/network-permission").await;
    assert_eq!(pending["active"], false);
    assert_eq!(pending["checkpoint"]["permission_status"], "pending");
}

#[tokio::test]
async fn network_permission_delivery_retries_only_after_failure() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    let bridge = Arc::new(FailingOncePermissionBridge {
        attempts: std::sync::atomic::AtomicUsize::new(0),
    });
    state.swarm_bridge = bridge.clone();

    crate::routes::network::retry_network_permission_delivery(&state, std::time::Duration::ZERO)
        .await;

    assert_eq!(bridge.attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn unchanged_network_permission_does_not_increment_revision() {
    let (_dir, _app, _token, _policy, state) = build_test_app(100);
    let request = RegistrationRequest {
        version: 1,
        request_id: "request-pending".to_owned(),
        network_id: "test-network".to_owned(),
        agent_did: state.agent_did.clone(),
        nickname: "Agent".to_owned(),
        agent_card: None,
        agent_card_hash: None,
        tenant_instance_id: None,
        nonce: "nonce-pending".to_owned(),
        signature_b64: "opaque-for-persistence-test".to_owned(),
    };

    let (first, first_changed) = crate::routes::network::persist_network_permission_checkpoint(
        &state, &request, "pending", false, None,
    )
    .await
    .expect("persist first pending checkpoint");
    let (second, second_changed) = crate::routes::network::persist_network_permission_checkpoint(
        &state, &request, "pending", false, None,
    )
    .await
    .expect("persist repeated pending checkpoint");

    assert!(first_changed);
    assert!(!second_changed);
    assert_eq!(second.revision, first.revision);
}
