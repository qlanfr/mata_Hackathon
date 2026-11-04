//! aiopsd: control plane of the autonomous experiment engineer. Runs the state machine, calls the
//! model roles, submits jobs to the isolated runner, and serves the API and event stream.

mod agent;
mod api;
mod artifacts;
mod db;
mod events;
mod injection;
mod lease;
mod llm;
mod ops;
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
use crate::lease::Lease;
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
    /// Artifacts up to this many bytes stay inline in the control DB; larger ones go to the blob
    /// store (R2 in production).
    #[arg(long, env = "AIOPS_INLINE_LIMIT", default_value_t = 64 * 1024)]
    inline_limit: usize,
    /// Opaque deploy identifier, reported on /api/v1/info so a deploy can check which
    /// configuration the running instance started with.
    #[arg(long, env = "AIOPS_DEPLOY_STAMP", default_value = "")]
    deploy_stamp: String,
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
    /// Recorded answers for the replay provider; `prompts/replay/<scenario id>.json` if not set.
    #[arg(long, env = "AIOPS_REPLAY_SCRIPT")]
    replay_script: Option<PathBuf>,
    #[arg(long, env = "AIOPS_MIGRATIONS", default_value = "migrations")]
    migrations: PathBuf,
    /// Interval of the in-process tick. The Worker's cron tick covers sleeps and restarts.
    #[arg(long, env = "AIOPS_TICK_MS", default_value_t = 3000)]
    tick_ms: u64,
    /// Check the Meta API contract (tool round trip and JSON-schema answer) with META_API_KEY and
    /// --meta-model, print the result and exit. Nothing else starts.
    #[arg(long)]
    contract_test: bool,
    /// How many times a run may postpone a measurement because the machine is too noisy.
    #[arg(long, env = "AIOPS_NOISE_RETRIES", default_value_t = 3)]
    noise_retries: i64,
    /// Wait before a postponed measurement, times the attempt number.
    #[arg(long, env = "AIOPS_NOISE_RETRY_MINUTES", default_value_t = 20)]
    noise_retry_minutes: u64,
    /// Model tokens (input + output) all runs may use per UTC day; 0 means no limit.
    #[arg(long, env = "AIOPS_DAILY_TOKEN_LIMIT", default_value_t = 0)]
    daily_token_limit: u64,
    /// Runs that may be created per UTC day; 0 means no limit.
    #[arg(long, env = "AIOPS_DAILY_RUN_LIMIT", default_value_t = 0)]
    daily_run_limit: u64,
    /// Start one run every UTC day at HH:MM; empty means no schedule.
    #[arg(long, env = "AIOPS_DAILY_RUN_UTC", default_value = "")]
    daily_run_utc: String,
    /// Webhook told when a run finishes (Slack, Discord or any JSON endpoint).
    #[arg(
        long,
        env = "AIOPS_NOTIFY_URL",
        default_value = "",
        hide_env_values = true
    )]
    notify_url: String,
    /// Public base URL, for links in notifications.
    #[arg(long, env = "AIOPS_PUBLIC_URL", default_value = "")]
    public_url: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let opts = Opts::parse();
    if opts.contract_test {
        let key = std::env::var("META_API_KEY").unwrap_or_default();
        let meta = MetaProvider::new(&opts.meta_base_url, &key, &opts.meta_model)?;
        println!(
            "contract test: {} at {}",
            opts.meta_model, opts.meta_base_url
        );
        return llm::contract_test(&meta).await;
    }
    let repo_root = opts.repo_root.canonicalize()?;
    let spec = ScenarioSpec::load(&repo_root.join(&opts.scenario))?;
    let scenario_hash = spec.content_hash();

    let db = Db::connect(&opts.db, &repo_root.join(&opts.migrations)).await?;
    let db_for_lease = db.clone();
    let artifacts = Artifacts::new(db.clone(), BlobStore::from_url(&opts.artifacts)?)
        .with_inline_limit(opts.inline_limit);
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
            let script = opts
                .replay_script
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("prompts/replay/{}.json", spec.id)));
            Provider::Replay(ReplayProvider::load(&repo_root.join(script))?)
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
        lease: Lease::new(
            db_for_lease,
            "orchestrator",
            uuid::Uuid::new_v4().to_string(),
            LEASE_TTL_MS,
        ),
        deploy_stamp: opts.deploy_stamp.clone(),
        cron_ticks: std::sync::atomic::AtomicU64::new(0),
        last_cron_tick_ms: std::sync::atomic::AtomicU64::new(0),
        ops: ops::Ops::new(
            opts.noise_retries,
            opts.noise_retry_minutes,
            opts.daily_token_limit,
            opts.daily_run_limit,
            &opts.daily_run_utc,
            &opts.notify_url,
            &opts.public_url,
        )?,
    });
    tracing::info!(instance = %app.lease.holder(), "orchestrator instance");

    let ticker = app.clone();
    let interval = Duration::from_millis(opts.tick_ms.max(500));
    tokio::spawn(async move {
        while !ticker.lease.is_stopping() {
            if let Err(e) = ticker.tick().await {
                tracing::warn!(error = %format!("{e:#}"), "tick failed");
            }
            tokio::time::sleep(interval).await;
        }
    });

    // Keeps the lease while a single tick runs longer than the TTL (several model turns).
    let heart = app.clone();
    tokio::spawn(async move {
        while !heart.lease.is_stopping() {
            tokio::time::sleep(Duration::from_millis(LEASE_TTL_MS / 4)).await;
            if let Err(e) = heart.lease.renew().await {
                tracing::warn!(error = %format!("{e:#}"), "lease renewal failed");
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(&opts.listen).await?;
    tracing::info!(listen = %opts.listen, "serving API");
    axum::serve(listener, api::router(app.clone()))
        .with_graceful_shutdown(shutdown(app))
        .await?;
    Ok(())
}

/// Lease lifetime. A holder that dies without releasing blocks other instances this long.
const LEASE_TTL_MS: u64 = 60_000;

/// Waits for SIGTERM (Cloudflare stops containers with it; as PID 1 the process would otherwise
/// ignore it) or Ctrl-C, lets the tick in progress finish, and hands the lease over.
async fn shutdown(app: Arc<App>) {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM received"),
        _ = tokio::signal::ctrl_c() => tracing::info!("interrupt received"),
    }
    let _tick = app.tick_lock.lock().await;
    if let Err(e) = app.lease.stop_and_release().await {
        tracing::warn!(error = %format!("{e:#}"), "could not release the lease");
    }
    tracing::info!("lease released; shutting down");
    // Open SSE streams would hold graceful shutdown open indefinitely.
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        std::process::exit(0);
    });
}
