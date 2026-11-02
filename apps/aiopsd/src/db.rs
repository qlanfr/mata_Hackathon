//! Control database access. Production uses Cloudflare D1 through the Worker's outbound handler
//! (`http://d1.internal`); local development and tests use SQLite. Both speak the same SQL.
//!
//! D1 has no interactive transactions, so callers never hold one open. State changes use a
//! compare-and-swap `UPDATE ... WHERE state_version = ?` (check the returned change count), and
//! statements that must land together go through [`Db::batch`], which is atomic on both backends.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use serde::Serialize;
use serde_json::{Map, Value};

pub type Row = Map<String, Value>;

#[derive(Debug, Clone, Serialize)]
pub struct Stmt {
    pub sql: String,
    pub params: Vec<Value>,
}

pub fn stmt(sql: impl Into<String>, params: Vec<Value>) -> Stmt {
    Stmt {
        sql: sql.into(),
        params,
    }
}

#[derive(Clone)]
pub enum Db {
    D1 { http: reqwest::Client, base: String },
    Sqlite(Arc<Mutex<rusqlite::Connection>>),
}

impl Db {
    /// `d1:http://d1.internal`, `sqlite:path/to/file.db` or `sqlite::memory:`.
    pub async fn connect(url: &str, migrations_dir: &Path) -> anyhow::Result<Self> {
        if let Some(base) = url.strip_prefix("d1:") {
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?;
            return Ok(Db::D1 {
                http,
                base: base.trim_end_matches('/').to_owned(),
            });
        }
        let path = url
            .strip_prefix("sqlite:")
            .context("database URL must start with d1: or sqlite:")?;
        let conn = if path == ":memory:" {
            rusqlite::Connection::open_in_memory()?
        } else {
            if let Some(parent) = Path::new(path).parent() {
                std::fs::create_dir_all(parent)?;
            }
            rusqlite::Connection::open(path)?
        };
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON;",
        )?;
        apply_migrations(&conn, migrations_dir)?;
        Ok(Db::Sqlite(Arc::new(Mutex::new(conn))))
    }

    pub fn backend(&self) -> &'static str {
        match self {
            Db::D1 { .. } => "d1",
            Db::Sqlite(_) => "sqlite",
        }
    }

    pub async fn query(&self, sql: &str, params: Vec<Value>) -> anyhow::Result<Vec<Row>> {
        Ok(self.run(stmt(sql, params)).await?.0)
    }

    pub async fn query_one(&self, sql: &str, params: Vec<Value>) -> anyhow::Result<Option<Row>> {
        Ok(self.query(sql, params).await?.into_iter().next())
    }

    /// Runs a write and returns the number of changed rows.
    pub async fn execute(&self, sql: &str, params: Vec<Value>) -> anyhow::Result<u64> {
        Ok(self.run(stmt(sql, params)).await?.1)
    }

    async fn run(&self, s: Stmt) -> anyhow::Result<(Vec<Row>, u64)> {
        match self {
            Db::D1 { http, base } => {
                let resp = http.post(format!("{base}/query")).json(&s).send().await?;
                let body = d1_body(resp).await?;
                Ok(d1_result(&body))
            }
            Db::Sqlite(conn) => {
                let conn = conn.clone();
                tokio::task::spawn_blocking(move || {
                    let conn = conn.lock().expect("sqlite lock");
                    sqlite_run(&conn, &s)
                })
                .await?
            }
        }
    }

    /// Runs every statement in one transaction and returns each change count.
    pub async fn batch(&self, stmts: Vec<Stmt>) -> anyhow::Result<Vec<u64>> {
        match self {
            Db::D1 { http, base } => {
                let resp = http
                    .post(format!("{base}/batch"))
                    .json(&serde_json::json!({ "statements": stmts }))
                    .send()
                    .await?;
                let body = d1_body(resp).await?;
                let items = body.as_array().context("D1 batch did not return a list")?;
                Ok(items.iter().map(|r| d1_result(r).1).collect())
            }
            Db::Sqlite(conn) => {
                let conn = conn.clone();
                tokio::task::spawn_blocking(move || {
                    let mut conn = conn.lock().expect("sqlite lock");
                    let tx = conn.transaction()?;
                    let mut changes = Vec::with_capacity(stmts.len());
                    for s in &stmts {
                        changes.push(sqlite_run(&tx, s)?.1);
                    }
                    tx.commit()?;
                    Ok(changes)
                })
                .await?
            }
        }
    }
}

async fn d1_body(resp: reqwest::Response) -> anyhow::Result<Value> {
    let status = resp.status();
    let body: Value = resp.json().await.context("D1 proxy returned non-JSON")?;
    if !status.is_success() {
        anyhow::bail!(
            "D1 error ({status}): {}",
            body["error"].as_str().unwrap_or("unknown")
        );
    }
    Ok(body)
}

fn d1_result(body: &Value) -> (Vec<Row>, u64) {
    let rows = body["results"]
        .as_array()
        .map(|rows| rows.iter().filter_map(|r| r.as_object().cloned()).collect())
        .unwrap_or_default();
    let changes = body["meta"]["changes"].as_f64().unwrap_or(0.0) as u64;
    (rows, changes)
}

