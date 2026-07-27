//! On-disk record codec for the hot tier and the WAL.
//!
//! Records are MessagePack in its *named* form — structs encode as maps, the
//! same shape JSON used. The compact (array) form is smaller still, but this
//! codebase's models lean on `skip_serializing_if` and `#[serde(default)]`, and
//! positional encoding turns a skipped field into a decode error rather than a
//! missing one. Named encoding keeps serde's field-presence semantics exactly
//! as they were, so switching the codec is a performance change and not a
//! schema change.
//!
//! There is deliberately no version byte or JSON fallback. Every record this
//! writes is re-read by the same build: the hot tier is a recent-data window
//! the compactor rolls into Parquet, and the WAL's unconsumed window is a
//! crash gap measured in seconds.

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Encode one record for storage.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    rmp_serde::to_vec_named(value).context("record encode failed")
}

/// Decode one stored record.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    rmp_serde::from_slice(bytes).context("record decode failed")
}

/// FNV-1a 64, used to make hot-tier keys a function of record content.
///
/// Deterministic forever, which is the whole point: keys derived from it must
/// come out identical when the WAL replays a record that was already applied,
/// or replay would insert a duplicate instead of overwriting. That rules out
/// `DefaultHasher` (explicitly not stable across Rust releases) and any hasher
/// seeded per process. A non-cryptographic hash is fine here — these keys are
/// never a security boundary, and a collision needs two *different* records
/// that also share a nanosecond timestamp.
pub fn content_hash(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{LlmSpan, Span, SpanKind, SpanStatus};

    fn span() -> Span {
        let now = chrono::Utc::now();
        Span {
            trace_id: "t".into(),
            span_id: "s".into(),
            parent_span_id: None,
            service: "api".into(),
            operation: "GET /".into(),
            start_time: now,
            end_time: now,
            duration_ms: 12.5,
            status: SpanStatus::Error,
            attributes: [("k".to_string(), "v".to_string())].into_iter().collect(),
            events: vec![],
            kind: SpanKind::Server,
            llm: Some(LlmSpan {
                provider: "anthropic".into(),
                model: "claude".into(),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn records_round_trip_including_skipped_and_defaulted_fields() {
        // `LlmSpan` skips all its `None` options on the way out and `Span::kind`
        // is `#[serde(default)]`. Positional MessagePack would lose the field
        // alignment; the named form must not.
        let encoded = encode(&span()).unwrap();
        let back: Span = decode(&encoded).unwrap();
        assert_eq!(back.trace_id, "t");
        assert_eq!(back.duration_ms, 12.5);
        assert_eq!(back.status, SpanStatus::Error);
        assert_eq!(back.kind, SpanKind::Server);
        assert_eq!(back.attributes.get("k").map(String::as_str), Some("v"));
        let llm = back.llm.expect("llm extension survives the round trip");
        assert_eq!(llm.model, "claude");
        assert!(llm.input_tokens.is_none());
    }

    #[test]
    fn the_encoding_is_smaller_than_json() {
        // Not a hard requirement, but if it ever stopped being true the reason
        // to have switched would be gone.
        let s = span();
        assert!(encode(&s).unwrap().len() < serde_json::to_vec(&s).unwrap().len());
    }

    #[test]
    fn content_hash_is_stable_and_distinguishes_records() {
        // The literal is the load-bearing part: a hash that changes between
        // builds silently breaks replay idempotence, and nothing else would
        // catch it.
        assert_eq!(content_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(content_hash(b"tael"), content_hash(b"tael"));
        assert_ne!(content_hash(b"tael"), content_hash(b"teal"));
    }
}
