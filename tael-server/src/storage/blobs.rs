//! Content-addressed blob store for large payloads kept out of the columnar
//! tables — LLM prompts/completions and oversized log bodies.
//!
//! Blobs are keyed by `sha256(content)` and stored snap-compressed at
//! `<aa>/<bb>/<full-sha256>`. Identical content (e.g. a system prompt reused
//! across thousands of calls, or a repeated stack trace) is stored once — the
//! write is skipped when the hash already exists. See
//! `docs/tael-backend-design.md` → "Payload blobs".
//!
//! Storage sits on the shared [`ObjectBackend`](super::ObjectBackend): a local
//! directory by default (`<data_dir>/blobs`, overridable via `TAEL_BLOB_DIR`),
//! or a GCS bucket under the `cloud` feature. Content-addressing makes object
//! storage ideal — puts are idempotent and dedup is cross-node (two shards
//! writing the same payload collapse to one object).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::objstore::{DynObjectBackend, FsBackend};

/// A content-addressed blob store over a pluggable object backend.
pub struct BlobStore {
    backend: DynObjectBackend,
}

impl BlobStore {
    /// Open the blob store on the local filesystem at `<data_dir>/blobs`, or at
    /// `TAEL_BLOB_DIR` when set (e.g. a separate fast mount). This is the
    /// default, single-binary path.
    pub fn new(data_dir: &str) -> Result<Self> {
        let root = match std::env::var("TAEL_BLOB_DIR") {
            Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
            _ => Path::new(data_dir).join("blobs"),
        };
        Self::with_backend(std::sync::Arc::new(FsBackend::new(root)?))
    }

    /// Open the blob store on an arbitrary object backend (e.g. GCS). The path
    /// layout is identical, so the backend is a transparent swap.
    pub fn with_backend(backend: DynObjectBackend) -> Result<Self> {
        Ok(Self { backend })
    }

    /// Hash `content`, write it snap-compressed if not already present, and
    /// return the hex sha256. Idempotent: re-putting identical content is a
    /// cheap no-op (the dedup property).
    pub fn put(&self, content: &[u8]) -> Result<String> {
        let hash = hex::encode(Sha256::digest(content));
        let key = key_for(&hash);
        if self.backend.exists(&key)? {
            return Ok(hash);
        }
        let compressed = snap::raw::Encoder::new()
            .compress_vec(content)
            .context("compressing blob")?;
        self.backend.put(&key, &compressed)?;
        Ok(hash)
    }

    /// Fetch and decompress a blob by hex sha256. `Ok(None)` if it doesn't
    /// exist (e.g. GC'd under retention).
    pub fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        match self.backend.get(&key_for(hash))? {
            Some(compressed) => {
                let content = snap::raw::Decoder::new()
                    .decompress_vec(&compressed)
                    .context("decompressing blob")?;
                Ok(Some(content))
            }
            None => Ok(None),
        }
    }

    /// List every stored blob hash.
    pub fn list_hashes(&self) -> Result<Vec<String>> {
        Ok(self
            .backend
            .list("")?
            .iter()
            .filter_map(|key| key.rsplit('/').next().map(str::to_string))
            .collect())
    }

    /// Delete a blob by hash. Missing blobs are a no-op.
    pub fn remove(&self, hash: &str) -> Result<()> {
        self.backend.delete(&key_for(hash))
    }

    /// Mark-and-sweep GC: delete every blob whose hash is not in `live`.
    /// Returns the number of blobs removed. (Blobs are written before their
    /// referencing row, so an orphan = a row that never landed, or a row that
    /// retention has since dropped — both safe to collect.)
    ///
    /// **Single-owner contract:** on a shared object store this must run on
    /// exactly one node with a `live` set spanning all shards, or it will
    /// delete blobs other shards still reference. `lib.rs` enforces this by
    /// disabling per-node GC when the blob store is shared (GCS) unless this
    /// node is the designated coordinator.
    pub fn gc(&self, live: &std::collections::HashSet<String>) -> Result<usize> {
        let mut removed = 0;
        for hash in self.list_hashes()? {
            if !live.contains(&hash) {
                self.remove(&hash)?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// `<aa>/<bb>/<hash>` — two-level sharding to keep listing/dir sizes sane.
/// Falls back to a flat key for pathologically short hashes.
fn key_for(hash: &str) -> String {
    if hash.len() >= 4 {
        format!("{}/{}/{}", &hash[0..2], &hash[2..4], hash)
    } else {
        hash.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (BlobStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = BlobStore::new(dir.path().to_str().unwrap()).unwrap();
        (s, dir)
    }

    #[test]
    fn put_then_get_round_trips() {
        let (s, _d) = store();
        let content = b"You are a helpful assistant.";
        let hash = s.put(content).unwrap();
        assert_eq!(hash.len(), 64); // sha256 hex
        assert_eq!(s.get(&hash).unwrap().as_deref(), Some(&content[..]));
    }

    #[test]
    fn identical_content_dedups_to_one_file() {
        let (s, _d) = store();
        let h1 = s.put(b"same system prompt").unwrap();
        let h2 = s.put(b"same system prompt").unwrap();
        assert_eq!(h1, h2);
        // Exactly one blob exists.
        assert_eq!(
            s.list_hashes().unwrap().len(),
            1,
            "duplicate content should produce one blob"
        );
    }

    #[test]
    fn distinct_content_distinct_hashes() {
        let (s, _d) = store();
        assert_ne!(s.put(b"prompt a").unwrap(), s.put(b"prompt b").unwrap());
    }

    #[test]
    fn missing_hash_returns_none() {
        let (s, _d) = store();
        assert!(s.get(&"0".repeat(64)).unwrap().is_none());
    }

    #[test]
    fn gc_removes_only_unreferenced_blobs() {
        use std::collections::HashSet;
        let (s, _d) = store();
        let keep = s.put(b"referenced prompt").unwrap();
        let _drop = s.put(b"orphaned prompt").unwrap();
        assert_eq!(s.list_hashes().unwrap().len(), 2);

        let live: HashSet<String> = [keep.clone()].into_iter().collect();
        let removed = s.gc(&live).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(s.list_hashes().unwrap(), vec![keep.clone()]);
        assert!(s.get(&keep).unwrap().is_some());
    }

    #[test]
    fn blob_dir_env_override_relocates_store() {
        let data = tempfile::tempdir().unwrap();
        let blobs = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded test; restored immediately after construction.
        unsafe { std::env::set_var("TAEL_BLOB_DIR", blobs.path()) };
        let s = BlobStore::new(data.path().to_str().unwrap()).unwrap();
        unsafe { std::env::remove_var("TAEL_BLOB_DIR") };
        let hash = s.put(b"relocated").unwrap();
        // Lands under the override dir, not <data_dir>/blobs.
        assert!(s.get(&hash).unwrap().is_some());
        assert!(
            !data.path().join("blobs").exists(),
            "nothing should be written under data_dir when TAEL_BLOB_DIR is set"
        );
    }
}
