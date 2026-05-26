//! Full-text search over LLM payloads, via Tantivy.
//!
//! Indexes prompt + completion text per span (keyed by `trace_id`/`span_id`) so
//! agents can ask "which traces mention 'rate limit'?" — search the *content*
//! of LLM calls, not just structured attributes. The index is a derived,
//! droppable artifact: losing it loses only search, not data (see
//! `docs/tael-backend-design.md` → "Search"). HNSW semantic search is the
//! feature-gated follow-on (off by default; needs an embedding source).

use std::collections::HashSet;
use std::sync::Mutex;

use anyhow::Result;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, STORED, STRING, TEXT, Value};
use tantivy::{Index, IndexWriter, TantivyDocument, doc};

pub struct SearchIndex {
    index: Index,
    writer: Mutex<IndexWriter>,
    trace_id: Field,
    span_id: Field,
    body: Field,
}

impl SearchIndex {
    /// Open (or create) the span payload index under `<data_dir>/search/spans`.
    pub fn open(data_dir: &str) -> Result<Self> {
        let mut sb = Schema::builder();
        let trace_id = sb.add_text_field("trace_id", STRING | STORED);
        let span_id = sb.add_text_field("span_id", STRING | STORED);
        let body = sb.add_text_field("body", TEXT);
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
        })
    }

    /// Index one span's payload text. Cheap; the commit (below) makes it
    /// searchable.
    pub fn index_span(&self, trace_id: &str, span_id: &str, text: &str) -> Result<()> {
        let writer = self.writer.lock().unwrap();
        writer.add_document(doc!(
            self.trace_id => trace_id,
            self.span_id => span_id,
            self.body => text,
        ))?;
        Ok(())
    }

    /// Make buffered documents searchable. Call once per ingest batch.
    pub fn commit(&self) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer.commit()?;
        Ok(())
    }

    /// Return the set of `trace_id`s whose span payloads match `query`
    /// (Tantivy query syntax over the body text).
    pub fn search_trace_ids(&self, query: &str, limit: usize) -> Result<HashSet<String>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        let parser = QueryParser::for_index(&self.index, vec![self.body]);
        let parsed = parser.parse_query(query)?;
        let hits = searcher.search(&parsed, &TopDocs::with_limit(limit))?;
        let mut out = HashSet::new();
        for (_score, addr) in hits {
            let doc: TantivyDocument = searcher.doc(addr)?;
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
        idx.index_span("t1", "s1", "You are a helpful assistant. Summarize the rate limit policy.")
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
