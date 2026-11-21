//! Process-wide desktop state: the data root and reusable warm caches
//! (e.g., live MCP server connections so turns avoid respawning stdio tools).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use zerith_core::Paths;
use zerith_mcp::McpRegistry;
use zerith_sessions::Store;

type McpCacheEntry = ((PathBuf, u64), Arc<McpRegistry>);

/// Bounded cache of connected [`McpRegistry`] instances keyed by `(workspace, fingerprint)`.
#[derive(Default)]
pub struct McpCache {
    generation: AtomicU64,
    entries: Mutex<Vec<McpCacheEntry>>,
}

impl McpCache {
    /// Increments the cache generation and drops all warm MCP registries (and
    /// their stdio child processes).
    pub fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = self.entries.lock() {
            guard.clear();
        }
    }

    pub fn get_or_build(
        &self,
        workspace: &Path,
        fingerprint: u64,
        max_entries: usize,
        build: impl FnOnce() -> McpRegistry,
    ) -> Arc<McpRegistry> {
        let gen = self.generation.load(Ordering::SeqCst);
        let key_hash = {
            let mut h = DefaultHasher::new();
            gen.hash(&mut h);
            fingerprint.hash(&mut h);
            h.finish()
        };
        let key = (workspace.to_path_buf(), key_hash);
        if let Ok(guard) = self.entries.lock() {
            if let Some((_, existing)) = guard.iter().find(|(k, _)| *k == key) {
                return Arc::clone(existing);
            }
        }
        let built = Arc::new(build());
        // If invalidated while we were building, do not re-insert stale state.
        if self.generation.load(Ordering::SeqCst) == gen {
            if let Ok(mut guard) = self.entries.lock() {
                if let Some((_, existing)) = guard.iter().find(|(k, _)| *k == key) {
                    return Arc::clone(existing);
                }
                let cap = max_entries.clamp(1, 256);
                while guard.len() >= cap {
                    guard.remove(0);
                }
                guard.push((key, Arc::clone(&built)));
            }
        }
        built
    }
}

/// Computes a fast filesystem metadata fingerprint over the config files that
/// define which MCP servers exist (`mcp.toml`, workspace `.zerith/mcp.toml`,
/// `settings.toml`, and `plugins/manifest.json`).
pub fn mcp_config_fingerprint(root: &Path, workspace: &Path) -> u64 {
    let mut h = DefaultHasher::new();
    let paths = [
        root.join("mcp.toml"),
        root.join("settings.toml"),
        root.join("plugins").join("manifest.json"),
        workspace.join(".zerith").join("mcp.toml"),
        workspace.join(".zerith").join("settings.toml"),
    ];
    for p in &paths {
        p.hash(&mut h);
        match std::fs::metadata(p) {
            Ok(m) => {
                true.hash(&mut h);
                m.len().hash(&mut h);
                if let Ok(t) = m.modified() {
                    if let Ok(d) = t.duration_since(UNIX_EPOCH) {
                        d.as_nanos().hash(&mut h);
                    }
                }
            }
            Err(_) => false.hash(&mut h),
        }
    }
    h.finish()
}

type StoredConn = Mutex<Option<(PathBuf, Store)>>;

/// Process-wide state handed to every command.
#[derive(Clone)]
pub struct AppState {
    root: PathBuf,
    mcp_cache: Arc<McpCache>,
    store: Arc<StoredConn>,
}

impl AppState {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            mcp_cache: Arc::new(McpCache::default()),
            store: Arc::new(Mutex::new(None)),
        }
    }

    /// The resolved data root (`ZERITH_HOME`, else the OS user dir).
    pub fn resolve() -> Self {
        Self::new(Paths::resolve().root().to_path_buf())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn mcp_cache(&self) -> Arc<McpCache> {
        Arc::clone(&self.mcp_cache)
    }

    pub fn invalidate_mcp_cache(&self) {
        self.mcp_cache.invalidate();
    }

    /// Runs `f` against a cached SQLite [`Store`] connection for this `AppState`,
    /// reopening transparently only if the DB path changed or no handle exists.
    pub fn with_store<T>(&self, f: impl FnOnce(&Store) -> Result<T, String>) -> Result<T, String> {
        let db = self.db();
        let mut guard = self
            .store
            .lock()
            .map_err(|_| "banco em estado inconsistente".to_string())?;
        let needs_open = match guard.as_ref() {
            Some((p, _)) => *p != db,
            None => true,
        };
        if needs_open {
            let st = Store::open(&db).map_err(|e| e.to_string())?;
            *guard = Some((db, st));
        }
        let (_, store) = guard.as_ref().expect("store populated above");
        f(store)
    }

    pub fn reset_store(&self) {
        if let Ok(mut guard) = self.store.lock() {
            *guard = None;
        }
    }

    /// `<root>/zerith.db`: sessions, memory and usage in one file, as the CLI
    /// uses it.
    pub fn db(&self) -> PathBuf {
        Paths::with_root(&self.root).db()
    }

    /// Creates the folder layout on first run; idempotent.
    pub fn ensure(&self) -> std::io::Result<()> {
        Paths::with_root(&self.root).ensure()
    }
}
