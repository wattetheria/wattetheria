use uuid::Uuid;
use wattetheria_kernel::brain::{RuntimeSessionContext, RuntimeSessionMode};

pub(crate) fn agent_event_runtime_session_id(
    agent_did: &str,
    network_id: &str,
    scope_hint: Option<&str>,
    mode: RuntimeSessionMode,
) -> Option<String> {
    match mode {
        RuntimeSessionMode::Stable => None,
        RuntimeSessionMode::StablePerScope => scope_hint
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|scope_hint| {
                let base = RuntimeSessionContext::identity(agent_did, network_id).session_id();
                format!("{base}:{scope_hint}")
            }),
        RuntimeSessionMode::NewPerInteraction => {
            let base = RuntimeSessionContext::identity(agent_did, network_id).session_id();
            let suffix = 100_000 + (Uuid::new_v4().as_u128() % 900_000);
            Some(format!("{base}:{suffix:06}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_mode_does_not_precompute_agent_event_session() {
        assert_eq!(
            agent_event_runtime_session_id(
                "did:key:zAgent",
                "mainnet:watt-etheria",
                Some("group:crew-7"),
                RuntimeSessionMode::Stable,
            ),
            None
        );
    }

    #[test]
    fn scoped_stable_mode_appends_scope_hint() {
        assert_eq!(
            agent_event_runtime_session_id(
                "did:key:zAgent",
                "mainnet:watt-etheria",
                Some(" group:crew-7 "),
                RuntimeSessionMode::StablePerScope,
            )
            .as_deref(),
            Some("wattetheria:identity:did:key:zAgent:mainnet:watt-etheria:group:crew-7")
        );
    }

    #[test]
    fn scoped_stable_mode_falls_back_to_identity_session_without_scope() {
        for scope_hint in [None, Some(""), Some("   ")] {
            assert_eq!(
                agent_event_runtime_session_id(
                    "did:key:zAgent",
                    "mainnet:watt-etheria",
                    scope_hint,
                    RuntimeSessionMode::StablePerScope,
                ),
                None
            );
        }
    }

    #[test]
    fn new_interaction_mode_appends_six_digit_suffix() {
        let session_id = agent_event_runtime_session_id(
            "did:key:zAgent",
            "mainnet:watt-etheria",
            Some("group:crew-7"),
            RuntimeSessionMode::NewPerInteraction,
        )
        .expect("new interaction session id");
        let prefix = "wattetheria:identity:did:key:zAgent:mainnet:watt-etheria:";
        let suffix = session_id
            .strip_prefix(prefix)
            .expect("session id should keep identity base prefix");

        assert_eq!(suffix.len(), 6);
        assert!(suffix.chars().all(|ch| ch.is_ascii_digit()));
    }
}
