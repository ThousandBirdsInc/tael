//! tael-server: OTLP ingest, tiered storage, and the REST/gRPC query API.
//!
//! Shipped as a library so the `tael` binary can embed it behind `tael serve`
//! (a single `cargo install`), while still being usable as a standalone crate.
//! [`run`] is the default CLI-style entrypoint; [`run_embedded`] starts the same
//! server in quiet mode for in-process integrations. [`ServerConfig`] configures
//! the listeners and storage.

// `from_str` on these enums predates and mirrors the codebase's own
// convention; renaming them to satisfy the trait-confusion lint would be a
// breaking change to a public API for no behavioral gain.
#![allow(clippy::should_implement_trait)]
// Constructors that mirror a wide CLI flag set or a REST router's dependency
// list are long by nature; bundling them into a struct would only move the
// argument count somewhere less visible.
#![allow(clippy::too_many_arguments)]

pub mod alerts;
mod api;
pub mod auth;
mod cluster;
mod config;
mod ingest;
mod log_bus;
mod promql;
pub mod retention;
pub mod scoring;
pub mod similarity;
mod span_bus;
#[cfg(feature = "sql")]
pub mod sql;
mod storage;
pub mod suites;
pub mod tenancy;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::net::TcpListener;
use tonic::transport::Server as TonicServer;
use tracing_subscriber::EnvFilter;

pub use config::{
    DEFAULT_DD_AGENT_ADDR, DEFAULT_OTLP_HTTP_ADDR, ServerConfig, StorageBackend,
    parse_dd_agent_addr, parse_otlp_http_addr,
};
#[cfg(feature = "duckdb")]
pub use storage::DuckDbStore;
pub use storage::models::{
    LogRecord, LogSeverity, MetricPoint, MetricType, Span, SpanEvent, SpanKind, SpanStatus,
    TraceQuery,
};
pub use storage::{
    BlobStore, FanoutStore, RemoteStore, RemoteWalSink, Store, TaelBackend, WalSink,
};
use storage::{StoreLocation, open_comments, open_object_backend};

use log_bus::LogBus;
use span_bus::SpanBus;

/// Controls output that the tael-server library owns directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerOutputMode {
    /// Install the default tracing subscriber when possible and print the
    /// startup banner to stdout. This is the right mode for `tael serve`.
    Default,
    /// Do not install a tracing subscriber and do not print the startup banner.
    /// Existing application-level tracing subscribers may still receive Tael
    /// events; this only prevents the library from claiming stdout/stderr on
    /// its own.
    Quiet,
}

/// Options for running the server process beyond listener/storage config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerRunOptions {
    pub output: ServerOutputMode,
}

impl Default for ServerRunOptions {
    fn default() -> Self {
        Self {
            output: ServerOutputMode::Default,
        }
    }
}

impl ServerRunOptions {
    /// Quiet options for embedding Tael inside another CLI/TUI process.
    pub fn quiet() -> Self {
        Self {
            output: ServerOutputMode::Quiet,
        }
    }

    fn is_quiet(self) -> bool {
        matches!(self.output, ServerOutputMode::Quiet)
    }
}

/// Whether this node may garbage-collect the (possibly shared) blob store.
///
/// On a shared store, per-node mark-and-sweep would delete blobs other shards
/// still reference, so exactly one owner must run it. In a coordinated cluster
/// the elected leader is that owner — checked live each pass, so GC ownership
/// follows failover instead of dying with a statically designated node.
/// Without a cluster the operator designates the owner statically.
#[derive(Clone)]
enum BlobGcOwnership {
    /// Node-local blob store: every node owns its own blobs and GCs freely.
    Always,
    /// Shared store, no cluster, not the designated coordinator.
    Never,
    /// Shared store in a coordinated cluster: GC only while leader.
    Leader(Arc<cluster::ClusterCoordinator>),
}

impl BlobGcOwnership {
    fn may_gc(&self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Leader(c) => c.is_leader(),
        }
    }
}

