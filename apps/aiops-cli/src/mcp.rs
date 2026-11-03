//! MCP server over stdio (JSON-RPC 2.0, one message per line). An AI client such as Claude Code
//! or Cursor calls these tools to start an experiment on the deployed service, wait for the Rust
//! evaluator's verdict and read the evidence. The model on the client side can start and read
//! runs; it cannot change a verdict, and it never sees the admin token, which comes from the
//! server's environment.
//!
//! stdout carries only protocol messages; diagnostics go to stderr.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Stdout};
use tokio::sync::Mutex;

use crate::api::{is_terminal, Api};
use crate::report;

const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const POLL: Duration = Duration::from_secs(10);

type Out = Arc<Mutex<Stdout>>;

pub async fn serve(api: Api) -> anyhow::Result<()> {
    let out: Out = Arc::new(Mutex::new(tokio::io::stdout()));
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    eprintln!("aiops MCP server for {} ready on stdio", api.base());
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                send(
                    &out,
                    &error_response(Value::Null, -32700, &format!("parse error: {e}")),
                )
                .await?;
                continue;
            }
        };
        // Batches were allowed by older protocol versions.
        if let Value::Array(batch) = message {
            let mut answers = Vec::new();
            for m in &batch {
                if let Some(a) = handle(&api, &out, m).await {
                    answers.push(a);
                }
            }
            if !answers.is_empty() {
                send(&out, &Value::Array(answers)).await?;
            }
        } else if let Some(answer) = handle(&api, &out, &message).await {
            send(&out, &answer).await?;
        }
    }
    Ok(())
}

async fn send(out: &Out, message: &Value) -> anyhow::Result<()> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    let mut out = out.lock().await;
    out.write_all(line.as_bytes()).await?;
    out.flush().await?;
    Ok(())
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The answer to one message, or `None` for notifications and stray responses.
async fn handle(api: &Api, out: &Out, message: &Value) -> Option<Value> {
    let method = message["method"].as_str()?;
    let id = message.get("id").cloned();
    let params = &message["params"];
    let result = match method {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => Ok(call_tool(api, out, params).await),
        _ if id.is_none() => return None, // notifications/initialized, notifications/cancelled, ...
        other => Err((-32601, format!("method `{other}` is not supported"))),
    };
    let id = id?;
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => error_response(id, code, &message),
    })
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = if SUPPORTED_VERSIONS.contains(&asked) {
        asked
    } else {
        SUPPORTED_VERSIONS[0]
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "aiops", "title": "AIOps Experiment Engineer", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Performance experiments on a deployed service. A Meta model reads the service's evidence and proposes patches; a Rust harness builds each patch in isolation, checks that every response stays identical and measures p95 latency against the unchanged service; only the Rust evaluator decides VERIFIED, REJECTED or INCONCLUSIVE. Start with start_experiment (or report_alert), then call wait_for_run until finished is true, then read the verified trial with get_trial. A run takes about 7 to 15 minutes. Report verdicts and numbers exactly as returned; do not claim an improvement the evaluator did not verify.",
    })
}

fn tool(name: &str, title: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": schema,
        "annotations": { "readOnlyHint": read_only, "openWorldHint": true },
    })
}

