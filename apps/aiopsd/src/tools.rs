//! Typed, read-only tools the model can call. Each tool checks its own arguments and the role's
//! permission, bounds its output size, and returns JSON. Tool output is evidence to analyse,
//! never instructions. Changing code is not a tool: the experimenter proposes a patch in its
//! final answer and the orchestrator validates and submits it.

use std::path::{Path, PathBuf};

use aiops_core::patch::PathPolicy;
use aiops_core::report::BaselineReport;
use aiops_core::scenario::ScenarioSpec;
use serde_json::{json, Value};

use crate::db::{Db, RowExt};
use crate::llm::{Role, ToolSpec};

const MAX_SOURCE_CHARS: usize = 24_000;

pub struct ToolContext<'a> {
    pub db: &'a Db,
    pub run_id: &'a str,
    pub spec: &'a ScenarioSpec,
    pub repo_root: &'a Path,
    pub baseline: &'a BaselineReport,
    pub aa_noise: Option<f64>,
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

pub fn allowed(role: Role, name: &str) -> bool {
    match name {
        "query_metrics"
        | "read_request_evidence"
        | "list_source"
        | "read_source"
        | "search_experiments" => true,
        "read_trial_result" => role == Role::Experimenter,
        _ => false,
    }
}

pub fn specs_for(role: Role) -> Vec<ToolSpec> {
    let all = vec![
        ToolSpec {
            name: "query_metrics",
            description: "Baseline measurements for this run: fixed load plan, p50/p95/p99 latency of A/A pairs, error and drop counts, A/A noise, and per-request SQL statistics from the service logs.",
            parameters: object(json!({}), &[]),
        },
        ToolSpec {
            name: "read_request_evidence",
            description: "The slowest request records from the baseline run, each with request id, URI, status, latency, SQL statement count and DB time. For services without a per-request log: the most frequent SQL statements of a measured baseline run, with calls per request.",
            parameters: object(json!({ "limit": { "type": "integer", "minimum": 1, "maximum": 20 } }), &["limit"]),
        },
        ToolSpec {
            name: "list_source",
            description: "Files of the service under test, with sizes and whether a patch may edit them.",
            parameters: object(json!({}), &[]),
        },
        ToolSpec {
            name: "read_source",
            description: "Text of one source file of the service under test (path relative to the service root, e.g. src/repo.rs). Long files are cut; pass `start_line` (1-based) to read on from `next_line`.",
            parameters: object(json!({ "path": { "type": "string" }, "start_line": { "type": "integer", "minimum": 1 } }), &["path"]),
        },
        ToolSpec {
            name: "search_experiments",
            description: "Earlier experiments on this scenario from any run: hypothesis, outcome and measured improvement. Failures are included.",
            parameters: object(json!({ "limit": { "type": "integer", "minimum": 1, "maximum": 20 } }), &["limit"]),
        },
        ToolSpec {
            name: "read_trial_result",
            description: "Results of the experiments already tried in this run: outcome, the evaluator's reasons, per-pair p95 improvements and failed golden cases.",
            parameters: object(json!({}), &[]),
        },
    ];
    all.into_iter().filter(|t| allowed(role, t.name)).collect()
}

/// Runs a tool and returns its JSON result. Errors are returned as `{ "error": ... }` so the
/// model can correct itself; they never abort the run.
pub async fn execute(ctx: &ToolContext<'_>, role: Role, name: &str, arguments: &str) -> Value {
    if !allowed(role, name) {
        return json!({ "error": format!("tool `{name}` is not available to the {} role", role.as_str()) });
    }
    let args: Value = match serde_json::from_str(if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    }) {
        Ok(v) => v,
        Err(e) => return json!({ "error": format!("arguments are not valid JSON: {e}") }),
    };
    let result = match name {
        "query_metrics" => Ok(query_metrics(ctx)),
        "read_request_evidence" => Ok(read_request_evidence(ctx, limit(&args))),
        "list_source" => list_source(ctx),
        "read_source" => read_source(
            ctx,
            args["path"].as_str().unwrap_or_default(),
            args["start_line"].as_u64().map(|n| n as usize),
        ),
        "search_experiments" => search_experiments(ctx, limit(&args)).await,
        "read_trial_result" => read_trial_result(ctx).await,
        _ => Err(anyhow::anyhow!("unknown tool `{name}`")),
    };
    result.unwrap_or_else(|e| json!({ "error": e.to_string() }))
}

