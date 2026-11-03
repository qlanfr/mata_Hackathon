//! `aiops`: command line and MCP server for the experiment engineer.
//!
//!   aiops info                      the deployment: scenario, model, version
//!   aiops runs                      recent runs
//!   aiops run --wait                start an experiment and wait for the verdict
//!   aiops status <run_id>           a run's trials and verdicts
//!   aiops trial <trial_id> --diff   one trial's measurements and patch
//!   aiops alert <fingerprint>       report a symptom from monitoring
//!   aiops cancel <run_id>           stop a run
//!   aiops mcp                       serve the same operations as MCP tools over stdio
//!
//! The deployment comes from --url or AIOPS_URL. Starting, alerting and cancelling need the admin
//! token in AIOPS_ADMIN_TOKEN; reading does not.

mod api;
mod mcp;
mod report;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use serde_json::Value;

use api::{is_terminal, Api};

#[derive(Parser)]
#[command(
    name = "aiops",
    version,
    about = "Run and read performance experiments: the AI proposes, the Rust harness decides."
)]
struct Cli {
    /// The deployment (Cloudflare Worker URL, or a local aiopsd).
    #[arg(
        long,
        env = "AIOPS_URL",
        default_value = "https://aiops-edge.yh9365.workers.dev",
        global = true
    )]
    url: String,
    /// Admin token for starting, alerting and cancelling. Read from the environment only.
    #[arg(
        long,
        env = "AIOPS_ADMIN_TOKEN",
        hide_env_values = true,
        hide = true,
        global = true
    )]
    token: Option<String>,
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The deployment: scenario, model provider, version and health.
    Info,
    /// Recent runs, newest first.
    Runs {
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Start an experiment. With --wait, follow it to the verdict (exit code 0 if VERIFIED,
    /// 2 if it finished without a verified change, 3 if the wait timed out).
    Run {
        /// What to improve; defaults to the scenario's goal.
        #[arg(long)]
        goal: Option<String>,
        /// Meta model ID for this run.
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        wait: bool,
        /// Longest wait with --wait.
        #[arg(long, default_value_t = 3600)]
        timeout_s: u64,
    },
    /// A run's state, trials and verdicts.
    Status { run_id: String },
    /// One trial: verdict, reasons, measurements and (with --diff) the patch.
    Trial {
        trial_id: String,
        #[arg(long)]
        diff: bool,
    },
    /// Report a symptom from monitoring: starts a run, or joins the active one with the same fingerprint.
    Alert {
        fingerprint: String,
        #[arg(long)]
        service: Option<String>,
        #[arg(long)]
        summary: Option<String>,
    },
    /// Stop a run.
    Cancel { run_id: String },
    /// Serve the operations as MCP tools over stdio, for AI clients such as Claude Code or Cursor.
    Mcp,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn print(json: bool, data: &Value, text: impl FnOnce() -> String) {
    if json {
        println!("{}", serde_json::to_string_pretty(data).unwrap_or_default());
    } else {
        println!("{}", text());
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let api = Api::new(&cli.url, cli.token.clone())?;
    match cli.command {
        Command::Mcp => {
            mcp::serve(api).await?;
        }
        Command::Info => {
            let info = api.info().await?;
            print(cli.json, &info, || {
                let mut text = format!(
                    "{}\n  scenario {}  provider {}  version {}  deploy {}",
                    api.base(),
                    info["scenario"].as_str().unwrap_or("-"),
                    info["provider"].as_str().unwrap_or("-"),
                    info["version"].as_str().unwrap_or("-"),
                    info["deploy_stamp"].as_str().unwrap_or("-"),
                );
                let ops = &info["ops"];
                if ops.is_object() {
                    let limit = |v: &Value| match v.as_u64() {
                        Some(0) | None => "none".to_owned(),
                        Some(n) => n.to_string(),
                    };
                    text.push_str(&format!(
                        "\n  schedule {}  daily runs {}  daily tokens {}  noise retries {} every {} min  notifications {}",
                        ops["daily_run_utc"].as_str().map(|t| format!("{t} UTC")).unwrap_or_else(|| "off".into()),
                        limit(&ops["daily_run_limit"]),
                        limit(&ops["daily_token_limit"]),
                        ops["noise_retries"],
                        ops["noise_retry_minutes"],
                        if ops["notifications"].as_bool() == Some(true) { "on" } else { "off" },
                    ));
                }
                text
            });
        }
        Command::Runs { limit } => {
            let runs: Vec<Value> = api.runs().await?.into_iter().take(limit).collect();
            print(cli.json, &Value::Array(runs.clone()), || {
                runs.iter()
                    .map(|r| {
                        let summary: String = r["summary"]
                            .as_str()
                            .or(r["error"].as_str())
                            .unwrap_or("")
                            .chars()
                            .take(90)
                            .collect();
                        format!(
                            "{}  {:<10} {:<22} {}",
                            r["id"].as_str().unwrap_or("-"),
                            r["state"].as_str().unwrap_or("-"),
                            r["provider"].as_str().unwrap_or("-"),
                            summary
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        Command::Run {
            goal,
            model,
            wait,
            timeout_s,
        } => {
            let run_id = api.start(goal.as_deref(), model.as_deref()).await?;
            eprintln!("started run {run_id}\n  {}", api.run_url(&run_id));
            if !wait {
                print(
                    cli.json,
                    &serde_json::json!({ "run_id": run_id, "url": api.run_url(&run_id) }),
                    || run_id.clone(),
                );
                return Ok(ExitCode::SUCCESS);
            }
            return follow(&api, &run_id, Duration::from_secs(timeout_s), cli.json).await;
        }
        Command::Status { run_id } => {
            let summary = report::run_summary(&api, &run_id).await?;
            print(cli.json, &summary, || report::run_text(&summary));
        }
        Command::Trial { trial_id, diff } => {
            let summary =
                report::trial_summary(&api, &trial_id, if diff { 200_000 } else { 0 }).await?;
            print(cli.json, &summary, || {
                let m = &summary["measurements"];
                let mut text = format!(
                    "trial {}  {}\n  {}\n  reasons: {}\n  p95 {} -> {} ms | SQL per request {} -> {} | golden {}",
                    summary["trial_id"].as_str().unwrap_or("-"),
                    summary["outcome"].as_str().unwrap_or("-"),
                    summary["url"].as_str().unwrap_or("-"),
                    summary["reasons"],
                    m["p95_ms"]["baseline"],
                    m["p95_ms"]["candidate"],
                    m["sql_per_request"]["baseline"],
                    m["sql_per_request"]["candidate"],
                    m["golden"].as_str().unwrap_or("-"),
                );
                if diff {
                    text.push_str("\n\n");
                    text.push_str(summary["diff"].as_str().unwrap_or_default());
                }
                text
            });
        }
        Command::Alert {
            fingerprint,
            service,
            summary,
        } => {
            let answer = api
                .alert(&fingerprint, service.as_deref(), summary.as_deref())
                .await?;
            let run_id = answer["run_id"].as_str().unwrap_or("-");
            print(cli.json, &answer, || {
                format!(
                    "{} run {run_id}\n  {}",
                    if answer["deduplicated"] == true {
                        "joined"
                    } else {
                        "started"
                    },
                    api.run_url(run_id)
                )
            });
        }
        Command::Cancel { run_id } => {
            let answer = api.cancel(&run_id).await?;
            print(cli.json, &answer, || {
                format!(
                    "cancel requested for run {run_id} (now {}); it becomes CANCELLED once the runner confirms its processes stopped",
                    answer["state"].as_str().unwrap_or("-")
                )
            });
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Prints each state change to stderr until the run finishes, then the summary to stdout.
async fn follow(
    api: &Api,
    run_id: &str,
    timeout: Duration,
    json: bool,
) -> anyhow::Result<ExitCode> {
    let started = Instant::now();
    let mut last = String::new();
    loop {
        let state = api.run(run_id).await?["run"]["state"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if state != last {
            eprintln!("[{:>4}s] {state}", started.elapsed().as_secs());
            last = state.clone();
        }
        if is_terminal(&state) {
            break;
        }
        if started.elapsed() >= timeout {
            eprintln!(
                "still {state} after {}s; follow it with `aiops status {run_id}`",
                timeout.as_secs()
            );
            return Ok(ExitCode::from(3));
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    let summary = report::run_summary(api, run_id).await?;
    print(json, &summary, || report::run_text(&summary));
    Ok(if summary["verified"] == true {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}
