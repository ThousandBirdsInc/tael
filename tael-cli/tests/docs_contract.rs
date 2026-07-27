//! The agent contract must describe the CLI that exists.
//!
//! `SKILL.md` and `llm.txt` are not documentation in the usual sense — they
//! are the interface an agent reads to learn what tael can do, and an agent
//! cannot discover a command that neither file mentions. Historically they
//! drifted: commands shipped undocumented, and `llm.txt` described the storage
//! engine as DuckDB long after that stopped being true.
//!
//! This test walks the real clap tree and fails when a command is missing from
//! both documents, so drift is caught at `cargo test` rather than by an agent
//! failing to find a feature.

use clap::CommandFactory;

/// Commands deliberately absent from the agent-facing docs.
///
/// `SKILL.md` tells agents to skip the TUI and GUI (they are human surfaces),
/// and `serve` is an operator command rather than query surface. Each entry is
/// an explicit decision, not a backlog.
const NOT_AGENT_FACING: &[&str] = &["gui", "live", "serve"];

/// Collect every command path in the tree, e.g. `eval suite push`.
fn command_paths(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix} {name}")
        };
        out.push(path.clone());
        command_paths(sub, &path, out);
    }
}

fn documented(docs: &str, path: &str) -> bool {
    // Match on the invocation as a user would write it. Checking for
    // `tael <path>` avoids matching an unrelated prose mention of a bare word
    // like "review" or "diff".
    docs.contains(&format!("tael {path}")) || docs.contains(&format!("`{path}`"))
}

#[test]
fn every_command_appears_in_the_agent_contract() {
    let skill = include_str!("../../SKILL.md");
    let llm = include_str!("../../llm.txt");
    let docs = format!("{skill}\n{llm}");

    let mut paths = Vec::new();
    command_paths(&tael_cli::Cli::command(), "", &mut paths);
    assert!(!paths.is_empty(), "the clap tree should have subcommands");

    let mut undocumented: Vec<&String> = paths
        .iter()
        .filter(|p| !NOT_AGENT_FACING.contains(&p.as_str()))
        // A parent whose children are documented is covered; only leaves and
        // standalone commands need their own mention.
        .filter(|p| {
            !paths
                .iter()
                .any(|other| other.starts_with(&format!("{p} ")))
        })
        .filter(|p| !documented(&docs, p))
        .collect();
    undocumented.sort();

    assert!(
        undocumented.is_empty(),
        "these commands are missing from SKILL.md and llm.txt, so an agent \
         cannot discover them:\n  {}\n\nDocument them, or add them to \
         NOT_AGENT_FACING with a reason.",
        undocumented
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

#[test]
fn the_docs_do_not_describe_a_storage_engine_we_stopped_using() {
    // This exact claim survived in llm.txt long after tael-backend became the
    // default, and it changed how an agent reasoned about query cost.
    let llm = include_str!("../../llm.txt");
    assert!(
        !llm.contains("server is single-node DuckDB"),
        "llm.txt still claims the server is single-node DuckDB; \
         the default engine is tael-backend"
    );
}

#[test]
fn documented_exit_codes_match_the_implementation() {
    // Agents branch on these; a doc that disagrees with the binary is worse
    // than no doc.
    use tael_cli::exit::ExitCategory;
    let docs = format!(
        "{}\n{}",
        include_str!("../../SKILL.md"),
        include_str!("../../llm.txt")
    );
    for (category, code) in [
        (ExitCategory::NoResults, 2),
        (ExitCategory::BadQuery, 3),
        (ExitCategory::Unreachable, 4),
        (ExitCategory::Unauthorized, 5),
        (ExitCategory::ConditionMet, 6),
    ] {
        assert_eq!(category.code(), code, "{category:?} changed its code");
        assert!(
            docs.contains(&format!("{code}  ")),
            "exit code {code} ({category:?}) is not documented"
        );
    }
}
