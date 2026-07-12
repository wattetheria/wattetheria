use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{Html, IntoResponse, Response};

use crate::state::ControlPlaneState;

const SUPERVISION_CONSOLE_HTML: &str = include_str!("supervision_console/template.html");
const SUPERVISION_FAVICON_PNG: &[u8] = include_bytes!("supervision_console/public/favicon.png");
const SUPERVISION_FONT_FILES: &[(&str, &[u8])] = &[
    (
        "albert-sans-v4-latin-regular.woff2",
        include_bytes!("supervision_console/public/fonts/albert-sans-v4-latin-regular.woff2"),
    ),
    (
        "albert-sans-v4-latin-500.woff2",
        include_bytes!("supervision_console/public/fonts/albert-sans-v4-latin-500.woff2"),
    ),
    (
        "albert-sans-v4-latin-600.woff2",
        include_bytes!("supervision_console/public/fonts/albert-sans-v4-latin-600.woff2"),
    ),
    (
        "dm-sans-v17-latin-regular.woff2",
        include_bytes!("supervision_console/public/fonts/dm-sans-v17-latin-regular.woff2"),
    ),
    (
        "dm-sans-v17-latin-500.woff2",
        include_bytes!("supervision_console/public/fonts/dm-sans-v17-latin-500.woff2"),
    ),
    (
        "dm-sans-v17-latin-600.woff2",
        include_bytes!("supervision_console/public/fonts/dm-sans-v17-latin-600.woff2"),
    ),
    (
        "fraunces-v38-latin-600.woff2",
        include_bytes!("supervision_console/public/fonts/fraunces-v38-latin-600.woff2"),
    ),
    (
        "outfit-v15-latin-regular.woff2",
        include_bytes!("supervision_console/public/fonts/outfit-v15-latin-regular.woff2"),
    ),
    (
        "outfit-v15-latin-500.woff2",
        include_bytes!("supervision_console/public/fonts/outfit-v15-latin-500.woff2"),
    ),
    (
        "outfit-v15-latin-600.woff2",
        include_bytes!("supervision_console/public/fonts/outfit-v15-latin-600.woff2"),
    ),
    (
        "playwrite-us-trad-v11-latin-regular.woff2",
        include_bytes!(
            "supervision_console/public/fonts/playwrite-us-trad-v11-latin-regular.woff2"
        ),
    ),
    (
        "OFL.txt",
        include_bytes!("supervision_console/public/fonts/OFL.txt"),
    ),
];
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
    include_str!("supervision_console/js/message-refresh.js"),
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

pub(crate) async fn supervision_font(Path(file): Path<String>) -> Response {
    for (name, bytes) in SUPERVISION_FONT_FILES {
        if *name == file {
            let content_type = if std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("woff2"))
            {
                "font/woff2"
            } else {
                "text/plain; charset=utf-8"
            };
            return ([(CONTENT_TYPE, content_type)], *bytes).into_response();
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

fn render_supervision_console(bootstrap_control_token: &str) -> String {
    SUPERVISION_CONSOLE_HTML
        .replace("__SUPERVISION_CONSOLE_CSS__", SUPERVISION_CONSOLE_CSS)
        .replace("__SUPERVISION_CONSOLE_JS__", SUPERVISION_CONSOLE_JS)
        .replace("__BOOTSTRAP_CONTROL_TOKEN__", bootstrap_control_token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_refresh_script_loads_before_console_bootstrap() {
        let polling = SUPERVISION_CONSOLE_JS
            .find("const messageRefreshBaseDelayMs")
            .expect("message refresh script");
        let bootstrap = SUPERVISION_CONSOLE_JS
            .find("document.getElementById(\"load-identities\")")
            .expect("console bootstrap script");

        assert!(polling < bootstrap);
    }

    #[test]
    fn message_refresh_is_visibility_aware_and_scoped_to_message_views() {
        let script = include_str!("supervision_console/js/message-refresh.js");

        assert!(script.contains("document.visibilityState === \"visible\""));
        assert!(script.contains("page === \"swarm\" || page === \"social\""));
        assert!(script.contains("messageRefreshBaseDelayMs = 10000"));
        assert!(script.contains("messageRefreshMaxDelayMs = 60000"));
        assert!(script.contains("/v1/client/friends/messages?"));
        assert!(script.contains("lastConsolePayload !== payload"));
        assert!(!script.contains("refreshConsole("));
    }

    #[test]
    fn message_refresh_handles_empty_hive_recovery_and_avoids_duplicate_dm_fetch() {
        let hives = include_str!("supervision_console/js/hives.js");
        let refresh = include_str!("supervision_console/js/refresh.js");

        assert!(hives.contains("changed: !hadCachedMessages ||"));
        assert!(refresh.contains("restartMessageRefreshForCurrentView({ immediate: false })"));
    }
}
