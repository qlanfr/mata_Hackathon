//! Per-request JSON logs written by the service under test. These are the observation evidence:
//! request id, URI, status, latency, SQL statement count and DB time.

use std::path::Path;

use aiops_core::report::ServiceLogSummary;
use serde_json::Value;

/// Summary over every request line, plus the `top` slowest records verbatim.
pub fn summarize(path: &Path, top: usize) -> (ServiceLogSummary, Vec<Value>) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let records: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v.get("sql_count").is_some())
        .collect();
    if records.is_empty() {
        return (ServiceLogSummary::default(), Vec::new());
    }
    let n = records.len() as f64;
    let field = |v: &Value, key: &str| v[key].as_f64().unwrap_or(0.0);
    let summary = ServiceLogSummary {
        requests: records.len() as u64,
        avg_sql_count: records.iter().map(|r| field(r, "sql_count")).sum::<f64>() / n,
        max_sql_count: records
            .iter()
            .map(|r| field(r, "sql_count") as u64)
            .max()
            .unwrap_or(0),
        avg_db_ms: records.iter().map(|r| field(r, "db_ms")).sum::<f64>() / n,
        avg_latency_ms: records.iter().map(|r| field(r, "latency_ms")).sum::<f64>() / n,
    };
    let mut slowest = records;
    slowest.sort_by(|a, b| field(b, "latency_ms").total_cmp(&field(a, "latency_ms")));
    slowest.truncate(top);
    (summary, slowest)
}
