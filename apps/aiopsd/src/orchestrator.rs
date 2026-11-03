//! The run state machine. Each `tick` advances every unfinished run by at most one step, using
//! only what is in the control DB, so the process can restart at any point and continue.
//!
//! QUEUED → COLLECTING → BASELINING → PLANNING → PREPARING → BUILDING → CHECKING → BENCHMARKING
//! → EVALUATING → (PLANNING | COMPLETED | EXHAUSTED); any state → CANCELLING → CANCELLED; FAILED.
//!
//! The runner does the work of COLLECTING..BENCHMARKING inside one job; the run state follows the
//! job's phase. Only the evaluator decides outcomes; the model's own claims never change state.

use std::path::{Path, PathBuf};

use aiops_core::evaluator::{self, Outcome};
use aiops_core::patch::{apply_patch, PatchProposal, PathPolicy};
use aiops_core::report::{BaselineReport, TrialReport};
use aiops_core::scenario::{Budget, ScenarioSpec};
use anyhow::Context;
use serde_json::{json, Value};

use crate::agent::{self, AgentError, Limits, RunUsage};
use crate::artifacts::Artifacts;
use crate::db::{stmt, Db, Row, RowExt, Stmt};
use crate::events;
use crate::lease::Lease;
use crate::llm::{Provider, Role};
use crate::roles::{ExperimentPlan, Observation};
use crate::runner_client::{JobState, RunnerClient, Submit};
use crate::tools::ToolContext;

pub const TERMINAL: [&str; 4] = ["COMPLETED", "EXHAUSTED", "FAILED", "CANCELLED"];
/// A runner that cannot be reached for this long loses its job.
const RUNNER_TIMEOUT_MS: u64 = 10 * 60 * 1000;

pub struct App {
    pub db: Db,
    pub artifacts: Artifacts,
    pub runner: RunnerClient,
    pub provider: Provider,
    pub spec: ScenarioSpec,
    pub scenario_hash: String,
    pub repo_root: PathBuf,
    pub tick_lock: tokio::sync::Mutex<()>,
    /// Only the lease holder advances runs (see `lease.rs`).
    pub lease: Lease,
    /// From AIOPS_DEPLOY_STAMP; see /api/v1/info.
    pub deploy_stamp: String,
    /// Ticks received from the Worker's cron (`POST /internal/tick?source=cron`), since start.
    pub cron_ticks: std::sync::atomic::AtomicU64,
    pub last_cron_tick_ms: std::sync::atomic::AtomicU64,
}

#[derive(Debug, Clone)]
pub struct Run {
    pub id: String,
    pub state: String,
    pub version: i64,
    pub goal: String,
    pub cancel_requested: bool,
    pub usage: RunUsage,
    pub budget: Budget,
    pub aa_noise: Option<f64>,
    pub baseline_artifact_id: Option<String>,
    pub observation: Option<Value>,
    /// The scenario the run was created for. Every runner report must match it.
    pub scenario_hash: String,
    /// `meta:<model>` or `replay`, fixed when the run is created.
    pub provider: String,
}

impl Run {
    /// The model this run uses, when it was created with one (Meta runs only).
    pub fn model(&self) -> Option<&str> {
        self.provider.strip_prefix("meta:")
    }
}

impl Run {
    fn from_row(row: &Row) -> anyhow::Result<Self> {
        Ok(Self {
            id: row.str("id"),
            state: row.str("state"),
            version: row.i64("state_version"),
            goal: row.str("goal"),
            cancel_requested: row.i64("cancel_requested") != 0,
            usage: serde_json::from_value(row.json("usage")).unwrap_or_default(),
            budget: serde_json::from_value(row.json("budget")).context("run budget is invalid")?,
            aa_noise: row.f64_opt("aa_noise"),
            baseline_artifact_id: row.opt_str("baseline_artifact_id"),
            observation: Some(row.json("observation")).filter(|v| !v.is_null()),
            scenario_hash: row.str("scenario_hash"),
            provider: row.str("provider"),
        })
    }
}

/// Maps a runner job phase to the run state it represents.
pub fn state_for_phase(kind: &str, phase: &str) -> Option<&'static str> {
    let measuring = phase.starts_with("warmup") || phase.starts_with("measure") || phase == "stop";
    match kind {
        "baseline" if measuring => Some("BASELINING"),
        "baseline" if phase != "queued" => Some("COLLECTING"),
        "trial" => match phase {
            "prepare" | "database" | "build-baseline" => Some("PREPARING"),
            "build" => Some("BUILDING"),
            "start" | "correctness" => Some("CHECKING"),
            _ if measuring => Some("BENCHMARKING"),
            _ => None,
        },
        _ => None,
    }
}

/// Forward order of the non-terminal states.
const ORDER: [&str; 9] = [
    "QUEUED",
    "COLLECTING",
    "BASELINING",
    "PLANNING",
    "PREPARING",
    "BUILDING",
    "CHECKING",
    "BENCHMARKING",
    "EVALUATING",
];

fn state_rank(state: &str) -> usize {
    ORDER.iter().position(|s| *s == state).unwrap_or(usize::MAX)
}

fn is_unique_violation(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains("UNIQUE constraint failed")
}

impl App {
    // -----------------------------------------------------------------------------------------
    // Entry points used by the API

