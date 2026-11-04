//! The three jobs the runner performs: baseline (A/A + evidence), trial (one patch), golden.
//!
//! Jobs return reports, not errors, for anything that is a property of the patch or the
//! environment. Services are always stopped: on success explicitly, otherwise by `Drop`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;

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
use crate::sqlstats::SqlMeter;
use crate::workspace;

const PORT_BASELINE: u16 = 18081;
const PORT_CANDIDATE: u16 = 18082;
const PORT_BASELINE_B: u16 = 18083;

/// Shared handle for one running job: its cancellation token, current phase and phase history.
/// The history lets a caller that polls every few seconds still see phases that were shorter
/// than its polling interval.
#[derive(Clone, Default)]
pub struct JobControl {
    pub cancel: CancellationToken,
    phases: Arc<Mutex<Vec<(String, u64)>>>,
}

impl JobControl {
    pub fn phase(&self) -> String {
        self.phases
            .lock()
            .expect("phase lock")
            .last()
            .map(|p| p.0.clone())
            .unwrap_or_else(|| "queued".into())
    }

    /// Every phase entered so far, with its start time (epoch ms).
    pub fn history(&self) -> Vec<(String, u64)> {
        self.phases.lock().expect("phase lock").clone()
    }

    fn set_phase(&self, phase: &str) {
        tracing::info!(phase, "job phase");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.phases
            .lock()
            .expect("phase lock")
            .push((phase.to_owned(), now));
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

/// Logs in once if the scenario needs it. Returns a client that sends the token with every
/// request, and the token for the load script. The services of one job share their data
/// directory, so a token issued by one of them is valid for all of them.
async fn authenticate(
    rt: &Runtime,
    http: reqwest::Client,
    base_url: &str,
) -> anyhow::Result<(reqwest::Client, Option<String>)> {
    let Some(login) = &rt.spec.service.login else {
        return Ok((http, None));
    };
    let resp = http
        .post(format!("{base_url}{}", login.path))
        .form(&login.form)
        .send()
        .await?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.context("the login answer is not JSON")?;
    anyhow::ensure!(status.is_success(), "login failed with {status}: {body}");
    let token = body[&login.token_field]
        .as_str()
        .with_context(|| format!("the login answer has no `{}`", login.token_field))?
        .to_owned();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse()?,
    );
    let authed = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .default_headers(headers)
        .build()?;
    Ok((authed, Some(token)))
}

fn sql_meter(rt: &Runtime) -> Option<SqlMeter> {
    rt.spec
        .database
        .statement_stats
        .then(|| SqlMeter::new(Postgres::new(rt), &rt.spec.database.name))
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

/// Golden request paths first, then the sampled ones (see `correctness::sample_paths`).
fn probe_paths(rt: &Runtime, cases: &[correctness::GoldenCase]) -> Vec<String> {
    let mut paths: Vec<String> = cases.iter().map(|c| c.request.path.clone()).collect();
    if let Some(sample) = &rt.spec.correctness.sample {
        paths.extend(correctness::sample_paths(sample, rt.spec.load.users));
    }
    paths
}

fn service_config<'a>(
    rt: &'a Runtime,
    label: &'a str,
    binary: &'a Path,
    port: u16,
    db: &'a str,
    dir: &Path,
) -> ServiceConfig<'a> {
    let data_dir = dir.join("data");
    let _ = std::fs::create_dir_all(&data_dir);
    let fill = |v: &str| {
        v.replace("{port}", &port.to_string())
            .replace("{database_url}", db)
            .replace("{pool_size}", &rt.spec.service.pool_size.to_string())
            .replace("{data_dir}", &data_dir.to_string_lossy())
    };
    ServiceConfig {
        label,
        binary,
        port,
        database_url: db,
        pool_size: rt.spec.service.pool_size,
        log_path: dir.join(format!("{label}.log")),
        startup_timeout: Duration::from_secs(rt.spec.service.startup_timeout_s),
        health_path: rt.spec.service.health_path(),
        extra_env: rt
            .spec
            .service
            .env
            .iter()
            .map(|(k, v)| (k.clone(), fill(v)))
            .collect(),
    }
}

/// What one measured pair needs besides the two services: the bearer token, if any, and the SQL
/// meter, if the scenario counts statements. `top_label` names the service whose first measured
/// run supplies the most frequent statements.
struct Measure<'a> {
    auth: Option<&'a str>,
    meter: Option<SqlMeter>,
    top_label: Option<&'a str>,
}

/// Runs `first` then `second` with the same plan, returning stats in that order.
/// Keeps a service stopped (SIGSTOP) while it lives; resumes it when dropped, also on errors.
struct Frozen<'a>(&'a Service);

