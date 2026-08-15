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
    include_str!("supervision_console/css/agent-identities.css"),
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
    include_str!("supervision_console/js/agent-identities.js"),
    "\n",
    include_str!("supervision_console/js/wallet.js"),
    "\n",
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
    fn guilds_console_surface_is_removed_while_organization_identity_data_remains() {
        let template = include_str!("supervision_console/template.html");
        let identity_list = include_str!("supervision_console/js/identity-list.js");
        let identity_styles = include_str!("supervision_console/css/identity.css");

        assert!(!template.contains("Guilds"));
        assert!(!template.contains("data-view=\"organizations\""));
        assert!(!template.contains("data-page=\"organizations\""));
        assert!(!SUPERVISION_CONSOLE_JS.contains("renderOrganizations("));
        assert!(!SUPERVISION_CONSOLE_JS.contains("organization_limit"));
        assert!(identity_list.contains("identity-organizations"));
        assert!(identity_list.contains("No organizations"));
        assert!(identity_styles.contains(".identity-organizations"));
        assert!(!identity_list.contains("guilds"));
        assert!(!identity_styles.contains("identity-guilds"));
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

    #[test]
    fn service_agent_identity_delete_requires_destructive_confirmation() {
        let script = include_str!("supervision_console/js/agent-identities.js");

        assert!(script.contains("data-service-agent-delete"));
        assert!(script.contains("confirmDialog({"));
        assert!(script.contains("Delete Service Agent DID"));
        assert!(script.contains("This cannot be undone."));
        assert!(script.contains("{ method: \"DELETE\", auth: true }"));
        assert!(script.contains("<th>Service Agent</th><th>Service DID</th>"));
        assert!(
            !script.contains("[\"service_agent_identity_id\", identity.service_agent_identity_id")
        );
        assert!(!script.contains("`Import ${agentId} Credential`"));
        assert!(script.contains("\"Import Service Agent Credential\""));
    }

    #[test]
    fn runtime_identity_import_requires_preview_confirmation_and_uses_theme_tokens() {
        let template = include_str!("supervision_console/template.html");
        let script = include_str!("supervision_console/js/agent-identities.js");
        let styles = include_str!("supervision_console/css/agent-identities.css");

        assert!(template.contains("id=\"agent-identity-import-preview\""));
        assert!(template.contains("id=\"agent-identity-preview-title\""));
        assert!(template.contains("id=\"agent-identity-preview-did-label\""));
        assert!(template.contains("id=\"agent-identity-preview-did\""));
        assert!(template.contains("id=\"agent-identity-preview-uri\""));
        assert!(template.contains("id=\"agent-identity-preview-fingerprint\""));
        assert!(template.contains("id=\"agent-identity-modal-back\""));
        assert!(template.contains("id=\"agent-identity-confirmed-did\" type=\"hidden\""));
        assert!(template.contains("id=\"runtime-identity-import-warning\""));
        assert!(template.contains("Use on a newly initialized node only"));
        assert!(
            template
                .contains("Importing after local data exists can make that data inconsistent with the new Runtime Agent identity.")
        );
        assert!(script.contains("/v1/wattetheria/agent-identities/runtime/import/preview"));
        assert!(script.contains("/v1/wattetheria/agent-identities/service-agents/import/preview"));
        assert!(script.contains("showRuntimeIdentityImportPreview(result, runtime)"));
        assert!(script.contains("qs(\"runtime-identity-import-warning\").hidden = !runtime"));
        assert!(script.contains("agent_did: phase === \"preview\" ? confirmedDid : did || null"));
        assert!(script.contains("service_did: phase === \"preview\" ? confirmedDid : did || null"));
        assert!(script.contains("generation !== agentIdentityModalGeneration"));
        assert!(script.contains("qs(\"agent-identity-operation\").value !== operation"));
        assert!(script.contains("dataset.phase === \"submitting\""));
        assert!(script.contains("setAgentIdentityModalDismissalDisabled(true)"));
        assert!(script.contains(
            "resetAgentIdentityModal();\n        try {\n          await loadManagedAgentIdentities();"
        ));
        assert!(script.contains("Confirm Runtime Agent DID"));
        assert!(script.contains("Confirm Service Agent DID"));
        assert!(script.contains("\"Derived Service Agent Identity\""));
        assert!(script.contains("\"service_did\""));
        assert!(script.contains("Confirm Import"));
        assert!(script.contains("Nothing has been imported yet."));
        assert!(script.contains("Import succeeded, but the identity view could not refresh."));
        assert!(styles.contains(".identity-transition-status"));
        assert!(styles.contains("border-left: 3px solid var(--accent)"));
        assert!(styles.contains("background: var(--accent-soft)"));
        assert!(styles.contains("color: var(--accent-strong)"));
        assert!(styles.contains(".identity-import-warning"));
        assert!(styles.contains(".identity-import-warning[hidden]"));
        assert!(!styles.contains(".identity-activation-notice"));
        assert!(!script.contains("Agent Card publication will then use the new DID."));
        assert!(!script.contains("Runtime Agent DID staged. Restart the node to activate it."));
    }

    #[test]
    fn provider_identity_is_separate_and_its_credentials_are_manageable() {
        let template = include_str!("supervision_console/template.html");
        let script = include_str!("supervision_console/js/agent-identities.js");
        let styles = include_str!("supervision_console/css/agent-identities.css");
        let runtime_tab = template
            .find("data-agent-identity-kind=\"runtime\"")
            .expect("Runtime Agent tab");
        let service_tab = template
            .find("data-agent-identity-kind=\"service\"")
            .expect("Service Agents tab");
        let provider_tab = template
            .find("data-agent-identity-kind=\"provider\"")
            .expect("Provider Identity tab");

        assert!(runtime_tab < service_tab);
        assert!(service_tab < provider_tab);
        assert!(template.contains("id=\"provider-identity-view\""));
        assert!(template.contains("data-provider-identity-tab=\"credentials\""));
        assert!(template.contains("data-provider-identity-tab=\"presentations\""));
        assert!(template.contains("id=\"provider-credential-import\""));
        assert!(template.contains("id=\"provider-agent-credentials\""));
        assert!(template.contains("id=\"provider-identity-export\""));
        assert!(script.contains("/v1/wattetheria/provider-identity"));
        assert!(script.contains("/v1/wattetheria/provider-identity/export"));
        assert!(script.contains("/v1/wattetheria/provider-identity/credentials"));
        assert!(script.contains("exportIdentityBackup(\"provider\")"));
        assert!(script.contains("Export ${identityLabel} DID"));
        assert!(script.contains("openAgentCredentialModal(\"provider\")"));
        assert!(script.contains("renderProviderIdentityManagement()"));
        assert!(!template.contains("id=\"provider-identity-import\""));
        assert!(!script.contains("switchProviderIdentity"));
        assert!(styles.contains("#provider-identity-view"));
        assert!(styles.contains("border-bottom-color: var(--accent-strong)"));
        assert!(styles.contains("color: var(--accent-strong)"));
        assert!(!styles.contains("border-bottom-color: var(--blue)"));
    }

    #[test]
    fn new_servicenet_publication_only_lists_unpublished_unbound_dids() {
        let script = include_str!("supervision_console/js/agent-identities.js");

        assert!(script.contains("identity.service_agent_identity_id === current"));
        assert!(
            script.contains("const current = selected === undefined ? select.value : selected")
        );
        assert!(!script.contains("const current = selected || select.value"));
        assert!(
            script.contains(
                "!identity.bound_agent_id && identity.agent_card_status !== \"published\""
            )
        );
        assert!(script.contains("No unpublished Service Agent DID available"));
        assert!(!script.contains(
            "identity.bound_agent_id && identity.service_agent_identity_id !== current ? \" disabled\""
        ));
    }

    #[test]
    fn published_servicenet_agent_did_is_read_only_when_editing() {
        let template = include_str!("supervision_console/template.html");
        let script = include_str!("supervision_console/js/servicenet.js");

        assert!(template.contains("id=\"servicenet-identity-select-field\""));
        assert!(template.contains("id=\"servicenet-identity-readonly-field\" hidden"));
        assert!(template.contains("id=\"servicenet-identity-readonly\" type=\"text\" readonly"));
        assert!(template.contains("Agent DID is fixed after publication."));
        assert!(
            script.contains(
                "qs(\"servicenet-identity-select-field\").hidden = editingPublishedAgent"
            )
        );
        assert!(script.contains(
            "qs(\"servicenet-identity-readonly-field\").hidden = !editingPublishedAgent"
        ));
        assert!(script.contains("selectedIdentity?.service_did || agent.service_did || \"\""));
    }

    #[test]
    fn runtime_and_service_agent_dids_require_confirmation_before_export() {
        let template = include_str!("supervision_console/template.html");
        let script = include_str!("supervision_console/js/agent-identities.js");

        assert!(template.contains("id=\"runtime-identity-export\""));
        assert!(script.contains("data-service-agent-export"));
        assert!(script.contains("/v1/wattetheria/agent-identities/runtime/export"));
        assert!(script.contains("/service-agents/${encodeURIComponent(identityId)}/export"));
        assert!(script.contains("Anyone with this file controls the DID."));
        assert!(script.contains("confirmText: \"Export DID\""));
        assert!(script.contains("URL.createObjectURL(blob)"));
        assert!(script.contains("URL.revokeObjectURL(url)"));
    }
}
