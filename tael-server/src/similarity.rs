//! Trace similarity and clustering.
//!
//! "Has this failure happened before?" is the question that turns a one-off
//! incident into a tracked issue, and nothing in tael could answer it. Text
//! search only finds traces sharing a literal term, which misses the two
//! failures that are the same problem worded differently.
//!
//! Embeddings come from a user-supplied command (`--embed-cmd`), not from a
//! model provider tael calls. That is the same boundary online scoring draws:
//! holding provider credentials is the trust surface a local-first tool
//! advertises not having, and an embedding source is exactly the kind of thing
//! that would otherwise smuggle one in.
//!
//! Clustering is left to the caller to interpret. tael groups traces and names
//! exemplars; deciding what a cluster *means* — and whether it deserves an
//! issue — is the calling agent's job, which is the same division of labor as
//! the natural-language query layer tael deliberately does not have.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// One trace's embedding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEmbedding {
    pub trace_id: String,
    pub vector: Vec<f32>,
}

/// A trace's similarity to a query trace.
#[derive(Debug, Clone, Serialize)]
pub struct Neighbor {
    pub trace_id: String,
    /// Cosine similarity in [-1, 1]; 1.0 is identical direction.
    pub similarity: f32,
}

/// A group of traces that embedded close together.
#[derive(Debug, Clone, Serialize)]
pub struct Cluster {
    pub id: usize,
    /// The member nearest the centroid — the best single trace to read to
    /// understand what the cluster is.
    pub exemplar: String,
    pub size: usize,
    pub members: Vec<String>,
    /// Mean similarity of members to the centroid. A loose cluster is a hint
    /// that the grouping is weak, not that the traces are unrelated.
    pub cohesion: f32,
}

/// Cosine similarity between two vectors.
///
/// Returns `None` on a dimension mismatch or a zero vector rather than a
/// misleading 0.0: "these are unrelated" and "this comparison is meaningless"
/// are different answers, and only one of them should rank in a result list.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return None;
    }
    Some(dot / (norm_a.sqrt() * norm_b.sqrt()))
}

/// Rank `candidates` by similarity to `query`, most similar first.
pub fn nearest(
    query: &TraceEmbedding,
    candidates: &[TraceEmbedding],
    limit: usize,
    min_similarity: f32,
) -> Vec<Neighbor> {
    let mut out: Vec<Neighbor> = candidates
        .iter()
        // The query trace is trivially its own nearest neighbor and would
        // occupy a result slot for nothing.
        .filter(|c| c.trace_id != query.trace_id)
        .filter_map(|c| {
            cosine_similarity(&query.vector, &c.vector).map(|similarity| Neighbor {
                trace_id: c.trace_id.clone(),
                similarity,
            })
        })
        .filter(|n| n.similarity >= min_similarity)
        .collect();
    out.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    out
}

/// Group embeddings into `k` clusters with k-means.
///
/// Initial centroids are spread deterministically across the input rather than
/// chosen at random: a clustering that returns different groups each run is
/// not something an agent can act on, and reproducibility matters more here
/// than squeezing out a marginally better partition.
pub fn cluster(
    embeddings: &[TraceEmbedding],
    k: usize,
    max_iterations: usize,
) -> Result<Vec<Cluster>> {
    if embeddings.is_empty() {
        return Ok(Vec::new());
    }
    if k == 0 {
        bail!("cluster count must be at least 1");
    }
    let dim = embeddings[0].vector.len();
    if dim == 0 {
        bail!("embeddings have no dimensions");
    }
    if embeddings.iter().any(|e| e.vector.len() != dim) {
        bail!("embeddings have inconsistent dimensions; they must all come from the same model");
    }

    let k = k.min(embeddings.len());
    // Evenly spaced seeds: deterministic, and spread across the input rather
    // than clustered at the front.
    let stride = embeddings.len() / k;
    let mut centroids: Vec<Vec<f32>> = (0..k)
        .map(|i| embeddings[i * stride].vector.clone())
        .collect();

    let mut assignments = vec![0usize; embeddings.len()];
    for _ in 0..max_iterations {
        let mut changed = false;
        for (i, e) in embeddings.iter().enumerate() {
            let best = centroids
                .iter()
                .enumerate()
                .filter_map(|(c, centroid)| cosine_similarity(&e.vector, centroid).map(|s| (c, s)))
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(c, _)| c)
                .unwrap_or(0);
            if assignments[i] != best {
                assignments[i] = best;
                changed = true;
            }
        }
        if !changed {
            break;
        }

        // Recompute centroids as the mean of their members.
        let mut sums = vec![vec![0.0f32; dim]; k];
        let mut counts = vec![0usize; k];
        for (i, e) in embeddings.iter().enumerate() {
            let c = assignments[i];
            counts[c] += 1;
            for (d, v) in e.vector.iter().enumerate() {
                sums[c][d] += v;
            }
        }
        for (c, centroid) in centroids.iter_mut().enumerate() {
            if counts[c] == 0 {
                // An emptied centroid would attract nothing forever; leaving it
                // in place lets a later iteration reclaim it.
                continue;
            }
            for (d, slot) in centroid.iter_mut().enumerate() {
                *slot = sums[c][d] / counts[c] as f32;
            }
        }
    }

    let mut clusters: Vec<Cluster> = Vec::new();
    for (c, centroid) in centroids.iter().enumerate() {
        let members: Vec<&TraceEmbedding> = embeddings
            .iter()
            .enumerate()
            .filter(|(i, _)| assignments[*i] == c)
            .map(|(_, e)| e)
            .collect();
        if members.is_empty() {
            continue;
        }
        let scored: Vec<(f32, &TraceEmbedding)> = members
            .iter()
            .map(|m| (cosine_similarity(&m.vector, centroid).unwrap_or(0.0), *m))
            .collect();
        let exemplar = scored
            .iter()
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, e)| e.trace_id.clone())
            .unwrap_or_default();
        let cohesion = scored.iter().map(|(s, _)| s).sum::<f32>() / scored.len() as f32;

        clusters.push(Cluster {
            id: clusters.len(),
            exemplar,
            size: members.len(),
            members: members.iter().map(|m| m.trace_id.clone()).collect(),
            cohesion,
        });
    }
    // Biggest first: the largest cluster is usually the most worth naming.
    clusters.sort_by_key(|b| std::cmp::Reverse(b.size));
    for (i, c) in clusters.iter_mut().enumerate() {
        c.id = i;
    }
    Ok(clusters)
}

