//! Seed a data directory with synthetic spans, for smoke-testing a real server
//! against a real on-disk store.
//!
//!     cargo run --release -p tael-server --example seed -- <data-dir> [offset] [crash]
//!
//! `offset` shifts the generated ids so a second run adds spans rather than
//! overwriting the first run's. `crash` skips the final flush and aborts,
//! leaving records in the WAL that were applied but never checkpointed — the
//! state a real crash leaves, and the one whose replay has to be idempotent.
use tael_server::{Span, SpanKind, SpanStatus, Store, TaelBackend};

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).expect("usage: seed <data-dir>");
    let offset: usize = std::env::args()
        .nth(2)
        .map_or(0, |s| s.parse().expect("offset"));
    let crash = std::env::args().nth(3).is_some_and(|s| s == "crash");
    let backend = TaelBackend::new(&dir)?;
    let now = chrono::Utc::now();
    let mut spans = Vec::new();
    for i in offset..offset + 3_000usize {
        let start = now - chrono::Duration::milliseconds(50 * (3_000 - i) as i64);
        spans.push(Span {
            trace_id: format!("{:032x}", i / 5),
            span_id: format!("{i:016x}"),
            parent_span_id: (i % 5 != 0).then(|| format!("{:016x}", i - 1)),
            service: ["api", "worker", "db"][i % 3].into(),
            operation: format!("op-{}", i % 4),
            start_time: start,
            end_time: start + chrono::Duration::milliseconds(10),
            duration_ms: (i % 300) as f64,
            status: if i % 25 == 0 {
                SpanStatus::Error
            } else {
                SpanStatus::Ok
            },
            attributes: [("env".to_string(), "prod".to_string())]
                .into_iter()
                .collect(),
            events: vec![],
            kind: SpanKind::Server,
            llm: None,
        });
    }
    for chunk in spans.chunks(500) {
        backend.insert_spans(chunk)?;
    }
    if crash {
        println!(
            "seeded {} spans into {dir}, aborting without flush",
            spans.len()
        );
        std::process::abort();
    }
    backend.flush()?;
    println!("seeded {} spans into {dir}", spans.len());
    Ok(())
}
