//! aiops-runner: builds a patched copy of the target service, checks it against golden cases,
//! and measures it against the baseline under a fixed load. It reports raw results; the
//! verdict comes from `aiops_core::evaluator`.

mod api;
mod builder;
mod config;
mod correctness;
mod exploits;
mod invariants;
mod jobs;
mod load;
mod logs;
mod pg;
mod proc;
mod service;
mod sqlstats;
mod workspace;

use std::path::PathBuf;
use std::sync::Arc;

use aiops_core::evaluator;
use aiops_core::patch::PatchProposal;
use aiops_core::report::BaselineReport;
use anyhow::Context;
use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;

use crate::config::{Opts, Runtime};
use crate::jobs::JobControl;

#[derive(Parser)]
#[command(name = "aiops-runner", version, about = "Isolated experiment runner")]
struct Cli {
    #[command(flatten)]
    opts: Opts,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the job API (used behind the Cloudflare Worker).
    Serve {
        #[arg(long, env = "AIOPS_LISTEN", default_value = "0.0.0.0:8080")]
        listen: String,
    },
    /// Build the baseline, check it, and measure A/A noise. Prints a BaselineReport.
    Baseline {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Measure one patch against the baseline. Prints the TrialReport and its evaluation.
    Trial {
        #[arg(long)]
        patch: PathBuf,
        /// A BaselineReport whose A/A noise feeds the evaluation.
        #[arg(long)]
        baseline_report: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Correctness checks only (golden, invariants, sample) for one patch, without load. Prints
    /// the TrialReport. Every check runs even after one fails, so mutation testing can see which
    /// checks catch a change.
    Check {
        #[arg(long)]
        patch: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Regenerate the golden cases from the baseline.
    Golden,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let rt = Runtime::init(cli.opts).await?;
    tracing::info!(boot_id = rt.boot_id(), scenario = %rt.spec.id, hash = %&rt.scenario_hash[..12], "runner ready");

    let one_shot = !matches!(cli.command, Command::Serve { .. });
    let result = run(&rt, cli.command).await;
    if one_shot {
        // Leave nothing running: the next CLI run may use another work directory.
        crate::pg::Postgres::new(&rt).stop().await;
    }
    result
}

async fn run(rt: &Runtime, command: Command) -> anyhow::Result<()> {
    let rt = rt.clone();
    match command {
        Command::Serve { listen } => serve(rt, &listen).await,
        Command::Baseline { out } => {
            let ctl = interruptible();
            let report = jobs::run_baseline(&rt, &ctl).await;
            emit(&serde_json::to_value(&report)?, out)
        }
        Command::Trial {
            patch,
            baseline_report,
            out,
        } => {
            let patch: PatchProposal = serde_json::from_str(
                &std::fs::read_to_string(&patch)
                    .with_context(|| format!("cannot read {}", patch.display()))?,
            )
            .context("invalid patch file")?;
            let aa_noise = match baseline_report {
                Some(path) => {
                    let base: BaselineReport =
                        serde_json::from_str(&std::fs::read_to_string(&path)?)?;
                    evaluator::aa_noise(&base)
                }
                None => None,
            };
            let ctl = interruptible();
            let report = jobs::run_trial(&rt, &patch, &ctl).await;
            let evaluation = evaluator::evaluate(&rt.spec.contract, &report, aa_noise);
            emit(
                &serde_json::json!({ "evaluation": evaluation, "report": report }),
                out,
            )
        }
        Command::Check { patch, out } => {
            let patch: PatchProposal = serde_json::from_str(
                &std::fs::read_to_string(&patch)
                    .with_context(|| format!("cannot read {}", patch.display()))?,
            )
            .context("invalid patch file")?;
            let ctl = interruptible();
            let report =
                jobs::run_trial_mode(&rt, &patch, &ctl, jobs::TrialMode::CorrectnessOnly).await;
            emit(&serde_json::to_value(&report)?, out)
        }
        Command::Golden => {
            let written = jobs::generate_golden(&rt, &JobControl::default()).await?;
            for line in written {
                println!("{line}");
            }
            Ok(())
        }
    }
}

/// A job control that is cancelled by Ctrl-C, so local runs clean up their processes too.
fn interruptible() -> JobControl {
    let ctl = JobControl::default();
    let cancel = ctl.cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel.cancel();
        }
    });
    ctl
}

fn emit(value: &serde_json::Value, out: Option<PathBuf>) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    match out {
        Some(path) => std::fs::write(path, text + "\n")?,
        None => println!("{text}"),
    }
    Ok(())
}

async fn serve(rt: Runtime, listen: &str) -> anyhow::Result<()> {
    let shutdown = CancellationToken::new();
    let app = api::router(Arc::new(rt), shutdown.clone());
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(%listen, "serving job API");
    let signal = shutdown.clone();
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        // Cancel running jobs first; they stop their processes before the server exits.
        signal.cancel();
    });
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.cancelled().await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        })
        .await?;
    Ok(())
}
