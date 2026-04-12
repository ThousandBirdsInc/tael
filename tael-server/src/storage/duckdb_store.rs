use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, Utc};
use duckdb::{params, Connection};

use super::models::{
    LogQuery, LogRecord, LogSeverity, MetricPoint, MetricQuery, MetricType, Span, SpanStatus,
    TraceComment, TraceQuery,
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
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub span_count: i64,
    pub trace_count: i64,
    pub avg_duration_ms: f64,
    pub error_rate: f64,
}
