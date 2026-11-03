//! Raw results the runner returns. The runner measures; it never judges.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One fixed-rate load run against one service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoadStats {
    /// `baseline`, `candidate`, or `baseline-b` for the second side of an A/A pair.
    pub label: String,
    pub rate: u32,
    pub duration_s: u32,
    /// Iterations the plan called for (`rate * duration_s`).
    pub planned: u64,
    /// Iterations actually scheduled: `max(planned, iterations + dropped)`. k6 can schedule one
    /// more than planned (an arrival at t=0 and at t=duration). The denominator for every ratio.
    pub offered: u64,
    /// Iterations k6 finished.
    pub iterations: u64,
    /// Iterations k6 could not start because it ran out of VUs.
    pub dropped: u64,
    pub ok: u64,
    pub bad_status: u64,
    pub timeouts: u64,
    pub net_errors: u64,
    /// Latency of successful responses only.
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl LoadStats {
    pub fn errors(&self) -> u64 {
        self.bad_status + self.timeouts + self.net_errors
    }

    pub fn error_rate(&self) -> f64 {
        ratio(self.errors(), self.offered)
    }

    /// Successful responses over offered requests. Unfinished requests never count as success.
    pub fn success_ratio(&self) -> f64 {
        ratio(self.ok, self.offered)
    }

    /// Every planned iteration either finished or was explicitly dropped. Iterations cut off
    /// at the end of the run (still in flight after the grace period) count against this.
    pub fn drained(&self) -> bool {
        self.iterations + self.dropped >= self.planned
    }
}

fn ratio(n: u64, d: u64) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 / d as f64
    }
}

/// One baseline run and one candidate run, back to back. `order` records which ran first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pair {
    pub index: u32,
    /// `BC` (baseline first) or `CB` (candidate first).
    pub order: String,
    pub baseline: LoadStats,
    pub candidate: LoadStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildReport {
    pub ok: bool,
    pub cached: bool,
    pub duration_ms: u64,
    pub log_sha256: String,
    /// Last few kilobytes of the build log.
    pub log_tail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CaseResult {
    pub name: String,
    pub passed: bool,
    /// First difference found, for failed cases.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CorrectnessReport {
    pub total: usize,
    pub passed: usize,
    pub cases: Vec<CaseResult>,
}

impl CorrectnessReport {
    pub fn all_passed(&self) -> bool {
        self.total > 0 && self.passed == self.total
    }

    pub fn failed_names(&self) -> Vec<String> {
        self.cases
            .iter()
            .filter(|c| !c.passed)
            .map(|c| c.name.clone())
            .collect()
    }
}

/// Aggregated per-request logs written by the service under test.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ServiceLogSummary {
    pub requests: u64,
    pub avg_sql_count: f64,
    pub max_sql_count: u64,
    pub avg_db_ms: f64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EnvInfo {
    pub runner_boot_id: String,
    pub hostname: String,
    pub cpus: usize,
    pub rustc: String,
    pub postgres: String,
    pub k6: String,
}

/// Result of measuring one patch against the baseline.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TrialReport {
    pub trial_id: String,
    pub scenario_id: String,
    pub scenario_hash: String,
    pub patch_hash: String,
    /// Set when the patch was refused before anything ran.
    pub patch_error: Option<String>,
    /// Unified diff of the candidate against the baseline source.
    pub diff: String,
    pub build: Option<BuildReport>,
    pub correctness: Option<CorrectnessReport>,
    /// Golden check against the baseline itself; a failure here means the environment is wrong.
    pub baseline_correctness: Option<CorrectnessReport>,
    /// Response invariants on the candidate's golden and sampled responses.
    #[serde(default)]
    pub invariants: Option<CorrectnessReport>,
    /// The same invariants on the baseline. A failure is a bug the original code already has.
    #[serde(default)]
    pub baseline_invariants: Option<CorrectnessReport>,
    /// Sampled requests answered by both services, compared with each other. Only mismatches are
    /// listed in `cases`; `total` and `passed` count every sampled request.
    #[serde(default)]
    pub sample: Option<CorrectnessReport>,
    pub pairs: Vec<Pair>,
    /// Vulnerability-fix scenarios: every attack sent to the baseline and to the candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityReport>,
    /// The candidate process exited during the trial (crash, OOM kill, ...).
    pub candidate_crashed: Option<String>,
    pub baseline_logs: Option<ServiceLogSummary>,
    pub candidate_logs: Option<ServiceLogSummary>,
    /// Runner-side failure unrelated to the patch's merit (timeouts, tool errors, cancellation).
    pub execution_error: Option<String>,
    pub env: EnvInfo,
    pub timings_ms: BTreeMap<String, u64>,
}

/// Result of the run-level baseline job: A/A noise and the evidence the observer reads.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BaselineReport {
    pub scenario_id: String,
    pub scenario_hash: String,
    pub source_hash: String,
    pub build: Option<BuildReport>,
    pub correctness: Option<CorrectnessReport>,
    /// Response invariants on the baseline's golden and sampled responses.
    #[serde(default)]
    pub invariants: Option<CorrectnessReport>,
    /// Baseline against a second copy of itself.
    pub aa_pairs: Vec<Pair>,
    pub logs: ServiceLogSummary,
    /// Slowest request log records, verbatim.
    pub slow_requests: Vec<serde_json::Value>,
    /// Most frequent SQL statements during one measured baseline run (`pg_stat_statements`),
    /// for scenarios with `database.statement_stats`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statements: Vec<serde_json::Value>,
    /// Vulnerability-fix scenarios: every attack sent to the original service.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exploits: Vec<ExploitResult>,
    pub execution_error: Option<String>,
    pub env: EnvInfo,
    pub timings_ms: BTreeMap<String, u64>,
}

/// One attack request sent to one service.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExploitResult {
    pub name: String,
    pub path: String,
    /// Not shown to the model.
    #[serde(default)]
    pub held_out: bool,
    /// HTTP status, or 0 when no answer came back.
    pub status: u16,
    /// The response body contains the exploit's leak text.
    pub leaked: bool,
    /// The start of the response body.
    pub excerpt: String,
}

impl ExploitResult {
    /// The attack worked: a successful answer carrying data the caller must not get.
    pub fn exploited(&self) -> bool {
        (200..300).contains(&self.status) && self.leaked
    }

    /// No answer, or a server error: the attack is not refused cleanly.
    pub fn errored(&self) -> bool {
        self.status == 0 || self.status >= 500
    }
}

/// The attacks of a vulnerability-fix trial, sent to both services.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SecurityReport {
    pub baseline: Vec<ExploitResult>,
    pub candidate: Vec<ExploitResult>,
}
