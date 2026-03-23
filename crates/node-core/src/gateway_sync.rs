use crate::cli::Cli;
use crate::gateway_registry::{GatewayRegistryClient, discover_gateways};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;
use tokio::time::{Duration, MissedTickBehavior, interval, sleep};
use tracing::{info, warn};
use wattetheria_control_plane::{
    ClientExportQuery, ControlPlaneState, push_signed_public_client_snapshot,
};

const MIN_GATEWAY_DISCOVERY_INTERVAL_SEC: u64 = 30;
const MIN_GATEWAY_PUSH_INTERVAL_SEC: u64 = 10;
const MAX_GATEWAY_STARTUP_JITTER_SEC: u64 = 15;
const MAX_DISCOVERED_GATEWAY_FANOUT: usize = 8;

pub fn spawn_gateway_publish_task(
    cli: &Cli,
    control_state: ControlPlaneState,
) -> Option<tokio::task::JoinHandle<()>> {
    if cli.gateway_urls.is_empty() && cli.gateway_registry_urls.is_empty() {
        return None;
    }

    let gateway_urls = cli.gateway_urls.clone();
    let gateway_registry_urls = cli.gateway_registry_urls.clone();
    let interval_sec = cli
        .gateway_push_interval_sec
        .max(MIN_GATEWAY_PUSH_INTERVAL_SEC);
    let discovery_interval_sec = cli
        .gateway_discovery_interval_sec
        .max(MIN_GATEWAY_DISCOVERY_INTERVAL_SEC);
    let startup_jitter_sec =
        gateway_startup_jitter_secs(&control_state.identity.agent_did, interval_sec);
    Some(tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                warn!(%error, "gateway publisher could not build HTTP client");
                return;
            }
        };
        let registry_client = match GatewayRegistryClient::new(15) {
            Ok(client) => client,
            Err(error) => {
                warn!(%error, "gateway publisher could not build registry client");
                return;
            }
        };
        if startup_jitter_sec > 0 {
            info!(
                startup_jitter_sec,
                "delaying first gateway snapshot publish to avoid synchronized bursts"
            );
            sleep(Duration::from_secs(startup_jitter_sec)).await;
        }
        let query = ClientExportQuery {
            peer_limit: Some(200),
            task_limit: Some(500),
            organization_limit: Some(500),
            rpc_log_limit: Some(50),
            leaderboard_limit: Some(200),
            ..ClientExportQuery::default()
        };
        let mut ticker = interval(Duration::from_secs(interval_sec));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut discovered_gateway_urls = Vec::<String>::new();
        let mut known_registry_urls = gateway_registry_urls
            .iter()
            .map(|url| url.trim_end_matches('/').to_string())
            .collect::<Vec<_>>();
        let mut last_discovery_refresh = None::<Instant>;
        loop {
            ticker.tick().await;
            if !known_registry_urls.is_empty()
                && should_refresh_discovery(last_discovery_refresh, discovery_interval_sec)
            {
                let (registry_urls, discovered_urls) =
                    discover_gateways(&registry_client, &known_registry_urls).await;
                known_registry_urls = registry_urls;
                if !discovered_urls.is_empty() || discovered_gateway_urls.is_empty() {
                    discovered_gateway_urls = discovered_urls;
                }
                last_discovery_refresh = Some(Instant::now());
                info!(
                    registry_count = known_registry_urls.len(),
                    discovered_gateway_count = discovered_gateway_urls.len(),
                    "refreshed gateway registry discovery"
                );
            }
            let publish_gateway_urls =
                select_publish_gateways(&gateway_urls, &discovered_gateway_urls);
            for gateway_url in &publish_gateway_urls {
                match push_signed_public_client_snapshot(
                    &client,
                    gateway_url,
                    &control_state,
                    &query,
                )
                .await
                {
                    Ok(snapshot) => info!(
                        gateway_url,
                        node_id = %snapshot.payload.node_id,
                        generated_at = snapshot.payload.generated_at,
                        "published client snapshot to gateway"
                    ),
                    Err(error) => warn!(gateway_url, %error, "gateway snapshot publish failed"),
                }
            }
        }
    }))
}

fn should_refresh_discovery(last_refresh: Option<Instant>, discovery_interval_sec: u64) -> bool {
    match last_refresh {
        None => true,
        Some(last_refresh) => last_refresh.elapsed().as_secs() >= discovery_interval_sec,
    }
}

fn select_publish_gateways(
    static_gateway_urls: &[String],
    discovered_gateway_urls: &[String],
) -> Vec<String> {
    let mut selected = Vec::<String>::new();
    for gateway_url in static_gateway_urls {
        let normalized = gateway_url.trim_end_matches('/').to_string();
        if !normalized.is_empty() && !selected.iter().any(|existing| existing == &normalized) {
            selected.push(normalized);
        }
    }
    for gateway_url in discovered_gateway_urls
        .iter()
        .take(MAX_DISCOVERED_GATEWAY_FANOUT)
    {
        let normalized = gateway_url.trim_end_matches('/').to_string();
        if !normalized.is_empty() && !selected.iter().any(|existing| existing == &normalized) {
            selected.push(normalized);
        }
    }
    selected
}

fn gateway_startup_jitter_secs(agent_did: &str, interval_sec: u64) -> u64 {
    let jitter_window = interval_sec.min(MAX_GATEWAY_STARTUP_JITTER_SEC);
    if jitter_window == 0 {
        return 0;
    }
    let mut hasher = DefaultHasher::new();
    agent_did.hash(&mut hasher);
    hasher.finish() % (jitter_window + 1)
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_DISCOVERED_GATEWAY_FANOUT, gateway_startup_jitter_secs, select_publish_gateways,
        should_refresh_discovery,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn startup_jitter_is_deterministic_and_bounded() {
        let first = gateway_startup_jitter_secs("agent-alpha", 30);
        let second = gateway_startup_jitter_secs("agent-alpha", 30);
        assert_eq!(first, second);
        assert!(first <= 15);

        let tight_interval = gateway_startup_jitter_secs("agent-alpha", 5);
        assert!(tight_interval <= 5);
    }

    #[test]
    fn discovery_refresh_runs_initially_and_after_interval() {
        assert!(should_refresh_discovery(None, 30));
        assert!(!should_refresh_discovery(Some(Instant::now()), 30));
        assert!(should_refresh_discovery(
            Some(Instant::now().checked_sub(Duration::from_secs(31)).unwrap(),),
            30
        ));
    }

    #[test]
    fn publish_gateway_selection_dedupes_and_caps_discovered_urls() {
        let static_urls = vec![
            "https://gw-a.example".to_string(),
            "https://gw-b.example/".to_string(),
        ];
        let discovered_urls = (0..(MAX_DISCOVERED_GATEWAY_FANOUT + 3))
            .map(|index| format!("https://gw-{index}.example/"))
            .collect::<Vec<_>>();
        let selected = select_publish_gateways(&static_urls, &discovered_urls);
        assert_eq!(selected[0], "https://gw-a.example");
        assert_eq!(selected[1], "https://gw-b.example");
        assert_eq!(selected.len(), MAX_DISCOVERED_GATEWAY_FANOUT + 2);
        assert!(selected.iter().all(|url| !url.ends_with('/')));
    }
}
