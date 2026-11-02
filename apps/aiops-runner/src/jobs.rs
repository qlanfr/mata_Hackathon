//! The three jobs the runner performs: baseline (A/A + evidence), trial (one patch), golden.
//!
//! Jobs return reports, not errors, for anything that is a property of the patch or the
//! environment. Services are always stopped: on success explicitly, otherwise by `Drop`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aiops_core::patch::{apply_patch, PatchProposal, PathPolicy};
use aiops_core::report::{BaselineReport, BuildReport, LoadStats, Pair, TrialReport};
use tokio_util::sync::CancellationToken;

use crate::config::Runtime;
use crate::correctness;
use crate::load::{self, LoadPlan};
use crate::logs;
use crate::pg::Postgres;
use crate::proc::{is_cancelled, Cancelled};
use crate::service::{Service, ServiceConfig};
use crate::workspace;

const PORT_BASELINE: u16 = 18081;
const PORT_CANDIDATE: u16 = 18082;
const PORT_BASELINE_B: u16 = 18083;

/// Shared handle for one running job: its cancellation token and current phase.
#[derive(Clone, Default)]
pub struct JobControl {
    pub cancel: CancellationToken,
    phase: Arc<Mutex<String>>,
}

impl JobControl {
    pub fn phase(&self) -> String {
        self.phase.lock().expect("phase lock").clone()
    }

    fn set_phase(&self, phase: &str) {
        tracing::info!(phase, "job phase");
        *self.phase.lock().expect("phase lock") = phase.to_owned();
    }

    fn check(&self) -> anyhow::Result<()> {
        if self.cancel.is_cancelled() {
            Err(Cancelled.into())
        } else {
            Ok(())
        }
    }
}

struct Timer<'a> {
    timings: &'a mut BTreeMap<String, u64>,
    started: Instant,
}

impl<'a> Timer<'a> {
    fn new(timings: &'a mut BTreeMap<String, u64>) -> Self {
        Self {
            timings,
            started: Instant::now(),
        }
    }

    fn lap(&mut self, name: &str) {
        self.timings
            .insert(name.to_owned(), self.started.elapsed().as_millis() as u64);
        self.started = Instant::now();
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client")
}

/// Builds the unmodified target once per source hash and reuses the binary afterwards.
async fn baseline_binary(
    rt: &Runtime,
    ctl: &JobControl,
) -> anyhow::Result<(PathBuf, String, BuildReport)> {
    let source = rt.repo_path(&rt.spec.target.source_dir);
    let source_hash = workspace::source_hash(&source)?;
    let dir = rt.work_dir.join("baseline").join(&source_hash[..16]);
    let bin = dir.join("bin").join(&rt.spec.target.binary);
    if bin.exists() {
        let report = BuildReport {
            ok: true,
            cached: true,
            duration_ms: 0,
            log_sha256: String::new(),
            log_tail: String::new(),
        };
        return Ok((bin, source_hash, report));
    }
    let src = dir.join("src");
    let _ = std::fs::remove_dir_all(&src);
    workspace::copy_tree(&source, &src)?;
    let report = crate::builder::build(rt, &src, &bin, &dir.join("build.log"), &ctl.cancel).await?;
    Ok((bin, source_hash, report))
}

fn service_config<'a>(
    rt: &Runtime,
    label: &'a str,
    binary: &'a Path,
    port: u16,
    db: &'a str,
    dir: &Path,
) -> ServiceConfig<'a> {
    ServiceConfig {
        label,
        binary,
        port,
        database_url: db,
        pool_size: rt.spec.service.pool_size,
        log_path: dir.join(format!("{label}.log")),
        startup_timeout: Duration::from_secs(rt.spec.service.startup_timeout_s),
    }
}

/// Runs `first` then `second` with the same plan, returning stats in that order.
async fn measure_pair(
    rt: &Runtime,
    ctl: &JobControl,
    dir: &Path,
    index: u32,
    first: (&str, &str),
    second: (&str, &str),
) -> anyhow::Result<(LoadStats, LoadStats)> {
    let plan = LoadPlan::measure(rt);
    let mut a = load::run(
        rt,
        plan,
        first.1,
        &format!("{}-{index}", first.0),
        dir,
        &ctl.cancel,
    )
    .await?;
    a.label = first.0.to_owned();
    let mut b = load::run(
        rt,
        plan,
        second.1,
        &format!("{}-{index}", second.0),
        dir,
        &ctl.cancel,
    )
    .await?;
    b.label = second.0.to_owned();
    Ok((a, b))
}

// ---------------------------------------------------------------------------------------------
// Baseline job

pub async fn run_baseline(rt: &Runtime, ctl: &JobControl) -> BaselineReport {
    let mut report = BaselineReport {
        scenario_id: rt.spec.id.clone(),
        scenario_hash: rt.scenario_hash.clone(),
        env: rt.env.clone(),
        ..Default::default()
    };
    if let Err(err) = baseline_inner(rt, ctl, &mut report).await {
        report.execution_error = Some(if is_cancelled(&err) {
            "cancelled".into()
        } else {
            format!("{err:#}")
        });
    }
    report
}