/// Periodically roll aged signals into the cold tier and drop expired
/// partitions, following the resolved [`RetentionPolicy`]. Runs the (blocking)
/// compaction off the async executor. A 0-hour hot-tier window compacts
/// everything, which is what the tests rely on.
fn spawn_span_compactor(
    backend: Arc<TaelBackend>,
    blobs: Arc<BlobStore>,
    gc_ownership: BlobGcOwnership,
    policy: retention::RetentionPolicy,
) {
    tokio::spawn(async move {
        let mut tick =
            tokio::time::interval(std::time::Duration::from_secs(policy.compact_interval_secs));
        loop {
            tick.tick().await;
            let backend = Arc::clone(&backend);
            let blobs = Arc::clone(&blobs);
            let policy = policy.clone();
            // Sampled per pass, so a node that loses leadership stops GCing on
            // its next tick and the new leader picks it up.
            let blob_gc_enabled = gc_ownership.may_gc();
            let result = tokio::task::spawn_blocking(move || {
                // One clock for the whole pass, so signals don't drift apart
                // across a long compaction.
                let cutoffs = policy.cutoffs(chrono::Utc::now());
                let started = std::time::Instant::now();
                let mut compacted = backend.compact_spans(cutoffs.hot_tier)?;
                compacted += backend.compact_logs_metrics(cutoffs.hot_tier)?;
                let dropped = backend.enforce_retention(&cutoffs)?;
                // Payload blob GC: drop blobs no live row references (e.g. rows
                // just removed by retention). Runs after partition drops. Skipped
                // when this node doesn't own GC over a shared blob store (the
                // single-owner guard), to avoid deleting blobs other shards
                // reference.
                let blobs_gcd = if blob_gc_enabled {
                    let live = backend.collect_live_blob_hashes()?;
                    blobs.gc(&live)?
                } else {
                    0
                };
                let elapsed_ms = started.elapsed().as_millis() as f64;
                // The engine reports on itself through its own metric pipeline,
                // so compaction health is queryable and alertable like any
                // other series instead of living only in log lines.
                let points = engine_metric_points(&[
                    ("tael.engine.compacted_rows", compacted as f64),
                    ("tael.engine.partitions_dropped", dropped as f64),
                    ("tael.engine.blobs_gcd", blobs_gcd as f64),
                    ("tael.engine.maintenance_ms", elapsed_ms),
                ]);
                if let Err(e) = backend.insert_metrics(&points) {
                    tracing::warn!(error = %e, "failed to record engine metrics");
                }
                anyhow::Ok((compacted, dropped, blobs_gcd))
            })
            .await;
            match result {
                Ok(Ok((c, d, g))) if c > 0 || d > 0 || g > 0 => tracing::info!(
                    compacted = c,
                    partitions_dropped = d,
                    blobs_gcd = g,
                    "tael-backend maintenance"
                ),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "maintenance failed"),
                Err(e) => tracing::warn!(error = %e, "maintenance task panicked"),
            }
        }
    });
}

/// Gauge points for the engine's own health, emitted under the `tael` service
/// each maintenance pass.
fn engine_metric_points(values: &[(&str, f64)]) -> Vec<storage::models::MetricPoint> {
    let now = chrono::Utc::now();
    values
        .iter()
        .map(|(name, value)| storage::models::MetricPoint {
            timestamp: now,
            service: "tael".to_string(),
            name: (*name).to_string(),
            metric_type: storage::models::MetricType::Gauge,
            value: *value,
            unit: String::new(),
            attributes: std::collections::HashMap::new(),
            histogram: None,
        })
        .collect()
}

/// Evaluate alert rules on a schedule and deliver the resulting transitions.
///
/// Span-derived series are written before each pass so a rule can reference
/// error rate or p95 latency without a service first emitting them as metrics.
fn spawn_alert_evaluator(
    store: Arc<dyn Store>,
    alerts: Arc<alerts::AlertStore>,
    interval_secs: u64,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        loop {
            tick.tick().await;
            if alerts.list().is_empty() {
                continue;
            }

            // Derivation and evaluation both hit storage synchronously.
            let derived = {
                let store = Arc::clone(&store);
                let alerts = Arc::clone(&alerts);
                tokio::task::spawn_blocking(move || {
                    // The window matches the shortest rule window so derived
                    // points stay fresh enough for every rule to see them.
                    let window = alerts
                        .list()
                        .iter()
                        .map(|r| r.window_seconds)
                        .min()
                        .unwrap_or(300);
                    let points = alerts::span_derived_points(store.as_ref(), window)?;
                    store.insert_metrics(&points)?;
                    anyhow::Ok(alerts.evaluate_all(store.as_ref(), chrono::Utc::now()))
                })
                .await
            };

            let events = match derived {
                Ok(Ok(events)) => events,
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "alert evaluation pass failed");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "alert evaluation task panicked");
                    continue;
                }
            };

            for event in events {
                tracing::info!(
                    rule = %event.rule,
                    state = ?event.state,
                    "alert state changed"
                );
                let rules = alerts.list();
                let Some(rule) = rules.iter().find(|r| r.name == event.rule) else {
                    continue;
                };
                for sink in &rule.sinks {
                    alerts::deliver(&event, sink).await;
                }
            }
        }
    });
}

