use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, Utc};
use duckdb::{params, Connection};

use super::models::{
    Anomaly, AnomalyReport, CorrelateReport, ErrorOperation, LogQuery, LogRecord, LogSeverity,
    LogSummary, MetricPoint, MetricQuery, MetricSummary, MetricType, ServiceSummary, Span,
    SpanStatus, SummaryReport, TraceComment, TraceQuery, TraceSummary,
};

fn parse_timestamp(s: &str) -> DateTime<Utc> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
        .map(|dt| dt.and_utc())
        .unwrap_or_default()
}

fn row_to_span(row: &duckdb::Row<'_>) -> duckdb::Result<Span> {
    let attrs_str: String = row.get(9)?;
    let events_str: String = row.get(10)?;
    let status_str: String = row.get(8)?;
    let parent: Option<String> = row.get(2)?;
    let start_str: String = row.get(5)?;
    let end_str: String = row.get(6)?;

    Ok(Span {
        trace_id: row.get(0)?,
        span_id: row.get(1)?,
        parent_span_id: if parent.as_deref() == Some("") {
            None
        } else {
            parent
        },
        service: row.get(3)?,
        operation: row.get(4)?,
        start_time: parse_timestamp(&start_str),
        end_time: parse_timestamp(&end_str),
        duration_ms: row.get(7)?,
        status: SpanStatus::from_str(&status_str),
        attributes: serde_json::from_str(&attrs_str).unwrap_or_default(),
        events: serde_json::from_str(&events_str).unwrap_or_default(),
    })
}

pub struct DuckDbStore {
    conn: Mutex<Connection>,
}

impl DuckDbStore {
    pub fn new(data_dir: &str) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = Path::new(data_dir).join("tael.duckdb");
        let conn = Connection::open(db_path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS spans (
                trace_id       VARCHAR NOT NULL,
                span_id        VARCHAR NOT NULL,
                parent_span_id VARCHAR,
                service        VARCHAR NOT NULL,
                operation      VARCHAR NOT NULL,
                start_time     TIMESTAMP NOT NULL,
                end_time       TIMESTAMP NOT NULL,
                duration_ms    DOUBLE NOT NULL,
                status         VARCHAR NOT NULL DEFAULT 'unset',
                attributes     JSON,
                events         JSON,
                PRIMARY KEY (trace_id, span_id)
            );

            CREATE INDEX IF NOT EXISTS idx_spans_service ON spans(service);
            CREATE INDEX IF NOT EXISTS idx_spans_start_time ON spans(start_time);
            CREATE INDEX IF NOT EXISTS idx_spans_trace_id ON spans(trace_id);
            CREATE INDEX IF NOT EXISTS idx_spans_status ON spans(status);

            CREATE TABLE IF NOT EXISTS trace_comments (
                id         VARCHAR NOT NULL PRIMARY KEY,
                trace_id   VARCHAR NOT NULL,
                span_id    VARCHAR,
                author     VARCHAR NOT NULL,
                body       VARCHAR NOT NULL,
                created_at TIMESTAMP NOT NULL DEFAULT current_timestamp::TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS idx_comments_trace_id ON trace_comments(trace_id);

            CREATE TABLE IF NOT EXISTS logs (
                timestamp          TIMESTAMP NOT NULL,
                observed_timestamp TIMESTAMP NOT NULL,
                trace_id           VARCHAR,
                span_id            VARCHAR,
                severity           VARCHAR NOT NULL DEFAULT 'unspecified',
                severity_text      VARCHAR NOT NULL DEFAULT '',
                body               VARCHAR NOT NULL DEFAULT '',
                service            VARCHAR NOT NULL,
                attributes         JSON
            );

            CREATE INDEX IF NOT EXISTS idx_logs_service ON logs(service);
            CREATE INDEX IF NOT EXISTS idx_logs_timestamp ON logs(timestamp);
            CREATE INDEX IF NOT EXISTS idx_logs_severity ON logs(severity);
            CREATE INDEX IF NOT EXISTS idx_logs_trace_id ON logs(trace_id);

            CREATE TABLE IF NOT EXISTS metrics (
                timestamp   TIMESTAMP NOT NULL,
                service     VARCHAR NOT NULL,
                name        VARCHAR NOT NULL,
                metric_type VARCHAR NOT NULL DEFAULT 'unknown',
                value       DOUBLE NOT NULL,
                unit        VARCHAR NOT NULL DEFAULT '',
                attributes  JSON
            );

            CREATE INDEX IF NOT EXISTS idx_metrics_service ON metrics(service);
            CREATE INDEX IF NOT EXISTS idx_metrics_name ON metrics(name);
            CREATE INDEX IF NOT EXISTS idx_metrics_timestamp ON metrics(timestamp);
            ",
        )?;
        Ok(())
    }

