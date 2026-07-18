//! Local append-only JSONL comments store — the default, single-binary path.
//!
//! Comments are appended one JSON object per line to
//! `<data_dir>/trace_comments.jsonl` and indexed in memory by `trace_id` at
//! open. This is the exact store that previously lived inline in
//! `backend/mod.rs`, lifted behind [`CommentsStore`].

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

use super::CommentsStore;
use crate::storage::models::TraceComment;

#[derive(Debug)]
pub struct JsonlComments {
    path: PathBuf,
    comments: Mutex<HashMap<String, Vec<TraceComment>>>,
}

impl JsonlComments {
    pub fn open(data_dir: &str) -> Result<Self> {
        let path = Path::new(data_dir).join("trace_comments.jsonl");
        let mut comments: HashMap<String, Vec<TraceComment>> = HashMap::new();
        if path.exists() {
            let file = std::fs::File::open(&path)
                .with_context(|| format!("opening {}", path.display()))?;
            for line in std::io::BufReader::new(file).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let comment: TraceComment = serde_json::from_str(&line)
                    .with_context(|| format!("decoding {}", path.display()))?;
                comments
                    .entry(comment.trace_id.clone())
                    .or_default()
                    .push(comment);
            }
        }
        Ok(Self {
            path,
            comments: Mutex::new(comments),
        })
    }
}

impl CommentsStore for JsonlComments {
    fn add(
        &self,
        trace_id: &str,
        span_id: Option<&str>,
        author: &str,
        body: &str,
    ) -> Result<TraceComment> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let comment = TraceComment {
            id: uuid::Uuid::new_v4().to_string(),
            trace_id: trace_id.to_string(),
            span_id: span_id.map(str::to_string),
            author: author.to_string(),
            body: body.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        writeln!(file, "{}", serde_json::to_string(&comment)?)?;
        self.comments
            .lock()
            .expect("comment store lock poisoned")
            .entry(trace_id.to_string())
            .or_default()
            .push(comment.clone());
        Ok(comment)
    }

    fn get(&self, trace_id: &str) -> Result<Vec<TraceComment>> {
        Ok(self
            .comments
            .lock()
            .expect("comment store lock poisoned")
            .get(trace_id)
            .cloned()
            .unwrap_or_default())
    }

    fn list_recent(&self, limit: usize) -> Result<Vec<TraceComment>> {
        let mut all: Vec<TraceComment> = self
            .comments
            .lock()
            .expect("comment store lock poisoned")
            .values()
            .flatten()
            .cloned()
            .collect();
        // Newest first; created_at is RFC 3339, so the lexicographic order is
        // the chronological order.
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        all.truncate(limit);
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_then_get_round_trips_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_str().unwrap();
        {
            let store = JsonlComments::open(data).unwrap();
            store.add("t1", Some("s1"), "alice", "looks slow").unwrap();
            store.add("t1", None, "bob", "agreed").unwrap();
            assert_eq!(store.get("t1").unwrap().len(), 2);
            assert!(store.get("other").unwrap().is_empty());
        }
        // Reopen: comments are reloaded from the JSONL file.
        let reopened = JsonlComments::open(data).unwrap();
        let comments = reopened.get("t1").unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].author, "alice");
        assert_eq!(comments[0].span_id.as_deref(), Some("s1"));
    }
}