impl<'a> Frozen<'a> {
    fn new(service: &'a Service) -> Self {
        service.pause();
        Self(service)
    }
}

impl Drop for Frozen<'_> {
    fn drop(&mut self) {
        self.0.resume();
    }
}

/// Measures `first`, then `second`. While one is measured the other is frozen, so a candidate
/// cannot burn CPU or hold resources to slow the baseline down while it waits its turn.
async fn measure_pair(
    rt: &Runtime,
    ctl: &JobControl,
    dir: &Path,
    index: u32,
    first: &Service,
    second: &Service,
    m: &mut Measure<'_>,
) -> anyhow::Result<(LoadStats, LoadStats)> {
    let a = {
        let _idle = Frozen::new(second);
        measure_one(rt, ctl, dir, index, (&first.label, &first.base_url), m).await?
    };
    let b = {
        let _idle = Frozen::new(first);
        measure_one(rt, ctl, dir, index, (&second.label, &second.base_url), m).await?
    };
    Ok((a, b))
}

async fn measure_one(
    rt: &Runtime,
    ctl: &JobControl,
    dir: &Path,
    index: u32,
    (label, url): (&str, &str),
    m: &mut Measure<'_>,
) -> anyhow::Result<LoadStats> {
    if let Some(meter) = &m.meter {
        meter.begin().await?;
    }
    let mut stats = load::run(
        rt,
        LoadPlan::measure(rt),
        (url, m.auth),
        &format!("{label}-{index}"),
        dir,
        &ctl.cancel,
    )
    .await?;
    stats.label = label.to_owned();
    if let Some(meter) = &mut m.meter {
        meter
            .end(label, stats.iterations, m.top_label == Some(label))
            .await?;
    }
    Ok(stats)
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
    let (http, token) = authenticate(rt, http, &a.base_url).await?;
    let mut m = Measure {
        auth: token.as_deref(),
        meter: sql_meter(rt),
        top_label: Some("baseline"),
    };
    timer.lap("start");

    ctl.set_phase("correctness");
    let cases = correctness::load_cases(&rt.repo_path(&rt.spec.correctness.cases_dir))?;
    let checked = correctness::check(
        &http,
        &a.base_url,
        &cases,
        rt.spec.correctness.unordered_arrays,
    )
    .await;
    let passed = checked.all_passed();
    report.correctness = Some(checked);
    anyhow::ensure!(passed, "baseline does not match the golden cases");
    // The original code is checked against the API's own rules too: a violation here is a bug
    // it already had, and no candidate can be judged against it.
    let responses =
        correctness::fetch_each(&http, &a.base_url, &probe_paths(rt, &cases), &ctl.cancel).await?;
    let invariants = crate::invariants::check(&rt.spec.correctness.invariants, &responses);
    let ok = invariants.all_passed() || invariants.total == 0;
    let first = invariants
        .cases
        .first()
        .map(|c| format!("{}: {}", c.name, c.detail.clone().unwrap_or_default()));
    report.invariants = Some(invariants);
    anyhow::ensure!(
        ok,
        "the original service violates its response invariants: {}",
        first.unwrap_or_default()
    );
    timer.lap("correctness");

    // Vulnerability fixes: every attack must work on the original, or there is nothing to fix.
    // Nothing is measured under load.
    if let Some(security) = &rt.spec.security {
        ctl.set_phase("attacks");
        report.exploits = crate::exploits::run(&http, &a.base_url, security).await;
        timer.lap("attacks");
        a.stop().await;
        b.stop().await;
        report.timings_ms = timings;
        if let Some(e) = report.exploits.iter().find(|e| !e.exploited()) {
            anyhow::bail!(
                "attack `{}` does not work on the original service (status {}, leak {}); there is nothing to fix",
                e.name,
                e.status,
                if e.leaked { "found" } else { "not found" }
            );
        }
        return Ok(());
    }

    ctl.set_phase("warmup");
    for svc in [&a, &b] {
        load::run(
            rt,
            LoadPlan::warmup(rt),
            (&svc.base_url, m.auth),
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
            measure_pair(rt, ctl, &dir, index, &a, &b, &mut m).await?
        } else {
            let (sb, sa) = measure_pair(rt, ctl, &dir, index, &b, &a, &mut m).await?;
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
    // A service without its own SQL log gets its SQL counts from the database instead.
    if let Some(meter) = &m.meter {
        if report.logs.requests == 0 {
            report.logs = meter.summary("baseline").unwrap_or_default();
        }
        report.statements = meter.top.clone();
    }
    timer.lap("stop");
    report.timings_ms = timings;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Trial job

/// How far a trial goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialMode {
    /// Correctness, then the paired load measurement.
    Full,
    /// Every correctness check (golden, invariants, sample) on both services, even after one
    /// fails, and no load. Used by mutation testing to see which checks catch a change.
    CorrectnessOnly,
}

pub async fn run_trial(rt: &Runtime, patch: &PatchProposal, ctl: &JobControl) -> TrialReport {
    run_trial_mode(rt, patch, ctl, TrialMode::Full).await
}

pub async fn run_trial_mode(
    rt: &Runtime,
    patch: &PatchProposal,
    ctl: &JobControl,
    mode: TrialMode,
) -> TrialReport {
    let mut report = TrialReport {
        trial_id: uuid::Uuid::new_v4().to_string(),
        scenario_id: rt.spec.id.clone(),
        scenario_hash: rt.scenario_hash.clone(),
        patch_hash: patch.content_hash(),
        env: rt.env.clone(),
        ..Default::default()
    };
    let dir = rt.work_dir.join("trials").join(&report.trial_id);
    if let Err(err) = trial_inner(rt, patch, ctl, &dir, &mut report, mode).await {
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
    mode: TrialMode,
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
    let (http, token) = authenticate(rt, http, &base.base_url).await?;
    let mut m = Measure {
        auth: token.as_deref(),
        meter: sql_meter(rt),
        top_label: None,
    };
    timer.lap("start");

    if let Some(security) = &rt.spec.security {
        ctl.set_phase("attacks");
        report.security = Some(aiops_core::report::SecurityReport {
            baseline: crate::exploits::run(&http, &base.base_url, security).await,
            candidate: crate::exploits::run(&http, &cand.base_url, security).await,
        });
        timer.lap("attacks");
    }

    ctl.set_phase("correctness");
    let cases = correctness::load_cases(&rt.repo_path(&rt.spec.correctness.cases_dir))?;
    let base_check = correctness::check(
        &http,
        &base.base_url,
        &cases,
        rt.spec.correctness.unordered_arrays,
    )
    .await;
    let cand_check = correctness::check(
        &http,
        &cand.base_url,
        &cases,
        rt.spec.correctness.unordered_arrays,
    )
    .await;
    let mut proceed = base_check.all_passed() && cand_check.all_passed();
    report.baseline_correctness = Some(base_check);
    report.correctness = Some(cand_check);
    if proceed || mode == TrialMode::CorrectnessOnly {
        // Beyond the stored answers: the same golden and sampled requests to both services,
        // checked against the response invariants and compared with each other.
        let paths = probe_paths(rt, &cases);
        let base_resp = correctness::fetch_each(&http, &base.base_url, &paths, &ctl.cancel).await?;
        let cand_resp = correctness::fetch_each(&http, &cand.base_url, &paths, &ctl.cancel).await?;
        let sets = &rt.spec.correctness.invariants;
        let base_inv = crate::invariants::check(sets, &base_resp);
        let cand_inv = crate::invariants::check(sets, &cand_resp);
        let sampled = correctness::compare(
            &base_resp[cases.len()..],
            &cand_resp[cases.len()..],
            rt.spec.correctness.unordered_arrays,
        );
        let clean = |r: &aiops_core::report::CorrectnessReport| r.total == 0 || r.all_passed();
        proceed = clean(&base_inv) && clean(&cand_inv) && clean(&sampled);
        report.baseline_invariants = Some(base_inv);
        report.invariants = Some(cand_inv);
        report.sample = Some(sampled);
    }
    timer.lap("correctness");

    if proceed && mode == TrialMode::Full && rt.spec.security.is_none() {
        ctl.set_phase("warmup");
        for svc in [&base, &cand] {
            load::run(
                rt,
                LoadPlan::warmup(rt),
                (&svc.base_url, m.auth),
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
                measure_pair(rt, ctl, dir, index, &base, &cand, &mut m).await?
            } else {
                let (c, b) = measure_pair(rt, ctl, dir, index, &cand, &base, &mut m).await?;
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
    for (label, slot) in [
        ("baseline", &mut report.baseline_logs),
        ("candidate", &mut report.candidate_logs),
    ] {
        let mut summary = logs::summarize(&dir.join(format!("{label}.log")), 0).0;
        if summary.requests == 0 {
            if let Some(from_db) = m.meter.as_ref().and_then(|meter| meter.summary(label)) {
                summary = from_db;
            }
        }
        *slot = Some(summary);
    }
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
    let (http, _) = authenticate(rt, http, &svc.base_url).await?;
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
