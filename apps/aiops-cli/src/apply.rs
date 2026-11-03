//! `aiops apply`: puts a VERIFIED patch into a checkout, so it can become a pull request.
//!
//! The patch is applied with the harness's own code (`aiops_core::patch::apply_patch`), so the
//! same path policy, the same security gate and the same "search text matches exactly once" rule
//! hold here as in the runner. Only VERIFIED trials are applied, and only when the patch is the
//! one that was measured (its content hash equals the trial's).

use std::path::Path;

use aiops_core::patch::{apply_patch, PatchProposal, PathPolicy};
use aiops_core::scenario::ScenarioSpec;
use serde_json::{json, Value};

use crate::api::Api;
use crate::report;

pub async fn apply(
    api: &Api,
    trial_id: &str,
    scenario: &Path,
    repo: &Path,
) -> anyhow::Result<Value> {
    let detail = api.trial(trial_id).await?;
    let trial = &detail["trial"];
    let outcome = trial["outcome"].as_str().unwrap_or("unfinished");
    anyhow::ensure!(
        outcome == "VERIFIED",
        "trial {trial_id} is {outcome}; only VERIFIED patches are applied"
    );
    let patch: PatchProposal = serde_json::from_value(detail["patch"].clone())
        .map_err(|e| anyhow::anyhow!("the trial's patch cannot be read: {e}"))?;
    let measured = trial["patch_hash"].as_str().unwrap_or_default();
    anyhow::ensure!(
        patch.content_hash() == measured,
        "the patch returned by the API does not match the hash of the patch that was measured"
    );

    let spec = ScenarioSpec::load(scenario)?;
    let run_id = trial["run_id"].as_str().unwrap_or_default().to_owned();
    let run = api.run(&run_id).await?;
    let scenario_hash = run["run"]["scenario_hash"].as_str().unwrap_or_default();
    let same_scenario = scenario_hash == spec.content_hash();

    let policy = PathPolicy::new(
        spec.target.allowed_paths.clone(),
        spec.target.denied_paths.clone(),
    );
    let root = repo.join(&spec.target.source_dir);
    let changes = apply_patch(&root, &patch, &policy, spec.target.max_edits)
        .map_err(|e| anyhow::anyhow!("the patch does not apply to {}: {e}", root.display()))?;
    let files: Vec<String> = changes
        .iter()
        .map(|c| format!("{}/{}", spec.target.source_dir, c.path))
        .collect();

    let summary = report::trial_summary(api, trial_id, 0).await?;
    let title = format!(
        "perf({}): {}",
        spec.id,
        first_line(patch.summary.as_str(), 72)
    );
    let body = pr_body(api, &summary, &run["run"], &patch, &files, same_scenario);
    Ok(json!({
        "trial_id": trial_id,
        "run_id": run_id,
        "files": files,
        "same_scenario": same_scenario,
        "title": title,
        "body": body,
    }))
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= max {
        line.to_owned()
    } else {
        let cut: String = line.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

fn num(v: &Value, digits: usize) -> String {
    v.as_f64()
        .map(|x| format!("{x:.digits$}"))
        .unwrap_or_else(|| "-".into())
}

fn pct(v: &Value) -> String {
    v.as_f64()
        .map(|x| format!("{:.1}%", x * 100.0))
        .unwrap_or_else(|| "-".into())
}

/// The pull request description: what changed, the measured evidence and how to check it again.
fn pr_body(
    api: &Api,
    s: &Value,
    run: &Value,
    patch: &PatchProposal,
    files: &[String],
    same_scenario: bool,
) -> String {
    let m = &s["measurements"];
    let pairs = s["pair_improvements"]
        .as_array()
        .map(|a| a.iter().map(pct).collect::<Vec<_>>().join(" / "))
        .unwrap_or_else(|| "-".into());
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n\nThis change was proposed by a model and **VERIFIED by the Rust harness**: built in an isolated runner, checked against the original service's responses, and measured against the unchanged service in alternating pairs. The model did not judge its own patch.\n\n",
        patch.summary.trim()
    ));
    out.push_str(&format!(
        "**Hypothesis:** {}\n\n",
        s["hypothesis"].as_str().unwrap_or("-")
    ));
    out.push_str("| | Before | After |\n| --- | --- | --- |\n");
    out.push_str(&format!(
        "| p95 latency (mean of pairs) | {} ms | {} ms |\n",
        num(&m["p95_ms"]["baseline"], 1),
        num(&m["p95_ms"]["candidate"], 1)
    ));
    out.push_str(&format!(
        "| SQL statements per request | {} | {} |\n",
        num(&m["sql_per_request"]["baseline"], 1),
        num(&m["sql_per_request"]["candidate"], 1)
    ));
    out.push_str(&format!(
        "| Median pairwise p95 improvement | | {} (pairs {pairs}) |\n\n",
        pct(&s["median_p95_improvement"])
    ));
    out.push_str(&format!(
        "Correctness: golden {} · invariants {} · sampled requests {}\n\n",
        m["golden"].as_str().unwrap_or("-"),
        m["invariants"].as_str().unwrap_or("-"),
        m["sample"].as_str().unwrap_or("-"),
    ));
    out.push_str(&format!("Files: {}\n\n", files.join(", ")));
    out.push_str(&format!(
        "Evidence: [run]({}) · [trial]({}) · model `{}` · patch `{}`\n",
        api.run_url(run["id"].as_str().unwrap_or_default()),
        s["url"].as_str().unwrap_or_default(),
        run["provider"].as_str().unwrap_or("-"),
        &patch.content_hash()[..12],
    ));
    if !same_scenario {
        out.push_str("\n> The scenario in this checkout differs from the one the run was judged against. The patch still applied cleanly, but the evidence above belongs to the earlier scenario.\n");
    }
    out.push_str("\n> The harness checks behaviour on the scenario's data and load; it is not a code review. Review before merging.\n");
    out
}