/// Run the user's embedding command over one text, returning its vector.
///
/// The command receives the text on stdin and must print a JSON array of
/// numbers. Keeping it a subprocess is what keeps model credentials out of
/// tael entirely.
pub async fn embed(command: &str, text: &str) -> Result<Vec<f32>> {
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning embed command `{command}`"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes()).await?;
        // Dropping stdin closes it, which most embedders wait for.
        drop(stdin);
    }

    let output = child.wait_with_output().await?;
    if !output.status.success() {
        bail!(
            "embed command exited {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let vector: Vec<f32> = serde_json::from_str(stdout.trim())
        .with_context(|| "embed command must print a JSON array of numbers")?;
    if vector.is_empty() {
        bail!("embed command returned an empty vector");
    }
    Ok(vector)
}

/// Persisted embeddings, keyed by trace id.
///
/// A flat file rather than an HNSW index: at the scale where clustering is
/// useful for an investigation — thousands of traces, not millions — a linear
/// scan is milliseconds, and an approximate index would add a dependency and a
/// recall cliff for no benefit anyone would notice.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EmbeddingStore {
    #[serde(default)]
    pub embeddings: HashMap<String, Vec<f32>>,
}

impl EmbeddingStore {
    pub fn path(data_dir: &str) -> std::path::PathBuf {
        std::path::Path::new(data_dir).join("embeddings.json")
    }

    pub fn load(data_dir: &str) -> Result<Self> {
        match std::fs::read(Self::path(data_dir)) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("parsing embeddings"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("reading embeddings"),
        }
    }