    pub fn insert_spans(&self, spans: &[Span]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO spans
             (trace_id, span_id, parent_span_id, service, operation,
              start_time, end_time, duration_ms, status, attributes, events)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )?;

        for span in spans {
            let attrs = serde_json::to_string(&span.attributes)?;
            let events = serde_json::to_string(&span.events)?;
            stmt.execute(params![
                span.trace_id,
                span.span_id,
                span.parent_span_id,
                span.service,
                span.operation,
                span.start_time.format("%Y-%m-%d %H:%M:%S%.6f").to_string(),
                span.end_time.format("%Y-%m-%d %H:%M:%S%.6f").to_string(),
                span.duration_ms,
                span.status.to_string(),
                attrs,
                events,
            ])?;
        }
        Ok(())
    }

    pub fn query_traces(&self, query: &TraceQuery) -> Result<Vec<Span>> {
        let conn = self.conn.lock().unwrap();

        let mut sql = String::from(
            "SELECT trace_id, span_id, parent_span_id, service, operation,
                    start_time::VARCHAR, end_time::VARCHAR, duration_ms, status, attributes, events
             FROM spans WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn duckdb::ToSql>> = Vec::new();

        if let Some(ref svc) = query.service {
            sql.push_str(" AND service = ?");
            param_values.push(Box::new(svc.clone()));
        }
        if let Some(ref op) = query.operation {
            sql.push_str(" AND operation LIKE ?");
            param_values.push(Box::new(format!("%{op}%")));
        }
        if let Some(min) = query.min_duration_ms {
            sql.push_str(" AND duration_ms >= ?");
            param_values.push(Box::new(min));
        }
        if let Some(max) = query.max_duration_ms {
            sql.push_str(" AND duration_ms <= ?");
            param_values.push(Box::new(max));
        }
        if let Some(ref status) = query.status {
            sql.push_str(" AND status = ?");
            param_values.push(Box::new(status.clone()));
        }
        if let Some(secs) = query.last_seconds {
            sql.push_str(&format!(" AND start_time >= current_timestamp::TIMESTAMP - INTERVAL '{secs} seconds'"));
        }

        sql.push_str(" ORDER BY start_time DESC");

        let limit = query.limit.unwrap_or(100);
        sql.push_str(&format!(" LIMIT {limit}"));

        let params_ref: Vec<&dyn duckdb::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_ref.as_slice(), row_to_span)?;

        let mut spans = Vec::new();
        for row in rows {
            spans.push(row?);
        }
        Ok(spans)
    }

    pub fn get_trace(&self, trace_id: &str) -> Result<Vec<Span>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT trace_id, span_id, parent_span_id, service, operation,
                    start_time::VARCHAR, end_time::VARCHAR, duration_ms, status, attributes, events
             FROM spans
             WHERE trace_id = ?
             ORDER BY start_time ASC",
        )?;

        let rows = stmt.query_map(params![trace_id], row_to_span)?;

        let mut spans = Vec::new();
        for row in rows {
            spans.push(row?);
        }
        Ok(spans)
    }

    pub fn add_comment(
        &self,
        trace_id: &str,
        span_id: Option<&str>,
        author: &str,
        body: &str,
    ) -> Result<TraceComment> {
        let conn = self.conn.lock().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO trace_comments (id, trace_id, span_id, author, body)
             VALUES (?, ?, ?, ?, ?)",
            params![id, trace_id, span_id, author, body],
        )?;

        let mut stmt = conn.prepare(
            "SELECT id, trace_id, span_id, author, body, created_at::VARCHAR
             FROM trace_comments WHERE id = ?",
        )?;
        let comment = stmt.query_row(params![id], |row| {
            Ok(TraceComment {
                id: row.get(0)?,
                trace_id: row.get(1)?,
                span_id: row.get(2)?,
                author: row.get(3)?,
                body: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        Ok(comment)
    }

    pub fn get_comments(&self, trace_id: &str) -> Result<Vec<TraceComment>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, trace_id, span_id, author, body, created_at::VARCHAR
             FROM trace_comments
             WHERE trace_id = ?
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![trace_id], |row| {
            Ok(TraceComment {
                id: row.get(0)?,
                trace_id: row.get(1)?,
                span_id: row.get(2)?,
                author: row.get(3)?,
                body: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        let mut comments = Vec::new();
        for row in rows {
            comments.push(row?);
        }
        Ok(comments)
    }

    pub fn list_services(&self) -> Result<Vec<ServiceInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT service,
                    COUNT(*) as span_count,
                    COUNT(DISTINCT trace_id) as trace_count,
                    AVG(duration_ms) as avg_duration,
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END)::DOUBLE / COUNT(*)::DOUBLE as error_rate
             FROM spans
             GROUP BY service
             ORDER BY span_count DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(ServiceInfo {
                name: row.get(0)?,
                span_count: row.get(1)?,
                trace_count: row.get(2)?,
                avg_duration_ms: row.get(3)?,
                error_rate: row.get(4)?,
            })
        })?;

        let mut services = Vec::new();
        for row in rows {
            services.push(row?);
        }
        Ok(services)
    }

    // ── Log storage ─────────────────────────────────────────────────

    pub fn insert_logs(&self, logs: &[LogRecord]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "INSERT INTO logs
             (timestamp, observed_timestamp, trace_id, span_id, severity,
              severity_text, body, service, attributes)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )?;

        for log in logs {
            let attrs = serde_json::to_string(&log.attributes)?;
            stmt.execute(params![
                log.timestamp.format("%Y-%m-%d %H:%M:%S%.6f").to_string(),
                log.observed_timestamp.format("%Y-%m-%d %H:%M:%S%.6f").to_string(),
                log.trace_id,
                log.span_id,
                log.severity.to_string(),
                log.severity_text,
                log.body,
                log.service,
                attrs,
            ])?;
        }
        Ok(())
    }

    pub fn query_logs(&self, query: &LogQuery) -> Result<Vec<LogRecord>> {
        let conn = self.conn.lock().unwrap();

        let mut sql = String::from(
            "SELECT timestamp::VARCHAR, observed_timestamp::VARCHAR, trace_id, span_id,
                    severity, severity_text, body, service, attributes
             FROM logs WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn duckdb::ToSql>> = Vec::new();

        if let Some(ref svc) = query.service {
            sql.push_str(" AND service = ?");
            param_values.push(Box::new(svc.clone()));
        }
        if let Some(ref sev) = query.severity {
            sql.push_str(" AND severity = ?");
            param_values.push(Box::new(sev.clone()));
        }
        if let Some(ref body) = query.body_contains {
            sql.push_str(" AND body LIKE ?");
            param_values.push(Box::new(format!("%{body}%")));
        }
        if let Some(ref tid) = query.trace_id {
            sql.push_str(" AND trace_id = ?");
            param_values.push(Box::new(tid.clone()));
        }
        if let Some(secs) = query.last_seconds {
            sql.push_str(&format!(
                " AND timestamp >= current_timestamp::TIMESTAMP - INTERVAL '{secs} seconds'"
            ));
        }

        sql.push_str(" ORDER BY timestamp DESC");

        let limit = query.limit.unwrap_or(100);
        sql.push_str(&format!(" LIMIT {limit}"));

        let params_ref: Vec<&dyn duckdb::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_ref.as_slice(), |row| {
            let ts_str: String = row.get(0)?;
            let obs_str: String = row.get(1)?;
            let attrs_str: String = row.get(8)?;
            let severity_str: String = row.get(4)?;

            Ok(LogRecord {
                timestamp: parse_timestamp(&ts_str),
                observed_timestamp: parse_timestamp(&obs_str),
                trace_id: row.get(2)?,
                span_id: row.get(3)?,
                severity: LogSeverity::from_str(&severity_str),
                severity_text: row.get(5)?,
                body: row.get(6)?,
                service: row.get(7)?,
                attributes: serde_json::from_str(&attrs_str).unwrap_or_default(),
            })
        })?;

        let mut logs = Vec::new();
        for row in rows {
            logs.push(row?);
        }
        Ok(logs)
    }

    // ── Metric storage ──────────────────────────────────────────────

    pub fn insert_metrics(&self, metrics: &[MetricPoint]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "INSERT INTO metrics
             (timestamp, service, name, metric_type, value, unit, attributes)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )?;

        for m in metrics {
            let attrs = serde_json::to_string(&m.attributes)?;
            stmt.execute(params![
                m.timestamp.format("%Y-%m-%d %H:%M:%S%.6f").to_string(),
                m.service,
                m.name,
                m.metric_type.to_string(),
                m.value,
                m.unit,
                attrs,
            ])?;
        }
        Ok(())
    }

    pub fn query_metrics(&self, query: &MetricQuery) -> Result<Vec<MetricPoint>> {
        let conn = self.conn.lock().unwrap();

        let mut sql = String::from(
            "SELECT timestamp::VARCHAR, service, name, metric_type, value, unit, attributes
             FROM metrics WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn duckdb::ToSql>> = Vec::new();

        if let Some(ref svc) = query.service {
            sql.push_str(" AND service = ?");
            param_values.push(Box::new(svc.clone()));
        }
        if let Some(ref name) = query.name {
            sql.push_str(" AND name = ?");
            param_values.push(Box::new(name.clone()));
        }
        if let Some(ref mt) = query.metric_type {
            sql.push_str(" AND metric_type = ?");
            param_values.push(Box::new(mt.clone()));
        }
        if let Some(secs) = query.last_seconds {
            sql.push_str(&format!(
                " AND timestamp >= current_timestamp::TIMESTAMP - INTERVAL '{secs} seconds'"
            ));
        }

        sql.push_str(" ORDER BY timestamp DESC");

        let limit = query.limit.unwrap_or(500);
        sql.push_str(&format!(" LIMIT {limit}"));

        let params_ref: Vec<&dyn duckdb::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_ref.as_slice(), |row| {
            let ts_str: String = row.get(0)?;
            let mt_str: String = row.get(3)?;
            let attrs_str: String = row.get(6)?;

            Ok(MetricPoint {
                timestamp: parse_timestamp(&ts_str),
                service: row.get(1)?,
                name: row.get(2)?,
                metric_type: MetricType::from_str(&mt_str),
                value: row.get(4)?,
                unit: row.get(5)?,
                attributes: serde_json::from_str(&attrs_str).unwrap_or_default(),
            })
        })?;

        let mut metrics = Vec::new();
        for row in rows {
            metrics.push(row?);
        }
        Ok(metrics)
    }

    pub fn query_summary(
        &self,
        last_seconds: i64,
        service: Option<&str>,
    ) -> Result<SummaryReport> {
        let conn = self.conn.lock().unwrap();

        let span_time_clause = format!(
            "start_time >= current_timestamp::TIMESTAMP - INTERVAL '{last_seconds} seconds'"
        );
        let ts_time_clause = format!(
            "timestamp >= current_timestamp::TIMESTAMP - INTERVAL '{last_seconds} seconds'"
        );

        let svc_clause = if service.is_some() { " AND service = ?" } else { "" };
        let svc_owned = service.map(|s| s.to_string());
        let build_params = || -> Vec<&dyn duckdb::ToSql> {
            match &svc_owned {
                Some(s) => vec![s as &dyn duckdb::ToSql],
                None => vec![],
            }
        };

        // ── Traces aggregate ─────────────────────────────────────────
        let traces_sql = format!(
            "SELECT
                COUNT(*)::BIGINT,
                COUNT(DISTINCT trace_id)::BIGINT,
                SUM(CASE WHEN status='error' THEN 1 ELSE 0 END)::BIGINT,
                COALESCE(AVG(duration_ms), 0.0),
                COALESCE(MAX(duration_ms), 0.0),
                COALESCE(quantile_cont(duration_ms, 0.5), 0.0),
                COALESCE(quantile_cont(duration_ms, 0.95), 0.0),
                COALESCE(quantile_cont(duration_ms, 0.99), 0.0)
             FROM spans WHERE {span_time_clause}{svc_clause}"
        );
        let mut stmt = conn.prepare(&traces_sql)?;
        let traces: TraceSummary = stmt.query_row(build_params().as_slice(), |row| {
            let span_count: i64 = row.get::<_, Option<i64>>(0)?.unwrap_or(0);
            let error_count: i64 = row.get::<_, Option<i64>>(2)?.unwrap_or(0);
            let error_rate = if span_count > 0 {
                error_count as f64 / span_count as f64
            } else {
                0.0
            };
            Ok(TraceSummary {
                span_count,
                trace_count: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                error_count,
                error_rate,
                avg_ms: row.get(3)?,
                max_ms: row.get(4)?,
                p50_ms: row.get(5)?,
                p95_ms: row.get(6)?,
                p99_ms: row.get(7)?,
            })
        })?;
        drop(stmt);

        // ── Top services by span count ───────────────────────────────
        let top_svc_sql = format!(
            "SELECT service,
                    COUNT(*)::BIGINT,
                    SUM(CASE WHEN status='error' THEN 1 ELSE 0 END)::DOUBLE / NULLIF(COUNT(*), 0)::DOUBLE,
                    COALESCE(quantile_cont(duration_ms, 0.95), 0.0)
             FROM spans WHERE {span_time_clause}{svc_clause}
             GROUP BY service
             ORDER BY 2 DESC
             LIMIT 5"
        );
        let mut stmt = conn.prepare(&top_svc_sql)?;
        let top_services: Vec<ServiceSummary> = stmt
            .query_map(build_params().as_slice(), |row| {
                Ok(ServiceSummary {
                    service: row.get(0)?,
                    span_count: row.get(1)?,
                    error_rate: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    p95_ms: row.get(3)?,
                })
            })?
            .collect::<duckdb::Result<_>>()?;
        drop(stmt);

        // ── Top error operations ─────────────────────────────────────
        let err_op_sql = format!(
            "SELECT service, operation, COUNT(*)::BIGINT
             FROM spans
             WHERE status='error' AND {span_time_clause}{svc_clause}
             GROUP BY service, operation
             ORDER BY 3 DESC
             LIMIT 5"
        );
        let mut stmt = conn.prepare(&err_op_sql)?;
        let top_error_operations: Vec<ErrorOperation> = stmt
            .query_map(build_params().as_slice(), |row| {
                Ok(ErrorOperation {
                    service: row.get(0)?,
                    operation: row.get(1)?,
                    error_count: row.get(2)?,
                })
            })?
            .collect::<duckdb::Result<_>>()?;
        drop(stmt);

        // ── Logs aggregate ───────────────────────────────────────────
        let logs_sql = format!(
            "SELECT
                COUNT(*)::BIGINT,
                SUM(CASE WHEN severity IN ('error','fatal') THEN 1 ELSE 0 END)::BIGINT,
                SUM(CASE WHEN severity='warn' THEN 1 ELSE 0 END)::BIGINT,
                SUM(CASE WHEN severity='info' THEN 1 ELSE 0 END)::BIGINT,
                SUM(CASE WHEN severity IN ('debug','trace') THEN 1 ELSE 0 END)::BIGINT
             FROM logs WHERE {ts_time_clause}{svc_clause}"
        );
        let mut stmt = conn.prepare(&logs_sql)?;
        let logs: LogSummary = stmt.query_row(build_params().as_slice(), |row| {
            Ok(LogSummary {
                total: row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                error: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                warn: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                info: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                debug: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
            })
        })?;
        drop(stmt);

        // ── Metrics aggregate ────────────────────────────────────────
        let metrics_sql = format!(
            "SELECT COUNT(*)::BIGINT, COUNT(DISTINCT name)::BIGINT
             FROM metrics WHERE {ts_time_clause}{svc_clause}"
        );
        let mut stmt = conn.prepare(&metrics_sql)?;
        let metrics: MetricSummary = stmt.query_row(build_params().as_slice(), |row| {
            Ok(MetricSummary {
                point_count: row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                unique_names: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            })
        })?;
        drop(stmt);

        Ok(SummaryReport {
            window_seconds: last_seconds,
            service_filter: service.map(|s| s.to_string()),
            traces,
            top_services,
            top_error_operations,
            logs,
            metrics,
        })
    }

    pub fn query_anomalies(
        &self,
        current_seconds: i64,
        baseline_seconds: i64,
        service: Option<&str>,
    ) -> Result<AnomalyReport> {
        let conn = self.conn.lock().unwrap();
        let svc_owned = service.map(|s| s.to_string());
        let svc_clause = if service.is_some() { " AND service = ?" } else { "" };

        // Per-service stats for a window whose end = now and start = now - window.
        let stats_sql = |window: i64| -> String {
            format!(
                "SELECT service,
                        COUNT(*)::BIGINT AS span_count,
                        SUM(CASE WHEN status='error' THEN 1 ELSE 0 END)::DOUBLE
                            / NULLIF(COUNT(*), 0)::DOUBLE AS error_rate,
                        COALESCE(quantile_cont(duration_ms, 0.95), 0.0) AS p95_ms
                 FROM spans
                 WHERE start_time >= current_timestamp::TIMESTAMP - INTERVAL '{window} seconds'{svc_clause}
                 GROUP BY service"
            )
        };

        let build_params = || -> Vec<&dyn duckdb::ToSql> {
            match &svc_owned {
                Some(s) => vec![s as &dyn duckdb::ToSql],
                None => vec![],
            }
        };

        let mut current_stats: std::collections::HashMap<String, (i64, f64, f64)> =
            std::collections::HashMap::new();
        let mut stmt = conn.prepare(&stats_sql(current_seconds))?;
        let rows = stmt.query_map(build_params().as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                row.get::<_, f64>(3)?,
            ))
        })?;
        for r in rows {
            let (svc, span_count, error_rate, p95) = r?;
            current_stats.insert(svc, (span_count, error_rate, p95));
        }
        drop(stmt);

        let mut baseline_stats: std::collections::HashMap<String, (i64, f64, f64)> =
            std::collections::HashMap::new();
        let mut stmt = conn.prepare(&stats_sql(baseline_seconds))?;
        let rows = stmt.query_map(build_params().as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                row.get::<_, f64>(3)?,
            ))
        })?;
        for r in rows {
            let (svc, span_count, error_rate, p95) = r?;
            baseline_stats.insert(svc, (span_count, error_rate, p95));
        }
        drop(stmt);

        let mut anomalies = Vec::new();
        for (svc, (cur_count, cur_err, cur_p95)) in &current_stats {
            if *cur_count < 5 {
                continue;
            }
            let (base_count, base_err, base_p95) =
                baseline_stats.get(svc).copied().unwrap_or((0, 0.0, 0.0));

            let err_delta = cur_err - base_err;
            if err_delta >= 0.05 && *cur_err > 0.0 {
                let severity = if err_delta >= 0.25 {
                    "critical"
                } else if err_delta >= 0.10 {
                    "warning"
                } else {
                    "info"
                };
                anomalies.push(Anomaly {
                    service: svc.clone(),
                    kind: "error_rate".into(),
                    severity: severity.into(),
                    current: *cur_err,
                    baseline: base_err,
                    delta: err_delta,
                    description: format!(
                        "Error rate rose {:.1}% → {:.1}% (baseline {:.1}%, {} spans)",
                        base_err * 100.0,
                        cur_err * 100.0,
                        base_err * 100.0,
                        cur_count
                    ),
                });
            }

            if base_p95 > 0.0 && base_count >= 5 {
                let ratio = cur_p95 / base_p95;
                if ratio >= 1.5 && *cur_p95 > 50.0 {
                    let severity = if ratio >= 3.0 {
                        "critical"
                    } else if ratio >= 2.0 {
                        "warning"
                    } else {
                        "info"
                    };
                    anomalies.push(Anomaly {
                        service: svc.clone(),
                        kind: "latency_p95".into(),
                        severity: severity.into(),
                        current: *cur_p95,
                        baseline: base_p95,
                        delta: cur_p95 - base_p95,
                        description: format!(
                            "p95 latency {:.1}ms → {:.1}ms ({:.1}× baseline)",
                            base_p95, cur_p95, ratio
                        ),
                    });
                }
            }
        }

        anomalies.sort_by(|a, b| {
            let rank = |s: &str| match s {
                "critical" => 0,
                "warning" => 1,
                _ => 2,
            };
            rank(&a.severity).cmp(&rank(&b.severity))
        });

        Ok(AnomalyReport {
            current_seconds,
            baseline_seconds,
            service_filter: svc_owned,
            anomalies,
        })
    }

    pub fn query_correlate(&self, trace_id: &str) -> Result<Option<CorrelateReport>> {
        let spans = self.get_trace(trace_id)?;
        if spans.is_empty() {
            return Ok(None);
        }

        let start_time = spans
            .iter()
            .map(|s| s.start_time)
            .min()
            .unwrap_or_else(Utc::now);
        let end_time = spans
            .iter()
            .map(|s| s.end_time)
            .max()
            .unwrap_or_else(Utc::now);
        let duration_ms = (end_time - start_time).num_milliseconds() as f64;
        let error_count = spans
            .iter()
            .filter(|s| matches!(s.status, SpanStatus::Error))
            .count() as i64;

        let mut services: Vec<String> =
            spans.iter().map(|s| s.service.clone()).collect();
        services.sort();
        services.dedup();

        let logs = self.query_logs(&LogQuery {
            trace_id: Some(trace_id.to_string()),
            limit: Some(500),
            ..Default::default()
        })?;

        // Metrics overlapping the trace's time range, scoped to touched services.
        let metrics = {
            let conn = self.conn.lock().unwrap();
            let start = start_time
                .naive_utc()
                .format("%Y-%m-%d %H:%M:%S%.6f")
                .to_string();
            let end = end_time
                .naive_utc()
                .format("%Y-%m-%d %H:%M:%S%.6f")
                .to_string();

            let placeholders = std::iter::repeat("?")
                .take(services.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT timestamp::VARCHAR, service, name, metric_type, value, unit, attributes
                 FROM metrics
                 WHERE timestamp BETWEEN ?::TIMESTAMP AND ?::TIMESTAMP
                   AND service IN ({placeholders})
                 ORDER BY timestamp ASC
                 LIMIT 500"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut params_vec: Vec<&dyn duckdb::ToSql> =
                vec![&start as &dyn duckdb::ToSql, &end as &dyn duckdb::ToSql];
            for s in &services {
                params_vec.push(s as &dyn duckdb::ToSql);
            }
            let rows = stmt.query_map(params_vec.as_slice(), |row| {
                let ts_str: String = row.get(0)?;
                let mt_str: String = row.get(3)?;
                let attrs_str: String = row.get(6)?;
                Ok(MetricPoint {
                    timestamp: parse_timestamp(&ts_str),
                    service: row.get(1)?,
                    name: row.get(2)?,
                    metric_type: MetricType::from_str(&mt_str),
                    value: row.get(4)?,
                    unit: row.get(5)?,
                    attributes: serde_json::from_str(&attrs_str).unwrap_or_default(),
                })
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        };

        Ok(Some(CorrelateReport {
            trace_id: trace_id.to_string(),
            span_count: spans.len(),
            services,
            start_time: start_time.to_rfc3339(),
            end_time: end_time.to_rfc3339(),
            duration_ms,
            error_count,
            logs,
            metrics,
        }))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub span_count: i64,
    pub trace_count: i64,
    pub avg_duration_ms: f64,
    pub error_rate: f64,
}
