//! Pluggable object backend shared by the cold (Parquet) tier and the
//! content-addressed blob store.
//!
//! Both tiers are key/value over an immutable-object namespace: write a whole
//! object at a `/`-separated key, read it back, list a prefix, delete. That is
//! exactly the surface an object store (GCS/S3) exposes, and also what a local
//! directory tree gives us. [`ObjectBackend`] is that surface.
//!
//! **Local-first is the spine.** [`FsBackend`] (a local directory) is always
//! compiled and is the default — `tael serve` on a laptop touches no new
//! dependency and keeps its exact on-disk layout and behavior. The GCS backend
//! is compiled only under the `cloud` feature and selected at runtime via
//! config; absent that feature, asking for it fails loudly at startup. See
//! `docs/tael-backend-design.md` → "Cold tier" / "Payload blobs" and the
//! local-first guarantee in the cloud-scale plan.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

/// A flat namespace of immutable objects addressed by `/`-separated keys.
///
/// Keys never start with `/` and are relative to the backend's root/prefix
/// (e.g. `spans/date=2026-05-28/hour=14/spans-<ulid>.parquet` or
/// `aa/bb/<sha256>`). Implementations must make [`put`](Self::put) atomic — a
/// concurrent [`get`](Self::get) sees either the old object or the new one,
/// never a partial write.
pub trait ObjectBackend: Send + Sync {
    /// Write `bytes` at `key`, atomically, creating any intermediate structure.
    /// Overwrites an existing object at `key`.
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()>;

    /// Read the object at `key`. `Ok(None)` if it does not exist.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// Whether an object exists at `key` (cheaper than [`get`](Self::get) when
    /// the body isn't needed — backs blob dedup).
    fn exists(&self, key: &str) -> Result<bool>;

    /// List the full keys of every object under `prefix` (recursive). `prefix`
    /// is a key prefix on `/` boundaries; `""` lists everything.
    fn list(&self, prefix: &str) -> Result<Vec<String>>;

    /// Delete the object at `key`. A missing object is a no-op.
    fn delete(&self, key: &str) -> Result<()>;
}

/// Shared handle to an object backend.
pub type DynObjectBackend = Arc<dyn ObjectBackend>;

/// Where a tier's objects live. Parsed from `TAEL_COLD_STORE` / `TAEL_BLOB_STORE`
/// (`fs` | `gcs`); `fs` is the default so a bare local run is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StoreLocation {
    #[default]
    Fs,
    Gcs,
}

impl StoreLocation {
    /// Parse a location name. Anything that isn't explicitly `gcs` is `fs`.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "gcs" | "google" | "gs" => StoreLocation::Gcs,
            _ => StoreLocation::Fs,
        }
    }
}

/// Open an object backend for one tier.
///
/// * [`StoreLocation::Fs`] → a [`FsBackend`] rooted at `fs_root` (always
///   available, zero extra dependencies).
/// * [`StoreLocation::Gcs`] → a GCS-backed store at `bucket_url`
///   (`gs://bucket/optional/prefix`). Requires the `cloud` feature; without it
///   this returns an error that names the fix, so a default build can never
///   silently fall back to local when the operator asked for GCS.
pub fn open_object_backend(
    location: StoreLocation,
    fs_root: &Path,
    bucket_url: Option<&str>,
) -> Result<DynObjectBackend> {
    match location {
        StoreLocation::Fs => Ok(Arc::new(FsBackend::new(fs_root)?)),
        StoreLocation::Gcs => {
            let url = bucket_url
                .filter(|u| !u.trim().is_empty())
                .context("GCS store selected but no bucket URL configured")?;
            open_gcs_backend(url)
        }
    }
}

#[cfg(feature = "cloud")]
fn open_gcs_backend(bucket_url: &str) -> Result<DynObjectBackend> {
    Ok(Arc::new(cloud_store::gcs_from_url(bucket_url)?))
}

#[cfg(not(feature = "cloud"))]
fn open_gcs_backend(_bucket_url: &str) -> Result<DynObjectBackend> {
    anyhow::bail!(
        "GCS object storage is not included in this build; rebuild with `--features cloud` \
         to use TAEL_COLD_STORE=gcs / TAEL_BLOB_STORE=gcs"
    )
}

