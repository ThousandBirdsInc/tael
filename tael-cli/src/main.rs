mod client;
mod commands;
mod output;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "tael", about = "AI-agent-native observability CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Output format
    #[arg(long, global = true, default_value = "json")]
    format: OutputFormat,

    /// Server address
    #[arg(long, global = true, default_value = "http://127.0.0.1:7701")]
    server: String,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Json,
    Table,
}

#[derive(Subcommand)]
enum Commands {
    /// Query telemetry data
    Query {
        #[command(subcommand)]
        signal: QuerySignal,
    },
    /// Get a specific resource by ID
    Get {
        #[command(subcommand)]
        resource: GetResource,
    },
    /// Add or view comments on traces
    Comment {
        #[command(subcommand)]
        action: CommentAction,
    },
    /// List known services and their health
    Services,
    /// Interactive TUI trace feed
    Live {
        /// Filter by service name
        #[arg(long)]
        service: Option<String>,
        /// Filter by status (ok, error, unset)
        #[arg(long)]
        status: Option<String>,
        /// Poll interval in seconds
        #[arg(long, default_value = "2")]
        interval: u64,
    },
    /// Aggregated health summary over a time window
    Summarize {
        /// Time window (e.g. 1h, 30m, 7d). Defaults to 1h.
        #[arg(long)]
        last: Option<String>,
        /// Filter to a single service
        #[arg(long)]
        service: Option<String>,
    },
    /// Surface services whose error-rate or p95 regressed vs a baseline window
    Anomalies {
        /// Current window (default 1h)
        #[arg(long)]
        last: Option<String>,
        /// Baseline window to compare against (default: 6× current)
        #[arg(long)]
        baseline: Option<String>,
        /// Filter to a single service
        #[arg(long)]
        service: Option<String>,
    },
    /// Pull spans, logs, and metrics for a trace ID
    Correlate {
        /// Trace ID to correlate across signals
        #[arg(long)]
        trace: String,
    },
    /// Poll the summary endpoint and print deltas between samples
    Watch {
        /// Summary window (default 1m)
        #[arg(long)]
        last: Option<String>,
        /// Filter to a single service
        #[arg(long)]
        service: Option<String>,
        /// Poll interval in seconds
        #[arg(long, default_value = "10")]
        interval: u64,
    },
    /// Server management
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },
    /// Install the tael Claude Code skill
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
}

