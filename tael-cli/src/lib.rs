//! tael-cli: the `tael` command-line interface, shipped as a library so host
//! applications can embed tael wholesale — the CLI command tree, the typed
//! REST client, the output renderers, and the interactive `tael live` TUI —
//! while the thin `tael` binary stays a one-line wrapper around [`run`].
//!
//! There are three levels of embedding, from coarsest to finest:
//!
//! 1. **Whole CLI**: mount [`Commands`] (a `clap::Subcommand`) and
//!    [`GlobalOpts`] (a `clap::Args`) inside your own clap tree, then dispatch
//!    with [`run_command`]. Your app gains every `tael` subcommand — `query`,
//!    `live`, `serve`, evals, issues, and the rest — under your own binary.
//!
//! 2. **Individual features**: call [`TaelClient`] for typed queries against a
//!    tael server, [`tui::run_with_options`] to hand the terminal to the live
//!    trace-feed TUI, or the functions in [`commands`] for CLI-style behavior
//!    (fetch + render) one command at a time.
//!
//! 3. **In-process server**: the re-exported [`tael_server`] crate runs the
//!    whole OTLP ingest/storage/query server inside your process —
//!    `tael_server::run_embedded` starts it quietly on a background task, and
//!    everything above works against it over localhost or a Unix socket.
//!
//! ```no_run
//! use tael_cli::{Commands, GlobalOpts, run_command};
//!
//! # async fn example() -> anyhow::Result<()> {
//! // Start an in-process tael server (no banner, no global subscriber).
//! tokio::spawn(tael_cli::tael_server::run_embedded(
//!     tael_cli::tael_server::ServerConfig::from_env(),
//! ));
//!
//! // Dispatch any CLI command programmatically...
//! run_command(Commands::Services, &GlobalOpts::default()).await?;
//!
//! // ...or take over the terminal with the `tael live` TUI.
//! tael_cli::tui::run_with_options(
//!     "http://127.0.0.1:7701",
//!     tael_cli::tui::LiveOptions::default(),
//! )
//! .await?;
//! # Ok(())
//! # }
//! ```

pub mod client;
pub mod commands;
pub mod exit;
pub mod mcp;
pub mod output;
pub mod tui;

pub use client::TaelClient;
/// Re-export of the server crate so embedders can run an in-process tael
/// server (`tael_server::run_embedded`) without adding a second dependency.
pub use tael_server;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// Where client commands point when no `--server`/`--port-rest`/
/// `--unix-socket` override is given.
pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:7701";

#[derive(Parser)]
#[command(name = "tael", about = "AI-agent-native observability CLI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    #[command(flatten)]
    pub opts: GlobalOpts,
}

impl Cli {
    /// Execute the parsed command — equivalent to the `tael` binary's main.
    pub async fn run(self) -> Result<()> {
        run_command(self.command, &self.opts).await
    }
}

/// The global flags shared by every `tael` subcommand. Flatten this into your
/// own clap struct with `#[command(flatten)]` when embedding [`Commands`], or
/// construct it directly (it implements [`Default`]) when dispatching
/// programmatically via [`run_command`].
#[derive(Args, Clone, Debug)]
pub struct GlobalOpts {
    /// Output format
    #[arg(long, global = true, default_value = "json")]
    pub format: OutputFormat,

    /// Server address
    #[arg(long, global = true, default_value = DEFAULT_SERVER_URL)]
    pub server: String,

    /// REST API port shorthand. For client commands, equivalent to
    /// `--server http://127.0.0.1:<port>`. For `serve`, sets the REST API
    /// listen port to `127.0.0.1:<port>`. Conflicts with `--server`.
    #[arg(long, global = true, conflicts_with = "server")]
    pub port_rest: Option<u16>,

    /// REST API Unix socket path. For client commands, connects over this
    /// socket. For `serve`, listens on this socket instead of a TCP REST port.
    #[arg(long, global = true, conflicts_with_all = ["server", "port_rest"])]
    pub unix_socket: Option<String>,

    /// OTLP gRPC ingest port (only used by `serve`). Sets the OTLP gRPC
    /// listen address to `127.0.0.1:<port>`. Ignored by client commands.
    #[arg(long, global = true)]
    pub port_otel: Option<u16>,

    /// API key for a server running with auth enabled. Falls back to
    /// TAEL_API_KEY. Sent as `Authorization: Bearer <key>`.
    #[arg(long, global = true)]
    pub api_key: Option<String>,
}

impl Default for GlobalOpts {
    fn default() -> Self {
        Self {
            format: OutputFormat::Json,
            server: DEFAULT_SERVER_URL.to_string(),
            port_rest: None,
            unix_socket: None,
            port_otel: None,
            api_key: None,
        }
    }
}

