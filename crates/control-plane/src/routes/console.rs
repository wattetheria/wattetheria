use axum::extract::State;
use axum::response::{Html, IntoResponse};

use crate::state::ControlPlaneState;

pub(crate) async fn supervision_console(
    State(state): State<ControlPlaneState>,
) -> impl IntoResponse {
    let bootstrap_control_token =
        serde_json::to_string(&state.auth_token).unwrap_or_else(|_| "\"\"".to_string());
    Html(
        include_str!("supervision_console.html")
            .replace("__BOOTSTRAP_CONTROL_TOKEN__", &bootstrap_control_token),
    )
}