fn tools() -> Vec<Value> {
    let run_id = json!({ "type": "string", "description": "Run ID, as returned by start_experiment or list_runs." });
    vec![
        tool(
            "service_info",
            "Service info",
            "The deployment the tools talk to: scenario, model provider, version and health.",
            json!({ "type": "object", "properties": {} }),
            true,
        ),
        tool(
            "list_runs",
            "List runs",
            "Recent experiment runs, newest first, with state, model and summary.",
            json!({ "type": "object", "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 } } }),
            true,
        ),
        tool(
            "start_experiment",
            "Start an experiment",
            "Start an autonomous performance experiment on the deployed service. The Meta model observes, proposes one patch at a time, and the Rust harness builds, checks and measures each until one is VERIFIED or the budget is spent. Uses model tokens and takes about 7 to 15 minutes. Returns the run ID immediately; follow it with wait_for_run.",
            json!({ "type": "object", "properties": {
                "goal": { "type": "string", "description": "What to improve, in plain words. Defaults to the scenario's goal." },
                "model": { "type": "string", "description": "Meta model ID for this run, e.g. muse-spark-1.3. Defaults to the configured model." }
            } }),
            false,
        ),
        tool(
            "report_alert",
            "Report an alert",
            "Report a production symptom (for example from monitoring). Starts an experiment, or joins the active run if one with the same fingerprint is already running.",
            json!({ "type": "object", "required": ["fingerprint"], "properties": {
                "fingerprint": { "type": "string", "description": "Identity of the symptom, e.g. orders-api:p95-high." },
                "service": { "type": "string" },
                "summary": { "type": "string", "description": "What was observed; becomes the run's goal." }
            } }),
            false,
        ),
        tool(
            "wait_for_run",
            "Wait for a run",
            "Wait until a run finishes or the timeout passes, then return its state, trials and, if a patch was VERIFIED, the before/after measurements. Call again while finished is false.",
            json!({ "type": "object", "required": ["run_id"], "properties": {
                "run_id": run_id,
                "timeout_s": { "type": "integer", "minimum": 10, "maximum": 900, "default": 300 }
            } }),
            true,
        ),
        tool(
            "get_run",
            "Get a run",
            "A run's current state, every trial with its verdict and reason, and the measurements of the verified trial.",
            json!({ "type": "object", "required": ["run_id"], "properties": { "run_id": run_id } }),
            true,
        ),
        tool(
            "get_trial",
            "Get a trial",
            "One trial: the evaluator's verdict and reasons, per-pair p95 improvements, p95 and SQL per request before and after, correctness checks, and the patch as a unified diff.",
            json!({ "type": "object", "required": ["trial_id"], "properties": {
                "trial_id": { "type": "string" },
                "max_diff_chars": { "type": "integer", "minimum": 0, "maximum": 50000, "default": 12000 }
            } }),
            true,
        ),
        tool(
            "cancel_run",
            "Cancel a run",
            "Ask a running experiment to stop. It becomes CANCELLED once the runner confirms its processes are gone.",
            json!({ "type": "object", "required": ["run_id"], "properties": { "run_id": run_id } }),
            false,
        ),
    ]
}

async fn call_tool(api: &Api, out: &Out, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    let args = &params["arguments"];
    let progress = params["_meta"]["progressToken"].clone();
    match run_tool(api, out, name, args, progress).await {
        Ok((text, data)) => json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": data,
            "isError": false,
        }),
        Err(e) => json!({
            "content": [{ "type": "text", "text": format!("{e:#}") }],
            "isError": true,
        }),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("`{key}` is required"))
}

fn opt_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().filter(|s| !s.trim().is_empty())
}

