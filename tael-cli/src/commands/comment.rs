use anyhow::Result;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::output;

pub async fn add(
    client: &TaelClient,
    format: &OutputFormat,
    trace_id: &str,
    body: &str,
    author: Option<&str>,
    span_id: Option<&str>,
) -> Result<()> {
    let result = client.add_comment(trace_id, body, author, span_id).await?;
    match format {
        OutputFormat::Json => output::print_json(&result),
        OutputFormat::Table => {
            if let Some(c) = result.get("comment") {
                let author = c["author"].as_str().unwrap_or("-");
                let body = c["body"].as_str().unwrap_or("-");
                let time = c["created_at"].as_str().unwrap_or("-");
                println!("Comment added by {author} at {time}: {body}");
            }
        }
    }
    Ok(())
}

pub async fn list(
    client: &TaelClient,
    format: &OutputFormat,
    trace_id: &str,
) -> Result<()> {
    let result = client.get_comments(trace_id).await?;
    match format {
        OutputFormat::Json => output::print_json(&result),
        OutputFormat::Table => {
            let comments = result
                .get("comments")
                .and_then(|c| c.as_array());
            match comments {
                Some(arr) if !arr.is_empty() => {
                    for c in arr {
                        let author = c["author"].as_str().unwrap_or("-");
                        let body = c["body"].as_str().unwrap_or("-");
                        let time = c["created_at"].as_str().unwrap_or("-");
                        let span = c["span_id"].as_str();
                        if let Some(sid) = span {
                            println!("[{time}] {author} (span {sid}): {body}");
                        } else {
                            println!("[{time}] {author}: {body}");
                        }
                    }
                }
                _ => println!("No comments for this trace."),
            }
        }
    }
    Ok(())
}
