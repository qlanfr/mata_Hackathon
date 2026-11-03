# CLI and MCP server

`aiops` is one Rust binary with two faces:
- a **command line** for people, scripts and CI;
- an **MCP server** (`aiops mcp`) for AI clients such as Claude Code or Cursor, so an agent can start an experiment, wait for the verdict and read the evidence with tool calls.

Both talk to the same API as the web UI: the deployed Cloudflare Worker by default, or a local aiopsd. Nothing about the verdict changes: the Meta model proposes, the Rust harness builds, checks and measures, and only the Rust evaluator decides.

## Install

```bash
cargo install --path apps/aiops-cli
```

Or download the `aiops-linux-x86_64` artifact from the **CLI and MCP** workflow.

## Configure

| Variable | Default | Needed for |
| --- | --- | --- |
| `AIOPS_URL` | `https://aiops-edge.yh9365.workers.dev` | everything |
| `AIOPS_ADMIN_TOKEN` | none | starting, alerting and cancelling (reading is public) |

The token is read from the environment only. It is never a command-line value, a tool argument or part of any output, so an AI client never sees it.

## Command line

```bash
aiops info
```

```bash
aiops run --wait
```

```bash
aiops status <run_id>
```

```bash
aiops trial <trial_id> --diff
```

```bash
aiops alert orders-api:p95-high --service orders-api --summary "p95 above 20 ms"
```

| Command | What it does |
| --- | --- |
| `info` | The deployment: scenario, model provider, version |
| `runs [--limit N]` | Recent runs, newest first |
| `run [--goal G] [--model M] [--wait]` | Starts an experiment; with `--wait`, prints each state change and then the verdict |
| `status <run_id>` | Every trial with its verdict and reason, and the measurements of the verified one |
| `trial <trial_id> [--diff]` | One trial: reasons, per-pair p95 change, p95 and SQL per request before and after, correctness checks, the patch |
| `alert <fingerprint>` | Reports a symptom from monitoring. Starts a run, or joins the active run with the same fingerprint |
| `cancel <run_id>` | Stops a run; it becomes CANCELLED once the runner confirms its processes are gone |
| `mcp` | Serves the MCP tools over stdio |

Every command takes `--json`. `run --wait` exits with **0** when a change was VERIFIED, **2** when the run finished without one, and **3** when the wait timed out, so a CI job or a cron entry can act on the result.

## MCP server

`aiops mcp` speaks MCP over stdio (JSON-RPC 2.0, protocol versions 2024-11-05 to 2025-06-18) and offers these tools:

| Tool | Read-only | What it does |
| --- | --- | --- |
| `service_info` | yes | The deployment the tools talk to |
| `list_runs` | yes | Recent runs |
| `start_experiment` | no | Starts a run and returns its ID at once |
| `report_alert` | no | Starts or joins a run for a monitoring symptom |
| `wait_for_run` | yes | Waits up to `timeout_s` (default 300) and returns the state, trials and verified measurements; sends progress notifications when the client asks for them |
| `get_run` | yes | The same summary without waiting |
| `get_trial` | yes | Verdict, reasons, measurements and the diff |
| `cancel_run` | no | Stops a run |

A run takes about 7 to 15 minutes, longer than most clients wait for one tool call. So `start_experiment` returns immediately, and the agent calls `wait_for_run` again while `finished` is false.

### Claude Code

```bash
claude mcp add aiops -e AIOPS_ADMIN_TOKEN="$AIOPS_ADMIN_TOKEN" -- aiops mcp
```

Then ask, for example: "The order list API is slow. Start an experiment and tell me what was verified."

### Cursor and other clients

```json
{
  "mcpServers": {
    "aiops": {
      "command": "aiops",
      "args": ["mcp"],
      "env": { "AIOPS_ADMIN_TOKEN": "<your admin token>" }
    }
  }
}
```

## What this makes possible

- **From a terminal or a script:** `aiops run --wait` runs the whole loop and returns a verdict as an exit code.
- **From monitoring:** a webhook or cron job calls `aiops alert <fingerprint>`; repeated alerts for the same symptom join one run instead of starting many.
- **From an AI agent:** the agent asks for an experiment and reads the measured result. It cannot pass its own patch, change the contract or see the token.

## Checked

- Locally, against aiopsd and the runner with the Replay provider: `aiops run --wait` ran a full experiment to VERIFIED (p95 22.5 to 2.8 ms, SQL per request 81.8 to 3.0, golden 14/14, invariants 209/209, sample 200/200, exit code 0). An MCP client then did the same with `start_experiment` and `wait_for_run`, receiving a progress notification for each state change. `alert` started a run, a second alert with the same fingerprint joined it, and `cancel` ended it as CANCELLED.
- Against the deployment: the **CLI and MCP** workflow runs the same MCP client there.

## Not built yet

- **One tenant.** One deployment, one scenario, one admin token. Several teams or services would need accounts, a scenario per service and isolation between them.
- **No repository connection.** The service's source is part of the deployment. Pointing the tool at a team's own repository, and opening a pull request with the verified patch, are not built.