fn limit(args: &Value) -> usize {
    args["limit"].as_u64().unwrap_or(5).clamp(1, 20) as usize
}

fn query_metrics(ctx: &ToolContext<'_>) -> Value {
    let b = ctx.baseline;
    let pairs: Vec<Value> = b
        .aa_pairs
        .iter()
        .map(|p| {
            json!({
                "pair": p.index,
                "a": { "p50_ms": p.baseline.p50_ms, "p95_ms": p.baseline.p95_ms, "p99_ms": p.baseline.p99_ms, "ok": p.baseline.ok, "errors": p.baseline.errors(), "dropped": p.baseline.dropped },
                "b": { "p50_ms": p.candidate.p50_ms, "p95_ms": p.candidate.p95_ms, "p99_ms": p.candidate.p99_ms, "ok": p.candidate.ok, "errors": p.candidate.errors(), "dropped": p.candidate.dropped },
            })
        })
        .collect();
    let p95s: Vec<f64> = b
        .aa_pairs
        .iter()
        .flat_map(|p| [p.baseline.p95_ms, p.candidate.p95_ms])
        .collect();
    json!({
        "load_plan": { "rate_rps": ctx.spec.load.rate, "duration_s": ctx.spec.load.duration_s, "users": ctx.spec.load.users, "endpoint": ctx.spec.load.endpoint.as_deref().unwrap_or("GET /users/{user_id}/orders?limit=20") },
        "sql_source": if ctx.spec.database.statement_stats && b.slow_requests.is_empty() { "pg_stat_statements during the measured baseline runs (no per-request service log)" } else { "the service's per-request log" },
        "metrics": [
            { "id": "metric:p95_ms", "value": aiops_core::evaluator::median(&p95s), "note": "median p95 of successful responses across A/A runs" },
            { "id": "metric:avg_sql_count", "value": b.logs.avg_sql_count },
            { "id": "metric:max_sql_count", "value": b.logs.max_sql_count },
            { "id": "metric:avg_db_ms", "value": b.logs.avg_db_ms },
            { "id": "metric:avg_latency_ms", "value": b.logs.avg_latency_ms },
            { "id": "metric:aa_noise", "value": ctx.aa_noise },
        ],
        "aa_pairs": pairs,
        "requests_logged": b.logs.requests,
    })
}

fn read_request_evidence(ctx: &ToolContext<'_>, limit: usize) -> Value {
    // Services without a per-request log: the most frequent SQL statements of one measured
    // baseline run instead, with how often each ran per request.
    if ctx.baseline.slow_requests.is_empty() && !ctx.baseline.statements.is_empty() {
        let statements: Vec<Value> = ctx
            .baseline
            .statements
            .iter()
            .take(limit)
            .cloned()
            .collect();
        return json!({
            "statements": statements,
            "note": "No per-request service log for this scenario. These are the most frequent SQL statements during one measured baseline run (pg_stat_statements), with calls per request.",
        });
    }
    let requests: Vec<Value> = ctx
        .baseline
        .slow_requests
        .iter()
        .take(limit)
        .map(|r| {
            json!({
                "id": format!("req:{}", r["request_id"]),
                "uri": r["uri"], "status": r["status"], "latency_ms": r["latency_ms"],
                "sql_count": r["sql_count"], "db_ms": r["db_ms"],
            })
        })
        .collect();
    json!({ "requests": requests })
}

fn source_root(ctx: &ToolContext<'_>) -> PathBuf {
    ctx.repo_root.join(&ctx.spec.target.source_dir)
}

fn policy(ctx: &ToolContext<'_>) -> PathPolicy {
    PathPolicy::new(
        ctx.spec.target.allowed_paths.clone(),
        ctx.spec.target.denied_paths.clone(),
    )
}

