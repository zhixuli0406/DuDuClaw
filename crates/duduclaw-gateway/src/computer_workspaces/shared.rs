//! One registry handle per home, opened off the async threads (review L4).
//!
//! Opening the registry touches the file system (canonicalize, chmod,
//! schema), so the gateway opens it once per home, on a blocking thread,
//! and keeps the handle. Queries on the cached handle are short WAL
//! transactions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use super::store::{StoreError, WorkspaceStore};

fn cache() -> &'static Mutex<HashMap<PathBuf, Arc<WorkspaceStore>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<WorkspaceStore>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The cached handle of `home`, if one was opened.
pub fn cached(home: &Path) -> Option<Arc<WorkspaceStore>> {
    cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(home)
        .cloned()
}

/// The handle of `home`, opening it when needed. Blocking: call it from a
/// blocking thread or [`shared_async`].
pub fn shared_blocking(home: &Path) -> Result<Arc<WorkspaceStore>, StoreError> {
    if let Some(store) = cached(home) {
        return Ok(store);
    }
    let store = Arc::new(WorkspaceStore::open(home)?);
    let mut map = cache().lock().unwrap_or_else(|p| p.into_inner());
    Ok(map.entry(home.to_path_buf()).or_insert(store).clone())
}

/// [`shared_blocking`] from async code: a cache hit is immediate, a miss
/// opens on a blocking thread.
pub async fn shared_async(home: &Path) -> Result<Arc<WorkspaceStore>, StoreError> {
    if let Some(store) = cached(home) {
        return Ok(store);
    }
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || shared_blocking(&home))
        .await
        .map_err(|e| StoreError::Unavailable(format!("registry open task: {e}")))?
}
