use crate::{Cli, load_civilization_runtime_state, load_space_catalog};
use clap::Parser;
use wattetheria_kernel::agent_identity::FileAgentIdentityStore;
use wattetheria_kernel::civilization::identities::{
    ControllerBindingRegistry, PublicIdentityRegistry,
};
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::local_db::{self, LocalDb};

const MINIMAL_SPACE_VERSION: &str =
    "sha256:a0fa85f21777ea47e03236f57e3fed17f049b4977080be4a182d22abb0f46c1b";

#[test]
fn no_catalog_override_uses_the_bundled_catalog() {
    let Some(catalog) = load_space_catalog(None).expect("load bundled Space catalog") else {
        return;
    };
    let default = catalog
        .spaces
        .iter()
        .find(|entry| entry.space_id == catalog.default_space_id)
        .expect("default Space exists");

    catalog
        .current(&default.space_id)
        .expect("default current package")
        .validate()
        .expect("default current package validates");
}

#[test]
fn catalog_override_loads_the_specified_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let package = serde_json::json!({
        "space_id": "example-space",
        "space_version": MINIMAL_SPACE_VERSION,
        "size": [1, 1],
        "tile_size": 1,
        "walkable_grid": [[1]],
        "places": [{
            "place_id": "example-space",
            "parent_place_id": null,
            "kind": "world",
            "name": {"en": "Example", "zh": "Example"},
            "tiles": [[0, 0]]
        }],
        "spawn_place_id": "example-space",
        "walk_speed": 1.0,
        "movement_search_radius": 1,
        "service_bindings": []
    });
    std::fs::write(
        dir.path().join("package.json"),
        serde_json::to_vec(&package).expect("serialize package"),
    )
    .expect("write package");
    let external_json = serde_json::json!({
        "default_space_id": "example-space",
        "spaces": [{
            "space_id": "example-space",
            "current_version": MINIMAL_SPACE_VERSION,
            "versions": [{
                "space_version": MINIMAL_SPACE_VERSION,
                "package_path": "package.json",
                "assets_path": "external-assets"
            }]
        }]
    });
    let catalog_path = dir.path().join("catalog.json");
    std::fs::write(
        &catalog_path,
        serde_json::to_vec(&external_json).expect("serialize external catalog"),
    )
    .expect("write external catalog");

    let catalog = load_space_catalog(Some(&catalog_path))
        .expect("load external Space catalog")
        .expect("external Space catalog exists");
    let current = catalog
        .spaces
        .iter()
        .find(|entry| entry.space_id == "example-space")
        .and_then(|entry| {
            entry
                .versions
                .iter()
                .find(|version| version.space_version == entry.current_version)
        })
        .expect("external current version");

    assert_eq!(
        current.assets_path,
        std::path::PathBuf::from("external-assets")
    );
}

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
