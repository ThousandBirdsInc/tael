//! Pluggable trace-comments store.
//!
//! Comments are the one piece of *user-authored* state tael holds (everything
//! else is ingested telemetry), so they get a durable, swappable backend:
//!
//! * [`jsonl`] — an append-only JSONL file under the data dir. The default,
//!   single-binary, zero-dependency path; behavior is identical to what shipped
//!   before this module existed.
//! * [`postgres`] — a shared Postgres instance (e.g. Cloud SQL), so a sharded
//!   cloud deployment keeps comments in one place independent of which shard
//!   owns a trace. Compiled only under the `cloud` feature.
//!
//! Selected at runtime via `TAEL_COMMENTS_STORE` (`jsonl` | `postgres`).

mod jsonl;
#[cfg(feature = "cloud")]
mod postgres;

use anyhow::Result;

use crate::config::{CommentsBackend, CommentsConfig};
use crate::storage::models::TraceComment;

pub use jsonl::JsonlComments;

/// A durable store for trace comments. Synchronous to match the [`Store`] trait
/// surface; cloud implementations bridge to async internally.
///
/// [`Store`]: crate::storage::Store
pub trait CommentsStore: Send + Sync {
    /// Append a comment on `trace_id` (optionally pinned to `span_id`) and
    /// return the stored record (with its generated id + timestamp).
    fn add(
        &self,
        trace_id: &str,
        span_id: Option<&str>,
        author: &str,
        body: &str,
    ) -> Result<TraceComment>;

    /// All comments on `trace_id`, in insertion order.
    fn get(&self, trace_id: &str) -> Result<Vec<TraceComment>>;
}

/// Build the configured comments store. Defaults to the local JSONL file;
/// `TAEL_COMMENTS_STORE=postgres` selects Postgres (requires `--features
/// cloud`, else this fails loudly so a default build never silently falls back
/// to local when the operator asked for Postgres).
pub fn open(config: &CommentsConfig, data_dir: &str) -> Result<Box<dyn CommentsStore>> {
    match config.backend {
        CommentsBackend::Jsonl => Ok(Box::new(JsonlComments::open(data_dir)?)),
        CommentsBackend::Postgres => open_postgres(config),
    }
}

#[cfg(feature = "cloud")]
fn open_postgres(config: &CommentsConfig) -> Result<Box<dyn CommentsStore>> {
    let url = config
        .database_url
        .as_deref()
        .filter(|u| !u.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("TAEL_COMMENTS_STORE=postgres requires TAEL_COMMENTS_DATABASE_URL")
        })?;
    Ok(Box::new(postgres::PgComments::connect(url)?))
}

#[cfg(not(feature = "cloud"))]
fn open_postgres(_config: &CommentsConfig) -> Result<Box<dyn CommentsStore>> {
    anyhow::bail!(
        "Postgres comments are not included in this build; rebuild with `--features cloud` \
         to use TAEL_COMMENTS_STORE=postgres"
    )
}
