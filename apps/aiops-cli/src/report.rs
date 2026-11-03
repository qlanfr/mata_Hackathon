//! Compact summaries of runs and trials, shared by the CLI's text output and the MCP tools.

use serde_json::{json, Value};

use crate::api::{is_terminal, Api};

fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

fn pct(x: Option<f64>) -> String {
    x.map(|v| format!("{:.1}%", v * 100.0))
        .unwrap_or_else(|| "-".into())
}

/// Before/after numbers of one trial report: mean p95 over the pairs, SQL statements per request
/// and the correctness checks.
pub fn measurements(report: &Value) -> Value {
    let pairs = report["pairs"].as_array().cloned().unwrap_or_default();
    let p95 = |side: &str| {
        mean(
            &pairs
                .iter()
                .filter_map(|p| p[side]["p95_ms"].as_f64())
                .collect::<Vec<_>>(),
        )
    };
    let checks = |key: &str| {
        let c = &report[key];
        if c.is_object() {
            json!(format!("{}/{}", c["passed"], c["total"]))
        } else {
            Value::Null
        }
    };
    json!({
        "p95_ms": { "baseline": p95("baseline"), "candidate": p95("candidate") },
        "sql_per_request": {
            "baseline": report["baseline_logs"]["avg_sql_count"],
            "candidate": report["candidate_logs"]["avg_sql_count"],
        },
        "golden": checks("correctness"),
        "invariants": checks("invariants"),
        "sample": checks("sample"),
        "build_ms": report["build"]["duration_ms"],
        "attacks": attacks(report),
    })
}

