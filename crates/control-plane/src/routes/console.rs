use axum::extract::State;
use axum::response::{Html, IntoResponse};

use crate::state::ControlPlaneState;

const SUPERVISION_CONSOLE_HTML: &str = include_str!("supervision_console/template.html");
const SUPERVISION_CONSOLE_CSS: &str = include_str!("supervision_console/styles.css");
const SUPERVISION_CONSOLE_JS: &str = include_str!("supervision_console/script.js");

pub(crate) async fn supervision_console(
    State(state): State<ControlPlaneState>,
) -> impl IntoResponse {
    let bootstrap_control_token =
        serde_json::to_string(&state.auth_token).unwrap_or_else(|_| "\"\"".to_string());
    Html(render_supervision_console(&bootstrap_control_token))
}

fn render_supervision_console(bootstrap_control_token: &str) -> String {
    SUPERVISION_CONSOLE_HTML
        .replace("__SUPERVISION_CONSOLE_CSS__", SUPERVISION_CONSOLE_CSS)
        .replace("__SUPERVISION_CONSOLE_JS__", SUPERVISION_CONSOLE_JS)
        .replace("__BOOTSTRAP_CONTROL_TOKEN__", bootstrap_control_token)
}
