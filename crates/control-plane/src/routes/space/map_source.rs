//! Where this node's logical map comes from: the gateway publishes the Space catalog and its
//! packages, the node pulls them and keeps a copy in its local database.
//!
//! At startup the node asks the gateway for `catalog.json`. When the versions it names are the
//! ones already cached, the cache is used as is; otherwise the packages are downloaded,
//! verified and cached. Without a reachable gateway the cache serves; without either, the
//! Space is unavailable while the rest of the node runs.
//!
//! At runtime nothing polls. Every Space response from the gateway carries the
//! `current_version` of the map it serves; when that differs from the map this node holds, one
//! refresh pulls the new catalog and package, verifies them and swaps the map in under the
//! presence lock, so no walk is signed half on the old map and half on the new. A version this
//! node does not know on another agent's record only earns one look at the catalog: the gateway's
//! `current_version` decides, never a record.
//!
//! `--space-catalog-path` keeps a local catalog pinned for debugging; the gateway never replaces
//! it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use wattetheria_kernel::local_db::{LocalDb, domain};
use wattetheria_kernel::space::{SpaceCatalog, SpaceCatalogEntry};

use crate::routes::mcp::normalized_gateway_api_resource_url;
use crate::state::ControlPlaneState;

const CATALOG_PATH: &str = "/api/spaces/catalog.json";

/// The map this node walks, and how it keeps up with the gateway.
#[derive(Debug)]
pub struct SpaceMaps {
    current: RwLock<Option<Arc<SpaceCatalog>>>,
    /// Map versions seen on other agents' records that already sent one catalog check.
    checked: Mutex<BTreeSet<String>>,
    /// One refresh at a time; a caller that finds one running leaves it to finish.
    refresh: tokio::sync::Mutex<()>,
    /// Refreshes started so far, so a request that waited on one does not start another.
    attempts: AtomicU64,
    /// A catalog from `--space-catalog-path`: the gateway never replaces it.
    pinned: bool,
}

impl SpaceMaps {
    /// A map the gateway never replaces: the `--space-catalog-path` override, or a test map.
    #[must_use]
    pub fn pinned(catalog: Option<SpaceCatalog>) -> Self {
        Self {
            current: RwLock::new(catalog.map(Arc::new)),
            checked: Mutex::default(),
            refresh: tokio::sync::Mutex::default(),
            attempts: AtomicU64::new(0),
            pinned: true,
        }
    }

    fn from_gateway(catalog: Option<SpaceCatalog>) -> Self {
        Self {
            pinned: false,
            ..Self::pinned(catalog)
        }
    }

    /// The map in use, `None` while the Space is unavailable.
    #[must_use]
    pub fn get(&self) -> Option<Arc<SpaceCatalog>> {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Where the map came from, for the startup log.
    #[must_use]
    pub fn source(&self) -> &'static str {
        match (self.pinned, self.get()) {
            (true, _) => "catalog_path",
            (false, Some(_)) => "gateway",
            (false, None) => "unavailable",
        }
    }

    fn replace(&self, catalog: Arc<SpaceCatalog>) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = Some(catalog);
    }

    fn current_version(&self, space_id: Option<&str>) -> Option<String> {
        let catalog = self.get()?;
        let space_id = space_id.unwrap_or(&catalog.default_space_id);
        catalog.current_version(space_id).map(str::to_owned)
    }

    /// Loads the map at startup: the override path, else the gateway's catalog against the
    /// cache, else the cache alone.
    pub async fn load(
        local_db: &LocalDb,
        gateway_urls: &[String],
        override_path: Option<&Path>,
    ) -> Result<Self> {
        if let Some(path) = override_path {
            return Ok(Self::pinned(Some(SpaceCatalog::load(path)?)));
        }
        let cached = match local_db.load_domain::<CachedCatalog>(domain::SPACE_CATALOG_CACHE) {
            Ok(cached) => cached.and_then(|cached| match cached.build() {
                Ok(catalog) => Some(catalog),
                Err(error) => {
                    tracing::warn!(%error, "cached Space catalog is unusable; ignoring it");
                    None
                }
            }),
            Err(error) => {
                tracing::warn!(%error, "cached Space catalog could not be read");
                None
            }
        };
        if gateway_urls.is_empty() {
            return Ok(Self::from_gateway(cached));
        }
        let client = super::gateway_client()?;
        match Published::fetch(&client, gateway_urls).await {
            Ok(published) => {
                if cached
                    .as_ref()
                    .is_some_and(|cached| published.matches(cached))
                {
                    tracing::info!("Space catalog matches the cached map");
                    return Ok(Self::from_gateway(cached));
                }
                match published.download(&client).await {
                    Ok((cached, catalog)) => {
                        local_db.save_domain(domain::SPACE_CATALOG_CACHE, &cached)?;
                        tracing::info!("Space catalog pulled from the gateway");
                        Ok(Self::from_gateway(Some(catalog)))
                    }
                    Err(error) => {
                        tracing::warn!(%error, cached = cached.is_some(), "Space package download failed; keeping the cached map");
                        Ok(Self::from_gateway(cached))
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, cached = cached.is_some(), "Space catalog unavailable from the gateway; Space runs on the cached map if there is one");
                Ok(Self::from_gateway(cached))
            }
        }
    }
}