/// Vulnerability-fix trials: how many attacks worked on the original and how many the candidate
/// blocked. `null` for performance trials.
fn attacks(report: &Value) -> Value {
    let Some(security) = report["security"].as_object() else {
        return Value::Null;
    };
    let worked = |e: &Value| {
        e["status"]
            .as_u64()
            .is_some_and(|s| (200..300).contains(&s))
            && e["leaked"] == true
    };
    let side = |key: &str| {
        security
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let (base, cand) = (side("baseline"), side("candidate"));
    json!({
        "total": cand.len(),
        "held_out": cand.iter().filter(|e| e["held_out"] == true).count(),
        "worked_on_original": base.iter().filter(|e| worked(e)).count(),
        "blocked_on_candidate": cand.iter().filter(|e| !worked(e)).count(),
    })
}

/// A run with its trials, and the measurements of the verified trial if there is one.
pub async fn run_summary(api: &Api, run_id: &str) -> anyhow::Result<Value> {
    let detail = api.run(run_id).await?;
    let run = &detail["run"];
    let state = run["state"].as_str().unwrap_or_default().to_owned();
    let trials: Vec<Value> = detail["trials"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|t| {
            json!({
                "trial_id": t["id"],
                "seq": t["seq"],
                "outcome": t["outcome"],
                "median_p95_improvement": t["evaluation"]["median_improvement"],
                "pair_improvements": t["evaluation"]["improvements"],
                "reason": t["evaluation"]["reasons"][0],
                "hypothesis": t["hypothesis"],
                "patch_summary": t["patch_summary"],
                "url": t["id"].as_str().map(|id| api.trial_url(id)),
            })
        })
        .collect();
    let verified = trials.iter().rev().find(|t| t["outcome"] == "VERIFIED");
    let measured = match verified.and_then(|t| t["trial_id"].as_str()) {
        Some(id) => measurements(&api.trial(id).await?["report"]),
        None => Value::Null,
    };
    Ok(json!({
        "run_id": run["id"],
        "url": api.run_url(run_id),
        "state": state,
        "finished": is_terminal(&state),
        "verified": verified.is_some(),
        "provider": run["provider"],
        "summary": run["summary"],
        "error": run["error"],
        "usage": run["usage"],
        "aa_noise": run["aa_noise"],
        "trials": trials,
        "verified_measurements": measured,
    }))
}

/// A trial's verdict, measurements, reasons and diff (cut to `max_diff` characters).
pub async fn trial_summary(api: &Api, trial_id: &str, max_diff: usize) -> anyhow::Result<Value> {
    let detail = api.trial(trial_id).await?;
    let trial = &detail["trial"];
    let report = &detail["report"];
    let diff = report["diff"].as_str().unwrap_or_default();
    let cut: String = diff.chars().take(max_diff).collect();
    Ok(json!({
        "trial_id": trial["id"],
        "run_id": trial["run_id"],
        "url": api.trial_url(trial_id),
        "outcome": trial["outcome"],
        "reasons": trial["evaluation"]["reasons"],
        "pair_improvements": trial["evaluation"]["improvements"],
        "median_p95_improvement": trial["evaluation"]["median_improvement"],
        "hypothesis": trial["hypothesis"],
        "patch_summary": trial["patch_summary"],
        "measurements": measurements(report),
        "diff": cut,
        "diff_truncated": diff.chars().count() > max_diff,
    }))
}

/// Human-readable lines for a run summary.
pub fn run_text(s: &Value) -> String {
    let mut out = vec![format!(
        "run {}  {}  {}",
        s["run_id"].as_str().unwrap_or("-"),
        s["state"].as_str().unwrap_or("-"),
        s["provider"].as_str().unwrap_or("-")
    )];
    out.push(format!("  {}", s["url"].as_str().unwrap_or_default()));
    let u = &s["usage"];
    if u["model_turns"].as_u64().unwrap_or(0) > 0 {
        out.push(format!(
            "  model: {} turns, {} tool calls, {} candidates, {} input / {} output tokens",
            u["model_turns"],
            u["tool_calls"],
            u["candidates"],
            u["input_tokens"],
            u["output_tokens"]
        ));
    }
    for t in s["trials"].as_array().into_iter().flatten() {
        out.push(format!(
            "  trial {}: {:<16} {:>7}  {}",
            t["seq"],
            t["outcome"].as_str().unwrap_or("RUNNING"),
            pct(t["median_p95_improvement"].as_f64()),
            t["reason"].as_str().unwrap_or_default()
        ));
    }
    let m = &s["verified_measurements"];
    if m["attacks"].is_object() {
        let a = &m["attacks"];
        out.push(format!(
            "  attacks: {} of {} blocked ({} held out from the model; {} worked on the original) | golden {}",
            a["blocked_on_candidate"], a["total"], a["held_out"], a["worked_on_original"],
            m["golden"].as_str().unwrap_or("-"),
        ));
    } else if m.is_object() {
        let num = |v: &Value, unit: &str| {
            v.as_f64()
                .map(|x| format!("{x:.1}{unit}"))
                .unwrap_or_else(|| "-".into())
        };
        out.push(format!(
            "  p95 {} -> {} | SQL per request {} -> {} | golden {} | invariants {} | sample {}",
            num(&m["p95_ms"]["baseline"], " ms"),
            num(&m["p95_ms"]["candidate"], " ms"),
            num(&m["sql_per_request"]["baseline"], ""),
            num(&m["sql_per_request"]["candidate"], ""),
            m["golden"].as_str().unwrap_or("-"),
            m["invariants"].as_str().unwrap_or("-"),
            m["sample"].as_str().unwrap_or("-"),
        ));
    }
    if let Some(summary) = s["summary"].as_str() {
        out.push(format!("  {summary}"));
    }
    if let Some(error) = s["error"].as_str() {
        out.push(format!("  error: {error}"));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurements_average_pairs_and_read_logs() {
        let report = json!({
            "pairs": [
                { "baseline": { "p95_ms": 20.0 }, "candidate": { "p95_ms": 2.0 } },
                { "baseline": { "p95_ms": 18.0 }, "candidate": { "p95_ms": 4.0 } },
            ],
            "baseline_logs": { "avg_sql_count": 81.8 },
            "candidate_logs": { "avg_sql_count": 3.0 },
            "correctness": { "passed": 14, "total": 14 },
        });
        let m = measurements(&report);
        assert_eq!(m["p95_ms"]["baseline"], 19.0);
        assert_eq!(m["p95_ms"]["candidate"], 3.0);
        assert_eq!(m["sql_per_request"]["candidate"], 3.0);
        assert_eq!(m["golden"], "14/14");
        assert_eq!(m["invariants"], Value::Null);
    }
}
