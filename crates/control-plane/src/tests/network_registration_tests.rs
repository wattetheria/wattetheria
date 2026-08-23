use super::*;

struct FailingOncePermissionBridge {
    attempts: std::sync::atomic::AtomicUsize,
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
