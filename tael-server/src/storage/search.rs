//! Full-text search over telemetry content, via Tantivy.
//!
//! Indexes three kinds of text, all keyed by `trace_id`: LLM prompt and
//! completion payloads, log bodies, and span attribute values. Searching by
//! trace ID means one query — "which traces mention 'rate limit'?" — spans
//! every signal at once, which is the question an investigation actually
//! starts from.
//!
//! Span attributes are indexed as `key=value` text because the structured
//! filters are exact-match: an agent that does not already know the exact
//! value of `http.url` cannot find it by filtering, but can find it by
//! searching.
//!
//! The index is a derived, droppable artifact: losing it loses only search,
//! not data (see `docs/tael-backend-design.md` → "Search"). HNSW semantic
//! search is the feature-gated follow-on (off by default; needs an embedding
//! source).

use std::collections::HashSet;
use std::sync::Mutex;

use anyhow::Result;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, STORED, STRING, Schema, TEXT, Value};
use tantivy::{Index, IndexWriter, TantivyDocument, doc};

/// What a document's text came from, so a search can be scoped to one signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    /// LLM prompt or completion payload.
    LlmPayload,
    /// A log record's body.
    LogBody,
    /// A span's attribute values, flattened to `key=value` text.
    SpanAttributes,
}

impl TextKind {
    fn as_str(self) -> &'static str {
        match self {
            TextKind::LlmPayload => "llm",
            TextKind::LogBody => "log",
            TextKind::SpanAttributes => "attrs",
        }
    }

    /// Parse a `--text-in` scope name.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "llm" | "payload" | "payloads" => Some(TextKind::LlmPayload),
            "log" | "logs" => Some(TextKind::LogBody),
            "attrs" | "attributes" => Some(TextKind::SpanAttributes),
            _ => None,
        }
    }
}

pub struct SearchIndex {
    index: Index,
    writer: Mutex<IndexWriter>,
    trace_id: Field,
    span_id: Field,
    body: Field,
    kind: Field,
}

impl SearchIndex {
    /// Open (or create) the span payload index under `<data_dir>/search/spans`.
    pub fn open(data_dir: &str) -> Result<Self> {
        let mut sb = Schema::builder();
        let trace_id = sb.add_text_field("trace_id", STRING | STORED);
        let span_id = sb.add_text_field("span_id", STRING | STORED);
        let body = sb.add_text_field("body", TEXT);
        let kind = sb.add_text_field("kind", STRING | STORED);
        let schema = sb.build();

        let dir = std::path::Path::new(data_dir).join("search").join("spans");
        std::fs::create_dir_all(&dir)?;
        let mmap = tantivy::directory::MmapDirectory::open(&dir)?;
        let index = Index::open_or_create(mmap, schema)?;
        let writer = index.writer(50_000_000)?;
        Ok(Self {
            index,
            writer: Mutex::new(writer),
            trace_id,
            span_id,
            body,
            kind,
        })
    }

    /// Index one span's LLM payload text. Cheap; the commit (below) makes it
    /// searchable.
    pub fn index_span(&self, trace_id: &str, span_id: &str, text: &str) -> Result<()> {
        self.index_text(trace_id, span_id, text, TextKind::LlmPayload)
    }

    /// Index a log record's body against its trace, so a text search finds the
    /// log and the spans around it together.
    pub fn index_log_body(&self, trace_id: &str, body: &str) -> Result<()> {
        self.index_text(trace_id, "", body, TextKind::LogBody)
    }

    /// Index a span's attribute values as `key=value` text.
    ///
    /// Skips spans with no attributes, and truncates pathological ones: an
    /// attribute carrying a whole request body would otherwise dominate the
    /// index for no search benefit.
    pub fn index_span_attributes(
        &self,
        trace_id: &str,
        span_id: &str,
        attributes: &std::collections::HashMap<String, String>,
    ) -> Result<()> {
        if attributes.is_empty() {
            return Ok(());
        }
        const MAX_ATTRIBUTE_CHARS: usize = 4096;
        let mut text = String::new();
        for (k, v) in attributes {
            if text.len() >= MAX_ATTRIBUTE_CHARS {
                break;
            }
            text.push_str(k);
            text.push('=');
            let take = v.len().min(MAX_ATTRIBUTE_CHARS.saturating_sub(text.len()));
            text.push_str(&v[..v.floor_char_boundary(take)]);
            text.push(' ');
        }
        self.index_text(trace_id, span_id, &text, TextKind::SpanAttributes)
    }

    fn index_text(&self, trace_id: &str, span_id: &str, text: &str, kind: TextKind) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let writer = self.writer.lock().unwrap();
        writer.add_document(doc!(
            self.trace_id => trace_id,
            self.span_id => span_id,
            self.body => text,
            self.kind => kind.as_str(),
        ))?;
        Ok(())
    }

    /// Make buffered documents searchable. Call once per ingest batch.
    pub fn commit(&self) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer.commit()?;
        Ok(())
    }

    /// Return the set of `trace_id`s whose indexed text matches `query`
    /// (Tantivy query syntax), across every kind of indexed text.
    pub fn search_trace_ids(&self, query: &str, limit: usize) -> Result<HashSet<String>> {
        self.search_trace_ids_in(query, limit, None)
    }

    /// Like [`Self::search_trace_ids`] but restricted to one kind of text.
    pub fn search_trace_ids_in(
        &self,
        query: &str,
        limit: usize,
        kind: Option<TextKind>,
    ) -> Result<HashSet<String>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        let parser = QueryParser::for_index(&self.index, vec![self.body]);
        let parsed = parser.parse_query(query)?;
        let hits = searcher.search(&parsed, &TopDocs::with_limit(limit))?;
        let mut out = HashSet::new();
        for (_score, addr) in hits {
            let doc: TantivyDocument = searcher.doc(addr)?;
            if let Some(want) = kind {
                let got = doc.get_first(self.kind).and_then(|v| v.as_str());
                if got != Some(want.as_str()) {
                    continue;
                }
            }
            if let Some(tid) = doc.get_first(self.trace_id).and_then(|v| v.as_str()) {
                out.insert(tid.to_string());
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_and_searches_payload_text() {
        let dir = tempfile::tempdir().unwrap();
        let idx = SearchIndex::open(dir.path().to_str().unwrap()).unwrap();
        idx.index_span(
            "t1",
            "s1",
            "You are a helpful assistant. Summarize the rate limit policy.",
        )
        .unwrap();
        idx.index_span("t2", "s2", "Translate this paragraph to French.")
            .unwrap();
        idx.commit().unwrap();

        let hits = idx.search_trace_ids("rate limit", 10).unwrap();
        assert!(hits.contains("t1"));
        assert!(!hits.contains("t2"));

        let none = idx.search_trace_ids("quantum", 10).unwrap();
        assert!(none.is_empty());
    }
}