/// Sample production traces and run each score rule's scorer against them.
///
/// Scoring runs off the ingest path entirely: a slow or hanging judge delays
/// only its own results, never a write. Results land as ordinary
/// `tael_eval_score` points, so they trend, alert, and compare exactly like
/// offline eval scores.
fn spawn_online_scorer(
    store: Arc<dyn Store>,
    rules: Arc<scoring::ScoreRuleStore>,
    interval_secs: u64,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        // Only consider traces from roughly the last pass, so a rule added
        // today does not immediately score a month of history.
        let window_seconds = (interval_secs * 2) as i64;
        loop {
            tick.tick().await;
            for rule in rules.list() {
                let candidates = {
                    let store = Arc::clone(&store);
                    let rules = Arc::clone(&rules);
                    let rule = rule.clone();
                    tokio::task::spawn_blocking(move || {
                        rules.select_candidates(store.as_ref(), &rule, window_seconds)
                    })
                    .await
                };
                let candidates = match candidates {
                    Ok(Ok(c)) => c,
                    Ok(Err(e)) => {
                        tracing::warn!(rule = %rule.name, error = %e, "selecting score candidates failed");
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(rule = %rule.name, error = %e, "score selection panicked");
                        continue;
                    }
                };

                for span in candidates {
                    match scoring::run_scorer(&rule, &span).await {
                        Ok(scores) => {
                            let written = record_online_scores(&store, &rule, &span, &scores);
                            rules.mark_scored(&rule.name, &span.trace_id, written);
                        }
                        Err(e) => {
                            tracing::warn!(
                                rule = %rule.name, trace = %span.trace_id, error = %e,
                                "scorer failed"
                            );
                            rules.mark_failed(&rule.name, &span.trace_id, e.to_string());
                        }
                    }
                }
            }
        }
    });
}

/// Write a scorer's output as `tael_eval_score` metric points.
///
/// The points carry the rule name, sample rate, and source trace so a score can
/// always be traced back to the traffic that produced it — a number with no
/// provenance is not evidence.
fn record_online_scores(
    store: &Arc<dyn Store>,
    rule: &scoring::ScoreRule,
    span: &storage::models::Span,
    scores: &[scoring::ScoreLine],
) -> u64 {
    let points: Vec<MetricPoint> = scores
        .iter()
        .map(|score| {
            let mut attributes = std::collections::HashMap::new();
            attributes.insert("rule".to_string(), rule.name.clone());
            attributes.insert("metric".to_string(), score.metric.clone());
            attributes.insert("trace_id".to_string(), span.trace_id.clone());
            attributes.insert("source".to_string(), "online".to_string());
            attributes.insert("sample".to_string(), rule.sample.to_string());
            attributes.insert("case_id".to_string(), span.trace_id.clone());
            MetricPoint {
                timestamp: chrono::Utc::now(),
                service: span.service.clone(),
                name: "tael_eval_score".to_string(),
                metric_type: MetricType::Gauge,
                value: score.value,
                unit: "score".to_string(),
                attributes,
                histogram: None,
            }
        })
        .collect();

    match store.insert_metrics(&points) {
        Ok(()) => points.len() as u64,
        Err(e) => {
            tracing::warn!(rule = %rule.name, error = %e, "writing online scores failed");
            0
        }
    }
}

/// Resolve the auth mode for this configuration and open the keystore.
///
/// The fail-closed rule lives here: a server reachable from off-box with no
/// usable keys refuses to start instead of publishing an unauthenticated
/// telemetry store. The refusal names both ways out (mint a key, or opt out
/// explicitly) because whichever one an operator wants, guessing the flag is
/// the last thing they should have to do at that moment.
fn setup_auth(config: &ServerConfig) -> Result<api::authz::AuthState> {
    let mut listeners: Vec<&str> = vec![&config.otlp_grpc_addr];
    // A Unix-socket REST listener is filesystem-scoped, so it doesn't widen
    // reachability the way a TCP bind does.
    if config.rest_api_socket.is_none() {
        listeners.push(&config.rest_api_addr);
    }
    if let Some(addr) = &config.otlp_http_addr {
        listeners.push(addr);
    }
    if let Some(addr) = &config.dd_agent_addr {
        listeners.push(addr);
    }

    let mode = auth::AuthMode::resolve(config.auth, &listeners);
    let state = api::authz::AuthState::new(mode, &config.data_dir)?;

    if mode == auth::AuthMode::Required {
        let keys = auth::KeyStore::load(&config.data_dir)?;
        if !keys.has_active_keys() {
            bail!(
                "refusing to start: this server listens on a non-loopback address \
                 ({}) but has no API keys, so it would accept telemetry and serve \
                 queries to anyone who can reach it.\n\n\
                 Create a key:\n  \
                 tael auth create-key --name my-agent --role writer --data-dir {}\n\n\
                 Or accept an unauthenticated listener explicitly (e.g. when the \
                 port is already firewalled or bound inside a container):\n  \
                 tael serve --auth off",
                listeners
                    .iter()
                    .find(|a| !a.starts_with("127.0.0.1") && !a.starts_with("localhost"))
                    .copied()
                    .unwrap_or("non-loopback"),
                config.data_dir,
            );
        }
        tracing::info!(keys = keys.keys.len(), "API key auth required");
    }

    Ok(state)
}

