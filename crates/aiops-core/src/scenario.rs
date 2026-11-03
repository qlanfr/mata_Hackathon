use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// A versioned experiment definition. Everything here is fixed before a run starts and is
/// outside the paths a model may edit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScenarioSpec {
    pub id: String,
    pub version: u32,
    pub description: String,
    pub target: TargetSpec,
    pub database: DatabaseSpec,
    pub service: ServiceSpec,
    pub correctness: CorrectnessSpec,
    pub load: LoadSpec,
    pub contract: Contract,
    pub budget: Budget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TargetSpec {
    /// Source of the service under test, relative to the repository root.
    pub source_dir: String,
    /// Name of the binary cargo produces.
    pub binary: String,
    /// Glob patterns (relative to `source_dir`) a patch may touch.
    pub allowed_paths: Vec<String>,
    /// Glob patterns that are never editable, even if an allowed pattern matches.
    pub denied_paths: Vec<String>,
    /// Upper bound on edits per patch.
    pub max_edits: usize,
    /// Extra `cargo build` arguments, e.g. `["--features", "postgresql"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cargo_args: Vec<String>,
    /// Extra environment for every build of this service, baseline and candidate alike: a pinned
    /// toolchain (`RUSTUP_TOOLCHAIN`) or a profile override.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub build_env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DatabaseSpec {
    pub name: String,
    pub schema: String,
    pub seed: String,
    /// Count SQL statements per request with `pg_stat_statements`, for services that do not log
    /// their own per-request SQL counts. Also records the most frequent statements as evidence.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub statement_stats: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    pub pool_size: u32,
    pub startup_timeout_s: u64,
    /// Readiness path; `/healthz` when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_path: Option<String>,
    /// Extra environment for the service. Values may use `{port}`, `{database_url}`,
    /// `{pool_size}` and `{data_dir}` (a directory shared by the services of one job).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Log in once per job; the token is sent as a bearer token with every check and load request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<LoginSpec>,
}

/// A form POST that returns a JSON object with a bearer token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LoginSpec {
    pub path: String,
    pub form: BTreeMap<String, String>,
    /// Field of the JSON answer that holds the token.
    pub token_field: String,
}

impl ServiceSpec {
    pub fn health_path(&self) -> &str {
        self.health_path.as_deref().unwrap_or("/healthz")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CorrectnessSpec {
    /// Directory of golden `*.json` cases, relative to the repository root.
    pub cases_dir: String,
    /// Requests used to (re)generate the golden cases from the baseline.
    pub requests: Vec<CaseRequest>,
    /// Named response invariant sets (built into the runner), checked on every successful golden
    /// and sampled response of both the baseline and the candidate. They state the API's own
    /// rules, so unlike the golden cases they also catch bugs the original code already had.
    #[serde(default)]
    pub invariants: Vec<String>,
    /// More requests, sent to both the baseline and the candidate and compared with each other.
    #[serde(default)]
    pub sample: Option<SampleSpec>,
    /// Compare JSON arrays as multisets: for APIs whose clients do not depend on element order.
    /// A patch may then reorder elements, but may not add, drop or change any.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unordered_arrays: bool,
}

/// Requests for the baseline-versus-candidate comparison: the first `requests` users of the load
/// script's user sequence (`1 + (i * 7919) % load.users`), each with `path`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SampleSpec {
    pub requests: u32,
    /// Request path with a `{user}` placeholder.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CaseRequest {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LoadSpec {
    /// k6 script, relative to the repository root.
    pub script: String,
    /// Fixed arrival rate in requests per second.
    pub rate: u32,
    pub warmup_s: u32,
    pub duration_s: u32,
    pub pre_allocated_vus: u32,
    pub max_vus: u32,
    pub request_timeout_ms: u32,
    /// Number of distinct users the load script cycles through.
    pub users: u32,
    /// The measured request, as shown to the model (for example `GET /api/sync`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

/// The pass/fail rules. Fixed before the run; models cannot change them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    /// Number of baseline/candidate measurement pairs.
    pub pairs: u32,
    /// Required median p95 improvement, as a fraction (0.20 = 20%).
    pub min_improvement: f64,
    /// Maximum tolerated baseline noise (A/A difference or baseline spread), as a fraction.
    pub max_noise: f64,
    /// Allowed increase in error rate of the candidate over the baseline, as a fraction of offered requests.
    pub max_error_rate_increase: f64,
    /// If the baseline itself fails more than this fraction of offered requests, the environment is unhealthy.
    pub max_baseline_error_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub max_candidates: u32,
    pub max_model_turns: u32,
    pub max_tool_calls: u32,
    pub max_run_minutes: u32,
    pub build_timeout_s: u64,
}

impl ScenarioSpec {
    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        let spec: ScenarioSpec = toml::from_str(text).context("invalid scenario.toml")?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read scenario {}", path.display()))?;
        Self::from_toml(&text)
    }

    /// Stable hash of the whole spec. Recorded with every run so results can be traced to
    /// the exact contract they were judged against.
    pub fn content_hash(&self) -> String {
        crate::sha256_hex(serde_json::to_vec(self).expect("spec serializes"))
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.contract.pairs >= 1,
            "contract.pairs must be at least 1"
        );
        anyhow::ensure!(
            self.contract.min_improvement > 0.0 && self.contract.min_improvement < 1.0,
            "contract.min_improvement must be between 0 and 1"
        );
        anyhow::ensure!(self.load.rate > 0, "load.rate must be positive");
        anyhow::ensure!(self.load.duration_s > 0, "load.duration_s must be positive");
        anyhow::ensure!(
            !self.target.allowed_paths.is_empty(),
            "target.allowed_paths must not be empty"
        );
        anyhow::ensure!(
            self.target.max_edits >= 1,
            "target.max_edits must be at least 1"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Options added later must not change the hash of a scenario that does not use them, or every
    /// recorded run of it would look like it was judged against another contract.
    #[test]
    fn unused_options_leave_the_scenario_hash_alone() {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scenarios/n-plus-one/scenario.toml"
        ))
        .unwrap();
        let spec = ScenarioSpec::from_toml(&text).unwrap();
        let json = serde_json::to_string(&spec).unwrap();
        for key in [
            "cargo_args",
            "build_env",
            "statement_stats",
            "health_path",
            "\"env\"",
            "login",
            "endpoint",
        ] {
            assert!(!json.contains(key), "{key} appears in {json}");
        }
        assert_eq!(spec.service.health_path(), "/healthz");
    }

    #[test]
    fn every_scenario_in_the_repository_parses() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scenarios");
        let mut seen = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path().join("scenario.toml");
            if path.exists() {
                ScenarioSpec::load(&path).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
                seen += 1;
            }
        }
        assert!(seen >= 2, "found {seen} scenarios");
    }
}
