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

    /// REST API port shorthand. For client commands, equivalent to
    /// `--server http://127.0.0.1:<port>`. For `serve`, sets the REST API
    /// listen port to `127.0.0.1:<port>`. Conflicts with `--server`.
    #[arg(long, global = true, conflicts_with = "server")]
    port_rest: Option<u16>,

    /// OTLP gRPC ingest port (only used by `serve`). Sets the OTLP gRPC
    /// listen address to `127.0.0.1:<port>`. Ignored by client commands.
    #[arg(long, global = true)]
    port_otel: Option<u16>,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Json,
    Table,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the tael server: OTLP ingest (gRPC :4317), storage, REST API (:7701)
    Serve {
        /// OTLP gRPC listen address (env: TAEL_OTLP_GRPC_ADDR)
        #[arg(long)]
        otlp_grpc_addr: Option<String>,
        /// REST API listen address (env: TAEL_REST_API_ADDR)
        #[arg(long)]
        rest_api_addr: Option<String>,
        /// Data directory (env: TAEL_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// WAL directory (env: TAEL_WAL_DIR)
        #[arg(long)]
        wal_dir: Option<String>,
        /// Storage backend: tael-backend (default) or duckdb (env: TAEL_STORAGE)
        #[arg(long)]
        storage: Option<String>,
    },
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
        /// Open the eval progress view
        #[arg(long)]
        evals: bool,
        /// Open a specific eval run in the eval progress view
        #[arg(long)]
        eval_run: Option<String>,
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
    /// Collect, score, report, and compare trace-native evals
    Eval {
        #[command(subcommand)]
        action: EvalAction,
    },
    /// Classify production failures into recurring issues
    Issue {
        #[command(subcommand)]
        action: IssueAction,
    },
    /// Define and inspect long-running reliability signals
    Signal {
        #[command(subcommand)]
        action: SignalAction,
    },
    /// Compare production experiment variants
    Experiment {
        #[command(subcommand)]
        action: ExperimentAction,
    },
    /// Record and list untrusted agent self diagnostics
    Diagnose {
        #[command(subcommand)]
        action: DiagnoseAction,
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
        /// Filter by span attribute, repeatable. Format: key=value
        /// (e.g. --attribute http.method=GET --attribute http.status_code=500)
        #[arg(long = "attribute")]
        attribute: Vec<String>,
        /// Full-text search over LLM prompt/completion payloads
        /// (tael-backend storage only; e.g. --text "rate limit")
        #[arg(long)]
        text: Option<String>,
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
    /// Run a read-only SQL query over the telemetry tables
    /// (spans, logs, metrics, trace_comments)
    Sql {
        /// The SQL query (SELECT/WITH only), e.g.
        /// "SELECT service, COUNT(*) FROM spans GROUP BY service"
        query: String,
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
enum EvalAction {
    /// Run a command once per JSONL case with TAEL_EVAL_* env vars
    Run {
        /// JSONL case file. Each line should include `case_id` or `id`.
        cases: String,
        /// Eval suite/dataset identifier
        #[arg(long)]
        suite: String,
        /// Shell command template. Supports {case_id}, {case_index}, {run_id}, {suite_id}.
        #[arg(long)]
        cmd: String,
        /// Source version for the evaluated code
        #[arg(long)]
        code_version: Option<String>,
        /// Explicit run ID. Defaults to run_YYYYMMDD_HHMMSS.
        #[arg(long)]
        run_id: Option<String>,
        /// OTLP endpoint exported to child processes
        #[arg(long, default_value = "http://127.0.0.1:4317")]
        otlp_endpoint: String,
    },
    /// Ingest JSONL score records as tael_eval_score metric points
    Score {
        /// Eval run ID
        run_id: String,
        /// JSONL scores file
        scores: String,
    },
    /// List recent eval runs
    Runs,
    /// Show one eval run summary
    Status {
        /// Eval run ID
        run_id: String,
    },
    /// List cases in a run
    Cases {
        /// Eval run ID
        run_id: String,
    },
    /// List raw scores in a run
    Scores {
        /// Eval run ID
        run_id: String,
    },
    /// Render an eval run report
    Report {
        /// Eval run ID
        run_id: String,
    },
    /// Compare a run against a baseline run
    Compare {
        /// Current eval run ID
        run_id: String,
        /// Baseline eval run ID
        baseline_run_id: String,
    },
    /// Manage golden cases promoted from traces
    Case {
        #[command(subcommand)]
        action: EvalCaseAction,
    },
    /// Inspect eval suite quality and hygiene
    Suite {
        #[command(subcommand)]
        action: EvalSuiteAction,
    },
}

#[derive(Subcommand)]
enum EvalCaseAction {
    /// Promote a production trace into a golden eval case
    Add {
        /// Source production trace
        #[arg(long)]
        from_trace: String,
        /// Suite/dataset name
        #[arg(long)]
        suite: String,
        /// Stable case identifier
        #[arg(long)]
        case_id: String,
        /// Representative failure mode
        #[arg(long)]
        failure_mode: Option<String>,
        /// Source issue this case protects
        #[arg(long)]
        source_issue_id: Option<String>,
        /// Mark this case as protecting a critical path
        #[arg(long)]
        critical_path: bool,
        /// Durable expected behavior for the case
        #[arg(long)]
        expected_behavior: Option<String>,
        /// Comment author
        #[arg(long)]
        author: Option<String>,
    },
    /// Link an existing eval case to an issue
    Link {
        /// Stable case identifier
        #[arg(long)]
        case_id: String,
        /// Issue identifier
        #[arg(long)]
        issue_id: String,
        /// Trace to annotate. If omitted, tael finds the eval case source trace.
        #[arg(long)]
        trace_id: Option<String>,
    },
}

#[derive(Subcommand)]
enum EvalSuiteAction {
    /// Inspect suite hygiene: provenance, expected behavior, duplicates, cost risks
    Inspect {
        /// Suite/dataset name
        suite: String,
        /// Maximum comments to scan
        #[arg(long, default_value = "50000")]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum IssueAction {
    /// Create a recurring issue from a trace
    Create {
        /// Source trace
        #[arg(long)]
        from_trace: String,
        /// Failure mode, e.g. tool_error or context_loss
        #[arg(long)]
        failure_mode: String,
        /// Impact: low, medium, high, critical
        #[arg(long)]
        impact: String,
        /// Short issue summary
        #[arg(long)]
        summary: String,
        /// Last successful step in the trace
        #[arg(long)]
        last_successful_step: Option<String>,
        /// First real failure in the trace
        #[arg(long)]
        first_failure: Option<String>,
        /// Comment author
        #[arg(long)]
        author: Option<String>,
    },
    /// List known issues
    List {
        /// Maximum comments to scan
        #[arg(long, default_value = "50000")]
        limit: u32,
    },
    /// List comments and cases linked to an issue
    Examples {
        /// Issue identifier
        issue_id: String,
        /// Maximum comments to scan
        #[arg(long, default_value = "50000")]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum SignalAction {
    /// Create a long-running reliability signal definition
    Create {
        /// Source trace for the signal definition
        #[arg(long)]
        from_trace: String,
        /// Signal name
        #[arg(long)]
        name: String,
        /// Query or classifier description used to identify the signal
        #[arg(long)]
        query: Option<String>,
        /// Failure mode this signal tracks
        #[arg(long)]
        failure_mode: Option<String>,
        /// Short summary
        #[arg(long)]
        summary: Option<String>,
        /// Comment author
        #[arg(long)]
        author: Option<String>,
    },
    /// Show signal trend from structured comments
    Trend {
        /// Signal name or failure mode
        name: String,
        /// Maximum comments to scan
        #[arg(long, default_value = "50000")]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum ExperimentAction {
    /// Compare variants using trace attributes tael.experiment.*
    Compare {
        /// Experiment identifier
        experiment_id: String,
        /// Optional signal/failure mode/category to count by variant
        #[arg(long)]
        signal: Option<String>,
        /// Time window (e.g. 1h, 24h, 7d)
        #[arg(long)]
        last: Option<String>,
    },
}

#[derive(Subcommand)]
enum DiagnoseAction {
    /// Record an untrusted self diagnostic on a trace
    Report {
        /// Trace ID
        #[arg(long)]
        trace_id: String,
        /// Optional span ID
        #[arg(long)]
        span_id: Option<String>,
        /// Category, e.g. missing_context, capability_gap, broken_tool
        #[arg(long)]
        category: String,
        /// Severity: low, medium, high, critical
        #[arg(long)]
        severity: String,
        /// Confidence: low, medium, high
        #[arg(long, default_value = "low")]
        confidence: String,
        /// Short diagnostic summary
        #[arg(long)]
        summary: String,
        /// Comment author
        #[arg(long)]
        author: Option<String>,
    },
    /// List self diagnostics
    List {
        /// Maximum comments to scan
        #[arg(long, default_value = "50000")]
        limit: u32,
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

    // `serve` runs the embedded server; it needs no REST client, so handle it
    // before constructing one.
    if let Commands::Serve {
        otlp_grpc_addr,
        rest_api_addr,
        data_dir,
        wal_dir,
        storage,
    } = cli.command
    {
        // Start from env defaults, then override with any explicit flags.
        let mut config = tael_server::ServerConfig::from_env();
        if let Some(a) = otlp_grpc_addr {
            config.otlp_grpc_addr = a;
        } else if let Some(p) = cli.port_otel {
            config.otlp_grpc_addr = format!("127.0.0.1:{p}");
        }
        if let Some(a) = rest_api_addr {
            config.rest_api_addr = a;
        } else if let Some(p) = cli.port_rest {
            config.rest_api_addr = format!("127.0.0.1:{p}");
        }
        if let Some(d) = data_dir {
            config.data_dir = d;
        }
        if let Some(d) = wal_dir {
            config.wal_dir = d;
        }
        if let Some(s) = storage {
            config.storage = tael_server::StorageBackend::parse(&s);
        }
        return tael_server::run(config).await;
    }

    let server_url = match cli.port_rest {
        Some(p) => format!("http://127.0.0.1:{p}"),
        None => cli.server.clone(),
    };
    let client = client::TaelClient::new(&server_url);

    match cli.command {
        // Handled above; the early return means this arm is never reached.
        Commands::Serve { .. } => unreachable!(),
        Commands::Query { signal } => match signal {
            QuerySignal::Traces {
                service,
                operation,
                min_duration,
                max_duration,
                status,
                last,
                limit,
                attribute,
                text,
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
                    attribute,
                    text,
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
            QuerySignal::Sql { query } => {
                commands::query::sql(&client, &cli.format, &query).await?;
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
            evals,
            eval_run,
        } => {
            tui::run(&server_url, service, status, interval, evals, eval_run).await?;
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
        Commands::Eval { action } => match action {
            EvalAction::Run {
                cases,
                suite,
                cmd,
                code_version,
                run_id,
                otlp_endpoint,
            } => {
                commands::eval::run(
                    &client,
                    &otlp_endpoint,
                    &cases,
                    &suite,
                    &cmd,
                    code_version,
                    run_id,
                )
                .await?;
            }
            EvalAction::Score { run_id, scores } => {
                commands::eval::score(&client, &cli.format, &run_id, &scores).await?;
            }
            EvalAction::Runs => {
                commands::eval::runs(&client, &cli.format).await?;
            }
            EvalAction::Status { run_id } => {
                commands::eval::status(&client, &cli.format, &run_id).await?;
            }
            EvalAction::Cases { run_id } => {
                commands::eval::cases(&client, &cli.format, &run_id).await?;
            }
            EvalAction::Scores { run_id } => {
                commands::eval::scores(&client, &cli.format, &run_id).await?;
            }
            EvalAction::Report { run_id } => {
                commands::eval::report(&client, &cli.format, &run_id).await?;
            }
            EvalAction::Compare {
                run_id,
                baseline_run_id,
            } => {
                commands::eval::compare(&client, &cli.format, &run_id, &baseline_run_id).await?;
            }
            EvalAction::Case { action } => match action {
                EvalCaseAction::Add {
                    from_trace,
                    suite,
                    case_id,
                    failure_mode,
                    source_issue_id,
                    critical_path,
                    expected_behavior,
                    author,
                } => {
                    commands::eval::case_add(
                        &client,
                        &cli.format,
                        &from_trace,
                        &suite,
                        &case_id,
                        failure_mode,
                        source_issue_id,
                        critical_path,
                        expected_behavior,
                        author,
                    )
                    .await?;
                }
                EvalCaseAction::Link {
                    case_id,
                    issue_id,
                    trace_id,
                } => {
                    commands::eval::case_link(&client, &cli.format, &case_id, &issue_id, trace_id)
                        .await?;
                }
            },
            EvalAction::Suite { action } => match action {
                EvalSuiteAction::Inspect { suite, limit } => {
                    commands::eval::suite_inspect(&client, &cli.format, &suite, limit).await?;
                }
            },
        },
        Commands::Issue { action } => match action {
            IssueAction::Create {
                from_trace,
                failure_mode,
                impact,
                summary,
                last_successful_step,
                first_failure,
                author,
            } => {
                commands::issue::create(
                    &client,
                    &cli.format,
                    &from_trace,
                    &failure_mode,
                    &impact,
                    &summary,
                    last_successful_step,
                    first_failure,
                    author,
                )
                .await?;
            }
            IssueAction::List { limit } => {
                commands::issue::list(&client, &cli.format, limit).await?;
            }
            IssueAction::Examples { issue_id, limit } => {
                commands::issue::examples(&client, &cli.format, &issue_id, limit).await?;
            }
        },
        Commands::Signal { action } => match action {
            SignalAction::Create {
                from_trace,
                name,
                query,
                failure_mode,
                summary,
                author,
            } => {
                commands::signal::create(
                    &client,
                    &cli.format,
                    &from_trace,
                    &name,
                    query,
                    failure_mode,
                    summary,
                    author,
                )
                .await?;
            }
            SignalAction::Trend { name, limit } => {
                commands::signal::trend(&client, &cli.format, &name, limit).await?;
            }
        },
        Commands::Experiment { action } => match action {
            ExperimentAction::Compare {
                experiment_id,
                signal,
                last,
            } => {
                commands::experiment::compare(&client, &cli.format, &experiment_id, signal, last)
                    .await?;
            }
        },
        Commands::Diagnose { action } => match action {
            DiagnoseAction::Report {
                trace_id,
                span_id,
                category,
                severity,
                confidence,
                summary,
                author,
            } => {
                commands::diagnose::report(
                    &client,
                    &cli.format,
                    &trace_id,
                    span_id,
                    &category,
                    &severity,
                    &confidence,
                    &summary,
                    author,
                )
                .await?;
            }
            DiagnoseAction::List { limit } => {
                commands::diagnose::list(&client, &cli.format, limit).await?;
            }
        },
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