/// Start the server with the default user-facing output behavior.
///
/// This is the right entrypoint for binaries such as `tael serve`: it installs a
/// default tracing subscriber if the process has not already done so and prints
/// a startup banner to stdout.
pub async fn run(config: ServerConfig) -> Result<()> {
    run_with_options(config, ServerRunOptions::default()).await
}

/// Start the server in quiet mode for in-process integrations.
///
/// Quiet mode avoids Tael-owned stdout/stderr setup so one-shot commands and
/// TUIs embedding the server can preserve their own output contract.
pub async fn run_embedded(config: ServerConfig) -> Result<()> {
    run_with_options(config, ServerRunOptions::quiet()).await
}

/// Start the server with explicit run options.
///
/// Runs until both listeners receive shutdown. The configured storage backend is
/// shared by OTLP ingest and REST query APIs, with the background maintenance
/// task enabled when running on tael-backend.
pub async fn run_with_options(mut config: ServerConfig, options: ServerRunOptions) -> Result<()> {
    // Initialize tracing for the server process in the default CLI mode.
    // `try_init` keeps embedding in a binary that already set a subscriber from
    // panicking. Quiet mode leaves all tracing ownership to the host process.
    if !options.is_quiet() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .try_init();
    }

    configure_walrus_data_dir(&config.wal_dir);

    // Resolve auth and retention before anything binds, so a misconfigured
    // deployment fails at startup rather than after it is already accepting
    // traffic.
    let auth_state = Arc::new(setup_auth(&config)?);
    let retention_policy =
        retention::RetentionPolicy::resolve(config.config_path.as_deref(), &config.data_dir)?;
    tracing::info!(
        traces_days = retention_policy.traces,
        logs_days = retention_policy.logs,
        metrics_days = retention_policy.metrics_raw,
        rollups_days = retention_policy.metrics_rollups,
        hot_tier_hours = retention_policy.hot_tier_hours,
        "retention policy resolved"
    );

    // Blob store: local filesystem by default; object storage when configured
    // (opt-in, requires the `cloud` feature — otherwise this fails loudly).
    let blobs = Arc::new(match config.object_store.blobs {
        StoreLocation::Fs => BlobStore::new(&config.data_dir)?,
        location => {
            let backend = open_object_backend(
                location,
                Path::new(&config.data_dir).join("blobs").as_path(),
                config.object_store.blob_bucket.as_deref(),
            )?;
            BlobStore::with_backend(backend)?
        }
    });

    // Cluster coordination (chitchat): automatic leader election + epoch fencing
    // of WAL replication (§5.1). On when TAEL_CLUSTER_LISTEN is set.
    let coordinator = match &config.cluster {
        Some(cs) => {
            let coord = cluster::ClusterCoordinator::start(cluster::ClusterConfig {
                node_id: cs.node_id.clone(),
                listen_addr: cs
                    .listen_addr
                    .parse()
                    .context("parsing TAEL_CLUSTER_LISTEN")?,
                advertise_addr: cs
                    .advertise_addr
                    .parse()
                    .context("parsing TAEL_CLUSTER_ADVERTISE")?,
                seeds: cs.seeds.clone(),
                cluster_id: cs.cluster_id.clone(),
            })
            .await?;
            Some(coord)
        }
        None => None,
    };

    // The payload search index is shared between the ingest path (writes) and
    // the tael-backend query path (reads); present only when that engine runs.
    let mut search: Option<Arc<storage::SearchIndex>> = None;
    let store: Arc<dyn Store> = if !config.query_shards.is_empty() {
        // Stateless query-tier mode: serve reads by scatter-gather over remote
        // shards, no local engine (`docs/tael-server-scaling-ha.md` §3, Phase 2).
        let shards = config
            .query_shards
            .iter()
            .map(|url| RemoteStore::new(url).map(|s| Arc::new(s) as Arc<dyn Store>))
            .collect::<Result<Vec<_>>>()?;
        tracing::info!(
            shards = shards.len(),
            "query fan-out mode: reads scatter-gather across remote shards (no local engine)"
        );
        Arc::new(FanoutStore::new(shards)?)
    } else {
        match config.storage {
            #[cfg(feature = "duckdb")]
            StorageBackend::Duckdb => Arc::new(DuckDbStore::new(&config.data_dir)?),
            #[cfg(not(feature = "duckdb"))]
            StorageBackend::Duckdb => {
                bail!(
                    "DuckDB storage is not included in this build; reinstall with `--features duckdb` to use --storage duckdb"
                )
            }
            StorageBackend::TaelBackend => {
                // WAL replication: when standbys are configured, this node is a
                // leader that ships every appended record to them before acking
                // (§5.1). With no standbys the write path is unchanged.
                let sinks: Vec<Arc<dyn WalSink>> = config
                    .wal_standbys
                    .iter()
                    .map(|url| {
                        // Stamp the leader epoch (for standby fencing) when a
                        // coordinator is running; otherwise ship unfenced.
                        let sink = match &coordinator {
                            Some(c) => RemoteWalSink::with_epoch(url, c.leader_epoch_handle()),
                            None => RemoteWalSink::new(url),
                        };
                        sink.map(|s| Arc::new(s) as Arc<dyn WalSink>)
                    })
                    .collect::<Result<Vec<_>>>()?;
                if !sinks.is_empty() {
                    tracing::info!(
                        standbys = sinks.len(),
                        required_acks = ?config.wal_required_acks,
                        "WAL replication enabled: shipping to standbys (leader)"
                    );
                }
                // Cold tier: local filesystem by default; object storage when
                // configured (opt-in, requires the `cloud` feature).
                let cold_backend = match config.object_store.cold {
                    StoreLocation::Fs => None,
                    location => Some(open_object_backend(
                        location,
                        Path::new(&config.data_dir).join("cold").as_path(),
                        config.object_store.cold_bucket.as_deref(),
                    )?),
                };
                // Comments: local JSONL by default; Postgres when configured.
                let comments = open_comments(&config.comments, &config.data_dir)?;
                let backend = Arc::new(TaelBackend::with_components(
                    &config.data_dir,
                    "tael-backend",
                    sinks,
                    config.wal_required_acks,
                    cold_backend,
                    comments,
                )?);
                search = Some(backend.search_index());
                // Blob GC single-owner guard: on a shared (object-store) blob
                // store, per-node mark-and-sweep would delete blobs other
                // shards still reference. In a coordinated cluster the elected
                // leader owns GC (re-checked every pass, so ownership follows
                // failover); without a cluster, the operator designates one
                // owner statically. Node-local stores GC freely.
                let gc_ownership = if !config.object_store.blobs_shared() {
                    BlobGcOwnership::Always
                } else if let Some(c) = &coordinator {
                    tracing::info!(
                        "blob GC leader-gated: shared blob store in a coordinated cluster, \
                         GC runs only while this node holds leadership"
                    );
                    BlobGcOwnership::Leader(Arc::clone(c))
                } else if config.object_store.blob_gc_coordinator {
                    BlobGcOwnership::Always
                } else {
                    tracing::info!(
                        "blob GC disabled on this node: shared blob store, not the GC coordinator \
                         (set TAEL_BLOB_GC_ROLE=coordinator on exactly one node)"
                    );
                    BlobGcOwnership::Never
                };
                spawn_span_compactor(
                    Arc::clone(&backend),
                    Arc::clone(&blobs),
                    gc_ownership,
                    retention_policy.clone(),
                );
                backend as Arc<dyn Store>
            }
        }
    };
    let bus = Arc::new(SpanBus::new()?);
    let log_bus = Arc::new(LogBus::new()?);
    let alert_store = Arc::new(alerts::AlertStore::open(&config.data_dir)?);
    let score_rules = Arc::new(scoring::ScoreRuleStore::open(&config.data_dir)?);
    let suite_store = Arc::new(suites::SuiteStore::open(&config.data_dir)?);
    spawn_online_scorer(Arc::clone(&store), Arc::clone(&score_rules), 60);
    // Evaluate more often than the compaction pass: an alert is only useful if
    // it fires close to when the condition started.
    spawn_alert_evaluator(Arc::clone(&store), Arc::clone(&alert_store), 30);

    tracing::info!(
        otlp_grpc = %config.otlp_grpc_addr,
        rest_api = %config.rest_api_addr,
        rest_api_socket = ?config.rest_api_socket,
        dd_agent = ?config.dd_agent_addr,
        data_dir = %config.data_dir,
        wal_dir = %config.wal_dir,
        storage = ?config.storage,
        "starting tael server"
    );

    // The OTLP services are shared by both transports: gRPC (:4317) and
    // HTTP/protobuf (:4318) hand decoded batches to the same implementations,
    // so there is one ingest path regardless of how a client speaks to it.
    let otlp_services = ingest::otlp_http::OtlpHttpState {
        traces: Arc::new(ingest::otlp::OtlpTraceService::new(
            Arc::clone(&store),
            Arc::clone(&blobs),
            search.clone(),
            Arc::clone(&bus),
        )),
        logs: Arc::new(ingest::otlp_logs::OtlpLogsService::new(
            Arc::clone(&store),
            Arc::clone(&blobs),
            search.clone(),
            Arc::clone(&log_bus),
        )),
        metrics: Arc::new(ingest::otlp_metrics::OtlpMetricsService::new(Arc::clone(
            &store,
        ))),
    };

    let grpc_handle = tokio::spawn({
        let otlp = otlp_services.clone();
        let grpc_auth = Arc::clone(&auth_state);
        let addr = config.otlp_grpc_addr.parse()?;
        async move {
            let trace_service = ingest::otlp::SharedTraceService(otlp.traces);
            let logs_service = ingest::otlp_logs::SharedLogsService(otlp.logs);
            let metrics_service = ingest::otlp_metrics::SharedMetricsService(otlp.metrics);
            TonicServer::builder()
                .add_service(
                    opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceServiceServer::with_interceptor(trace_service, api::authz::grpc_interceptor(Arc::clone(&grpc_auth))),
                )
                .add_service(
                    opentelemetry_proto::tonic::collector::logs::v1::logs_service_server::LogsServiceServer::with_interceptor(logs_service, api::authz::grpc_interceptor(Arc::clone(&grpc_auth))),
                )
                .add_service(
                    opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::MetricsServiceServer::with_interceptor(metrics_service, api::authz::grpc_interceptor(Arc::clone(&grpc_auth))),
                )
                .serve_with_shutdown(addr, shutdown_signal())
                .await
                .expect("gRPC server failed");
        }
    });

    let rest_handle = tokio::spawn({
        let store = Arc::clone(&store);
        let blobs = Arc::clone(&blobs);
        let bus = Arc::clone(&bus);
        let log_bus = Arc::clone(&log_bus);
        let cluster = coordinator.clone();
        let addr = config.rest_api_addr.clone();
        let socket = config.rest_api_socket.clone();
        let otlp = otlp_services.clone();
        let auth = Arc::clone(&auth_state);
        let alerts = Arc::clone(&alert_store);
        let scores = Arc::clone(&score_rules);
        let suites = Arc::clone(&suite_store);
        let data_dir = config.data_dir.clone();
        let multi_tenant = config.multi_tenant;
        async move {
            // OTLP/HTTP is mounted here as well as on its own listener, so a
            // deployment that can expose only one port still accepts it.
            let app = api::rest::router(
                store,
                blobs,
                bus,
                log_bus,
                cluster,
                alerts,
                scores,
                suites,
                data_dir,
                multi_tenant,
            )
            .merge(ingest::otlp_http::router(otlp))
            .layer(axum::middleware::from_fn_with_state(
                auth,
                api::authz::require_auth,
            ));
            if let Some(socket) = socket {
                #[cfg(unix)]
                {
                    prepare_unix_socket_path(&socket)?;
                    let listener = tokio::net::UnixListener::bind(&socket)
                        .with_context(|| format!("binding REST Unix socket {socket}"))?;
                    tracing::info!(%socket, "REST API listening on Unix socket");
                    let result = axum::serve(listener, app)
                        .with_graceful_shutdown(shutdown_signal())
                        .await
                        .context("REST server failed");
                    cleanup_unix_socket_path(&socket);
                    result?;
                }
                #[cfg(not(unix))]
                {
                    bail!("REST Unix sockets are only supported on Unix platforms");
                }
            } else {
                let listener = TcpListener::bind(&addr)
                    .await
                    .with_context(|| format!("binding REST addr {addr}"))?;
                tracing::info!(%addr, "REST API listening");
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown_signal())
                    .await
                    .context("REST server failed")?;
            }
            Ok::<(), anyhow::Error>(())
        }
    });

    // OTLP/HTTP listener on the spec's port, so SDKs defaulting to
    // `http/protobuf` need no server-side configuration. A bind failure is a
    // warning rather than fatal: the same routes stay mounted on the REST
    // listener, reachable via OTEL_EXPORTER_OTLP_ENDPOINT.
    let otlp_http_requested = config.otlp_http_addr.clone();
    let otlp_http_listener = match &config.otlp_http_addr {
        Some(addr) => match TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(%addr, "OTLP/HTTP listening");
                Some(listener)
            }
            Err(e) => {
                tracing::warn!(
                    %addr, error = %e,
                    "OTLP/HTTP port unavailable (another collector running?); \
                     OTLP/HTTP intake stays available on the REST listener"
                );
                config.otlp_http_addr = None;
                None
            }
        },
        None => None,
    };
    let otlp_http_handle = otlp_http_listener.map(|listener| {
        tokio::spawn({
            let state = otlp_services.clone();
            let auth = Arc::clone(&auth_state);
            async move {
                let app = ingest::otlp_http::router(state).layer(
                    axum::middleware::from_fn_with_state(auth, api::authz::require_auth),
                );
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown_signal())
                    .await
                    .context("OTLP/HTTP server failed")
            }
        })
    });
    let otlp_http_unavailable = match config.otlp_http_addr {
        None => otlp_http_requested,
        Some(_) => None,
    };

    // Dedicated Datadog trace-agent listener on the agent's default port, so
    // dd-trace clients work with no configuration at all. Bound here (not in
    // the task) so the startup banner reflects reality: a bind failure (most
    // likely a real Datadog agent already on 8126) is a warning, not fatal —
    // the same endpoints stay available on the REST listener via
    // DD_TRACE_AGENT_URL, and the banner falls back to that advice. The
    // requested address is kept so the banner can say why it fell back
    // (the tracing warning is filtered out under the default RUST_LOG).
    let dd_addr_requested = config.dd_agent_addr.clone();
    let dd_listener = match &config.dd_agent_addr {
        Some(addr) => match TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(%addr, "Datadog trace-agent intake listening");
                Some(listener)
            }
            Err(e) => {
                tracing::warn!(
                    %addr, error = %e,
                    "Datadog agent port unavailable (another agent running?); \
                     dd-trace intake stays available on the REST listener"
                );
                config.dd_agent_addr = None;
                None
            }
        },
        None => None,
    };
    let dd_handle = dd_listener.map(|listener| {
        tokio::spawn({
            let store = Arc::clone(&store);
            let blobs = Arc::clone(&blobs);
            let bus = Arc::clone(&bus);
            let log_bus = Arc::clone(&log_bus);
            let auth = Arc::clone(&auth_state);
            async move {
                let app = api::rest::dd_router(store, blobs, bus, log_bus).layer(
                    axum::middleware::from_fn_with_state(auth, api::authz::require_auth),
                );
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown_signal())
                    .await
                    .context("Datadog trace-agent server failed")
            }
        })
    });

    // Requested a dedicated agent listener but the bind failed above.
    let dd_addr_unavailable = match config.dd_agent_addr {
        None => dd_addr_requested,
        Some(_) => None,
    };

    if !options.is_quiet() {
        print_startup_banner(
            &config,
            dd_addr_unavailable.as_deref(),
            otlp_http_unavailable.as_deref(),
        );
    }

    // All listeners drain on SIGTERM/Ctrl-C; await them so in-flight requests
    // finish before we flush and exit (`docs/tael-server-scaling-ha.md` §5.4).
    let (grpc_res, rest_res) = tokio::join!(grpc_handle, rest_handle);
    grpc_res?;
    rest_res??;
    if let Some(otlp_http_handle) = otlp_http_handle {
        otlp_http_handle.await??;
    }
    if let Some(dd_handle) = dd_handle {
        dd_handle.await??;
    }

    // Best-effort flush so a restart/standby replays less WAL. Durability is
    // already guaranteed by the per-write WAL fsync.
    if let Err(e) = store.flush() {
        tracing::warn!(error = %e, "flush on shutdown failed");
    }
    tracing::info!("tael server stopped");

    Ok(())
}

