use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::{Html, IntoResponse};

use crate::state::ControlPlaneState;

const SUPERVISION_CONSOLE_HTML: &str = include_str!("supervision_console/template.html");
const SUPERVISION_FAVICON_PNG: &[u8] = include_bytes!("supervision_console/public/favicon.png");
const SUPERVISION_CONSOLE_CSS: &str = concat!(
    include_str!("supervision_console/css/theme.css"),
    "\n",
    include_str!("supervision_console/css/layout.css"),
    "\n",
    include_str!("supervision_console/css/identity-editor.css"),
    "\n",
    include_str!("supervision_console/css/nearby.css"),
    "\n",
    include_str!("supervision_console/css/navigation.css"),
    "\n",
    include_str!("supervision_console/css/workspace.css"),
    "\n",
    include_str!("supervision_console/css/servicenet.css"),
    "\n",
    include_str!("supervision_console/css/hives.css"),
    "\n",
    include_str!("supervision_console/css/forms.css"),
    "\n",
    include_str!("supervision_console/css/notices.css"),
    "\n",
    include_str!("supervision_console/css/overview.css"),
    "\n",
    include_str!("supervision_console/css/wallet.css"),
    "\n",
    include_str!("supervision_console/css/runtime.css"),
    "\n",
    include_str!("supervision_console/css/skills.css"),
    "\n",
    include_str!("supervision_console/css/components.css"),
    "\n",
    include_str!("supervision_console/css/identity.css"),
    "\n",
    include_str!("supervision_console/css/social.css"),
    "\n",
    include_str!("supervision_console/css/utilities.css"),
    "\n",
    include_str!("supervision_console/css/responsive.css"),
);
const SUPERVISION_CONSOLE_JS: &str = concat!(
    include_str!("supervision_console/js/state.js"),
    "\n",
    include_str!("supervision_console/js/navigation.js"),
    "\n",
    include_str!("supervision_console/js/dom.js"),
    "\n",
    include_str!("supervision_console/js/api.js"),
    "\n",
    include_str!("supervision_console/js/formatters.js"),
    "\n",
    include_str!("supervision_console/js/identity-core.js"),
    "\n",
    include_str!("supervision_console/js/rendering.js"),
    "\n",
    include_str!("supervision_console/js/identity-actions.js"),
    "\n",
    include_str!("supervision_console/js/refresh.js"),
    "\n",
    include_str!("supervision_console/js/logs-data.js"),
    "\n",
    include_str!("supervision_console/js/overview.js"),
    "\n",
    include_str!("supervision_console/js/missions.js"),
    "\n",
    include_str!("supervision_console/js/social.js"),
    "\n",
    include_str!("supervision_console/js/hives.js"),
    "\n",
    include_str!("supervision_console/js/identity-list.js"),
    "\n",
    include_str!("supervision_console/js/wallet.js"),
    "\n",
    include_str!("supervision_console/js/guilds.js"),
    "\n",
    include_str!("supervision_console/js/servicenet.js"),
    "\n",
    include_str!("supervision_console/js/skills.js"),
    "\n",
    include_str!("supervision_console/js/logs-rendering.js"),
    "\n",
    include_str!("supervision_console/js/runtime.js"),
    "\n",
    include_str!("supervision_console/js/bootstrap.js"),
);

pub(crate) async fn supervision_console(
    State(state): State<ControlPlaneState>,
) -> impl IntoResponse {
    let bootstrap_control_token =
        serde_json::to_string(&state.auth_token).unwrap_or_else(|_| "\"\"".to_string());
    Html(render_supervision_console(&bootstrap_control_token))
}

pub(crate) async fn supervision_favicon_png() -> impl IntoResponse {
    ([(CONTENT_TYPE, "image/png")], SUPERVISION_FAVICON_PNG)
}

fn render_supervision_console(bootstrap_control_token: &str) -> String {
    SUPERVISION_CONSOLE_HTML
        .replace("__SUPERVISION_CONSOLE_CSS__", SUPERVISION_CONSOLE_CSS)
        .replace("__SUPERVISION_CONSOLE_JS__", SUPERVISION_CONSOLE_JS)
        .replace("__BOOTSTRAP_CONTROL_TOKEN__", bootstrap_control_token)
}