async fn baseline_inner(
    rt: &Runtime,
    ctl: &JobControl,
    report: &mut BaselineReport,
) -> anyhow::Result<()> {
    let dir = rt
        .work_dir
        .join("baseline-runs")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir)?;
    let mut timings = BTreeMap::new();
    let mut timer = Timer::new(&mut timings);
    let http = client();

    ctl.set_phase("database");
    let db = Postgres::new(rt).ensure(rt, &ctl.cancel).await?;
    timer.lap("database");

    ctl.set_phase("build");
    let (bin, source_hash, build) = baseline_binary(rt, ctl).await?;
    report.source_hash = source_hash;
    report.build = Some(build.clone());
    anyhow::ensure!(build.ok, "baseline build failed: {}", build.log_tail);
    timer.lap("build");

    ctl.set_phase("start");
    let a = Service::start(
        service_config(rt, "baseline", &bin, PORT_BASELINE, &db, &dir),
        &http,
    )
    .await?;
    let b = Service::start(
        service_config(rt, "baseline-b", &bin, PORT_BASELINE_B, &db, &dir),
        &http,
    )
    .await?;
    timer.lap("start");

    ctl.set_phase("correctness");
    let cases = correctness::load_cases(&rt.repo_path(&rt.spec.correctness.cases_dir))?;
    let checked = correctness::check(&http, &a.base_url, &cases).await;
    let passed = checked.all_passed();
    report.correctness = Some(checked);
    anyhow::ensure!(passed, "baseline does not match the golden cases");
    timer.lap("correctness");

    ctl.set_phase("warmup");
    for svc in [&a, &b] {
        load::run(
            rt,
            LoadPlan::warmup(rt),
            &svc.base_url,
            &format!("warmup-{}", svc.label),
            &dir,
            &ctl.cancel,
        )
        .await?;
    }
    timer.lap("warmup");

    for index in 0..rt.spec.contract.pairs {
        ctl.check()?;
        ctl.set_phase(&format!("measure {}/{}", index + 1, rt.spec.contract.pairs));
        let (sa, sb) = if index.is_multiple_of(2) {
            measure_pair(
                rt,
                ctl,
                &dir,
                index,
                ("baseline", &a.base_url),
                ("baseline-b", &b.base_url),
            )
            .await?
        } else {
            let (sb, sa) = measure_pair(
                rt,
                ctl,
                &dir,
                index,
                ("baseline-b", &b.base_url),
                ("baseline", &a.base_url),
            )
            .await?;
            (sa, sb)
        };
        report.aa_pairs.push(Pair {
            index,
            order: if index.is_multiple_of(2) { "AB" } else { "BA" }.into(),
            baseline: sa,
            candidate: sb,
        });
    }
    timer.lap("measure");

    ctl.set_phase("stop");
    a.stop().await;
    b.stop().await;
    let (summary, slowest) = logs::summarize(&dir.join("baseline.log"), 10);
    report.logs = summary;
    report.slow_requests = slowest;
    timer.lap("stop");
    report.timings_ms = timings;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Trial job

pub async fn run_trial(rt: &Runtime, patch: &PatchProposal, ctl: &JobControl) -> TrialReport {
    let mut report = TrialReport {
        trial_id: uuid::Uuid::new_v4().to_string(),
        scenario_id: rt.spec.id.clone(),
        scenario_hash: rt.scenario_hash.clone(),
        patch_hash: patch.content_hash(),
        env: rt.env.clone(),
        ..Default::default()
    };
    let dir = rt.work_dir.join("trials").join(&report.trial_id);
    if let Err(err) = trial_inner(rt, patch, ctl, &dir, &mut report).await {
        report.execution_error = Some(if is_cancelled(&err) {
            "cancelled".into()
        } else {
            format!("{err:#}")
        });
    }
    if !rt.keep_workspaces {
        let _ = std::fs::remove_dir_all(&dir);
    }
    report
}