fn configure_walrus_data_dir(wal_dir: &str) {
    // walrus-rust currently exposes its storage root through process env only.
    // Tael owns the server process and sets this once before opening the WAL.
    unsafe {
        std::env::set_var("WALRUS_DATA_DIR", wal_dir);
    }
}

#[cfg(unix)]
fn prepare_unix_socket_path(socket: &str) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let path = std::path::Path::new(socket);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating REST socket directory {}", parent.display()))?;
    }

    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            bail!(
                "REST Unix socket path already exists: {}. Remove it if no server is running.",
                path.display()
            );
        }
        Ok(_) => {
            bail!(
                "REST Unix socket path exists and is not a socket: {}",
                path.display()
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("checking REST socket path {}", path.display())),
    }
}

#[cfg(unix)]
fn cleanup_unix_socket_path(socket: &str) {
    use std::os::unix::fs::FileTypeExt;

    let path = std::path::Path::new(socket);
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            if let Err(e) = std::fs::remove_file(path) {
                tracing::warn!(socket = %path.display(), error = %e, "failed to remove REST Unix socket");
            }
        }
        Ok(_) | Err(_) => {}
    }
}

/// Friendly stdout banner shown on startup so a user running `tael serve`
/// (with or without `--port`) immediately sees where to connect a CLI and
/// where to point an OTLP exporter. Goes through `println!` so it's visible
/// regardless of `RUST_LOG`.
///
/// `dd_addr_unavailable` is the requested dedicated agent address when its
/// bind failed: the dd-trace endpoints are always mounted on the REST
/// listener too, so the banner always has a Datadog address to show — it just
/// notes why the dedicated port isn't it.
fn print_startup_banner(
    config: &ServerConfig,
    dd_addr_unavailable: Option<&str>,
    otlp_http_unavailable: Option<&str>,
) {
    let rest = rest_endpoint_label(config);
    let otlp = &config.otlp_grpc_addr;
    let connect_flag = cli_connect_flag(config);

    println!("tael server starting");
    println!("  REST API     {rest}");
    println!("  OTLP gRPC    {otlp}");
    match &config.otlp_http_addr {
        Some(addr) => println!("  OTLP HTTP    {addr}"),
        None => match otlp_http_unavailable {
            Some(requested) => println!(
                "  OTLP HTTP    {} ({requested} unavailable — another collector running?)",
                rest_endpoint_label(config)
            ),
            None => println!(
                "  OTLP HTTP    {} (via REST listener)",
                rest_endpoint_label(config)
            ),
        },
    }
    match &config.dd_agent_addr {
        Some(addr) => println!("  dd-trace     {addr}"),
        None => match dd_addr_unavailable {
            Some(requested) => println!(
                "  dd-trace     {} ({requested} unavailable — another agent running?)",
                dd_agent_url(config)
            ),
            None => println!(
                "  dd-trace     {} (via REST listener)",
                dd_agent_url(config)
            ),
        },
    }
    println!("  data dir     {}", config.data_dir);
    println!("  WAL dir      {}", config.wal_dir);
    println!("  storage      {:?}", config.storage);
    println!();
    println!("Connect a CLI from this machine:");
    println!("  tael{connect_flag} services");
    println!("  tael{connect_flag} live");
    println!();
    println!("Point a service at this server (OTLP):");
    println!("  export OTEL_EXPORTER_OTLP_ENDPOINT=http://{otlp}");
    println!("  export OTEL_EXPORTER_OTLP_PROTOCOL=grpc");
    println!("  export OTEL_SERVICE_NAME=<your-service>");
    println!();
    println!("Or over OTLP/HTTP (the default protocol in several SDKs):");
    match &config.otlp_http_addr {
        Some(addr) => println!("  export OTEL_EXPORTER_OTLP_ENDPOINT=http://{addr}"),
        None => println!(
            "  export OTEL_EXPORTER_OTLP_ENDPOINT={}",
            rest_endpoint_label(config)
        ),
    }
    println!("  export OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf");
    println!();
    println!("Or a Datadog-instrumented service (dd-trace):");
    match &config.dd_agent_addr {
        // Listening on the agent's default port: dd-trace clients find it
        // with no configuration at all.
        Some(addr) if addr == config::DEFAULT_DD_AGENT_ADDR => {
            println!("  no exports needed — listening on the default agent port ({addr})");
        }
        Some(addr) => {
            println!("  export DD_TRACE_AGENT_URL=http://{addr}");
        }
        // Dedicated listener disabled or its port taken: the REST listener
        // still serves the trace-agent endpoints.
        None => {
            if let Some(requested) = dd_addr_unavailable {
                println!("  note: agent port {requested} was unavailable (another agent running?)");
            }
            println!("  export DD_TRACE_AGENT_URL={}", dd_agent_url(config));
        }
    }
    println!();
}

