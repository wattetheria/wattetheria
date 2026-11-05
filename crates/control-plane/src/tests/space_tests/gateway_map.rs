//! The map comes from the gateway: pulled at startup, cached in the local database, swapped in
//! at runtime when a Space response names another version, without moving the agent.

use super::*;
use crate::routes::space::map_source::{self, SpaceMaps};
use axum::Router;
use axum::routing::get;
use std::sync::atomic::{AtomicUsize, Ordering};
use wattetheria_kernel::space::SpacePackage;

/// A custom skin the mock gateway says this agent published.
const CUSTOM_SKIN: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";

/// A gateway that publishes one package for The Ville and can be told to publish another.
struct MapGateway {
    package: Arc<std::sync::Mutex<SpacePackage>>,
    package_fetches: Arc<AtomicUsize>,
    url: String,
    server: tokio::task::JoinHandle<()>,
}

impl MapGateway {
    async fn serve(package: SpacePackage) -> Self {
        let package = Arc::new(std::sync::Mutex::new(package));
        let package_fetches = Arc::new(AtomicUsize::new(0));
        let catalog_package = Arc::clone(&package);
        let served_package = Arc::clone(&package);
        let receipt_package = Arc::clone(&package);
        let fetches = Arc::clone(&package_fetches);
        let router = Router::new()
            .route(
                "/api/spaces/catalog.json",
                get(move || {
                    let version = catalog_package.lock().unwrap().space_version.clone();
                    async move {
                        Json(json!({
                            "default_space_id": "the-ville",
                            "spaces": [{"space_id": "the-ville", "current_version": version,
                                "versions": [{"space_version": version, "package_path": "the-ville/space-package.json", "assets_path": "the-ville/"}]}]
                        }))
                    }
                }),
            )
            .route(
                "/api/spaces/the-ville/space-package.json",
                get(move || {
                    fetches.fetch_add(1, Ordering::SeqCst);
                    let package = served_package.lock().unwrap().clone();
                    async move { Json(package) }
                }),
            )
            .route(
                "/api/spaces/presence/{agent_id}/record",
                get(|| async { StatusCode::NOT_FOUND }),
            )
            .route(
                "/api/skins",
                get(|| async { Json(json!({"skins": [{"skin_id": CUSTOM_SKIN}]})) }),
            )
            .route(
                "/api/ingest/event",
                axum::routing::post(move || {
                    let version = receipt_package.lock().unwrap().space_version.clone();
                    async move { Json(json!({"status": "ok", "current_version": version})) }
                }),
            );
        let (url, server) = super::query_resilience::serve(router).await;
        Self {
            package,
            package_fetches,
            url,
            server,
        }
    }

    fn publish(&self, package: SpacePackage) {
        *self.package.lock().unwrap() = package;
    }
}

