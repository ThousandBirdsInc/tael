//! Shared test fixtures for anything that needs a real storage engine.
//!
//! walrus keys its WAL by a process-global namespace, so two [`TaelBackend`]
//! instances built with the same key inside one test binary fight over the same
//! WAL directory — a failure that only shows up when tests run concurrently,
//! not when they run alone. Every test backend therefore gets a unique key, and
//! [`TestBackend`] cleans that namespace up on drop.

use std::sync::Arc;

use super::{BlobStore, TaelBackend};

/// A storage engine scoped to one test: unique WAL namespace, temp data dir,
/// both removed on drop.
pub(crate) struct TestBackend {
    pub backend: Arc<TaelBackend>,
    pub blobs: Arc<BlobStore>,
    /// Held for its `Drop` — removes the temp data directory.
    _dir: tempfile::TempDir,
    wal_key: String,
}

impl TestBackend {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().to_str().expect("utf-8 temp path").to_string();
        let wal_key = format!("tael-test-{}", uuid::Uuid::new_v4());
        let backend = Arc::new(
            TaelBackend::with_wal_key(&path, &wal_key).expect("open test storage backend"),
        );
        let blobs = Arc::new(BlobStore::new(&path).expect("open test blob store"));
        Self {
            backend,
            blobs,
            _dir: dir,
            wal_key,
        }
    }

    /// The engine as a `dyn Store` handle, which is what most call sites want.
    pub(crate) fn store(&self) -> Arc<dyn super::Store> {
        Arc::clone(&self.backend) as Arc<dyn super::Store>
    }
}

impl Drop for TestBackend {
    fn drop(&mut self) {
        // The WAL lives outside the temp dir (walrus resolves it against the
        // process-wide WALRUS_DATA_DIR), so it needs its own cleanup.
        let _ = std::fs::remove_dir_all(format!("wal_files/{}", self.wal_key));
    }
}