// ── Local filesystem backend (always compiled, the default) ──────────────

/// An [`ObjectBackend`] over a local directory tree. Keys map to paths under
/// `root`; writes go to a `<key>.tmp` sibling and are renamed into place so a
/// reader never observes a half-written object — the same durability the cold
/// tier and blob store relied on before this abstraction existed.
pub struct FsBackend {
    root: PathBuf,
}

impl FsBackend {
    /// Open (creating if needed) a filesystem backend rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating object dir {}", root.display()))?;
        Ok(Self { root })
    }

    fn path_for(&self, key: &str) -> PathBuf {
        let mut p = self.root.clone();
        for seg in key.split('/') {
            if !seg.is_empty() {
                p.push(seg);
            }
        }
        p
    }
}

impl ObjectBackend for FsBackend {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating object parent {}", parent.display()))?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes).with_context(|| format!("writing object {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("finalizing object {}", path.display()))?;
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match std::fs::read(self.path_for(key)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading object {key}")),
        }
    }

    fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.path_for(key).exists())
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        collect_keys(&self.root, &self.path_for(prefix), &mut out)?;
        Ok(out)
    }

    fn delete(&self, key: &str) -> Result<()> {
        match std::fs::remove_file(self.path_for(key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("removing object {key}")),
        }
    }
}

/// Recursively collect object keys (paths relative to `root`, `/`-joined,
/// skipping transient `.tmp` files) under `dir`.
fn collect_keys(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_keys(root, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) != Some("tmp") {
            if let Ok(rel) = path.strip_prefix(root) {
                // Normalize to '/'-separated keys regardless of OS separator.
                let key = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push(key);
            }
        }
    }
    Ok(())
}

// ── object_store-backed backend (cloud feature only) ─────────────────────

#[cfg(feature = "cloud")]
mod cloud_store {
    use std::sync::{Arc, OnceLock};

    use anyhow::{Context, Result};
    use bytes::Bytes;
    use object_store::{ObjectStore, gcp::GoogleCloudStorageBuilder, path::Path as ObjPath};
    use tokio::runtime::Runtime;
    use tokio_stream::StreamExt;

    use super::ObjectBackend;

    /// Build an [`ObjStoreBackend`] over Google Cloud Storage from a
    /// `gs://bucket[/prefix]` URL. Authentication is ambient (ADC / Workload
    /// Identity / `GOOGLE_APPLICATION_CREDENTIALS`) — no credentials handled here.
    pub fn gcs_from_url(url: &str) -> Result<ObjStoreBackend> {
        let rest = url
            .strip_prefix("gs://")
            .or_else(|| url.strip_prefix("gcs://"))
            .unwrap_or(url);
        let mut parts = rest.splitn(2, '/');
        let bucket = parts
            .next()
            .filter(|b| !b.is_empty())
            .context("GCS bucket URL missing bucket name")?;
        let prefix = parts.next().unwrap_or("").trim_matches('/');
        let store = GoogleCloudStorageBuilder::from_env()
            .with_bucket_name(bucket)
            .build()
            .with_context(|| format!("opening GCS bucket {bucket}"))?;
        Ok(ObjStoreBackend::new(Arc::new(store), prefix))
    }

