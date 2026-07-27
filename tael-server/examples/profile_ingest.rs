//! Timing probe for the ingest write path, isolating each stage.
//!
//! The benchmarks show a ~2.7ms fixed cost per insert_spans call regardless of
//! batch size, which points at fsync rather than per-record work. This says
//! which fsync.
use std::time::Instant;

use tael_server::{Span, SpanKind, SpanStatus, Store, TaelBackend};

fn span(i: usize) -> Span {
    let now = chrono::Utc::now();
    Span {
        trace_id: format!("trace{i}"),
        span_id: format!("span{i}"),
        parent_span_id: None,
        service: "api".into(),
        operation: "GET /".into(),
        start_time: now,
        end_time: now,
        duration_ms: 12.0,
        status: SpanStatus::Ok,
        attributes: Default::default(),
        events: vec![],
        kind: SpanKind::Server,
        llm: None,
    }
}

fn main() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let key = format!("profile-{}", uuid::Uuid::new_v4());
    unsafe { std::env::set_var("WALRUS_DATA_DIR", dir.path().join("wal")) };
    let backend = TaelBackend::with_wal_key(dir.path().to_str().unwrap(), &key)?;

    // Warm up so the first-call cost isn't attributed to steady state.
    for i in 0..10 {
        backend.insert_spans(&[span(i)])?;
    }

    for &batch in &[1usize, 10, 100, 1000] {
        let spans: Vec<Span> = (0..batch).map(span).collect();
        let iters = if batch >= 1000 { 20 } else { 100 };
        let start = Instant::now();
        for _ in 0..iters {
            backend.insert_spans(&spans)?;
        }
        let per_call = start.elapsed().as_secs_f64() / iters as f64;
        println!(
            "batch={batch:<5} {:>8.3}ms/call  {:>10.0} spans/s",
            per_call * 1000.0,
            batch as f64 / per_call
        );
    }
    let _ = std::fs::remove_dir_all(format!("wal_files/{key}"));
    Ok(())
}