    pub fn save(&self, data_dir: &str) -> Result<()> {
        let path = Self::path(data_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn to_vec(&self) -> Vec<TraceEmbedding> {
        self.embeddings
            .iter()
            .map(|(trace_id, vector)| TraceEmbedding {
                trace_id: trace_id.clone(),
                vector: vector.clone(),
            })
            .collect()
    }

    pub fn get(&self, trace_id: &str) -> Option<TraceEmbedding> {
        self.embeddings.get(trace_id).map(|v| TraceEmbedding {
            trace_id: trace_id.to_string(),
            vector: v.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str, v: &[f32]) -> TraceEmbedding {
        TraceEmbedding {
            trace_id: id.into(),
            vector: v.to_vec(),
        }
    }

    #[test]
    fn cosine_similarity_ranks_direction_not_magnitude() {
        // Same direction, different length: still identical.
        let a = cosine_similarity(&[1.0, 0.0], &[5.0, 0.0]).unwrap();
        assert!((a - 1.0).abs() < 1e-6, "{a}");
        let orthogonal = cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).unwrap();
        assert!(orthogonal.abs() < 1e-6);
        let opposite = cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]).unwrap();
        assert!((opposite + 1.0).abs() < 1e-6);
    }

    #[test]
    fn meaningless_comparisons_are_none_not_zero() {
        // "unrelated" and "cannot be compared" must not rank the same.
        assert!(cosine_similarity(&[1.0, 0.0], &[1.0]).is_none());
        assert!(cosine_similarity(&[], &[]).is_none());
        assert!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]).is_none());
    }

    #[test]
    fn nearest_excludes_the_query_and_respects_the_floor() {
        let query = e("q", &[1.0, 0.0]);
        let candidates = vec![
            e("q", &[1.0, 0.0]),
            e("close", &[0.9, 0.1]),
            e("orthogonal", &[0.0, 1.0]),
            e("opposite", &[-1.0, 0.0]),
        ];

        let hits = nearest(&query, &candidates, 10, 0.5);
        let ids: Vec<&str> = hits.iter().map(|n| n.trace_id.as_str()).collect();
        assert_eq!(ids, vec!["close"], "only `close` clears a 0.5 floor");

        let all = nearest(&query, &candidates, 10, -1.0);
        assert!(
            !all.iter().any(|n| n.trace_id == "q"),
            "a trace is trivially its own neighbor and must not fill a slot"
        );
        // Ranked most similar first.
        assert_eq!(all[0].trace_id, "close");
        assert_eq!(all.last().unwrap().trace_id, "opposite");
    }

    #[test]
    fn nearest_honors_the_limit() {
        let query = e("q", &[1.0, 0.0]);
        let candidates: Vec<TraceEmbedding> = (0..20)
            .map(|i| e(&format!("t{i}"), &[1.0, i as f32 * 0.01]))
            .collect();
        assert_eq!(nearest(&query, &candidates, 5, -1.0).len(), 5);
    }

    #[test]
    fn clustering_separates_two_obvious_groups() {
        // Two tight groups pointing in different directions.
        let mut embeddings = Vec::new();
        for i in 0..10 {
            embeddings.push(e(&format!("a{i}"), &[1.0, i as f32 * 0.01]));
        }
        for i in 0..6 {
            embeddings.push(e(&format!("b{i}"), &[i as f32 * 0.01, 1.0]));
        }

        let clusters = cluster(&embeddings, 2, 50).unwrap();
        assert_eq!(clusters.len(), 2);
        // Largest first.
        assert_eq!(clusters[0].size, 10);
        assert_eq!(clusters[1].size, 6);
        assert!(clusters[0].members.iter().all(|m| m.starts_with('a')));
        assert!(clusters[1].members.iter().all(|m| m.starts_with('b')));
        // Tight groups should report high cohesion.
        assert!(clusters[0].cohesion > 0.9, "{}", clusters[0].cohesion);
        assert!(clusters[0].members.contains(&clusters[0].exemplar));
    }

    #[test]
    fn clustering_is_deterministic() {
        // An agent cannot act on groupings that change every run.
        let embeddings: Vec<TraceEmbedding> = (0..30)
            .map(|i| e(&format!("t{i}"), &[(i % 3) as f32, (i % 5) as f32, 1.0]))
            .collect();
        let first = cluster(&embeddings, 3, 50).unwrap();
        let second = cluster(&embeddings, 3, 50).unwrap();
        assert_eq!(
            first.iter().map(|c| c.members.clone()).collect::<Vec<_>>(),
            second.iter().map(|c| c.members.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn clustering_handles_degenerate_input() {
        assert!(cluster(&[], 3, 10).unwrap().is_empty());
        assert!(cluster(&[e("a", &[1.0])], 0, 10).is_err());
        assert!(cluster(&[e("a", &[])], 1, 10).is_err());
        // More clusters than traces collapses to one per trace, not an error.
        let clusters = cluster(&[e("a", &[1.0, 0.0]), e("b", &[0.0, 1.0])], 10, 10).unwrap();
        assert_eq!(clusters.len(), 2);
    }

    #[test]
    fn mismatched_dimensions_are_refused() {
        // Mixing two embedding models produces meaningless distances; saying so
        // beats returning nonsense clusters.
        let err = cluster(&[e("a", &[1.0, 0.0]), e("b", &[1.0, 0.0, 0.0])], 2, 10)
            .unwrap_err()
            .to_string();
        assert!(err.contains("inconsistent dimensions"), "{err}");
    }

    #[test]
    fn embeddings_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let mut store = EmbeddingStore::default();
        store.embeddings.insert("t1".into(), vec![0.1, 0.2]);
        store.save(path).unwrap();

        let loaded = EmbeddingStore::load(path).unwrap();
        assert_eq!(loaded.get("t1").unwrap().vector, vec![0.1, 0.2]);
        assert!(loaded.get("missing").is_none());
        assert_eq!(loaded.to_vec().len(), 1);
    }

    #[tokio::test]
    async fn embed_runs_the_users_command() {
        let v = embed("echo '[0.1, 0.2, 0.3]'", "some trace text")
            .await
            .unwrap();
        assert_eq!(v, vec![0.1, 0.2, 0.3]);
    }

    #[tokio::test]
    async fn embed_passes_the_text_on_stdin() {
        // A real embedder reads the text; this proves it arrives.
        let v = embed("wc -c | xargs printf '[%s]'", "12345").await.unwrap();
        assert_eq!(v, vec![5.0]);
    }

    #[tokio::test]
    async fn a_failing_embed_command_reports_its_stderr() {
        let err = embed("echo 'no API key' >&2; exit 2", "x")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("exited 2"), "{err}");
        assert!(err.contains("no API key"), "{err}");
    }

    #[tokio::test]
    async fn non_json_output_is_rejected() {
        assert!(embed("echo 'not json'", "x").await.is_err());
        assert!(embed("echo '[]'", "x").await.is_err());
    }
}
