//! Observatory service entrypoint.

use anyhow::Result;
use std::sync::{Arc, RwLock};
use wattetheria_observatory_core::{StoreConfig, SummaryStore, app};

#[tokio::main]
async fn main() -> Result<()> {
    let port = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(8787);
    let config = StoreConfig::from_env();
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    let store = Arc::new(RwLock::new(SummaryStore::with_config(config.clone())));
    println!(
        "observatory listening on {port} (max_entries={}, max_age_sec={}, max_ingest_per_agent_min={})",
        config.max_entries, config.max_entry_age_sec, config.max_ingest_per_agent_per_minute
    );
    axum::serve(listener, app(store)).await?;
    Ok(())
}
