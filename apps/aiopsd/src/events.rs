//! The run timeline. Every state change, model call and tool call becomes an ordered event; the
//! UI streams them over SSE and the report links back to them.

use serde_json::{json, Value};

use crate::db::{stmt, Db, Stmt};

/// Statement that appends an event with the next sequence number for the run.
pub fn append_stmt(run_id: &str, kind: &str, payload: &Value) -> Stmt {
    stmt(
        "INSERT INTO run_events (run_id, seq, ts, kind, payload) \
         VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM run_events WHERE run_id = ?1), ?2, ?3, ?4)",
        vec![json!(run_id), json!(crate::now_ms()), json!(kind), json!(payload.to_string())],
    )
}

/// Appends one event. Retries a few times if a concurrent writer took the same sequence number.
pub async fn append(db: &Db, run_id: &str, kind: &str, payload: Value) -> anyhow::Result<()> {
    let mut last = None;
    for _ in 0..3 {
        let s = append_stmt(run_id, kind, &payload);
        match db.execute(&s.sql, s.params).await {
            Ok(_) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("at least one attempt"))
}
