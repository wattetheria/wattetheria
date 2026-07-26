use crate::{Cli, load_civilization_runtime_state};
use clap::Parser;
use wattetheria_kernel::agent_identity::FileAgentIdentityStore;
use wattetheria_kernel::civilization::identities::{
    ControllerBindingRegistry, PublicIdentityRegistry,
};
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::local_db::{self, LocalDb};

#[test]
fn restarted_runtime_import_replaces_the_persisted_local_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let identity_store = FileAgentIdentityStore::new(dir.path());
    let (previous_runtime, _) = identity_store
        .load_or_create_runtime_identity()
        .expect("previous Runtime identity");
    let imported_runtime = Identity::new_random();
    identity_store
        .stage_import(None, &imported_runtime.private_key)
        .expect("stage Runtime identity import");
    let (active_runtime, _) = identity_store
        .load_or_create_runtime_identity()
        .expect("activate imported Runtime identity");
    assert_eq!(active_runtime.agent_did, imported_runtime.agent_did);

    let db_path = local_db::prepare_primary_db(dir.path()).expect("prepare database");
    let local_db = LocalDb::open(&db_path).expect("open database");
    let mut public_identities = PublicIdentityRegistry::default();
    let previous_public = public_identities
        .ensure_local_default(&previous_runtime.agent_did)
        .expect("previous public identity");
    let mut controller_bindings = ControllerBindingRegistry::default();
    controller_bindings
        .ensure_local_wattswarm(&previous_public.public_id, &previous_runtime.agent_did);
    local_db
        .save_domain(
            local_db::domain::PUBLIC_IDENTITY_REGISTRY,
            &public_identities,
        )
        .expect("save previous public identity");
    local_db
        .save_domain(
            local_db::domain::CONTROLLER_BINDING_REGISTRY,
            &controller_bindings,
        )
        .expect("save previous controller binding");

    let cli = Cli::try_parse_from([
        "wattetheria-kernel",
        "--data-dir",
        dir.path().to_str().expect("UTF-8 data directory"),
    ])
    .expect("test CLI");
    let runtime_state =
        load_civilization_runtime_state(&cli, &active_runtime.compat_view(), &local_db)
            .expect("load civilization state after Runtime replacement");

    let identities = runtime_state.public_identity_registry.list();
    assert_eq!(identities.len(), 1);
    assert_eq!(
        identities[0].agent_did.as_deref(),
        Some(imported_runtime.agent_did.as_str())
    );
    assert!(
        runtime_state
            .public_identity_registry
            .get(&previous_public.public_id)
            .is_none()
    );
    assert!(
        runtime_state
            .controller_binding_registry
            .get(&previous_public.public_id)
            .is_none()
    );

    let persisted: PublicIdentityRegistry = local_db
        .load_domain(local_db::domain::PUBLIC_IDENTITY_REGISTRY)
        .expect("load persisted public identity registry")
        .expect("persisted public identity registry");
    assert_eq!(persisted.list(), identities);
}