fn list_source(ctx: &ToolContext<'_>) -> anyhow::Result<Value> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == "target" || name.to_string_lossy().starts_with('.') {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(root, &entry.path(), out)?;
            } else if kind.is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, entry.metadata()?.len()));
            }
        }
        Ok(())
    }
    let root = source_root(ctx);
    let mut files = Vec::new();
    walk(&root, &root, &mut files)?;
    files.sort();
    let policy = policy(ctx);
    let files: Vec<Value> = files
        .into_iter()
        .map(|(path, bytes)| json!({ "path": path, "bytes": bytes, "editable": policy.check(&path).is_ok() }))
        .collect();
    Ok(json!({ "files": files }))
}

fn read_source(
    ctx: &ToolContext<'_>,
    path: &str,
    start_line: Option<usize>,
) -> anyhow::Result<Value> {
    anyhow::ensure!(
        !path.is_empty()
            && !path.starts_with('/')
            && path
                .split('/')
                .all(|s| !s.is_empty() && !s.starts_with('.')),
        "`{path}` is not a plain relative path inside the service"
    );
    let full = source_root(ctx).join(path);
    let meta =
        std::fs::symlink_metadata(&full).map_err(|_| anyhow::anyhow!("`{path}` does not exist"))?;
    anyhow::ensure!(meta.is_file(), "`{path}` is not a regular file");
    let text = std::fs::read_to_string(&full)?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = start_line.unwrap_or(1).max(1);
    anyhow::ensure!(
        start <= lines.len().max(1),
        "`{path}` has {} lines",
        lines.len()
    );
    // Whole lines up to the size limit, so a file can be read on from `next_line`.
    let mut content = String::new();
    let mut end = start - 1;
    for line in &lines[start - 1..] {
        if !content.is_empty() && content.len() + line.len() > MAX_SOURCE_CHARS {
            break;
        }
        content.push_str(line);
        end += 1;
    }
    let truncated = end < lines.len();
    Ok(json!({
        "id": format!("src:{path}"),
        "path": path,
        "editable": policy(ctx).check(path).is_ok(),
        "lines": format!("{start}-{end} of {}", lines.len()),
        "truncated": truncated,
        "next_line": if truncated { json!(end + 1) } else { Value::Null },
        "content": content,
    }))
}

async fn search_experiments(ctx: &ToolContext<'_>, limit: usize) -> anyhow::Result<Value> {
    let rows = ctx
        .db
        .query(
            "SELECT run_id, hypothesis, outcome, median_improvement, patch_hash, created_at FROM experiment_cases \
             WHERE scenario_id = ?1 ORDER BY created_at DESC LIMIT ?2",
            vec![json!(ctx.spec.id), json!(limit)],
        )
        .await?;
    let cases: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "run_id": r.str("run_id"), "hypothesis": r.str("hypothesis"), "outcome": r.str("outcome"),
                "median_p95_improvement": r.f64_opt("median_improvement"), "patch_hash": r.str("patch_hash"),
            })
        })
        .collect();
    Ok(
        json!({ "cases": cases, "note": "Past results are leads to re-verify, not facts about this environment." }),
    )
}

async fn read_trial_result(ctx: &ToolContext<'_>) -> anyhow::Result<Value> {
    let rows = ctx
        .db
        .query(
            "SELECT seq, hypothesis, patch, patch_hash, outcome, evaluation FROM trials WHERE run_id = ?1 ORDER BY seq",
            vec![json!(ctx.run_id)],
        )
        .await?;
    let trials: Vec<Value> = rows
        .iter()
        .map(|r| {
            let eval = r.json("evaluation");
            json!({
                "trial": r.i64("seq"),
                "hypothesis": r.str("hypothesis"),
                "patch_summary": r.json("patch")["summary"],
                "patch_hash": r.str("patch_hash"),
                "outcome": r.opt_str("outcome"),
                "reasons": eval["reasons"],
                "p95_improvements": eval["improvements"],
                "median_improvement": eval["median_improvement"],
            })
        })
        .collect();
    Ok(json!({ "trials": trials }))
}