async fn run_tool(
    api: &Api,
    out: &Out,
    name: &str,
    args: &Value,
    progress: Value,
) -> anyhow::Result<(String, Value)> {
    let data = match name {
        "service_info" => api.info().await?,
        "list_runs" => {
            let limit = args["limit"].as_u64().unwrap_or(10).clamp(1, 50) as usize;
            let runs: Vec<Value> = api
                .runs()
                .await?
                .iter()
                .take(limit)
                .map(|r| {
                    json!({
                        "run_id": r["id"], "state": r["state"], "provider": r["provider"],
                        "summary": r["summary"], "created_at_ms": r["created_at"], "url": r["id"].as_str().map(|id| api.run_url(id)),
                    })
                })
                .collect();
            json!({ "runs": runs })
        }
        "start_experiment" => {
            let run_id = api
                .start(opt_arg(args, "goal"), opt_arg(args, "model"))
                .await?;
            json!({ "run_id": run_id, "url": api.run_url(&run_id), "next": "call wait_for_run with this run_id" })
        }
        "report_alert" => {
            let answer = api
                .alert(
                    str_arg(args, "fingerprint")?,
                    opt_arg(args, "service"),
                    opt_arg(args, "summary"),
                )
                .await?;
            let run_id = answer["run_id"].as_str().unwrap_or_default().to_owned();
            json!({ "run_id": run_id, "joined_existing_run": answer["deduplicated"], "url": api.run_url(&run_id), "next": "call wait_for_run with this run_id" })
        }
        "wait_for_run" => {
            let run_id = str_arg(args, "run_id")?;
            let timeout =
                Duration::from_secs(args["timeout_s"].as_u64().unwrap_or(300).clamp(10, 900));
            wait(api, out, run_id, timeout, &progress).await?
        }
        "get_run" => report::run_summary(api, str_arg(args, "run_id")?).await?,
        "get_trial" => {
            let max = args["max_diff_chars"]
                .as_u64()
                .unwrap_or(12_000)
                .min(50_000) as usize;
            report::trial_summary(api, str_arg(args, "trial_id")?, max).await?
        }
        "cancel_run" => {
            let run_id = str_arg(args, "run_id")?;
            let answer = api.cancel(run_id).await?;
            json!({ "run_id": run_id, "state": answer["state"] })
        }
        other => anyhow::bail!("unknown tool `{other}`"),
    };
    let text = if data["trials"].is_array() {
        format!(
            "{}\n\n{}",
            report::run_text(&data),
            serde_json::to_string_pretty(&data)?
        )
    } else {
        serde_json::to_string_pretty(&data)?
    };
    Ok((text, data))
}

/// Polls until the run is finished or `timeout` passes. Sends progress notifications when the
/// client asked for them, so it can tell a long wait from a hung server.
async fn wait(
    api: &Api,
    out: &Out,
    run_id: &str,
    timeout: Duration,
    progress: &Value,
) -> anyhow::Result<Value> {
    let started = Instant::now();
    let mut last = String::new();
    let mut step = 0u64;
    loop {
        let state = api.run(run_id).await?["run"]["state"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if !progress.is_null() && state != last {
            step += 1;
            let _ = send(
                out,
                &json!({ "jsonrpc": "2.0", "method": "notifications/progress",
                         "params": { "progressToken": progress, "progress": step, "message": format!("run {run_id}: {state}") } }),
            )
            .await;
        }
        last = state.clone();
        if is_terminal(&state) || started.elapsed() >= timeout {
            return report::run_summary(api, run_id).await;
        }
        tokio::time::sleep(
            POLL.min(timeout.saturating_sub(started.elapsed()))
                .max(Duration::from_secs(1)),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiates_the_protocol_version() {
        assert_eq!(
            initialize(&json!({ "protocolVersion": "2024-11-05" }))["protocolVersion"],
            "2024-11-05"
        );
        assert_eq!(
            initialize(&json!({ "protocolVersion": "1999-01-01" }))["protocolVersion"],
            SUPPORTED_VERSIONS[0]
        );
    }

    #[test]
    fn every_tool_has_an_object_schema() {
        let tools = tools();
        assert_eq!(tools.len(), 8);
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            assert!(t["description"].as_str().unwrap().len() > 20);
        }
    }

    #[tokio::test]
    async fn notifications_get_no_answer_and_unknown_methods_an_error() {
        let api = Api::new("http://127.0.0.1:9", None).unwrap();
        let out: Out = Arc::new(Mutex::new(tokio::io::stdout()));
        assert!(handle(
            &api,
            &out,
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
        )
        .await
        .is_none());
        let answer = handle(
            &api,
            &out,
            &json!({ "jsonrpc": "2.0", "id": 7, "method": "resources/list" }),
        )
        .await
        .unwrap();
        assert_eq!(answer["error"]["code"], -32601);
        let listed = handle(
            &api,
            &out,
            &json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/list" }),
        )
        .await
        .unwrap();
        assert_eq!(listed["id"], 8);
        assert!(listed["result"]["tools"].as_array().unwrap().len() == 8);
    }
}