/// Waits for a background refresh to land the version this test expects.
async fn wait_for_version(maps: &SpaceMaps, version: &str) {
    for _ in 0..100 {
        if versions(maps).as_deref() == Some(version) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("map still at {:?}, expected {version}", versions(maps));
}

fn versions(maps: &SpaceMaps) -> Option<String> {
    maps.get()
        .and_then(|catalog| catalog.current_version("the-ville").map(str::to_owned))
}

#[tokio::test]
async fn startup_pulls_the_map_from_the_gateway_and_caches_it_for_offline_starts() {
    let package = test_space_package();
    let gateway = MapGateway::serve(package.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let db = LocalDb::open(dir.path().join("node.db")).unwrap();

    let maps = SpaceMaps::load(&db, std::slice::from_ref(&gateway.url), None)
        .await
        .unwrap();
    assert_eq!(maps.source(), "gateway");
    assert_eq!(versions(&maps), Some(package.space_version.clone()));
    assert_eq!(gateway.package_fetches.load(Ordering::SeqCst), 1);

    // Same version published: the cache serves, the package is not downloaded again.
    let again = SpaceMaps::load(&db, std::slice::from_ref(&gateway.url), None)
        .await
        .unwrap();
    assert_eq!(versions(&again), Some(package.space_version.clone()));
    assert_eq!(gateway.package_fetches.load(Ordering::SeqCst), 1);

    // Gateway gone: the cache serves.
    gateway.server.abort();
    let offline = SpaceMaps::load(&db, &["http://127.0.0.1:9".to_owned()], None)
        .await
        .unwrap();
    assert_eq!(offline.source(), "gateway");
    assert_eq!(versions(&offline), Some(package.space_version));

    // Neither: the Space is unavailable, startup succeeds.
    let empty = LocalDb::open(dir.path().join("empty.db")).unwrap();
    let none = SpaceMaps::load(&empty, &["http://127.0.0.1:9".to_owned()], None)
        .await
        .unwrap();
    assert_eq!(none.source(), "unavailable");
    assert!(none.get().is_none());
}

#[tokio::test]
async fn a_new_version_from_the_gateway_swaps_the_map_and_keeps_the_agent_where_it_stands() {
    let (dir, _app, token, _policy, mut state) = build_test_app(100);
    let package = test_space_package();
    let gateway = MapGateway::serve(package.clone()).await;
    state.gateway_urls = vec![gateway.url.clone()];
    let path = dir.path().join("gateway-map.db");
    let mut state = disk_state(state, &path);
    state.space_catalog = Arc::new(
        SpaceMaps::load(&state.local_db, &state.gateway_urls, None)
            .await
            .unwrap(),
    );
    initialize_space_presence(&state).await.unwrap();
    let moved = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/move",
        json!({"place_id":"hobbs-cafe/cafe","activity":"coffee"}),
    )
    .await;
    let old: SignedSpacePresence = serde_json::from_value(moved["presence"].clone()).unwrap();
    assert_ne!(old.record.path, "");

    // Nothing to do while the gateway publishes the version this node holds.
    assert!(!map_source::refresh(&state).await.unwrap());

    // The gateway publishes a map that closes a tile off the walk (the agent id, and so its
    // walk, differs per run).
    let upgraded = package
        .places
        .iter()
        .filter(|place| place.place_id == "johnson-park")
        .flat_map(|place| place.tiles.iter().copied())
        .map(|tile| super::map_upgrade::upgraded_package(&package, tile))
        .find(|upgraded| {
            let mut record = old.record.clone();
            record.space_version.clone_from(&upgraded.space_version);
            upgraded.validate_record(&record).is_ok()
        })
        .unwrap();
    gateway.publish(upgraded.clone());
    let mut events = state.stream_tx.subscribe();
    assert!(map_source::refresh(&state).await.unwrap());

    assert_eq!(
        versions(&state.space_catalog),
        Some(upgraded.space_version.clone())
    );
    let registry = state.own_space_presence.lock().await;
    let network = test_network_id(&state);
    let current = registry.visible_get(&network, &upgraded).unwrap().clone();
    drop(registry);
    assert_eq!(current.record.space_version, upgraded.space_version);
    assert!(current.record.entered);
    assert_eq!(current.record.seq, old.record.seq + 1);
    // The walk continues on the new map: same origin, same path, same destination.
    assert_eq!(current.record.from_tile, old.record.from_tile);
    assert_eq!(current.record.path, old.record.path);
    assert_eq!(current.record.destination_place_id, "hobbs-cafe/cafe");
    assert_eq!(current.record.activity, "coffee");
    let event = events.try_recv().unwrap();
    assert_eq!(event.kind, "space.presence.updated");
    assert_eq!(event.payload, serde_json::to_value(&current).unwrap());

    // The cache holds the new map for the next start.
    let reloaded = SpaceMaps::load(&state.local_db, &["http://127.0.0.1:9".to_owned()], None)
        .await
        .unwrap();
    assert_eq!(versions(&reloaded), Some(upgraded.space_version));
    gateway.server.abort();
}

#[tokio::test]
async fn a_package_that_fails_verification_leaves_the_current_map_in_place() {
    let (dir, _app, _token, _policy, mut state) = build_test_app(100);
    let package = test_space_package();
    let gateway = MapGateway::serve(package.clone()).await;
    state.gateway_urls = vec![gateway.url.clone()];
    let mut state = disk_state(state, &dir.path().join("bad-map.db"));
    state.space_catalog = Arc::new(
        SpaceMaps::load(&state.local_db, &state.gateway_urls, None)
            .await
            .unwrap(),
    );
    // The catalog names a new version but the package content does not hash to it.
    let mut forged = package.clone();
    forged.space_version =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_owned();
    gateway.publish(forged);
    let error = map_source::refresh(&state).await.unwrap_err();
    assert!(error.to_string().contains("space version"), "{error}");
    assert_eq!(versions(&state.space_catalog), Some(package.space_version));
    gateway.server.abort();
}

#[tokio::test]
async fn a_pinned_catalog_ignores_what_the_gateway_publishes() {
    let (dir, _app, _token, _policy, mut state) = build_test_app(100);
    let package = test_space_package();
    let gateway = MapGateway::serve(super::map_upgrade::upgraded_package(&package, [0, 0])).await;
    state.gateway_urls = vec![gateway.url.clone()];
    let state = disk_state(state, &dir.path().join("pinned.db"));
    assert!(!map_source::refresh(&state).await.unwrap());
    map_source::observe_gateway_version(&state, Some("the-ville"), Some("sha256:elsewhere"));
    assert_eq!(versions(&state.space_catalog), Some(package.space_version));
    gateway.server.abort();
}

/// A node that only reports its position never reads the Space from the gateway; the push
/// receipt is what tells it the map changed.
#[tokio::test]
async fn a_push_receipt_naming_another_map_swaps_it_in() {
    let (dir, _app, _token, _policy, mut state) = build_test_app(100);
    let package = test_space_package();
    let gateway = MapGateway::serve(package.clone()).await;
    state.gateway_urls = vec![gateway.url.clone()];
    let mut state = disk_state(state, &dir.path().join("receipt.db"));
    state.space_catalog = Arc::new(
        SpaceMaps::load(&state.local_db, &state.gateway_urls, None)
            .await
            .unwrap(),
    );
    let mut events = state.stream_tx.subscribe();
    initialize_space_presence(&state).await.unwrap();
    let event = loop {
        let event = events.recv().await.unwrap();
        if event.kind == "space.presence.updated" {
            break event;
        }
    };
    let signed = crate::build_signed_node_event(&state, &event)
        .unwrap()
        .unwrap();

    let closed = package
        .places
        .iter()
        .find(|place| place.place_id == "johnson-park")
        .and_then(|place| place.tiles.first().copied())
        .unwrap();
    let upgraded = super::map_upgrade::upgraded_package(&package, closed);
    assert_ne!(upgraded.space_version, package.space_version);
    gateway.publish(upgraded.clone());
    let receipt = crate::push_signed_node_event(&reqwest::Client::new(), &gateway.url, &signed)
        .await
        .unwrap();
    assert_eq!(receipt["current_version"], upgraded.space_version.as_str());
    crate::observe_push_receipt(&state, &signed, &receipt);
    wait_for_version(&state.space_catalog, &upgraded.space_version).await;

    // A receipt without a version, as every non-Space push gets, changes nothing.
    crate::observe_push_receipt(&state, &signed, &json!({"status": "ok"}));
    assert_eq!(versions(&state.space_catalog), Some(upgraded.space_version));
    gateway.server.abort();
}

/// A node that started with no map (gateway down, nothing cached) and whose agent then enters.
async fn mapless_state(gateway_url: &str) -> (tempfile::TempDir, String, ControlPlaneState) {
    let (dir, _app, token, _policy, mut state) = build_test_app(100);
    let mut state = {
        let path = dir.path().join("mapless.db");
        state.gateway_urls = vec![gateway_url.to_owned()];
        disk_state(state, &path)
    };
    state.space_catalog = Arc::new(
        SpaceMaps::load(&state.local_db, &["http://127.0.0.1:9".to_owned()], None)
            .await
            .unwrap(),
    );
    assert!(state.space_catalog.get().is_none());
    (dir, token, state)
}

/// Without a map no Space response arrives to name a version, so nothing would ever retry:
/// the request itself pulls the map.
#[tokio::test]
async fn a_node_without_a_map_pulls_it_when_its_agent_enters() {
    let package = test_space_package();
    let gateway = MapGateway::serve(package.clone()).await;
    let (_dir, _token, state) = mapless_state(&gateway.url).await;
    let entered = initialize_space_presence(&state).await.unwrap();
    assert_eq!(entered.record.space_version, package.space_version);
    assert_eq!(versions(&state.space_catalog), Some(package.space_version));
    assert_eq!(gateway.package_fetches.load(Ordering::SeqCst), 1);
    gateway.server.abort();
}

/// A refresh already running (it holds the lock, so `refresh` from the request would return
/// `Ok(false)`) is waited for, and its map is used without a second download.
#[tokio::test]
async fn an_entry_during_a_running_refresh_waits_for_its_map() {
    let package = test_space_package();
    let gateway = MapGateway::serve(package.clone()).await;
    let (_dir, _token, state) = mapless_state(&gateway.url).await;
    let (refreshed, entered) = tokio::join!(
        map_source::refresh(&state),
        initialize_space_presence(&state)
    );
    assert!(refreshed.unwrap());
    assert_eq!(entered.unwrap().record.space_version, package.space_version);
    assert_eq!(gateway.package_fetches.load(Ordering::SeqCst), 1);
    gateway.server.abort();
}

#[tokio::test]
async fn without_a_map_or_a_gateway_entry_reports_the_space_unavailable() {
    let (_dir, _token, state) = mapless_state("http://127.0.0.1:9").await;
    let error = initialize_space_presence(&state).await.unwrap_err();
    assert!(
        error.to_string().contains("Space is unavailable"),
        "{error}"
    );
}

/// The new map closes the tile the agent stands on, so it respawns; what it wears stays.
#[tokio::test]
async fn a_respawn_on_a_new_map_keeps_the_agents_skin() {
    let (dir, _app, token, _policy, mut state) = build_test_app(100);
    let package = test_space_package();
    let gateway = MapGateway::serve(package.clone()).await;
    state.gateway_urls = vec![gateway.url.clone()];
    let mut state = disk_state(state, &dir.path().join("skin-swap.db"));
    state.space_catalog = Arc::new(
        SpaceMaps::load(&state.local_db, &state.gateway_urls, None)
            .await
            .unwrap(),
    );
    initialize_space_presence(&state).await.unwrap();
    let dressed = authed_post_json(
        app(state.clone()),
        &token,
        "/v1/wattetheria/space/skin",
        json!({"skin_id": CUSTOM_SKIN}),
    )
    .await;
    let dressed: SignedSpacePresence = serde_json::from_value(dressed["presence"].clone()).unwrap();
    assert_eq!(dressed.record.skin_id.as_deref(), Some(CUSTOM_SKIN));
    let avatar = dressed.record.avatar_id;
    assert!(avatar.is_some());

    let upgraded = super::map_upgrade::upgraded_package(&package, dressed.record.from_tile);
    gateway.publish(upgraded.clone());
    assert!(map_source::refresh(&state).await.unwrap());

    let registry = state.own_space_presence.lock().await;
    let current = registry
        .visible_get(&test_network_id(&state), &upgraded)
        .unwrap()
        .clone();
    drop(registry);
    assert_ne!(current.record.from_tile, dressed.record.from_tile);
    assert_eq!(current.record.avatar_id, avatar);
    assert_eq!(current.record.skin_id.as_deref(), Some(CUSTOM_SKIN));
    gateway.server.abort();
}