/// The catalog and the packages it names, as the gateway published them, kept in the local
/// database between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedCatalog {
    catalog: String,
    /// Package JSON by the `package_path` the catalog gives it.
    packages: BTreeMap<String, String>,
}

impl CachedCatalog {
    fn build(&self) -> Result<SpaceCatalog> {
        SpaceCatalog::from_sources(self.catalog.as_bytes(), |path| {
            let key = path.to_str().context("package path is not UTF-8")?;
            self.packages
                .get(key)
                .map(|json| json.clone().into_bytes())
                .with_context(|| format!("package {key} is not in the cache"))
        })
    }
}

/// The catalog the gateway publishes, before its packages are fetched.
#[derive(Debug, Deserialize)]
struct PublishedCatalog {
    default_space_id: String,
    spaces: Vec<SpaceCatalogEntry>,
}

struct Published {
    catalog_url: reqwest::Url,
    json: String,
    catalog: PublishedCatalog,
}

impl Published {
    /// The catalog from the first gateway that answers.
    async fn fetch(client: &reqwest::Client, gateway_urls: &[String]) -> Result<Self> {
        let mut last_error = None;
        for gateway in gateway_urls {
            match Self::fetch_from(client, gateway).await {
                Ok(published) => return Ok(published),
                Err(error) => {
                    tracing::debug!(gateway, %error, "Space catalog not fetched");
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no gateway configured")))
    }

    async fn fetch_from(client: &reqwest::Client, gateway: &str) -> Result<Self> {
        let catalog_url = reqwest::Url::parse(&normalized_gateway_api_resource_url(
            gateway,
            CATALOG_PATH,
            CATALOG_PATH,
        ))?;
        let json = client
            .get(catalog_url.clone())
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let catalog: PublishedCatalog =
            serde_json::from_str(&json).context("decode the published Space catalog")?;
        ensure!(
            !catalog.spaces.is_empty(),
            "the published Space catalog names no Space"
        );
        Ok(Self {
            catalog_url,
            json,
            catalog,
        })
    }

    /// Whether `local` is at the versions this catalog publishes.
    fn matches(&self, local: &SpaceCatalog) -> bool {
        let versions = |entries: &[SpaceCatalogEntry]| {
            entries
                .iter()
                .map(|entry| (entry.space_id.clone(), entry.current_version.clone()))
                .collect::<BTreeSet<_>>()
        };
        self.catalog.default_space_id == local.default_space_id
            && versions(&self.catalog.spaces) == versions(&local.spaces)
    }

    /// Fetches every package the catalog names, relative to the catalog like a static site,
    /// and builds the verified catalog from them.
    async fn download(&self, client: &reqwest::Client) -> Result<(CachedCatalog, SpaceCatalog)> {
        let mut packages = BTreeMap::new();
        for entry in &self.catalog.spaces {
            for version in &entry.versions {
                let path = version
                    .package_path
                    .to_str()
                    .context("package path is not UTF-8")?;
                if packages.contains_key(path) {
                    continue;
                }
                let url = self
                    .catalog_url
                    .join(path)
                    .with_context(|| format!("resolve package {path}"))?;
                let json = client
                    .get(url)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await
                    .with_context(|| format!("download package {path}"))?;
                packages.insert(path.to_owned(), json);
            }
        }
        let cached = CachedCatalog {
            catalog: self.json.clone(),
            packages,
        };
        let catalog = cached.build()?;
        Ok((cached, catalog))
    }
}

/// Notes the `current_version` a gateway Space response reports for `space_id` (the default
/// Space when none). A version other than the one this node holds starts one refresh.
pub(crate) fn observe_gateway_version(
    state: &ControlPlaneState,
    space_id: Option<&str>,
    version: Option<&str>,
) {
    let Some(version) = version else { return };
    if state.space_catalog.pinned
        || state.space_catalog.current_version(space_id).as_deref() == Some(version)
    {
        return;
    }
    tracing::info!(space_id = ?space_id, version, "gateway publishes another map version");
    spawn_refresh(state);
}

/// Notes the version another agent's record was signed on. A version this node has not seen
/// earns one look at the catalog; the gateway's `current_version` decides what to use.
pub(crate) fn observe_record_version(state: &ControlPlaneState, space_id: &str, version: &str) {
    if state.space_catalog.pinned
        || state
            .space_catalog
            .current_version(Some(space_id))
            .as_deref()
            == Some(version)
    {
        return;
    }
    let first_sight = state
        .space_catalog
        .checked
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(version.to_owned());
    if first_sight {
        tracing::info!(
            space_id,
            version,
            "a record carries an unknown map version; checking the catalog"
        );
        spawn_refresh(state);
    }
}

fn spawn_refresh(state: &ControlPlaneState) {
    let state = state.clone();
    tokio::spawn(async move {
        match refresh(&state).await {
            Ok(true) => tracing::info!("Space map updated from the gateway"),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, "Space map refresh failed; keeping the current map");
            }
        }
    });
}

/// Pulls the catalog and, when it names other versions than this node holds, the packages;
/// verifies them, caches them and swaps the map in. `Ok(true)` when the map changed. A
/// refresh already running is left to finish.
pub(crate) async fn refresh(state: &ControlPlaneState) -> Result<bool> {
    let maps = &state.space_catalog;
    if maps.pinned {
        return Ok(false);
    }
    let Ok(_running) = maps.refresh.try_lock() else {
        return Ok(false);
    };
    refresh_locked(state).await
}

/// The map for a Space request. A node without one (no gateway at startup, or the package
/// download failed with nothing cached) pulls it now instead of waiting for a version hint that
/// never comes: it waits for a refresh already running, or runs one. Requests that queued
/// behind a refresh read its result rather than each starting another. Without a map after
/// that the Space is unavailable, as before.
pub(crate) async fn ensure_map(state: &ControlPlaneState) -> Result<Arc<SpaceCatalog>> {
    let maps = &state.space_catalog;
    if maps.get().is_none() && !maps.pinned && !state.gateway_urls.is_empty() {
        let seen = maps.attempts.load(Ordering::SeqCst);
        let _running = maps.refresh.lock().await;
        if maps.get().is_none()
            && maps.attempts.load(Ordering::SeqCst) == seen
            && let Err(error) = refresh_locked(state).await
        {
            tracing::warn!(%error, "Space map not available from the gateway");
        }
    }
    super::registry::catalog(state)
}

/// The body of a refresh, under the refresh lock.
async fn refresh_locked(state: &ControlPlaneState) -> Result<bool> {
    let maps = &state.space_catalog;
    maps.attempts.fetch_add(1, Ordering::SeqCst);
    if state.gateway_urls.is_empty() {
        bail!("no gateway configured");
    }
    let client = super::gateway_client()?;
    let published = Published::fetch(&client, &state.gateway_urls).await?;
    if maps
        .get()
        .is_some_and(|current| published.matches(&current))
    {
        return Ok(false);
    }
    let (cached, catalog) = published.download(&client).await?;
    state
        .local_db
        .save_domain(domain::SPACE_CATALOG_CACHE, &cached)?;
    super::registry::adopt_map(state, Arc::new(catalog)).await?;
    Ok(true)
}

impl SpaceMaps {
    /// Swaps the map; the caller holds the presence lock so no signing straddles the change.
    pub(super) fn swap(&self, catalog: Arc<SpaceCatalog>) {
        self.replace(catalog);
    }
}
