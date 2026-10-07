use std::net::SocketAddr;
use std::sync::Arc;

use wattetheria_control_plane::{ControlPlaneState, PublicMcpUrl, serve_public_mcp};

use crate::Cli;

pub(super) fn configure(cli: &Cli, state: &mut ControlPlaneState) -> Option<SocketAddr> {
    let value = cli.mcp_public_bind.as_deref()?.trim();
    if value.is_empty() {
        return None;
    }
    if state.mcp_events.is_none() {
        tracing::error!(
            "public MCP listener disabled: subscription revocation store unavailable; local control plane continues"
        );
        return None;
    }
    let configured = (|| {
        let bind = value.parse::<SocketAddr>()?;
        let public_url = PublicMcpUrl::open(&cli.data_dir)?;
        Ok::<_, anyhow::Error>((bind, public_url))
    })();
    match configured {
        Ok((bind, public_url)) => {
            state.mcp_public_url = Some(Arc::new(public_url));
            Some(bind)
        }
        Err(error) => {
            tracing::error!(%error, "public MCP listener disabled; local control plane continues");
            None
        }
    }
}

pub(super) fn spawn(state: ControlPlaneState, bind: SocketAddr) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = serve_public_mcp(state, bind).await {
            tracing::error!(%error, "public MCP listener stopped");
        }
    })
}