impl GlobalOpts {
    /// The server URL client commands connect to, resolving the
    /// `--port-rest`/`--unix-socket` shorthands against `--server`.
    pub fn server_url(&self) -> String {
        match self.port_rest {
            Some(p) => format!("http://127.0.0.1:{p}"),
            None => match &self.unix_socket {
                Some(socket) => format!("unix://{socket}"),
                None => self.server.clone(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Json,
    Table,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run the tael server: OTLP ingest (gRPC :4317), Datadog trace-agent intake (:8126), storage, REST API (:7701)
    Serve {
        /// OTLP gRPC listen address (env: TAEL_OTLP_GRPC_ADDR)
        #[arg(long)]
        otlp_grpc_addr: Option<String>,
        /// OTLP/HTTP listen address; `off` disables the dedicated listener
        /// (env: TAEL_OTLP_HTTP_ADDR) [default: 127.0.0.1:4318]
        #[arg(long)]
        otlp_http_addr: Option<String>,
        /// REST API listen address (env: TAEL_REST_API_ADDR)
        #[arg(long)]
        rest_api_addr: Option<String>,
        /// REST API Unix socket path (env: TAEL_REST_API_SOCKET)
        #[arg(long)]
        rest_api_socket: Option<String>,
        /// Datadog trace-agent listen address; `off` disables the dedicated
        /// listener (env: TAEL_DD_AGENT_ADDR) [default: 127.0.0.1:8126]
        #[arg(long)]
        dd_agent_addr: Option<String>,
        /// Data directory (env: TAEL_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// WAL directory (env: TAEL_WAL_DIR)
        #[arg(long)]
        wal_dir: Option<String>,
        /// Storage backend: tael-backend (default). duckdb requires installing with --features duckdb.
        #[arg(long)]
        storage: Option<String>,
        /// Authentication: `off` or `required`. Defaults to off for
        /// loopback-only listeners and required when any listener is reachable
        /// off-box (env: TAEL_AUTH)
        #[arg(long)]
        auth: Option<String>,
        /// TOML config file with retention and compaction policy. Defaults to
        /// config.toml beside the data directory (env: TAEL_CONFIG)
        #[arg(long)]
        config: Option<String>,
    },
    /// Launch the desktop GUI (requires a build with `--features gui`)
    Gui,
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
        /// Stop and exit 6 when this condition becomes true. Repeatable (any
        /// match stops). Format: <field><op><value>, where value is a number
        /// or a multiple of the first sample.
        /// Examples: error_rate>0.05, p95_ms>2x, delta_error_count>0, span_count<1
        #[arg(long = "exit-on")]
        exit_on: Vec<String>,
        /// Give up after this many ticks and exit 0. Without it, `watch` polls
        /// until a condition trips or the process is interrupted.
        #[arg(long)]
        max_ticks: Option<u64>,
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
    /// Manage API keys (operates on the keystore in the data directory)
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Inspect or scaffold the retention and compaction config
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Expose tael's query surface to an AI agent over the Model Context Protocol
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Show the retention/compaction policy actually in effect
    Show {
        /// Config file path (env: TAEL_CONFIG)
        #[arg(long)]
        config: Option<String>,
        /// Data directory the config sits beside (env: TAEL_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
    },
    /// Write a commented config file with the recommended retention windows
    Init {
        /// Config file path (env: TAEL_CONFIG)
        #[arg(long)]
        config: Option<String>,
        /// Data directory the config sits beside (env: TAEL_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// Overwrite an existing config file
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum McpAction {
    /// Serve MCP over stdio. Configure this as an MCP server in your agent:
    /// {"command": "tael", "args": ["mcp", "serve"]}
    Serve,
}

#[derive(Subcommand)]
pub enum AuthAction {
    /// Mint a new API key. The key is printed once and is not recoverable.
    CreateKey {
        /// Label for this key, e.g. claude-code-prod
        #[arg(long)]
        name: String,
        /// Role: reader (query), writer (+ push telemetry), admin (+ manage keys)
        #[arg(long, default_value = "reader")]
        role: String,
        /// Tenant this key reads and writes within
        #[arg(long, default_value = "default")]
        tenant: String,
        /// Data directory holding the keystore (env: TAEL_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
    },
    /// List API keys and their roles. Never prints key material.
    List {
        /// Data directory holding the keystore (env: TAEL_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
    },
    /// Revoke a key by id. Takes effect without restarting the server.
    Revoke {
        /// Key id from `tael auth list`
        key_id: String,
        /// Data directory holding the keystore (env: TAEL_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum QuerySignal {
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
        /// Also report how the query executed: access path, tiers consulted,
        /// rows scanned vs returned, and hints about surprising results
        #[arg(long)]
        explain: bool,
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
pub enum GetResource {
    /// Get a full trace by trace ID
    Trace {
        /// The trace ID to look up
        trace_id: String,
    },
}

#[derive(Subcommand)]
pub enum CommentAction {
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
pub enum EvalAction {
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
pub enum EvalCaseAction {
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
pub enum EvalSuiteAction {
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
pub enum IssueAction {
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
pub enum SignalAction {
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
pub enum ExperimentAction {
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
pub enum DiagnoseAction {
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
pub enum ServerAction {
    /// Show server status
    Status,
}

#[derive(Subcommand)]
pub enum SkillAction {
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

/// Execute a full parsed [`Cli`] — the `tael` binary is a one-line wrapper
/// around this.
pub async fn run(cli: Cli) -> Result<()> {
    cli.run().await
}

/// Dispatch a single tael command with the given global options. This is the
/// embedding entrypoint: it powers every subcommand of the `tael` binary and
/// behaves identically when called from a host application — including
/// `Commands::Serve` (runs the server in-process until shutdown) and
/// `Commands::Live` (takes over the terminal with the live TUI).
pub async fn run_command(command: Commands, opts: &GlobalOpts) -> Result<()> {
    // `serve` runs the embedded server; it needs no REST client, so handle it
    // before constructing one.
    if let Commands::Serve {
        otlp_grpc_addr,
        otlp_http_addr,
        rest_api_addr,
        rest_api_socket,
        dd_agent_addr,
        data_dir,
        wal_dir,
        storage,
        auth,
        config: config_path,
    } = command
    {
        if opts.unix_socket.is_some() && rest_api_socket.is_some() {
            bail!("use either --unix-socket or --rest-api-socket, not both");
        }

        // Start from env defaults, then override with any explicit flags.
        let mut config = tael_server::ServerConfig::from_env();
        if let Some(a) = otlp_grpc_addr {
            config.otlp_grpc_addr = a;
        } else if let Some(p) = opts.port_otel {
            config.otlp_grpc_addr = format!("127.0.0.1:{p}");
        }
        if let Some(a) = otlp_http_addr {
            config.otlp_http_addr = tael_server::parse_otlp_http_addr(Some(a));
        }
        if let Some(socket) = rest_api_socket.or_else(|| opts.unix_socket.clone()) {
            config.rest_api_socket = Some(socket);
        } else if let Some(a) = rest_api_addr {
            config.rest_api_addr = a;
            config.rest_api_socket = None;
        } else if let Some(p) = opts.port_rest {
            config.rest_api_addr = format!("127.0.0.1:{p}");
            config.rest_api_socket = None;
        }
        if let Some(a) = dd_agent_addr {
            config.dd_agent_addr = tael_server::parse_dd_agent_addr(Some(a));
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
        if let Some(a) = auth {
            config.auth = Some(tael_server::auth::AuthMode::parse(&a)?);
        }
        if let Some(p) = config_path {
            config.config_path = Some(p);
        }
        return tael_server::run(config).await;
    }

    // Config inspection reads the same files the server does, so it works
    // whether or not one is running.
    if let Commands::Config { action } = command {
        let resolve_dir = |explicit: Option<String>| {
            explicit.unwrap_or_else(|| tael_server::ServerConfig::from_env().data_dir)
        };
        return match action {
            ConfigAction::Show { config, data_dir } => {
                commands::config::show(&opts.format, config.as_deref(), &resolve_dir(data_dir))
            }
            ConfigAction::Init {
                config,
                data_dir,
                force,
            } => commands::config::init(
                &opts.format,
                config.as_deref(),
                &resolve_dir(data_dir),
                force,
            ),
        };
    }

    // Key management works on the keystore file directly, so it must not need
    // a reachable server — the first key is minted before one can start.
    if let Commands::Auth { action } = command {
        let resolve_dir = |explicit: Option<String>| {
            explicit.unwrap_or_else(|| tael_server::ServerConfig::from_env().data_dir)
        };
        return match action {
            AuthAction::CreateKey {
                name,
                role,
                tenant,
                data_dir,
            } => commands::auth::create_key(
                &opts.format,
                &resolve_dir(data_dir),
                &name,
                &role,
                &tenant,
            ),
            AuthAction::List { data_dir } => {
                commands::auth::list_keys(&opts.format, &resolve_dir(data_dir))
            }
            AuthAction::Revoke { key_id, data_dir } => {
                commands::auth::revoke_key(&opts.format, &resolve_dir(data_dir), &key_id)
            }
        };
    }

    let server_url = opts.server_url();

    #[cfg(feature = "gui")]
    if let Commands::Gui = command {
        tael_gui::run_with_server(server_url);
        return Ok(());
    }

    // The GUI is opt-in (default = []); on a headless build the subcommand still
    // parses, but there's no Tauri app linked in, so explain how to get one.
    #[cfg(not(feature = "gui"))]
    if let Commands::Gui = command {
        bail!(
            "this `tael` was built without the desktop GUI. \
             Reinstall with `cargo install tael-cli --features gui` to enable `tael gui`."
        );
    }

    // An explicit --api-key wins; otherwise the client picks up TAEL_API_KEY.
    let client = match &opts.api_key {
        Some(key) => client::TaelClient::with_api_key(&server_url, Some(key)),
        None => client::TaelClient::new(&server_url),
    };

    match command {
        // Handled above; the early return means this arm is never reached.
        Commands::Serve { .. } => unreachable!(),
        // Handled above; the early return means this arm is never reached.
        Commands::Auth { .. } => unreachable!(),
        // Handled above; the early return means this arm is never reached.
        Commands::Config { .. } => unreachable!(),
        // Handled above; the early return / bail means this arm is never reached.
        Commands::Gui => unreachable!(),
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
                explain,
            } => {
                commands::query::traces(
                    &client,
                    &opts.format,
                    service,
                    operation,
                    min_duration,
                    max_duration,
                    status,
                    last,
                    limit,
                    attribute,
                    text,
                    explain,
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
                    &opts.format,
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
                    &opts.format,
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
                commands::query::sql(&client, &opts.format, &query).await?;
            }
        },
        Commands::Get { resource } => match resource {
            GetResource::Trace { trace_id } => {
                commands::get::trace(&client, &opts.format, &trace_id).await?;
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
                    &opts.format,
                    &trace_id,
                    &body,
                    Some(&author),
                    span_id.as_deref(),
                )
                .await?;
            }
            CommentAction::List { trace_id } => {
                commands::comment::list(&client, &opts.format, &trace_id).await?;
            }
        },
        Commands::Services => {
            commands::services::list(&client, &opts.format).await?;
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
            commands::summarize::run(&client, &opts.format, last, service).await?;
        }
        Commands::Anomalies {
            last,
            baseline,
            service,
        } => {
            commands::anomalies::run(&client, &opts.format, last, baseline, service).await?;
        }
        Commands::Correlate { trace } => {
            commands::correlate::run(&client, &opts.format, &trace).await?;
        }
        Commands::Watch {
            last,
            service,
            interval,
            exit_on,
            max_ticks,
        } => {
            commands::watch::run(
                &client,
                &opts.format,
                last,
                service,
                interval,
                exit_on,
                max_ticks,
            )
            .await?;
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
                commands::eval::score(&client, &opts.format, &run_id, &scores).await?;
            }
            EvalAction::Runs => {
                commands::eval::runs(&client, &opts.format).await?;
            }
            EvalAction::Status { run_id } => {
                commands::eval::status(&client, &opts.format, &run_id).await?;
            }
            EvalAction::Cases { run_id } => {
                commands::eval::cases(&client, &opts.format, &run_id).await?;
            }
            EvalAction::Scores { run_id } => {
                commands::eval::scores(&client, &opts.format, &run_id).await?;
            }
            EvalAction::Report { run_id } => {
                commands::eval::report(&client, &opts.format, &run_id).await?;
            }
            EvalAction::Compare {
                run_id,
                baseline_run_id,
            } => {
                commands::eval::compare(&client, &opts.format, &run_id, &baseline_run_id).await?;
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
                        &opts.format,
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
                    commands::eval::case_link(&client, &opts.format, &case_id, &issue_id, trace_id)
                        .await?;
                }
            },
            EvalAction::Suite { action } => match action {
                EvalSuiteAction::Inspect { suite, limit } => {
                    commands::eval::suite_inspect(&client, &opts.format, &suite, limit).await?;
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
                    &opts.format,
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
                commands::issue::list(&client, &opts.format, limit).await?;
            }
            IssueAction::Examples { issue_id, limit } => {
                commands::issue::examples(&client, &opts.format, &issue_id, limit).await?;
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
                    &opts.format,
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
                commands::signal::trend(&client, &opts.format, &name, limit).await?;
            }
        },
        Commands::Experiment { action } => match action {
            ExperimentAction::Compare {
                experiment_id,
                signal,
                last,
            } => {
                commands::experiment::compare(&client, &opts.format, &experiment_id, signal, last)
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
                    &opts.format,
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
                commands::diagnose::list(&client, &opts.format, limit).await?;
            }
        },
        Commands::Server { action } => match action {
            ServerAction::Status => {
                commands::server::status(&client, &opts.format).await?;
            }
        },
        Commands::Mcp { action } => match action {
            McpAction::Serve => {
                mcp::serve(client, &server_url).await?;
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
