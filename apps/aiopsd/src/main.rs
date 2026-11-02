//! aiopsd: control plane of the autonomous experiment engineer. Runs the state machine, calls the
//! model roles, submits jobs to the isolated runner, and serves the API and event stream.

mod agent;
mod api;
mod artifacts;
mod db;
mod events;
mod llm;
mod orchestrator;
mod roles;
mod runner_client;
mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aiops_core::scenario::ScenarioSpec;
use clap::Parser;

use crate::artifacts::{Artifacts, BlobStore};
use crate::db::Db;
use crate::llm::{MetaProvider, Provider, ReplayProvider};
use crate::orchestrator::App;
use crate::runner_client::RunnerClient;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Parser, Debug)]
#[command(name = "aiopsd", version, about = "AIOps experiment control plane")]
struct Opts {
    #[arg(long, env = "AIOPS_LISTEN", default_value = "0.0.0.0:8080")]
    listen: String,
    #[arg(long, env = "AIOPS_REPO_ROOT", default_value = ".")]
    repo_root: PathBuf,
    #[arg(
        long,
        env = "AIOPS_SCENARIO",
        default_value = "scenarios/n-plus-one/scenario.toml"
    )]
    scenario: PathBuf,
    /// `d1:http://d1.internal` in production, `sqlite:<file>` locally.
    #[arg(long, env = "AIOPS_DB", default_value = "sqlite:.work/control.db")]
    db: String,
    /// `r2:http://r2.internal` in production, `fs:<dir>` locally.
    #[arg(long, env = "AIOPS_ARTIFACTS", default_value = "fs:.work/artifacts")]
    artifacts: String,
    #[arg(
        long,
        env = "AIOPS_RUNNER_URL",
        default_value = "http://127.0.0.1:18080"
    )]
    runner_url: String,
    /// `meta`, `replay`, or `auto` (meta when META_API_KEY is set).
    #[arg(long, env = "AIOPS_PROVIDER", default_value = "auto")]
    provider: String,
    #[arg(long, env = "META_BASE_URL", default_value = "https://api.meta.ai/v1")]
    meta_base_url: String,
    #[arg(long, env = "META_MODEL", default_value = "")]
    meta_model: String,
    #[arg(
        long,
        env = "AIOPS_REPLAY_SCRIPT",
        default_value = "prompts/replay/n-plus-one.json"
    )]
    replay_script: PathBuf,
    #[arg(long, env = "AIOPS_MIGRATIONS", default_value = "migrations")]
    migrations: PathBuf,
    /// Interval of the in-process tick. The Worker's cron tick covers sleeps and restarts.
    #[arg(long, env = "AIOPS_TICK_MS", default_value_t = 3000)]
    tick_ms: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let opts = Opts::parse();
    let repo_root = opts.repo_root.canonicalize()?;
    let spec = ScenarioSpec::load(&repo_root.join(&opts.scenario))?;
    let scenario_hash = spec.content_hash();

    let db = Db::connect(&opts.db, &repo_root.join(&opts.migrations)).await?;
    let artifacts = Artifacts::new(db.clone(), BlobStore::from_url(&opts.artifacts)?);
    let api_key = std::env::var("META_API_KEY").unwrap_or_default();
    // `auto` uses Meta only when both the key and the model are configured, so a half-finished
    // configuration falls back to replay instead of crashing the control plane.
    let meta_ready = !api_key.is_empty() && !opts.meta_model.is_empty();
    let provider = match (opts.provider.as_str(), meta_ready) {
        ("meta", _) | ("auto", true) => Provider::Meta(MetaProvider::new(
            &opts.meta_base_url,
            &api_key,
            &opts.meta_model,
        )?),
        ("replay", _) | ("auto", false) => {
            Provider::Replay(ReplayProvider::load(&repo_root.join(&opts.replay_script))?)
        }
        (other, _) => anyhow::bail!("unknown provider `{other}`"),
    };
    tracing::info!(db = db.backend(), provider = %provider.label(), scenario = %spec.id, "aiopsd starting");

    let app = Arc::new(App {
        db,
        artifacts,
        runner: RunnerClient::new(&opts.runner_url)?,
        provider,
        spec,
        scenario_hash,
        repo_root,
        tick_lock: tokio::sync::Mutex::new(()),
    });

    let ticker = app.clone();
    let interval = Duration::from_millis(opts.tick_ms.max(500));
    tokio::spawn(async move {
        loop {
            if let Err(e) = ticker.tick().await {
                tracing::warn!(error = %format!("{e:#}"), "tick failed");
            }
            tokio::time::sleep(interval).await;
        }
    });

    let listener = tokio::net::TcpListener::bind(&opts.listen).await?;
    tracing::info!(listen = %opts.listen, "serving API");
    axum::serve(listener, api::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
