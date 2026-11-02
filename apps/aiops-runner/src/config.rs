use std::path::{Path, PathBuf};

use aiops_core::report::EnvInfo;
use aiops_core::scenario::ScenarioSpec;
use anyhow::Context;

#[derive(clap::Args, Debug, Clone)]
pub struct Opts {
    /// Repository root holding `fixtures/` and `scenarios/`.
    #[arg(long, env = "AIOPS_REPO_ROOT", default_value = ".")]
    pub repo_root: PathBuf,
    /// Scenario file, relative to the repository root.
    #[arg(
        long,
        env = "AIOPS_SCENARIO",
        default_value = "scenarios/n-plus-one/scenario.toml"
    )]
    pub scenario: PathBuf,
    /// Scratch space for workspaces, builds and the embedded database.
    #[arg(long, env = "AIOPS_WORK_DIR", default_value = ".work")]
    pub work_dir: PathBuf,
    /// Shared cargo target directory, so dependencies compile once.
    #[arg(long, env = "AIOPS_TARGET_DIR")]
    pub target_dir: Option<PathBuf>,
    /// Directory with `initdb`, `pg_ctl` and `postgres`. Detected with `pg_config` if omitted.
    #[arg(long, env = "AIOPS_PG_BIN")]
    pub pg_bin: Option<PathBuf>,
    #[arg(long, env = "AIOPS_PG_PORT", default_value_t = 55432)]
    pub pg_port: u16,
    /// Run the database server as this user (needed when the runner itself runs as root).
    #[arg(long, env = "AIOPS_PG_USER")]
    pub pg_user: Option<String>,
    #[arg(long, env = "AIOPS_K6", default_value = "k6")]
    pub k6: PathBuf,
    /// Build without network access (dependencies must already be in the cargo home).
    #[arg(long, env = "AIOPS_OFFLINE")]
    pub offline: bool,
    /// Keep trial workspaces after the trial ends (for debugging).
    #[arg(long, env = "AIOPS_KEEP_WORKSPACES")]
    pub keep_workspaces: bool,
    /// Override the scenario's arrival rate. Changes the scenario hash.
    #[arg(long, env = "AIOPS_RATE")]
    pub rate: Option<u32>,
    /// Override the measurement duration per run. Changes the scenario hash.
    #[arg(long, env = "AIOPS_DURATION_S")]
    pub duration_s: Option<u32>,
}

/// Resolved configuration shared by every job.
#[derive(Debug, Clone)]
pub struct Runtime {
    pub repo_root: PathBuf,
    pub work_dir: PathBuf,
    pub target_dir: PathBuf,
    pub pg_bin: PathBuf,
    pub pg_port: u16,
    pub pg_user: Option<String>,
    pub k6: PathBuf,
    pub offline: bool,
    pub keep_workspaces: bool,
    pub spec: ScenarioSpec,
    pub scenario_hash: String,
    pub env: EnvInfo,
}

impl Runtime {
    pub async fn init(opts: Opts) -> anyhow::Result<Self> {
        let repo_root = opts
            .repo_root
            .canonicalize()
            .with_context(|| format!("repository root {} not found", opts.repo_root.display()))?;
        let mut spec = ScenarioSpec::load(&repo_root.join(&opts.scenario))?;
        for set in &spec.correctness.invariants {
            anyhow::ensure!(
                crate::invariants::KNOWN.contains(&set.as_str()),
                "unknown invariant set `{set}` (known: {:?})",
                crate::invariants::KNOWN
            );
        }
        if let Some(rate) = opts.rate {
            spec.load.rate = rate;
        }
        if let Some(duration) = opts.duration_s {
            spec.load.duration_s = duration;
        }
        let scenario_hash = spec.content_hash();

        std::fs::create_dir_all(&opts.work_dir)?;
        let work_dir = opts.work_dir.canonicalize()?;
        let target_dir = opts
            .target_dir
            .clone()
            .unwrap_or_else(|| work_dir.join("target"));
        let pg_bin = match opts.pg_bin.clone() {
            Some(dir) => dir,
            None => PathBuf::from(command_output("pg_config", &["--bindir"]).await.context(
                "cannot find PostgreSQL server binaries; pass --pg-bin or install pg_config",
            )?),
        };

        let env = EnvInfo {
            runner_boot_id: uuid::Uuid::new_v4().to_string(),
            hostname: std::fs::read_to_string("/proc/sys/kernel/hostname")
                .map(|s| s.trim().to_owned())
                .unwrap_or_default(),
            cpus: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            rustc: command_output("rustc", &["--version"])
                .await
                .unwrap_or_default(),
            postgres: command_output(
                pg_bin.join("postgres").to_str().unwrap_or("postgres"),
                &["--version"],
            )
            .await
            .unwrap_or_default(),
            k6: command_output(opts.k6.to_str().unwrap_or("k6"), &["version"])
                .await
                .unwrap_or_default(),
        };

        Ok(Self {
            repo_root,
            work_dir,
            target_dir,
            pg_bin,
            pg_port: opts.pg_port,
            pg_user: opts.pg_user,
            k6: opts.k6,
            offline: opts.offline,
            keep_workspaces: opts.keep_workspaces,
            spec,
            scenario_hash,
            env,
        })
    }

    /// A path from the scenario, resolved against the repository root.
    pub fn repo_path(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.repo_root.join(rel)
    }

    pub fn boot_id(&self) -> &str {
        &self.env.runner_boot_id
    }
}

async fn command_output(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await?;
    anyhow::ensure!(
        out.status.success(),
        "`{program}` exited with {}",
        out.status
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