async fn trial_inner(
    rt: &Runtime,
    patch: &PatchProposal,
    ctl: &JobControl,
    dir: &Path,
    report: &mut TrialReport,
) -> anyhow::Result<()> {
    let mut timings = BTreeMap::new();
    let mut timer = Timer::new(&mut timings);
    let http = client();
    let src = dir.join("src");

    ctl.set_phase("prepare");
    workspace::copy_tree(&rt.repo_path(&rt.spec.target.source_dir), &src)?;
    let policy = PathPolicy::new(
        rt.spec.target.allowed_paths.clone(),
        rt.spec.target.denied_paths.clone(),
    );
    match apply_patch(&src, patch, &policy, rt.spec.target.max_edits) {
        Ok(changes) => report.diff = workspace::unified_diff(&changes),
        Err(err) => {
            report.patch_error = Some(err.to_string());
            report.timings_ms = timings;
            return Ok(());
        }
    }
    timer.lap("prepare");

    ctl.set_phase("database");
    let db = Postgres::new(rt).ensure(rt, &ctl.cancel).await?;
    timer.lap("database");

    ctl.set_phase("build-baseline");
    let (base_bin, _, base_build) = baseline_binary(rt, ctl).await?;
    anyhow::ensure!(
        base_build.ok,
        "baseline build failed: {}",
        base_build.log_tail
    );
    timer.lap("build_baseline");

    ctl.set_phase("build");
    let cand_bin = dir.join("bin").join(&rt.spec.target.binary);
    let build =
        crate::builder::build(rt, &src, &cand_bin, &dir.join("build.log"), &ctl.cancel).await?;
    let built = build.ok;
    report.build = Some(build);
    timer.lap("build");
    if !built {
        report.timings_ms = timings;
        return Ok(());
    }

    ctl.set_phase("start");
    let base = Service::start(
        service_config(rt, "baseline", &base_bin, PORT_BASELINE, &db, dir),
        &http,
    )
    .await?;
    let mut cand = match Service::start(
        service_config(rt, "candidate", &cand_bin, PORT_CANDIDATE, &db, dir),
        &http,
    )
    .await
    {
        Ok(svc) => svc,
        Err(err) => {
            report.candidate_crashed = Some(format!("failed to start: {err:#}"));
            report.timings_ms = timings;
            return Ok(());
        }
    };
    timer.lap("start");

    ctl.set_phase("correctness");
    let cases = correctness::load_cases(&rt.repo_path(&rt.spec.correctness.cases_dir))?;
    let base_check = correctness::check(&http, &base.base_url, &cases).await;
    let cand_check = correctness::check(&http, &cand.base_url, &cases).await;
    let proceed = base_check.all_passed() && cand_check.all_passed();
    report.baseline_correctness = Some(base_check);
    report.correctness = Some(cand_check);
    timer.lap("correctness");

    if proceed {
        ctl.set_phase("warmup");
        for svc in [&base, &cand] {
            load::run(
                rt,
                LoadPlan::warmup(rt),
                &svc.base_url,
                &format!("warmup-{}", svc.label),
                dir,
                &ctl.cancel,
            )
            .await?;
        }
        timer.lap("warmup");

        for index in 0..rt.spec.contract.pairs {
            ctl.check()?;
            ctl.set_phase(&format!("measure {}/{}", index + 1, rt.spec.contract.pairs));
            let (b, c) = if index.is_multiple_of(2) {
                measure_pair(
                    rt,
                    ctl,
                    dir,
                    index,
                    ("baseline", &base.base_url),
                    ("candidate", &cand.base_url),
                )
                .await?
            } else {
                let (c, b) = measure_pair(
                    rt,
                    ctl,
                    dir,
                    index,
                    ("candidate", &cand.base_url),
                    ("baseline", &base.base_url),
                )
                .await?;
                (b, c)
            };
            report.pairs.push(Pair {
                index,
                order: if index.is_multiple_of(2) { "BC" } else { "CB" }.into(),
                baseline: b,
                candidate: c,
            });
            if let Some(exit) = cand.exit_status() {
                report.candidate_crashed = Some(exit);
                break;
            }
        }
        timer.lap("measure");
    }

    ctl.set_phase("stop");
    if report.candidate_crashed.is_none() {
        report.candidate_crashed = cand.exit_status();
    }
    base.stop().await;
    cand.stop().await;
    report.baseline_logs = Some(logs::summarize(&dir.join("baseline.log"), 0).0);
    report.candidate_logs = Some(logs::summarize(&dir.join("candidate.log"), 0).0);
    timer.lap("stop");
    report.timings_ms = timings;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Golden job

/// Records the baseline's responses to the scenario's case requests as golden files.
pub async fn generate_golden(rt: &Runtime, ctl: &JobControl) -> anyhow::Result<Vec<String>> {
    let dir = rt.work_dir.join("golden-run");
    std::fs::create_dir_all(&dir)?;
    let http = client();
    let db = Postgres::new(rt).ensure(rt, &ctl.cancel).await?;
    let (bin, _, build) = baseline_binary(rt, ctl).await?;
    anyhow::ensure!(build.ok, "baseline build failed: {}", build.log_tail);
    let svc = Service::start(
        service_config(rt, "baseline", &bin, PORT_BASELINE, &db, &dir),
        &http,
    )
    .await?;
    let cases = correctness::capture(&http, &svc.base_url, &rt.spec.correctness.requests).await?;
    svc.stop().await;

    let out = rt.repo_path(&rt.spec.correctness.cases_dir);
    std::fs::create_dir_all(&out)?;
    let mut written = Vec::new();
    for case in cases {
        let path = out.join(format!("{}.json", case.name));
        std::fs::write(&path, serde_json::to_string_pretty(&case)? + "\n")?;
        written.push(format!("{} → {}", case.name, case.expect.status));
    }
    Ok(written)
}
