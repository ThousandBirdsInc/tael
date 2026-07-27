//! `tael embed`, `tael similar`, and `tael cluster`.
//!
//! "Has this failure happened before?" is the question that turns a one-off
//! incident into a tracked issue. Text search only finds traces sharing a
//! literal term, which misses two failures that are the same problem worded
//! differently.
//!
//! Embeddings come from a command the user supplies — tael never calls a model
//! provider, which is the same boundary online scoring draws.

use anyhow::Result;
use serde_json::json;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::exit::CategorizedError;
use crate::output::print_json;

pub async fn embed(
    client: &TaelClient,
    format: &OutputFormat,
    cmd: &str,
    last: Option<&str>,
    limit: u32,
) -> Result<()> {
    let result = client
        .build_embeddings(&json!({ "embed_cmd": cmd, "last": last, "limit": limit }))
        .await?;
    if let Some(error) = result["error"].as_str() {
        return Err(CategorizedError::new(crate::exit::ExitCategory::Failure, error).into());
    }
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => {
            println!(
                "Embedded {} trace(s), skipped {} already-embedded, {} failed. Corpus: {}.",
                result["embedded"].as_u64().unwrap_or(0),
                result["skipped"].as_u64().unwrap_or(0),
                result["failed"].as_u64().unwrap_or(0),
                result["total_embeddings"].as_u64().unwrap_or(0),
            );
            if let Some(err) = result["last_error"].as_str() {
                println!("last error: {err}");
            }
        }
    }
    Ok(())
}

pub async fn similar(
    client: &TaelClient,
    format: &OutputFormat,
    trace_id: &str,
    limit: u32,
    min_similarity: f32,
) -> Result<()> {
    let result = client
        .similar_traces(trace_id, limit, min_similarity)
        .await?;
    if let Some(error) = result["error"].as_str() {
        match format {
            OutputFormat::Json => print_json(&result),
            OutputFormat::Table => {
                println!("{error}");
                if let Some(hint) = result["hint"].as_str() {
                    println!("{hint}");
                }
            }
        }
        return Err(CategorizedError::no_results());
    }

    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => {
            let neighbors = result["neighbors"].as_array().cloned().unwrap_or_default();
            if neighbors.is_empty() {
                println!(
                    "No similar traces above the threshold ({} embedded traces searched).",
                    result["corpus_size"].as_u64().unwrap_or(0)
                );
                return Err(CategorizedError::no_results());
            }
            let mut table = comfy_table::Table::new();
            table.set_header(vec!["SIMILARITY", "TRACE"]);
            for n in &neighbors {
                table.add_row(vec![
                    format!("{:.3}", n["similarity"].as_f64().unwrap_or(0.0)),
                    n["trace_id"].as_str().unwrap_or("-").to_string(),
                ]);
            }
            println!("{table}");
        }
    }
    Ok(())
}

pub async fn cluster(client: &TaelClient, format: &OutputFormat, k: usize) -> Result<()> {
    let result = client.cluster_traces(k).await?;
    if let Some(error) = result["error"].as_str() {
        return Err(CategorizedError::new(crate::exit::ExitCategory::BadQuery, error).into());
    }
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => {
            let clusters = result["clusters"].as_array().cloned().unwrap_or_default();
            if clusters.is_empty() {
                println!("No clusters. Run `tael embed --cmd <embedder>` first.");
                return Err(CategorizedError::no_results());
            }
            let mut table = comfy_table::Table::new();
            table.set_header(vec!["CLUSTER", "SIZE", "COHESION", "EXEMPLAR TRACE"]);
            for c in &clusters {
                table.add_row(vec![
                    c["id"].as_u64().unwrap_or(0).to_string(),
                    c["size"].as_u64().unwrap_or(0).to_string(),
                    format!("{:.3}", c["cohesion"].as_f64().unwrap_or(0.0)),
                    c["exemplar"].as_str().unwrap_or("-").to_string(),
                ]);
            }
            println!("{table}");
            println!();
            println!("Read each exemplar with `tael get trace <id>` to name the cluster.");
            if let Some(note) = result["note"].as_str() {
                println!("{note}");
            }
        }
    }
    Ok(())
}