    /// Creates a run. With a fingerprint, an active run for the same symptom is reused instead:
    /// returns `Err(existing_run_id)` in that case.
    pub async fn create_run(
        &self,
        goal: &str,
        trigger: Value,
        fingerprint: Option<&str>,
        model: Option<&str>,
    ) -> anyhow::Result<Result<String, String>> {
        let id = uuid::Uuid::now_v7().to_string();
        let now = crate::now_ms();
        let provider = self.provider.label_for(model);
        let insert = self
            .db
            .execute(
                "INSERT INTO runs (id, fingerprint, scenario_id, scenario_hash, goal, trigger, state, provider, budget, usage, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'QUEUED', ?7, ?8, ?9, ?10, ?10)",
                vec![
                    json!(id),
                    json!(fingerprint),
                    json!(self.spec.id),
                    json!(self.scenario_hash),
                    json!(goal),
                    json!(trigger.to_string()),
                    json!(provider),
                    json!(serde_json::to_string(&self.spec.budget)?),
                    json!(serde_json::to_string(&RunUsage::default())?),
                    json!(now),
                ],
            )
            .await;
        if let Err(err) = insert {
            if fingerprint.is_some() && is_unique_violation(&err) {
                let existing = self
                    .db
                    .query_one(
                        "SELECT id FROM runs WHERE fingerprint = ?1 AND state NOT IN ('COMPLETED','EXHAUSTED','FAILED','CANCELLED')",
                        vec![json!(fingerprint)],
                    )
                    .await?
                    .map(|r| r.str("id"))
                    .unwrap_or_default();
                events::append(
                    &self.db,
                    &existing,
                    "alert_merged",
                    json!({ "trigger": trigger }),
                )
                .await?;
                return Ok(Err(existing));
            }
            return Err(err);
        }
        events::append(
            &self.db,
            &id,
            "run_created",
            json!({ "goal": goal, "trigger": trigger, "provider": provider, "scenario": self.spec.id, "scenario_hash": self.scenario_hash, "budget": self.spec.budget, "contract": self.spec.contract }),
        )
        .await?;
        Ok(Ok(id))
    }

    /// Records a cancel request. The tick moves the run to CANCELLING and, once the runner has
    /// confirmed its processes stopped, to CANCELLED. Idempotent.
    pub async fn request_cancel(&self, run_id: &str) -> anyhow::Result<Option<String>> {
        let changed = self
            .db
            .execute(
                "UPDATE runs SET cancel_requested = 1, updated_at = ?2 WHERE id = ?1 AND cancel_requested = 0 \
                 AND state NOT IN ('COMPLETED','EXHAUSTED','FAILED','CANCELLED')",
                vec![json!(run_id), json!(crate::now_ms())],
            )
            .await?;
        if changed == 1 {
            events::append(&self.db, run_id, "cancel_requested", json!({})).await?;
        }
        Ok(self
            .db
            .query_one("SELECT state FROM runs WHERE id = ?1", vec![json!(run_id)])
            .await?
            .map(|r| r.str("state")))
    }

    /// Advances every unfinished run by one step. Overlapping calls return immediately.
    pub async fn tick(&self) -> anyhow::Result<usize> {
        let Ok(_guard) = self.tick_lock.try_lock() else {
            return Ok(0);
        };
        // Another aiopsd instance (for example, the old one during a deploy rollout) may be
        // advancing the same runs. Only the lease holder ticks.
        if !self.lease.renew().await? {
            return Ok(0);
        }
        let rows = self
            .db
            .query(
                "SELECT * FROM runs WHERE state NOT IN ('COMPLETED','EXHAUSTED','FAILED','CANCELLED') ORDER BY created_at",
                vec![],
            )
            .await?;
        let mut advanced = 0;
        for row in &rows {
            let run = Run::from_row(row)?;
            match self.advance(&run).await {
                Ok(()) => advanced += 1,
                Err(err) => {
                    let message = format!("{err:#}");
                    tracing::warn!(run = %run.id, state = %run.state, error = %message, "step failed; will retry");
                    // Record a repeating failure once, not on every tick.
                    let last = self
                        .db
                        .query_one("SELECT kind, payload FROM run_events WHERE run_id = ?1 ORDER BY seq DESC LIMIT 1", vec![json!(run.id)])
                        .await
                        .ok()
                        .flatten();
                    let repeated = last.is_some_and(|e| {
                        e.str("kind") == "step_error"
                            && e.json("payload")["error"] == json!(message)
                    });
                    if !repeated {
                        let _ = events::append(
                            &self.db,
                            &run.id,
                            "step_error",
                            json!({ "state": run.state, "error": message }),
                        )
                        .await;
                    }
                }
            }
        }
        Ok(advanced)
    }

    async fn advance(&self, run: &Run) -> anyhow::Result<()> {
        if run.cancel_requested && run.state != "CANCELLING" {
            self.transition(
                run,
                "CANCELLING",
                &[],
                vec![],
                json!({ "reason": "cancel requested" }),
            )
            .await?;
            return Ok(());
        }
        match run.state.as_str() {
            "QUEUED" => self.start_baseline(run).await,
            "COLLECTING" | "BASELINING" | "PREPARING" | "BUILDING" | "CHECKING"
            | "BENCHMARKING" => self.drive_job(run).await,
            "PLANNING" => self.plan(run).await,
            "EVALUATING" => self.decide_next(run).await,
            "CANCELLING" => self.drive_cancel(run).await,
            other => anyhow::bail!("unknown state {other}"),
        }
    }

    // -----------------------------------------------------------------------------------------
    // State changes

    /// Compare-and-swap state change plus its event (and any extra statements) in one batch.
    async fn transition(
        &self,
        run: &Run,
        to: &str,
        sets: &[(&str, Value)],
        extra: Vec<Stmt>,
        payload: Value,
    ) -> anyhow::Result<bool> {
        let stmts = transition_stmts(run, to, sets, extra, payload, crate::now_ms());
        match self.db.batch(stmts).await {
            Ok(_) => Ok(true),
            Err(err) if is_cas_lost(&err) => {
                tracing::warn!(run = %run.id, from = %run.state, to, "state changed concurrently; transition skipped");
                Ok(false)
            }
            Err(err) => Err(err),
        }
    }

    async fn fail(&self, run: &Run, reason: &str, usage: Option<&RunUsage>) -> anyhow::Result<()> {
        let mut sets = vec![("error", json!(reason)), ("summary", json!(reason))];
        if let Some(u) = usage {
            sets.push(("usage", json!(serde_json::to_string(u)?)));
        }
        self.transition(run, "FAILED", &sets, vec![], json!({ "reason": reason }))
            .await?;
        Ok(())
    }