    /// A dedicated multi-thread runtime that owns all object-store I/O. Kept
    /// separate from the server's main runtime so the synchronous [`Store`]
    /// trait can drive async `object_store` calls from any thread — including a
    /// main-runtime worker — without the "cannot block within a runtime" panic
    /// that `Runtime::block_on` / `Handle::block_on` raise. Work is submitted
    /// with `spawn` and awaited over a std channel (no tokio runtime guard), so
    /// it is safe regardless of the caller's context. This is the
    /// `reqwest::blocking` pattern.
    fn io_runtime() -> &'static Runtime {
        static RT: OnceLock<Runtime> = OnceLock::new();
        RT.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("tael-objstore-io")
                .build()
                .expect("building object-store IO runtime")
        })
    }

    /// Run `fut` to completion on the dedicated IO runtime, blocking the caller.
    fn block<F, T>(fut: F) -> T
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        io_runtime().spawn(async move {
            let _ = tx.send(fut.await);
        });
        rx.recv().expect("object-store IO worker dropped")
    }

    /// An [`ObjectBackend`] over any `object_store::ObjectStore` (GCS in
    /// production; `InMemory` in tests), namespaced under a key prefix. The
    /// async `object_store` calls are driven on the dedicated IO runtime so the
    /// synchronous backend surface holds.
    pub struct ObjStoreBackend {
        store: Arc<dyn ObjectStore>,
        /// Key prefix within the bucket (the path component of the `gs://` URL).
        prefix: ObjPath,
    }

    impl ObjStoreBackend {
        /// Wrap an object store, namespacing all keys under `prefix`.
        pub fn new(store: Arc<dyn ObjectStore>, prefix: &str) -> Self {
            Self {
                store,
                prefix: ObjPath::from(prefix.trim_matches('/')),
            }
        }

        /// Prepend the configured prefix to a logical key.
        fn full(&self, key: &str) -> ObjPath {
            let mut p = self.prefix.clone();
            for seg in key.split('/').filter(|s| !s.is_empty()) {
                p = p.child(seg);
            }
            p
        }

        /// Strip the configured prefix from a stored path to recover the
        /// logical key.
        fn strip(&self, path: &ObjPath) -> String {
            let full = path.as_ref();
            let pre = self.prefix.as_ref();
            let rel = if pre.is_empty() {
                full
            } else {
                full.strip_prefix(pre)
                    .map(|s| s.trim_start_matches('/'))
                    .unwrap_or(full)
            };
            rel.to_string()
        }
    }

    impl ObjectBackend for ObjStoreBackend {
        fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
            let store = Arc::clone(&self.store);
            let path = self.full(key);
            let payload = Bytes::copy_from_slice(bytes);
            block(async move { store.put(&path, payload.into()).await })
                .with_context(|| format!("object put {key}"))?;
            Ok(())
        }

        fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
            let store = Arc::clone(&self.store);
            let path = self.full(key);
            let res = block(async move {
                match store.get(&path).await {
                    Ok(r) => match r.bytes().await {
                        Ok(b) => Ok(Some(b.to_vec())),
                        Err(e) => Err(e),
                    },
                    Err(object_store::Error::NotFound { .. }) => Ok(None),
                    Err(e) => Err(e),
                }
            })
            .with_context(|| format!("object get {key}"))?;
            Ok(res)
        }

        fn exists(&self, key: &str) -> Result<bool> {
            let store = Arc::clone(&self.store);
            let path = self.full(key);
            let found = block(async move {
                match store.head(&path).await {
                    Ok(_) => Ok::<_, object_store::Error>(true),
                    Err(object_store::Error::NotFound { .. }) => Ok(false),
                    Err(e) => Err(e),
                }
            })
            .with_context(|| format!("object head {key}"))?;
            Ok(found)
        }

        fn list(&self, prefix: &str) -> Result<Vec<String>> {
            let store = Arc::clone(&self.store);
            let full_prefix = self.full(prefix);
            let locations = block(async move {
                let mut out = Vec::new();
                let mut stream = store.list(Some(&full_prefix));
                while let Some(meta) = stream.next().await {
                    out.push(meta?.location);
                }
                Ok::<_, object_store::Error>(out)
            })
            .with_context(|| format!("object list {prefix}"))?;
            // Recover the logical keys by stripping the configured prefix.
            Ok(locations.iter().map(|loc| self.strip(loc)).collect())
        }

        fn delete(&self, key: &str) -> Result<()> {
            let store = Arc::clone(&self.store);
            let path = self.full(key);
            block(async move {
                match store.delete(&path).await {
                    Ok(()) => Ok(()),
                    Err(object_store::Error::NotFound { .. }) => Ok(()),
                    Err(e) => Err(e),
                }
            })
            .with_context(|| format!("object delete {key}"))?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs() -> (DynObjectBackend, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let b = open_object_backend(StoreLocation::Fs, dir.path(), None).unwrap();
        (b, dir)
    }

    #[test]
    fn put_get_round_trips() {
        let (b, _d) = fs();
        b.put("aa/bb/obj", b"hello").unwrap();
        assert_eq!(b.get("aa/bb/obj").unwrap().as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn missing_get_is_none_and_exists_false() {
        let (b, _d) = fs();
        assert!(b.get("nope").unwrap().is_none());
        assert!(!b.exists("nope").unwrap());
        b.put("yes", b"1").unwrap();
        assert!(b.exists("yes").unwrap());
    }

    #[test]
    fn list_returns_relative_keys_recursively() {
        let (b, _d) = fs();
        b.put("spans/date=2026-05-28/hour=14/s-1.parquet", b"x")
            .unwrap();
        b.put("spans/date=2026-05-28/hour=15/s-2.parquet", b"y")
            .unwrap();
        b.put("logs/date=2026-05-28/hour=14/l-1.parquet", b"z")
            .unwrap();
        let mut spans = b.list("spans").unwrap();
        spans.sort();
        assert_eq!(
            spans,
            vec![
                "spans/date=2026-05-28/hour=14/s-1.parquet".to_string(),
                "spans/date=2026-05-28/hour=15/s-2.parquet".to_string(),
            ]
        );
        assert_eq!(b.list("").unwrap().len(), 3);
    }

    #[test]
    fn delete_is_idempotent() {
        let (b, _d) = fs();
        b.put("k", b"v").unwrap();
        b.delete("k").unwrap();
        b.delete("k").unwrap(); // no-op
        assert!(b.get("k").unwrap().is_none());
    }

    // The object_store-backed path (used by GCS in production) is exercised
    // here against the in-memory store, so the prefix/list/dedup/delete logic
    // is verified without credentials or a network.
    #[cfg(feature = "cloud")]
    #[test]
    fn object_store_backend_round_trips_on_in_memory() {
        use super::cloud_store::ObjStoreBackend;
        use object_store::memory::InMemory;
        use std::sync::Arc;

        let b = ObjStoreBackend::new(Arc::new(InMemory::new()), "tenant/cold");
        // put/get/exists under a prefix.
        b.put("spans/date=2026-05-28/hour=14/s-1.parquet", b"hello")
            .unwrap();
        assert_eq!(
            b.get("spans/date=2026-05-28/hour=14/s-1.parquet")
                .unwrap()
                .as_deref(),
            Some(&b"hello"[..])
        );
        assert!(
            b.exists("spans/date=2026-05-28/hour=14/s-1.parquet")
                .unwrap()
        );
        assert!(!b.exists("spans/missing").unwrap());
        assert!(b.get("spans/missing").unwrap().is_none());

        // list returns logical keys (prefix stripped), recursively.
        b.put("spans/date=2026-05-28/hour=15/s-2.parquet", b"world")
            .unwrap();
        b.put("logs/date=2026-05-28/hour=14/l-1.parquet", b"log")
            .unwrap();
        let mut spans = b.list("spans").unwrap();
        spans.sort();
        assert_eq!(
            spans,
            vec![
                "spans/date=2026-05-28/hour=14/s-1.parquet".to_string(),
                "spans/date=2026-05-28/hour=15/s-2.parquet".to_string(),
            ]
        );

        // delete is idempotent.
        b.delete("logs/date=2026-05-28/hour=14/l-1.parquet")
            .unwrap();
        b.delete("logs/date=2026-05-28/hour=14/l-1.parquet")
            .unwrap();
        assert!(b.list("logs").unwrap().is_empty());
    }

    #[cfg(not(feature = "cloud"))]
    #[test]
    fn gcs_without_cloud_feature_fails_loud() {
        let dir = tempfile::tempdir().unwrap();
        let err = match open_object_backend(StoreLocation::Gcs, dir.path(), Some("gs://b/p")) {
            Ok(_) => panic!("expected GCS to be unavailable without the cloud feature"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("--features cloud"), "got: {err}");
    }
}
