//! Content-addressed blob store for large payloads kept out of the columnar
//! tables — LLM prompts/completions and oversized log bodies.
//!
//! Blobs are keyed by `sha256(content)` and stored snap-compressed at
//! `blobs/<aa>/<bb>/<full-sha256>`. Identical content (e.g. a system prompt
//! reused across thousands of calls, or a repeated stack trace) is stored
//! once — the write is skipped when the hash already exists. See
//! `docs/tael-backend-design.md` → "Payload blobs".

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// A local-filesystem content-addressed blob store. The same path layout is a
/// valid object-store key prefix, so a future S3/R2 backend is a swap behind
/// this type (design Phase 9).
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    /// Open (creating if needed) the blob store rooted at `<data_dir>/blobs`.
    pub fn new(data_dir: &str) -> Result<Self> {
        let root = Path::new(data_dir).join("blobs");
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating blob dir {}", root.display()))?;
        Ok(Self { root })
    }

    /// Hash `content`, write it snap-compressed if not already present, and
    /// return the hex sha256. Idempotent: re-putting identical content is a
    /// cheap no-op (the dedup property).
    pub fn put(&self, content: &[u8]) -> Result<String> {
        let hash = hex::encode(Sha256::digest(content));
        let path = self.path_for(&hash);
        if path.exists() {
            return Ok(hash);
        }
        let compressed = snap::raw::Encoder::new()
            .compress_vec(content)
            .context("compressing blob")?;
        // Parent dirs: blobs/<aa>/<bb>/
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating blob shard {}", parent.display()))?;
        }
        // Write to a temp file then rename, so a concurrent reader never sees a
        // half-written blob.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &compressed)
            .with_context(|| format!("writing blob {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("finalizing blob {}", path.display()))?;
        Ok(hash)
    }

    /// Fetch and decompress a blob by hex sha256. `Ok(None)` if it doesn't
    /// exist (e.g. GC'd under retention).
    pub fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path_for(hash);
        match std::fs::read(&path) {
            Ok(compressed) => {
                let content = snap::raw::Decoder::new()
                    .decompress_vec(&compressed)
                    .context("decompressing blob")?;
                Ok(Some(content))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading blob {hash}")),
        }
    }

    /// List every stored blob hash (walks the shard tree).
    pub fn list_hashes(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        collect_hashes(&self.root, &mut out)?;
        Ok(out)
    }

    /// Delete a blob by hash. Missing blobs are a no-op.
    pub fn remove(&self, hash: &str) -> Result<()> {
        match std::fs::remove_file(self.path_for(hash)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("removing blob {hash}")),
        }
    }

    /// Mark-and-sweep GC: delete every blob whose hash is not in `live`.
    /// Returns the number of blobs removed. (Blobs are written before their
    /// referencing row, so an orphan = a row that never landed, or a row that
    /// retention has since dropped — both safe to collect.)
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

    /// `blobs/<aa>/<bb>/<hash>` — two-level sharding to keep directory sizes
    /// sane. Falls back to a flat path for pathologically short hashes.
    fn path_for(&self, hash: &str) -> PathBuf {
        if hash.len() >= 4 {
            self.root.join(&hash[0..2]).join(&hash[2..4]).join(hash)
        } else {
            self.root.join(hash)
        }
    }
}

/// Collect blob hashes (leaf filenames, skipping temp files) under `dir`.
fn collect_hashes(dir: &Path, out: &mut Vec<String>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_hashes(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) != Some("tmp") {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                out.push(name.to_string());
            }
        }
    }
    Ok(())
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
        // Exactly one blob file exists on disk.
        let count = walk_count(&s.root);
        assert_eq!(count, 1, "duplicate content should produce one blob");
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

    fn walk_count(dir: &Path) -> usize {
        let mut n = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                n += walk_count(&path);
            } else if path.extension().and_then(|e| e.to_str()) != Some("tmp") {
                n += 1;
            }
        }
        n
    }
}
