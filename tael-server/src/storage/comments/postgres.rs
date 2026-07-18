//! Postgres-backed comments store (e.g. Cloud SQL) — the shared, cloud path.
//!
//! In a sharded deployment any shard can own a given trace, but comments are
//! user state that should be visible regardless of routing, so they live in one
//! shared Postgres instance rather than per-shard local files. Compiled only
//! under the `cloud` feature.
//!
//! The [`CommentsStore`] surface is synchronous (it matches the `Store` trait);
//! `sqlx` is async, so calls are driven on a dedicated IO runtime via a
//! `spawn` + std-channel bridge — safe to call from any thread, including a
//! main-runtime worker, without the "block within a runtime" panic.

use std::sync::OnceLock;

use anyhow::{Context, Result};
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions};
use tokio::runtime::Runtime;

use super::CommentsStore;
use crate::storage::models::TraceComment;

/// Dedicated runtime for comments DB IO (see module docs).
fn io_runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("tael-comments-io")
            .build()
            .expect("building comments IO runtime")
    })
}

fn block<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    io_runtime().spawn(async move {
        let _ = tx.send(fut.await);
    });
    rx.recv().expect("comments IO worker dropped")
}

pub struct PgComments {
    pool: PgPool,
}

impl PgComments {
    /// Connect to `url` and ensure the comments table exists. Small pool —
    /// comments are low-QPS, and a sharded fleet must stay under the instance's
    /// connection limit.
    pub fn connect(url: &str) -> Result<Self> {
        let url = url.to_string();
        let pool = block(async move {
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .connect(&url)
                .await
                .context("connecting to comments Postgres")?;
            // Idempotent schema bootstrap (no separate migration step needed for
            // a single table). Uses `IF NOT EXISTS` so repeated starts are safe.
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS tael_comments (\
                   seq        BIGSERIAL PRIMARY KEY,\
                   id         TEXT NOT NULL,\
                   trace_id   TEXT NOT NULL,\
                   span_id    TEXT,\
                   author     TEXT NOT NULL,\
                   body       TEXT NOT NULL,\
                   created_at TEXT NOT NULL)",
            )
            .execute(&pool)
            .await
            .context("creating tael_comments table")?;
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS tael_comments_trace_id_idx \
                 ON tael_comments (trace_id)",
            )
            .execute(&pool)
            .await
            .context("creating tael_comments index")?;
            anyhow::Ok(pool)
        })?;
        Ok(Self { pool })
    }
}

impl CommentsStore for PgComments {
    fn add(
        &self,
        trace_id: &str,
        span_id: Option<&str>,
        author: &str,
        body: &str,
    ) -> Result<TraceComment> {
        // Build the record in Rust so semantics match the JSONL store exactly
        // (generated id + rfc3339 timestamp).
        let comment = TraceComment {
            id: uuid::Uuid::new_v4().to_string(),
            trace_id: trace_id.to_string(),
            span_id: span_id.map(str::to_string),
            author: author.to_string(),
            body: body.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let pool = self.pool.clone();
        let row = comment.clone();
        block(async move {
            sqlx::query(
                "INSERT INTO tael_comments (id, trace_id, span_id, author, body, created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(&row.id)
            .bind(&row.trace_id)
            .bind(&row.span_id)
            .bind(&row.author)
            .bind(&row.body)
            .bind(&row.created_at)
            .execute(&pool)
            .await
            .context("inserting comment")
        })?;
        Ok(comment)
    }

    fn get(&self, trace_id: &str) -> Result<Vec<TraceComment>> {
        let pool = self.pool.clone();
        let trace_id = trace_id.to_string();
        let rows = block(async move {
            sqlx::query(
                "SELECT id, trace_id, span_id, author, body, created_at \
                 FROM tael_comments WHERE trace_id = $1 ORDER BY seq",
            )
            .bind(&trace_id)
            .fetch_all(&pool)
            .await
            .context("querying comments")
        })?;
        Ok(rows
            .into_iter()
            .map(|r| TraceComment {
                id: r.get("id"),
                trace_id: r.get("trace_id"),
                span_id: r.get("span_id"),
                author: r.get("author"),
                body: r.get("body"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    fn list_recent(&self, limit: usize) -> Result<Vec<TraceComment>> {
        let pool = self.pool.clone();
        let rows = block(async move {
            sqlx::query(
                "SELECT id, trace_id, span_id, author, body, created_at \
                 FROM tael_comments ORDER BY seq DESC LIMIT $1",
            )
            .bind(limit as i64)
            .fetch_all(&pool)
            .await
            .context("listing comments")
        })?;
        Ok(rows
            .into_iter()
            .map(|r| TraceComment {
                id: r.get("id"),
                trace_id: r.get("trace_id"),
                span_id: r.get("span_id"),
                author: r.get("author"),
                body: r.get("body"),
                created_at: r.get("created_at"),
            })
            .collect())
    }
}