    async fn exhaust(&self, run: &Run, reason: &str, usage: &RunUsage) -> anyhow::Result<()> {
        let sets = [
            ("summary", json!(reason)),
            ("usage", json!(serde_json::to_string(usage)?)),
        ];
        self.transition(run, "EXHAUSTED", &sets, vec![], json!({ "reason": reason }))
            .await?;
        Ok(())
    }

    async fn save_usage(&self, run: &Run, usage: &RunUsage) -> anyhow::Result<()> {
        self.db
            .execute(
                "UPDATE runs SET usage = ?2, updated_at = ?3 WHERE id = ?1",
                vec![
                    json!(run.id),
                    json!(serde_json::to_string(usage)?),
                    json!(crate::now_ms()),
                ],
            )
            .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Runner jobs

    fn job_insert(run_id: &str, trial_id: Option<&str>, kind: &str, attempt: i64) -> Stmt {
        let now = crate::now_ms();
        stmt(
            "INSERT INTO jobs (id, run_id, trial_id, kind, status, attempt, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?6)",
            vec![json!(uuid::Uuid::now_v7().to_string()), json!(run_id), json!(trial_id), json!(kind), json!(attempt), json!(now)],
        )
    }

    async fn active_job(&self, run_id: &str, statuses: &[&str]) -> anyhow::Result<Option<Row>> {
        let list = statuses
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(",");
        self.db
            .query_one(&format!("SELECT * FROM jobs WHERE run_id = ?1 AND status IN ({list}) ORDER BY created_at DESC LIMIT 1"), vec![json!(run_id)])
            .await
    }

    async fn update_job(&self, job_id: &str, sets: &[(&str, Value)]) -> anyhow::Result<()> {
        let mut params = vec![json!(job_id), json!(crate::now_ms())];
        let mut assignments = String::from("updated_at = ?2");
        for (column, value) in sets {
            params.push(value.clone());
            assignments.push_str(&format!(", {column} = ?{}", params.len()));
        }
        self.db
            .execute(
                &format!("UPDATE jobs SET {assignments} WHERE id = ?1"),
                params,
            )
            .await?;
        Ok(())
    }

    async fn start_baseline(&self, run: &Run) -> anyhow::Result<()> {
        let job = Self::job_insert(&run.id, None, "baseline", 1);
        self.transition(
            run,
            "COLLECTING",
            &[],
            vec![job],
            json!({ "job": "baseline" }),
        )
        .await?;
        Ok(())
    }

    async fn drive_job(&self, run: &Run) -> anyhow::Result<()> {
        let Some(job) = self.active_job(&run.id, &["pending", "submitted"]).await? else {
            return self
                .fail(
                    run,
                    &format!("no runner job is active in state {}", run.state),
                    None,
                )
                .await;
        };
        let job_id = job.str("id");
        let kind = job.str("kind");
        let now = crate::now_ms();

        if job.str("status") == "pending" {
            let patch = match job.opt_str("trial_id") {
                Some(trial_id) => {
                    let trial = self
                        .db
                        .query_one(
                            "SELECT patch FROM trials WHERE id = ?1",
                            vec![json!(trial_id)],
                        )
                        .await?
                        .context("trial missing")?;
                    Some(serde_json::from_value::<PatchProposal>(
                        trial.json("patch"),
                    )?)
                }
                None => None,
            };
            match self.runner.submit(&kind, patch.as_ref()).await {
                Ok(Submit::Accepted {
                    job_id: runner_job_id,
                    boot_id,
                }) => {
                    self.update_job(
                        &job_id,
                        &[
                            ("status", json!("submitted")),
                            ("runner_job_id", json!(runner_job_id)),
                            ("runner_boot_id", json!(boot_id)),
                            ("last_seen_at", json!(now)),
                            ("error", Value::Null),
                        ],
                    )
                    .await?;
                    events::append(&self.db, &run.id, "job_submitted", json!({ "kind": kind, "runner_job_id": runner_job_id, "runner_boot_id": boot_id, "attempt": job.i64("attempt") })).await?;
                }
                Ok(Submit::Busy) => {
                    self.note_job_problem(run, &job, "runner busy with another job")
                        .await?
                }
                Err(err) => {
                    if now.saturating_sub(job.i64("created_at") as u64) > RUNNER_TIMEOUT_MS {
                        return self
                            .fail(
                                run,
                                &format!("runner unreachable for 10 minutes: {err:#}"),
                                None,
                            )
                            .await;
                    }
                    self.note_job_problem(run, &job, &format!("{err:#}"))
                        .await?;
                }
            }
            return Ok(());
        }

        let runner_job_id = job.str("runner_job_id");
        let status = match self.runner.status(&runner_job_id).await {
            Ok(status) => status,
            Err(err) => {
                let last_seen = job.i64("last_seen_at") as u64;
                if now.saturating_sub(last_seen) > RUNNER_TIMEOUT_MS {
                    return self
                        .job_lost(
                            run,
                            &job,
                            &format!("runner unreachable for 10 minutes: {err:#}"),
                        )
                        .await;
                }
                return self.note_job_problem(run, &job, &format!("{err:#}")).await;
            }
        };
        if status.boot_id != job.str("runner_boot_id") || matches!(status.state, JobState::Unknown)
        {
            // The runner restarted: its processes and workspaces for this job are gone, so a
            // retry cannot duplicate work that is still running.
            return self
                .job_lost(
                    run,
                    &job,
                    &format!(
                        "runner restarted (boot {} → {})",
                        job.str("runner_boot_id"),
                        status.boot_id
                    ),
                )
                .await;
        }
        match status.state {
            JobState::Running { phase } => {
                self.update_job(
                    &job_id,
                    &[("runner_phase", json!(phase)), ("last_seen_at", json!(now))],
                )
                .await?;
                self.follow_phases(run, &kind, &status.history).await?;
                Ok(())
            }
            JobState::Cancelling => Ok(()),
            JobState::Cancelled => {
                self.job_lost(run, &job, "runner cancelled the job unexpectedly")
                    .await
            }
            JobState::Done { result } => {
                self.update_job(
                    &job_id,
                    &[("status", json!("done")), ("last_seen_at", json!(now))],
                )
                .await?;
                // Record phases that finished between two polls before closing the job.
                let run = self.follow_phases(run, &kind, &status.history).await?;
                match kind.as_str() {
                    "baseline" => self.finish_baseline(&run, result).await,
                    _ => self.finish_trial(&run, &job, result).await,
                }
            }
            JobState::Unknown => unreachable!("handled above"),
        }
    }

    /// Moves the run through every state the runner's phase history implies, in order, so the
    /// timeline shows phases shorter than the polling interval. Returns the updated run.
    async fn follow_phases(
        &self,
        run: &Run,
        kind: &str,
        history: &[String],
    ) -> anyhow::Result<Run> {
        let mut current = run.clone();
        let mut previous: Option<&str> = None;
        for phase in history {
            let Some(target) = state_for_phase(kind, phase) else {
                continue;
            };
            if previous == Some(target) {
                continue;
            }
            previous = Some(target);
            if state_rank(target) <= state_rank(&current.state) {
                continue;
            }
            if !self
                .transition(
                    &current,
                    target,
                    &[],
                    vec![],
                    json!({ "runner_phase": phase }),
                )
                .await?
            {
                break;
            }
            current.state = target.to_owned();
            current.version += 1;
        }
        Ok(current)
    }

    /// Records a transient problem once (not on every tick).
    async fn note_job_problem(&self, run: &Run, job: &Row, problem: &str) -> anyhow::Result<()> {
        if job.opt_str("error").as_deref() != Some(problem) {
            self.update_job(&job.str("id"), &[("error", json!(problem))])
                .await?;
            events::append(
                &self.db,
                &run.id,
                "job_waiting",
                json!({ "kind": job.str("kind"), "problem": problem }),
            )
            .await?;
        }
        Ok(())
    }

    async fn job_lost(&self, run: &Run, job: &Row, reason: &str) -> anyhow::Result<()> {
        let attempt = job.i64("attempt");
        self.update_job(
            &job.str("id"),
            &[("status", json!("lost")), ("error", json!(reason))],
        )
        .await?;
        if attempt < 2 {
            let retry = Self::job_insert(
                &run.id,
                job.opt_str("trial_id").as_deref(),
                &job.str("kind"),
                attempt + 1,
            );
            self.db.execute(&retry.sql, retry.params).await?;
            events::append(&self.db, &run.id, "job_lost", json!({ "kind": job.str("kind"), "reason": reason, "retrying": true, "attempt": attempt + 1 })).await?;
            return Ok(());
        }
        events::append(
            &self.db,
            &run.id,
            "job_lost",
            json!({ "kind": job.str("kind"), "reason": reason, "retrying": false }),
        )
        .await?;
        match job.opt_str("trial_id") {
            None => {
                self.fail(run, &format!("baseline job lost twice: {reason}"), None)
                    .await
            }
            Some(trial_id) => {
                let evaluation = json!({ "outcome": Outcome::ExecutionError, "reasons": [format!("runner job lost twice: {reason}")], "improvements": [], "median_improvement": null });
                self.db
                    .execute(
                        "UPDATE trials SET status = 'DONE', outcome = 'EXECUTION_ERROR', evaluation = ?2, updated_at = ?3 WHERE id = ?1",
                        vec![json!(trial_id), json!(evaluation.to_string()), json!(crate::now_ms())],
                    )
                    .await?;
                self.transition(
                    run,
                    "EVALUATING",
                    &[],
                    vec![],
                    json!({ "trial_id": trial_id, "outcome": "EXECUTION_ERROR" }),
                )
                .await?;
                Ok(())
            }
        }
    }

    async fn finish_baseline(&self, run: &Run, result: Value) -> anyhow::Result<()> {
        let report: BaselineReport = serde_json::from_value(result.clone())
            .context("runner returned an invalid baseline report")?;
        let artifact_id = self
            .artifacts
            .put_json(&run.id, "baseline_report", &result)
            .await?;
        if let Some(reason) = scenario_mismatch(run, &report.scenario_hash) {
            return self.fail(run, &reason, None).await;
        }
        if let Some(err) = &report.execution_error {
            return self
                .fail(run, &format!("baseline failed: {err}"), None)
                .await;
        }
        let aa = evaluator::aa_noise(&report);
        let correctness = report
            .correctness
            .as_ref()
            .map(|c| json!({ "passed": c.passed, "total": c.total }));
        events::append(
            &self.db,
            &run.id,
            "baseline_measured",
            json!({
                "artifact_id": artifact_id, "aa_noise": aa,
                "aa_pairs": report.aa_pairs.iter().map(|p| json!({ "a_p95_ms": p.baseline.p95_ms, "b_p95_ms": p.candidate.p95_ms, "dropped": p.baseline.dropped + p.candidate.dropped })).collect::<Vec<_>>(),
                "logs": report.logs, "correctness": correctness, "env": report.env, "timings_ms": report.timings_ms,
            }),
        )
        .await?;
        if let Some(noise) = aa {
            if noise > self.spec.contract.max_noise {
                let reason = format!("A/A noise {:.1}% exceeds {:.1}%: the environment is too noisy to judge changes", noise * 100.0, self.spec.contract.max_noise * 100.0);
                return self.fail(run, &reason, None).await;
            }
        }
        self.transition(
            run,
            "PLANNING",
            &[
                ("aa_noise", json!(aa)),
                ("baseline_artifact_id", json!(artifact_id)),
            ],
            vec![],
            json!({ "aa_noise": aa }),
        )
        .await?;
        Ok(())
    }

    async fn finish_trial(&self, run: &Run, job: &Row, result: Value) -> anyhow::Result<()> {
        let trial_id = job.opt_str("trial_id").context("trial job without trial")?;
        let report: TrialReport = serde_json::from_value(result.clone())
            .context("runner returned an invalid trial report")?;
        let artifact_id = self
            .artifacts
            .put_json(&run.id, "trial_report", &result)
            .await?;
        if let Some(reason) = scenario_mismatch(run, &report.scenario_hash) {
            return self.fail(run, &reason, None).await;
        }
        let evaluation = evaluator::evaluate(&self.spec.contract, &report, run.aa_noise);
        let outcome = serde_json::to_value(evaluation.outcome)?;
        let trial = self
            .db
            .query_one(
                "SELECT seq, hypothesis, patch_hash FROM trials WHERE id = ?1",
                vec![json!(trial_id)],
            )
            .await?
            .context("trial missing")?;
        let now = crate::now_ms();

        let mut extra = vec![stmt(
            "UPDATE trials SET status = 'DONE', outcome = ?2, evaluation = ?3, report_artifact_id = ?4, updated_at = ?5 WHERE id = ?1",
            vec![json!(trial_id), outcome.clone(), json!(serde_json::to_string(&evaluation)?), json!(artifact_id), json!(now)],
        )];
        if evaluation.outcome.is_verdict() {
            let conditions = json!({ "scenario_hash": report.scenario_hash, "env": report.env, "load": self.spec.load, "contract": self.spec.contract, "aa_noise": run.aa_noise });
            extra.push(stmt(
                "INSERT INTO experiment_cases (id, scenario_id, run_id, trial_id, hypothesis, patch_hash, outcome, median_improvement, conditions, validation, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'experiment_validated', ?10)",
                vec![
                    json!(uuid::Uuid::now_v7().to_string()), json!(self.spec.id), json!(run.id), json!(trial_id),
                    json!(trial.str("hypothesis")), json!(trial.str("patch_hash")), outcome.clone(),
                    json!(evaluation.median_improvement), json!(conditions.to_string()), json!(now),
                ],
            ));
        }
        let correctness = report
            .correctness
            .as_ref()
            .map(|c| json!({ "passed": c.passed, "total": c.total, "failed": c.failed_names() }));
        let payload = json!({
            "trial_id": trial_id, "trial": trial.i64("seq"), "outcome": outcome, "reasons": evaluation.reasons,
            "improvements": evaluation.improvements, "median_improvement": evaluation.median_improvement,
            "correctness": correctness,
            "pairs": report.pairs.iter().map(|p| json!({ "order": p.order, "baseline_p95_ms": p.baseline.p95_ms, "candidate_p95_ms": p.candidate.p95_ms })).collect::<Vec<_>>(),
            "sql_per_request": { "baseline": report.baseline_logs.as_ref().map(|l| l.avg_sql_count), "candidate": report.candidate_logs.as_ref().map(|l| l.avg_sql_count) },
            "build_ms": report.build.as_ref().map(|b| b.duration_ms),
            "artifact_id": artifact_id,
        });
        self.transition(run, "EVALUATING", &[], extra, payload)
            .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Planning (model roles)

    async fn baseline_report(&self, run: &Run) -> anyhow::Result<BaselineReport> {
        let id = run
            .baseline_artifact_id
            .as_deref()
            .context("run has no baseline report")?;
        let value = self
            .artifacts
            .get_json(id)
            .await?
            .context("baseline report artifact missing")?;
        Ok(serde_json::from_value(value)?)
    }

    fn limits(run: &Run) -> Limits {
        Limits {
            max_model_turns: run.budget.max_model_turns,
            max_tool_calls: run.budget.max_tool_calls,
        }
    }

    async fn plan(&self, run: &Run) -> anyhow::Result<()> {
        let baseline = self.baseline_report(run).await?;
        let ctx = ToolContext {
            db: &self.db,
            run_id: &run.id,
            spec: &self.spec,
            repo_root: &self.repo_root,
            baseline: &baseline,
            aa_noise: run.aa_noise,
        };
        let mut usage = run.usage.clone();
        let limits = Self::limits(run);

        let Some(observation) = run.observation.clone() else {
            events::append(
                &self.db,
                &run.id,
                "agent_started",
                json!({ "role": "observer" }),
            )
            .await?;
            let result = agent::run_role::<Observation>(
                &self.provider,
                run.model(),
                Role::Observer,
                0,
                self.observer_message(run),
                &ctx,
                &mut usage,
                &limits,
            )
            .await;
            return match result {
                Ok(obs) => {
                    let value = serde_json::to_value(&obs)?;
                    self.db
                        .execute(
                            "UPDATE runs SET observation = ?2, usage = ?3, updated_at = ?4 WHERE id = ?1",
                            vec![json!(run.id), json!(value.to_string()), json!(serde_json::to_string(&usage)?), json!(crate::now_ms())],
                        )
                        .await?;
                    events::append(&self.db, &run.id, "observation", value).await
                }
                Err(AgentError::Budget(m)) => {
                    self.exhaust(run, &format!("observer stopped: {m}"), &usage)
                        .await
                }
                Err(e) => {
                    self.fail(run, &format!("observer failed: {e}"), Some(&usage))
                        .await
                }
            };
        };

        let tried = self
            .db
            .query_one(
                "SELECT COUNT(*) AS n FROM trials WHERE run_id = ?1",
                vec![json!(run.id)],
            )
            .await?
            .map(|r| r.i64("n"))
            .unwrap_or(0) as u32;
        if usage.candidates >= run.budget.max_candidates {
            return self
                .exhaust(
                    run,
                    &format!("no verified change after {} candidate(s)", usage.candidates),
                    &usage,
                )
                .await;
        }
        events::append(
            &self.db,
            &run.id,
            "agent_started",
            json!({ "role": "experimenter", "round": tried }),
        )
        .await?;
        let result = agent::run_role::<ExperimentPlan>(
            &self.provider,
            run.model(),
            Role::Experimenter,
            tried,
            self.experimenter_message(run, &observation, tried),
            &ctx,
            &mut usage,
            &limits,
        )
        .await;
        let plan = match result {
            Ok(plan) => plan,
            Err(AgentError::Budget(m)) => {
                return self
                    .exhaust(run, &format!("experimenter stopped: {m}"), &usage)
                    .await
            }
            Err(e) => {
                return self
                    .fail(run, &format!("experimenter failed: {e}"), Some(&usage))
                    .await
            }
        };
        let plan_view = json!({ "hypothesis": plan.hypothesis, "evidence": plan.evidence, "expected_effect": plan.expected_effect, "falsified_if": plan.falsified_if, "action": plan.action });

        let Some(patch) = plan.patch.clone().filter(|_| plan.action == "experiment") else {
            let reason = plan
                .stop_reason
                .clone()
                .unwrap_or_else(|| "the experimenter stopped".into());
            events::append(&self.db, &run.id, "plan", plan_view).await?;
            return self
                .exhaust(run, &format!("experimenter stopped: {reason}"), &usage)
                .await;
        };
        usage.candidates += 1;
        let patch_hash = patch.content_hash();

        if self
            .db
            .query_one(
                "SELECT id FROM trials WHERE run_id = ?1 AND patch_hash = ?2",
                vec![json!(run.id), json!(patch_hash)],
            )
            .await?
            .is_some()
        {
            events::append(
                &self.db,
                &run.id,
                "duplicate_patch",
                json!({ "patch_hash": patch_hash, "summary": patch.summary }),
            )
            .await?;
            return self.save_usage(run, &usage).await;
        }

        let trial_id = uuid::Uuid::now_v7().to_string();
        let seq = tried as i64 + 1;
        let now = crate::now_ms();
        let insert_trial = |status: &str, outcome: Option<&str>, evaluation: Option<Value>| {
            stmt(
                "INSERT INTO trials (id, run_id, seq, hypothesis, plan, patch, patch_hash, status, outcome, evaluation, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                vec![
                    json!(trial_id), json!(run.id), json!(seq), json!(plan.hypothesis), json!(plan_view.to_string()),
                    json!(serde_json::to_string(&patch).unwrap_or_default()), json!(patch_hash), json!(status),
                    json!(outcome), json!(evaluation.map(|e| e.to_string())), json!(now),
                ],
            )
        };

        // Check the patch against the policy and the current source before using the runner.
        if let Err(problem) = self.preflight(&patch) {
            let evaluation = json!({ "outcome": "POLICY_REJECTED", "reasons": [format!("patch refused: {problem}")], "improvements": [], "median_improvement": null });
            let s = insert_trial("DONE", Some("POLICY_REJECTED"), Some(evaluation));
            self.db
                .batch(vec![
                    s,
                    stmt(
                        "UPDATE runs SET usage = ?2, updated_at = ?3 WHERE id = ?1",
                        vec![
                            json!(run.id),
                            json!(serde_json::to_string(&usage)?),
                            json!(now),
                        ],
                    ),
                ])
                .await?;
            events::append(&self.db, &run.id, "patch_refused", json!({ "trial_id": trial_id, "trial": seq, "problem": problem, "plan": plan_view })).await?;
            return Ok(());
        }

        let job = Self::job_insert(&run.id, Some(&trial_id), "trial", 1);
        let payload = json!({
            "trial_id": trial_id, "trial": seq, "plan": plan_view, "patch_summary": patch.summary, "patch_hash": patch_hash,
            "files": patch.edits.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
        });
        self.transition(
            run,
            "PREPARING",
            &[("usage", json!(serde_json::to_string(&usage)?))],
            vec![insert_trial("RUNNING", None, None), job],
            payload,
        )
        .await?;
        Ok(())
    }

    /// Applies the patch to a scratch copy of the source: path policy, protected files and
    /// exact single matches are all checked before anything is built.
    fn preflight(&self, patch: &PatchProposal) -> Result<(), String> {
        let scratch =
            std::env::temp_dir().join(format!("aiopsd-preflight-{}", uuid::Uuid::new_v4()));
        let result = (|| {
            copy_tree(&self.repo_root.join(&self.spec.target.source_dir), &scratch)
                .map_err(|e| e.to_string())?;
            let policy = PathPolicy::new(
                self.spec.target.allowed_paths.clone(),
                self.spec.target.denied_paths.clone(),
            );
            apply_patch(&scratch, patch, &policy, self.spec.target.max_edits)
                .map(|_| ())
                .map_err(|e| e.to_string())
        })();
        let _ = std::fs::remove_dir_all(&scratch);
        result
    }

    async fn decide_next(&self, run: &Run) -> anyhow::Result<()> {
        let last = self
            .db
            .query_one(
                "SELECT * FROM trials WHERE run_id = ?1 ORDER BY seq DESC LIMIT 1",
                vec![json!(run.id)],
            )
            .await?
            .context("EVALUATING without a trial")?;
        let outcome = last.str("outcome");
        let evaluation = last.json("evaluation");
        if outcome == "VERIFIED" {
            let median = evaluation["median_improvement"].as_f64().unwrap_or(0.0);
            let summary = format!("Trial {} verified: median p95 improvement {:.1}% with all golden cases passing. {}", last.i64("seq"), median * 100.0, last.str("hypothesis"));
            self.transition(
                run,
                "COMPLETED",
                &[("summary", json!(summary))],
                vec![],
                json!({ "trial_id": last.str("id"), "outcome": outcome }),
            )
            .await?;
            return Ok(());
        }
        // A noisy measurement says nothing about the change itself: measure the same patch once
        // more before planning anything new. Rejected patches are never re-run.
        if outcome == "INCONCLUSIVE" {
            let trial_id = last.str("id");
            let measured = self
                .db
                .query_one(
                    "SELECT COUNT(*) AS n FROM jobs WHERE trial_id = ?1 AND status = 'done'",
                    vec![json!(trial_id)],
                )
                .await?
                .map(|r| r.i64("n"))
                .unwrap_or(0);
            if measured < 2 {
                let reopen = stmt(
                    "UPDATE trials SET status = 'RUNNING', updated_at = ?2 WHERE id = ?1",
                    vec![json!(trial_id), json!(crate::now_ms())],
                );
                let job = Self::job_insert(&run.id, Some(&trial_id), "trial", 1);
                self.transition(run, "PREPARING", &[], vec![reopen, job], json!({ "trial_id": trial_id, "remeasure": true, "reasons": evaluation["reasons"] }))
                    .await?;
                return Ok(());
            }
        }
        if run.usage.candidates >= run.budget.max_candidates {
            return self
                .exhaust(
                    run,
                    &format!(
                        "no verified change after {} candidate(s); last outcome {outcome}",
                        run.usage.candidates
                    ),
                    &run.usage,
                )
                .await;
        }
        if run.usage.model_turns >= run.budget.max_model_turns {
            return self
                .exhaust(run, "model turn budget spent", &run.usage)
                .await;
        }
        self.transition(
            run,
            "PLANNING",
            &[],
            vec![],
            json!({ "after": outcome, "reasons": evaluation["reasons"] }),
        )
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Cancellation

    async fn drive_cancel(&self, run: &Run) -> anyhow::Result<()> {
        let Some(job) = self
            .active_job(&run.id, &["pending", "submitted", "cancelling"])
            .await?
        else {
            return self.finish_cancel(run, "no runner job was active").await;
        };
        match job.str("status").as_str() {
            "pending" => {
                self.update_job(&job.str("id"), &[("status", json!("cancelled"))])
                    .await?;
                self.finish_cancel(run, "the runner job had not started")
                    .await
            }
            "submitted" => {
                self.runner.cancel(&job.str("runner_job_id")).await?;
                self.update_job(&job.str("id"), &[("status", json!("cancelling"))])
                    .await?;
                events::append(
                    &self.db,
                    &run.id,
                    "cancel_sent",
                    json!({ "runner_job_id": job.str("runner_job_id") }),
                )
                .await
            }
            _ => {
                let status = self.runner.status(&job.str("runner_job_id")).await?;
                let restarted = status.boot_id != job.str("runner_boot_id");
                match status.state {
                    JobState::Cancelled | JobState::Done { .. } | JobState::Unknown => {
                        self.update_job(&job.str("id"), &[("status", json!("cancelled"))])
                            .await?;
                        let how = if restarted {
                            "the runner restarted, so its processes are gone"
                        } else {
                            "the runner confirmed its processes stopped"
                        };
                        self.finish_cancel(run, how).await
                    }
                    _ if restarted => {
                        self.update_job(&job.str("id"), &[("status", json!("cancelled"))])
                            .await?;
                        self.finish_cancel(run, "the runner restarted, so its processes are gone")
                            .await
                    }
                    _ => Ok(()), // still stopping
                }
            }
        }
    }

    async fn finish_cancel(&self, run: &Run, how: &str) -> anyhow::Result<()> {
        let evaluation = json!({ "outcome": "EXECUTION_ERROR", "reasons": ["cancelled"], "improvements": [], "median_improvement": null });
        let close_trial = stmt(
            "UPDATE trials SET status = 'DONE', outcome = 'EXECUTION_ERROR', evaluation = ?2, updated_at = ?3 WHERE run_id = ?1 AND status = 'RUNNING'",
            vec![json!(run.id), json!(evaluation.to_string()), json!(crate::now_ms())],
        );
        let summary = format!("Cancelled: {how}.");
        self.transition(
            run,
            "CANCELLED",
            &[("summary", json!(summary))],
            vec![close_trial],
            json!({ "cleanup": how }),
        )
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Messages for the model roles

    fn observer_message(&self, run: &Run) -> String {
        let c = &self.spec.contract;
        format!(
            "Scenario: {} — {}\nGoal: {}\nLoad: {} at a fixed {} requests per second.\n\
             Acceptance contract (fixed; nobody in this run can change it): a change is accepted only if every golden response stays identical \
             and the median p95 improvement over {} paired runs is at least {:.0}%, with every pair improving.\n\
             Inspect the baseline evidence and the code, then answer.",
            self.spec.id,
            self.spec.description,
            run.goal,
            self.spec.load.endpoint.as_deref().unwrap_or("GET /users/{user_id}/orders?limit=20"),
            self.spec.load.rate,
            c.pairs,
            c.min_improvement * 100.0
        )
    }

    fn experimenter_message(&self, run: &Run, observation: &Value, tried: u32) -> String {
        let c = &self.spec.contract;
        format!(
            "Goal: {}\nObservation from the observation role (JSON):\n{}\n\n\
             Editable paths (relative to the service root): {:?}. Protected paths: {:?}. At most {} edits.\n\
             Acceptance contract: identical golden responses, median p95 improvement ≥ {:.0}% over {} paired runs, every pair improving, no new errors.\n\
             Experiments already tried in this run: {tried} (candidate budget {}). Read them with read_trial_result before proposing another.",
            run.goal,
            serde_json::to_string_pretty(observation).unwrap_or_default(),
            self.spec.target.allowed_paths,
            self.spec.target.denied_paths,
            self.spec.target.max_edits,
            c.min_improvement * 100.0,
            c.pairs,
            run.budget.max_candidates,
        )
    }
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if entry.file_name() == "target" {
            continue;
        }
        let kind = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// A runner report measured against a different scenario than the run was created for (for
/// example, a deploy changed the golden cases while the run was in progress). Its results cannot
/// be attributed to the run's scenario, so the run stops instead of recording them.
fn scenario_mismatch(run: &Run, reported: &str) -> Option<String> {
    (reported != run.scenario_hash).then(|| {
        format!(
            "scenario changed during the run: the runner measured scenario {} but the run was created for {}; start a new run",
            &reported[..reported.len().min(12)],
            &run.scenario_hash[..run.scenario_hash.len().min(12)]
        )
    })
}

/// The statements of one state transition, run as a single batch (one transaction):
/// the compare-and-set UPDATE, the guard that aborts the batch when the UPDATE matched nothing
/// (`migrations/0003_cas_guard.sql`), the caller's follow-up statements, and the state event.
fn transition_stmts(
    run: &Run,
    to: &str,
    sets: &[(&str, Value)],
    extra: Vec<Stmt>,
    payload: Value,
    now: u64,
) -> Vec<Stmt> {
    let mut params = vec![json!(to), json!(now)];
    let mut assignments =
        String::from("state = ?1, state_version = state_version + 1, updated_at = ?2");
    for (column, value) in sets {
        params.push(value.clone());
        assignments.push_str(&format!(", {column} = ?{}", params.len()));
    }
    if TERMINAL.contains(&to) {
        params.push(json!(now));
        assignments.push_str(&format!(", finished_at = ?{}", params.len()));
    }
    params.push(json!(run.id));
    let id_at = params.len();
    params.push(json!(run.version));
    let version_at = params.len();
    let update = stmt(
        format!(
            "UPDATE runs SET {assignments} WHERE id = ?{id_at} AND state_version = ?{version_at}"
        ),
        params,
    );
    // changes() is the row count of the UPDATE just above; 0 violates the cas_lost CHECK and
    // rolls the whole batch back.
    let guard = stmt(
        "INSERT OR REPLACE INTO cas_guard (id, won) VALUES (1, changes())",
        vec![],
    );

    let mut body = payload;
    if let Value::Object(map) = &mut body {
        map.insert("from".into(), json!(run.state));
        map.insert("to".into(), json!(to));
    }
    let event = stmt(
        "INSERT INTO run_events (run_id, seq, ts, kind, payload) \
         SELECT ?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM run_events WHERE run_id = ?1), ?2, 'state', ?3",
        vec![json!(run.id), json!(now), json!(body.to_string())],
    );
    let mut stmts = vec![update, guard];
    stmts.extend(extra);
    stmts.push(event);
    stmts
}

/// Whether a batch failed because its compare-and-set lost (see `transition_stmts`).
fn is_cas_lost(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains("cas_lost")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_phases_map_to_run_states() {
        assert_eq!(state_for_phase("baseline", "build"), Some("COLLECTING"));
        assert_eq!(
            state_for_phase("baseline", "measure 2/3"),
            Some("BASELINING")
        );
        assert_eq!(state_for_phase("baseline", "queued"), None);
        assert_eq!(state_for_phase("trial", "prepare"), Some("PREPARING"));
        assert_eq!(state_for_phase("trial", "build"), Some("BUILDING"));
        assert_eq!(state_for_phase("trial", "correctness"), Some("CHECKING"));
        assert_eq!(state_for_phase("trial", "warmup"), Some("BENCHMARKING"));
        assert_eq!(
            state_for_phase("trial", "measure 1/3"),
            Some("BENCHMARKING")
        );
    }

    async fn db_with_run() -> Db {
        let migrations = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let db = Db::connect("sqlite::memory:", &migrations).await.unwrap();
        db.execute(
            "INSERT INTO runs (id, scenario_id, scenario_hash, goal, trigger, state, provider, budget, created_at, updated_at) \
             VALUES ('r1', 's', 'h', 'g', '{}', 'PREPARING', 'replay', '{}', 0, 0)",
            vec![],
        )
        .await
        .unwrap();
        db
    }

    fn run_at(version: i64) -> Run {
        Run {
            id: "r1".into(),
            state: "PREPARING".into(),
            version,
            goal: "g".into(),
            cancel_requested: false,
            usage: RunUsage::default(),
            budget: serde_json::from_value(json!({ "max_candidates": 1, "max_model_turns": 1, "max_tool_calls": 1, "max_run_minutes": 1, "build_timeout_s": 1 })).unwrap(),
            aa_noise: None,
            baseline_artifact_id: None,
            observation: None,
            scenario_hash: "s1".into(),
            provider: "replay".into(),
        }
    }

    #[test]
    fn reports_from_another_scenario_are_refused() {
        assert_eq!(scenario_mismatch(&run_at(0), "s1"), None);
        let reason = scenario_mismatch(&run_at(0), "s2").unwrap();
        assert!(
            reason.contains("scenario changed during the run"),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn a_lost_transition_writes_nothing() {
        let db = db_with_run().await;
        let job = |id: &str| {
            stmt(
            "INSERT INTO jobs (id, run_id, kind, status, attempt, created_at, updated_at) VALUES (?1, 'r1', 'trial', 'pending', 1, 0, 0)",
            vec![json!(id)],
        )
        };
        // Two orchestrators read the run at version 0 and make the same transition.
        let first = transition_stmts(&run_at(0), "BUILDING", &[], vec![job("j1")], json!({}), 1);
        db.batch(first).await.unwrap();
        let second = transition_stmts(&run_at(0), "BUILDING", &[], vec![job("j2")], json!({}), 2);
        let err = db.batch(second).await.unwrap_err();
        assert!(is_cas_lost(&err), "unexpected error: {err:#}");

        let count = |sql: &'static str| {
            let db = db.clone();
            async move { db.query_one(sql, vec![]).await.unwrap().unwrap().i64("n") }
        };
        assert_eq!(
            count("SELECT COUNT(*) AS n FROM jobs").await,
            1,
            "the loser's job was rolled back"
        );
        assert_eq!(
            count("SELECT COUNT(*) AS n FROM run_events").await,
            1,
            "one state event"
        );
        assert_eq!(count("SELECT state_version AS n FROM runs").await, 1);
    }
}