/// The `DD_TRACE_AGENT_URL` value that reaches this server's REST listener,
/// where the Datadog trace-agent endpoints are also mounted (used when the
/// dedicated agent-port listener is disabled). dd-trace clients accept both
/// `http://` and `unix://` agent URLs.
fn dd_agent_url(config: &ServerConfig) -> String {
    match &config.rest_api_socket {
        Some(socket) => format!("unix://{socket}"),
        None => format!("http://{}", config.rest_api_addr),
    }
}

/// Pick the CLI flag (if any) needed to reach this REST listener. Empty when
/// REST is on the CLI default `127.0.0.1:7701`; `--port-rest N` when only the
/// port differs; full `--server …` otherwise.
fn cli_connect_flag(config: &ServerConfig) -> String {
    if let Some(socket) = &config.rest_api_socket {
        return format!(" --unix-socket {socket}");
    }

    let rest_addr = &config.rest_api_addr;
    let (host, port) = match rest_addr.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => return String::new(),
    };
    let local = matches!(
        host,
        "127.0.0.1" | "localhost" | "0.0.0.0" | "::1" | "[::1]"
    );
    match (local, port) {
        (true, "7701") => String::new(),
        (true, p) => format!(" --port-rest {p}"),
        (false, _) => format!(" --server http://{rest_addr}"),
    }
}

fn rest_endpoint_label(config: &ServerConfig) -> String {
    match &config.rest_api_socket {
        Some(socket) => format!("unix://{socket}"),
        None => format!("http://{}", config.rest_api_addr),
    }
}

/// Resolve when the process is asked to stop: Ctrl-C, or SIGTERM on Unix
/// (the orchestrator's graceful-stop signal). Both listeners await their own
/// copy; the OS delivers the signal to every registered handler.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received; draining listeners");
}