fn to_sql(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as S;
    match v {
        Value::Null => S::Null,
        Value::Bool(b) => S::Integer(*b as i64),
        Value::Number(n) => n
            .as_i64()
            .map(S::Integer)
            .unwrap_or_else(|| S::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => S::Text(s.clone()),
        other => S::Text(other.to_string()),
    }
}

fn sqlite_run(conn: &rusqlite::Connection, s: &Stmt) -> anyhow::Result<(Vec<Row>, u64)> {
    let mut prepared = conn
        .prepare(&s.sql)
        .with_context(|| format!("prepare: {}", s.sql))?;
    let params: Vec<rusqlite::types::Value> = s.params.iter().map(to_sql).collect();
    let names: Vec<String> = prepared
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    if names.is_empty() {
        let changes = prepared.execute(rusqlite::params_from_iter(params.iter()))?;
        return Ok((Vec::new(), changes as u64));
    }
    let mut rows = prepared.query(rusqlite::params_from_iter(params.iter()))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut map = Map::new();
        for (i, name) in names.iter().enumerate() {
            use rusqlite::types::ValueRef;
            let v = match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => Value::from(n),
                ValueRef::Real(f) => Value::from(f),
                ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Blob(_) => Value::Null,
            };
            map.insert(name.clone(), v);
        }
        out.push(map);
    }
    drop(rows);
    Ok((out, conn.changes()))
}

/// Applies `NNNN_*.sql` files in order, once each. Production applies the same files with
/// `wrangler d1 migrations apply`.
fn apply_migrations(conn: &rusqlite::Connection, dir: &Path) -> anyhow::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS _aiops_migrations (name TEXT PRIMARY KEY, applied_at INTEGER NOT NULL)")?;
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("migrations directory {} not found", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    files.sort();
    for file in files {
        let name = file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let done: bool = conn.query_row(
            "SELECT COUNT(*) FROM _aiops_migrations WHERE name = ?1",
            [&name],
            |r| r.get::<_, i64>(0),
        )? > 0;
        if done {
            continue;
        }
        let sql = std::fs::read_to_string(&file)?;
        conn.execute_batch(&format!("BEGIN; {sql}; COMMIT;"))
            .with_context(|| format!("migration {name}"))?;
        conn.execute(
            "INSERT INTO _aiops_migrations (name, applied_at) VALUES (?1, ?2)",
            rusqlite::params![name, crate::now_ms() as i64],
        )?;
    }
    Ok(())
}

/// Typed accessors for rows that came back as JSON maps.
pub trait RowExt {
    fn str(&self, key: &str) -> String;
    fn opt_str(&self, key: &str) -> Option<String>;
    fn i64(&self, key: &str) -> i64;
    fn f64_opt(&self, key: &str) -> Option<f64>;
    /// A TEXT column holding JSON.
    fn json(&self, key: &str) -> Value;
}

impl RowExt for Row {
    fn str(&self, key: &str) -> String {
        self.opt_str(key).unwrap_or_default()
    }

    fn opt_str(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Null) | None => None,
            Some(other) => Some(other.to_string()),
        }
    }

    fn i64(&self, key: &str) -> i64 {
        self.get(key)
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
            .unwrap_or(0)
    }

    fn f64_opt(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64)
    }

    fn json(&self, key: &str) -> Value {
        self.opt_str(key)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn db() -> Db {
        let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        Db::connect("sqlite::memory:", &migrations).await.unwrap()
    }

    #[tokio::test]
    async fn cas_update_succeeds_only_once() {
        let db = db().await;
        db.execute(
            "INSERT INTO runs (id, scenario_id, scenario_hash, goal, trigger, state, provider, budget, created_at, updated_at) \
             VALUES ('r1', 's', 'h', 'g', '{}', 'QUEUED', 'replay', '{}', 0, 0)",
            vec![],
        )
        .await
        .unwrap();
        let cas = "UPDATE runs SET state = ?1, state_version = state_version + 1 WHERE id = 'r1' AND state_version = ?2";
        assert_eq!(
            db.execute(cas, vec![json!("COLLECTING"), json!(0)])
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            db.execute(cas, vec![json!("PLANNING"), json!(0)])
                .await
                .unwrap(),
            0,
            "stale version must not win"
        );
        let row = db
            .query_one(
                "SELECT state, state_version FROM runs WHERE id = 'r1'",
                vec![],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.str("state"), row.i64("state_version")),
            ("COLLECTING".into(), 1)
        );
    }

    #[tokio::test]
    async fn batch_rolls_back_as_a_whole() {
        let db = db().await;
        let insert = |seq: i64| {
            stmt("INSERT INTO run_events (run_id, seq, ts, kind, payload) VALUES ('r', ?1, 0, 'k', '{}')", vec![json!(seq)])
        };
        db.batch(vec![insert(1), insert(2)]).await.unwrap();
        // Duplicate primary key in the second statement: the first must not stick either.
        assert!(db.batch(vec![insert(3), insert(2)]).await.is_err());
        let rows = db
            .query("SELECT seq FROM run_events ORDER BY seq", vec![])
            .await
            .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.i64("seq")).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[tokio::test]
    async fn active_fingerprint_is_unique_until_the_run_ends() {
        let db = db().await;
        let insert = |id: &str| {
            format!(
                "INSERT INTO runs (id, fingerprint, scenario_id, scenario_hash, goal, trigger, state, provider, budget, created_at, updated_at) \
                 VALUES ('{id}', 'fp', 's', 'h', 'g', '{{}}', 'QUEUED', 'replay', '{{}}', 0, 0)"
            )
        };
        db.execute(&insert("a"), vec![]).await.unwrap();
        assert!(
            db.execute(&insert("b"), vec![]).await.is_err(),
            "second active run with the same fingerprint"
        );
        db.execute("UPDATE runs SET state = 'COMPLETED' WHERE id = 'a'", vec![])
            .await
            .unwrap();
        db.execute(&insert("c"), vec![]).await.unwrap();
    }
}
