use crate::cli::Cli;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use tokio::time::{Duration, MissedTickBehavior, interval, sleep};
use tracing::{info, warn};
use wattetheria_control_plane::{
    ClientExportQuery, ControlPlaneState, push_signed_public_client_snapshot,
};

const MIN_GATEWAY_PUSH_INTERVAL_SEC: u64 = 10;
const MAX_GATEWAY_STARTUP_JITTER_SEC: u64 = 15;

pub fn spawn_gateway_publish_task(
    cli: &Cli,
    control_state: ControlPlaneState,
) -> Option<tokio::task::JoinHandle<()>> {
    if cli.gateway_urls.is_empty() {
        return None;
    }

    let gateway_urls = cli.gateway_urls.clone();
    let interval_sec = cli
        .gateway_push_interval_sec
        .max(MIN_GATEWAY_PUSH_INTERVAL_SEC);
    let startup_jitter_sec =
        gateway_startup_jitter_secs(&control_state.identity.agent_id, interval_sec);
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
        loop {
            ticker.tick().await;
            for gateway_url in &gateway_urls {
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

fn gateway_startup_jitter_secs(agent_id: &str, interval_sec: u64) -> u64 {
    let jitter_window = interval_sec.min(MAX_GATEWAY_STARTUP_JITTER_SEC);
    if jitter_window == 0 {
        return 0;
    }
    let mut hasher = DefaultHasher::new();
    agent_id.hash(&mut hasher);
    hasher.finish() % (jitter_window + 1)
}

#[cfg(test)]
mod tests {
    use super::gateway_startup_jitter_secs;

    #[test]
    fn startup_jitter_is_deterministic_and_bounded() {
        let first = gateway_startup_jitter_secs("agent-alpha", 30);
        let second = gateway_startup_jitter_secs("agent-alpha", 30);
        assert_eq!(first, second);
        assert!(first <= 15);

        let tight_interval = gateway_startup_jitter_secs("agent-alpha", 5);
        assert!(tight_interval <= 5);
    }
}
