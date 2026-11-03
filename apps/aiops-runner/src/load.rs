//! Fixed-arrival-rate load with k6. Baseline and candidate always get the same plan.

use std::path::Path;
use std::time::Duration;

use aiops_core::report::LoadStats;
use anyhow::Context;
use serde_json::Value;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::config::Runtime;
use crate::proc::run_checked;

#[derive(Debug, Clone, Copy)]
pub struct LoadPlan {
    pub rate: u32,
    pub duration_s: u32,
}

impl LoadPlan {
    pub fn measure(rt: &Runtime) -> Self {
        Self {
            rate: rt.spec.load.rate,
            duration_s: rt.spec.load.duration_s,
        }
    }

    pub fn warmup(rt: &Runtime) -> Self {
        Self {
            rate: rt.spec.load.rate,
            duration_s: rt.spec.load.warmup_s.max(1),
        }
    }
}

pub async fn run(
    rt: &Runtime,
    plan: LoadPlan,
    target: (&str, Option<&str>),
    label: &str,
    out_dir: &Path,
    cancel: &CancellationToken,
) -> anyhow::Result<LoadStats> {
    let (target_url, auth) = target;
    let summary_path = out_dir.join(format!("k6-{label}.json"));
    let _ = std::fs::remove_file(&summary_path);
    let load = &rt.spec.load;
    let mut cmd = Command::new(&rt.k6);
    cmd.args(["run", "--quiet"])
        .arg(rt.repo_path(&load.script))
        .env("K6_NO_USAGE_REPORT", "true")
        .env("TARGET", target_url)
        .env("RATE", plan.rate.to_string())
        .env("DURATION_S", plan.duration_s.to_string())
        .env("PRE_VUS", load.pre_allocated_vus.to_string())
        .env("MAX_VUS", load.max_vus.to_string())
        .env("TIMEOUT_MS", load.request_timeout_ms.to_string())
        .env("USERS", load.users.to_string())
        .env("SUMMARY_PATH", &summary_path);
    if let Some(token) = auth {
        cmd.env("AUTH_TOKEN", token);
    }
    let timeout = Duration::from_secs(plan.duration_s as u64 + 60);
    run_checked(
        &mut cmd,
        &out_dir.join(format!("k6-{label}.log")),
        timeout,
        cancel,
        "k6",
    )
    .await?;

    let text = std::fs::read_to_string(&summary_path).context("k6 wrote no summary")?;
    let summary: Value = serde_json::from_str(&text).context("k6 summary is not JSON")?;
    Ok(parse_summary(&summary, plan, label))
}

pub fn parse_summary(summary: &Value, plan: LoadPlan, label: &str) -> LoadStats {
    let metrics = &summary["metrics"];
    let count = |name: &str| metrics[name]["values"]["count"].as_f64().unwrap_or(0.0) as u64;
    let latency = &metrics["http_req_duration{expected_response:true}"]["values"];
    let pct = |key: &str| latency[key].as_f64().unwrap_or(0.0);
    let planned = plan.rate as u64 * plan.duration_s as u64;
    let iterations = count("iterations");
    let dropped = count("dropped_iterations");
    LoadStats {
        label: label.to_owned(),
        rate: plan.rate,
        duration_s: plan.duration_s,
        planned,
        offered: planned.max(iterations + dropped),
        iterations,
        dropped,
        ok: count("ok_responses"),
        bad_status: count("bad_status"),
        timeouts: count("timeouts"),
        net_errors: count("net_errors"),
        p50_ms: pct("p(50)"),
        p95_ms: pct("p(95)"),
        p99_ms: pct("p(99)"),
        max_ms: pct("max"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_counts_and_success_latency() {
        let summary = json!({"metrics": {
            "iterations": {"values": {"count": 1990}},
            "dropped_iterations": {"values": {"count": 10}},
            "ok_responses": {"values": {"count": 1985}},
            "timeouts": {"values": {"count": 5}},
            "http_req_duration{expected_response:true}": {"values": {"p(50)": 3.0, "p(95)": 9.5, "p(99)": 12.0, "max": 40.0}}
        }});
        let s = parse_summary(
            &summary,
            LoadPlan {
                rate: 100,
                duration_s: 20,
            },
            "baseline",
        );
        assert_eq!(
            (
                s.offered,
                s.iterations,
                s.dropped,
                s.ok,
                s.timeouts,
                s.bad_status
            ),
            (2000, 1990, 10, 1985, 5, 0)
        );
        assert_eq!(s.p95_ms, 9.5);
        assert!(s.drained());
    }

    #[test]
    fn extra_scheduled_iteration_raises_the_denominator() {
        let summary = json!({"metrics": {
            "iterations": {"values": {"count": 2001}},
            "ok_responses": {"values": {"count": 2001}},
        }});
        let s = parse_summary(
            &summary,
            LoadPlan {
                rate: 100,
                duration_s: 20,
            },
            "baseline",
        );
        assert_eq!((s.planned, s.offered), (2000, 2001));
        assert_eq!(s.success_ratio(), 1.0);
    }
}