#[derive(Subcommand)]
enum QuerySignal {
    /// Search and filter traces
    Traces {
        /// Filter by service name
        #[arg(long)]
        service: Option<String>,
        /// Filter by operation name (substring match)
        #[arg(long)]
        operation: Option<String>,
        /// Minimum span duration (e.g. 100ms, 1s, 500)
        #[arg(long)]
        min_duration: Option<String>,
        /// Maximum span duration
        #[arg(long)]
        max_duration: Option<String>,
        /// Filter by status (ok, error, unset)
        #[arg(long)]
        status: Option<String>,
        /// Time window (e.g. 1h, 30m, 7d)
        #[arg(long)]
        last: Option<String>,
        /// Max results to return
        #[arg(long, default_value = "100")]
        limit: u32,
    },
    /// Search and filter metrics
    Metrics {
        /// PromQL subset expression (e.g. `rate(http_requests[5m])`)
        #[arg(long)]
        query: Option<String>,
        /// Filter by service name (ignored when --query is set)
        #[arg(long)]
        service: Option<String>,
        /// Filter by metric name (ignored when --query is set)
        #[arg(long)]
        name: Option<String>,
        /// Filter by metric type (gauge, sum, histogram, summary)
        #[arg(long = "type")]
        metric_type: Option<String>,
        /// Time window (e.g. 1h, 30m, 7d)
        #[arg(long)]
        last: Option<String>,
        /// Max results to return
        #[arg(long, default_value = "500")]
        limit: u32,
    },
    /// Search and filter logs
    Logs {
        /// Filter by service name
        #[arg(long)]
        service: Option<String>,
        /// Filter by severity (trace, debug, info, warn, error, fatal)
        #[arg(long)]
        severity: Option<String>,
        /// Search log body text (substring match)
        #[arg(long)]
        body_contains: Option<String>,
        /// Filter by trace ID
        #[arg(long)]
        trace_id: Option<String>,
        /// Time window (e.g. 1h, 30m, 7d)
        #[arg(long)]
        last: Option<String>,
        /// Max results to return
        #[arg(long, default_value = "100")]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum GetResource {
    /// Get a full trace by trace ID
    Trace {
        /// The trace ID to look up
        trace_id: String,
    },
}

#[derive(Subcommand)]
enum CommentAction {
    /// Add a comment to a trace
    Add {
        /// The trace ID to comment on
        trace_id: String,
        /// Comment text
        body: String,
        /// Author name
        #[arg(long, default_value = "cli")]
        author: String,
        /// Optional span ID to attach comment to
        #[arg(long)]
        span_id: Option<String>,
    },
    /// List comments on a trace
    List {
        /// The trace ID to list comments for
        trace_id: String,
    },
}

#[derive(Subcommand)]
enum ServerAction {
    /// Show server status
    Status,
}

#[derive(Subcommand)]
enum SkillAction {
    /// Install SKILL.md into Claude Code's skills directory
    Install {
        /// Install into ./.claude/skills/tael/ instead of ~/.claude/skills/tael/
        #[arg(long)]
        project: bool,
        /// Overwrite an existing SKILL.md at the destination
        #[arg(long)]
        force: bool,
    },
    /// Print the destination path without writing anything
    Where {
        /// Resolve the project-local path instead of the personal path
        #[arg(long)]
        project: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = client::TaelClient::new(&cli.server);

    match cli.command {
        Commands::Query { signal } => match signal {
            QuerySignal::Traces {
                service,
                operation,
                min_duration,
                max_duration,
                status,
                last,
                limit,
            } => {
                commands::query::traces(
                    &client,
                    &cli.format,
                    service,
                    operation,
                    min_duration,
                    max_duration,
                    status,
                    last,
                    limit,
                )
                .await?;
            }
            QuerySignal::Metrics {
                query,
                service,
                name,
                metric_type,
                last,
                limit,
            } => {
                commands::query::metrics(
                    &client,
                    &cli.format,
                    query,
                    service,
                    name,
                    metric_type,
                    last,
                    limit,
                )
                .await?;
            }
            QuerySignal::Logs {
                service,
                severity,
                body_contains,
                trace_id,
                last,
                limit,
            } => {
                commands::query::logs(
                    &client,
                    &cli.format,
                    service,
                    severity,
                    body_contains,
                    trace_id,
                    last,
                    limit,
                )
                .await?;
            }
        },
        Commands::Get { resource } => match resource {
            GetResource::Trace { trace_id } => {
                commands::get::trace(&client, &cli.format, &trace_id).await?;
            }
        },
        Commands::Comment { action } => match action {
            CommentAction::Add {
                trace_id,
                body,
                author,
                span_id,
            } => {
                commands::comment::add(
                    &client,
                    &cli.format,
                    &trace_id,
                    &body,
                    Some(&author),
                    span_id.as_deref(),
                )
                .await?;
            }
            CommentAction::List { trace_id } => {
                commands::comment::list(&client, &cli.format, &trace_id).await?;
            }
        },
        Commands::Services => {
            commands::services::list(&client, &cli.format).await?;
        }
        Commands::Live {
            service,
            status,
            interval,
        } => {
            tui::run(&cli.server, service, status, interval).await?;
        }
        Commands::Summarize { last, service } => {
            commands::summarize::run(&client, &cli.format, last, service).await?;
        }
        Commands::Anomalies {
            last,
            baseline,
            service,
        } => {
            commands::anomalies::run(&client, &cli.format, last, baseline, service).await?;
        }
        Commands::Correlate { trace } => {
            commands::correlate::run(&client, &cli.format, &trace).await?;
        }
        Commands::Watch {
            last,
            service,
            interval,
        } => {
            commands::watch::run(&client, &cli.format, last, service, interval).await?;
        }
        Commands::Server { action } => match action {
            ServerAction::Status => {
                commands::server::status(&client, &cli.format).await?;
            }
        },
        Commands::Skill { action } => match action {
            SkillAction::Install { project, force } => {
                commands::skill::install(project, force)?;
            }
            SkillAction::Where { project } => {
                commands::skill::print_path(project)?;
            }
        },
    }

    Ok(())
}
