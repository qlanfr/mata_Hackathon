//! SQL statement counts from `pg_stat_statements`, for services that do not log their own
//! per-request SQL counts. Load runs are sequential, so resetting the statistics before a run
//! and reading them after it attributes every statement in between to that run's service.

use std::collections::BTreeMap;

use aiops_core::report::ServiceLogSummary;
use serde_json::Value;

use crate::pg::Postgres;

#[derive(Debug, Clone, Copy, Default)]
pub struct Totals {
    pub calls: f64,
    pub exec_ms: f64,
    pub requests: u64,
}

pub struct SqlMeter {
    pg: Postgres,
    database: String,
    pub per_label: BTreeMap<String, Totals>,
    /// Most frequent statements of the first measured run of the label passed to `end`.
    pub top: Vec<Value>,
}

impl SqlMeter {
    pub fn new(pg: Postgres, database: &str) -> Self {
        Self {
            pg,
            database: database.to_owned(),
            per_label: BTreeMap::new(),
            top: Vec::new(),
        }
    }

    pub async fn begin(&self) -> anyhow::Result<()> {
        self.pg
            .query("postgres", "SELECT pg_stat_statements_reset()")
            .await?;
        Ok(())
    }

    /// Adds the statements since `begin` to `label`, which served `requests` requests. With
    /// `keep_top`, also keeps the run's most frequent statements if none are kept yet.
    pub async fn end(&mut self, label: &str, requests: u64, keep_top: bool) -> anyhow::Result<()> {
        let filter = format!(
            "FROM pg_stat_statements s JOIN pg_database d ON d.oid = s.dbid WHERE d.datname = '{}' \
             AND s.query NOT ILIKE '%pg_stat_statements%'",
            self.database.replace('\'', "")
        );
        let totals: Value = serde_json::from_str(
            &self
                .pg
                .query(
                    "postgres",
                    &format!("SELECT json_build_object('calls', COALESCE(SUM(s.calls), 0), 'ms', COALESCE(SUM(s.total_exec_time), 0)) {filter}"),
                )
                .await?,
        )?;
        let entry = self.per_label.entry(label.to_owned()).or_default();
        entry.calls += totals["calls"].as_f64().unwrap_or(0.0);
        entry.exec_ms += totals["ms"].as_f64().unwrap_or(0.0);
        entry.requests += requests;

        if keep_top && self.top.is_empty() && requests > 0 {
            let rows = self
                .pg
                .query(
                    "postgres",
                    &format!(
                        "SELECT COALESCE(json_agg(t), '[]') FROM (SELECT s.calls, s.mean_exec_time AS mean_ms, \
                         left(regexp_replace(s.query, '\\s+', ' ', 'g'), 400) AS query {filter} \
                         ORDER BY s.calls DESC LIMIT 10) t"
                    ),
                )
                .await?;
            let rows: Vec<Value> = serde_json::from_str(&rows)?;
            self.top = rows
                .into_iter()
                .enumerate()
                .map(|(i, r)| {
                    let calls = r["calls"].as_f64().unwrap_or(0.0);
                    serde_json::json!({
                        "id": format!("sql:{}", i + 1),
                        "calls": calls,
                        "calls_per_request": calls / requests as f64,
                        "mean_ms": r["mean_ms"],
                        "query": r["query"],
                    })
                })
                .collect();
        }
        Ok(())
    }

    /// The label's totals in the shape of the service-log summary: SQL statements and statement
    /// time per request. Per-request maxima are not known here.
    pub fn summary(&self, label: &str) -> Option<ServiceLogSummary> {
        let t = self.per_label.get(label)?;
        if t.requests == 0 {
            return None;
        }
        let n = t.requests as f64;
        Some(ServiceLogSummary {
            requests: t.requests,
            avg_sql_count: t.calls / n,
            max_sql_count: 0,
            avg_db_ms: t.exec_ms / n,
            avg_latency_ms: 0.0,
        })
    }
}
