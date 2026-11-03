# Running unattended

The deployment can run without anyone watching: it starts a run on a schedule, waits out a noisy machine instead of failing, stays inside a daily budget, says when a run is finished, and turns a verified patch into a pull request.

All of it is configuration of the existing deployment. The Worker's 1-minute cron keeps aiopsd awake and is what starts the scheduled run, so nothing else has to be running.

## Settings

Set as Worker vars in `edge/wrangler.jsonc`; each is also an aiopsd flag and environment variable.

| Var | Deployed value | What it does |
| --- | --- | --- |
| `AIOPS_DAILY_RUN_UTC` | `17:00` | One run per UTC day at this time (02:00 in Korea). Empty turns the schedule off |
| `AIOPS_DAILY_RUN_LIMIT` | `30` | New runs per UTC day. Past it, `POST /api/v1/runs` answers 429; an alert still joins a run already in progress for the same fingerprint |
| `AIOPS_DAILY_TOKEN_LIMIT` | `1500000` | Model tokens (input + output) per UTC day. Past it, a run stops before calling the model and ends EXHAUSTED with that reason. A Meta run uses about 40 to 70 thousand |
| `AIOPS_NOISE_RETRIES` | `3` | How often a measurement that was too noisy to judge is tried again later |
| `AIOPS_NOISE_RETRY_MINUTES` | `20` | Wait before retry *n*: *n* × this (20, 40, 60 minutes) |
| `AIOPS_PUBLIC_URL` | the Worker URL | Links in notifications |

Secret: `NOTIFY_WEBHOOK_URL`. Add it as a repository secret and the deploy workflow passes it to the Worker. It is never shown: `/api/v1/info` only reports whether notifications are on.

`aiops info` prints the active settings:

```text
https://aiops-edge.yh9365.workers.dev
  scenario n-plus-one  provider meta:muse-spark-1.3  version 0.1.0  deploy 1073a692c708
  schedule 17:00 UTC  daily runs 30  daily tokens 1500000  noise retries 3 every 20 min  notifications off
```

## Retrying later on a noisy machine

Shared machines are sometimes too noisy to measure a 20% change. On one afternoon the A/A check on Cloudflare varied 17.8% and then 276%. The harness refuses to judge then; before this change, the run simply failed.

Now two cases wait and measure again:
- the baseline's A/A noise is above the contract's limit;
- a trial is still INCONCLUSIVE after its immediate re-measurement.

The job gets a `not_before` time and the run records an `env_retry` event: the reason, the attempt, and when it will be measured again. The model is not called again for the same patch. After the last retry, the run fails or the trial stays INCONCLUSIVE as before.

## Notifications

When a run reaches COMPLETED, EXHAUSTED, FAILED or CANCELLED, aiopsd posts to the webhook:
- **Slack** incoming webhooks get `{"text": ...}`.
- **Discord** webhooks get `{"content": ...}`.
- **Any other URL** gets the JSON event: run, scenario, state, the verified trial and its median improvement, summary, provider, trigger, link.

```text
[n-plus-one] run 01a1a1c7-…: VERIFIED: p95 improved 88.7%. Trial 1 verified: … https://aiops-edge.yh9365.workers.dev/runs/01a1a1c7-…
```

The outcome is an event on the run: `notified` with the HTTP status, or `notify_failed`. A failing webhook never changes the run.

## Pull requests from verified runs

`aiops apply <trial_id>` puts a VERIFIED patch into a checkout. Before applying anything, it checks:
- the trial is VERIFIED;
- the patch the API returns is the exact one that was measured (same content hash).

It then applies the patch with the harness's own code, so the same path policy, security gate and exactly-once rule hold. Finally it writes a pull request title and a description with the evidence: before and after, the per-pair improvements or the blocked attacks, correctness checks, and links.

The **PR from a verified run** workflow wraps it in four steps:
1. Picks the run. Three triggers:
   - The `run_id` input (`latest` means the newest VERIFIED run).
   - A push to `.github/pr-request`.
   - The nightly schedule (19:47 UTC), which picks the scheduled run of that day.
2. Applies the patch and builds the patched service.
3. Uploads the diff and the description, and shows them in the job summary.
4. Opens a pull request, but only when asked: the `open_pr` input, or the repository variable `AIOPS_AUTO_PR=true` for the nightly run. Everything else is a dry run.

Opening the pull request with the workflow's token needs "Allow GitHub Actions to create and approve pull requests" in the repository's Actions settings. Without it, the branch `aiops/verified-<run>` is still pushed and the job prints a compare link.

## Checked

Locally (aiopsd, runner and PostgreSQL in the development container, Replay provider, SQLite):

| Feature | How | Result |
| --- | --- | --- |
| Daily run limit | limit 1, second `POST /api/v1/runs` | 429 with the reason |
| Daily token limit | limit 1,000 with 2,200 tokens already used today | run EXHAUSTED before any model call: "the daily model budget of 1000 tokens is spent (2200 today, UTC)" |
| Retry on noise | a scenario copy with `max_noise` 0.001, 1 retry after 1 minute | `env_retry` event, measured again a minute later, then FAILED; webhook received the JSON event (HTTP 200) |
| Schedule | daily run at the next minute | a run with trigger `schedule` started on the tick, VERIFIED 87.5%; Slack-format text received; no second run that day |
| `aiops apply` | the VERIFIED trial of a local run, into a copy of the repository | the patch applied and the description was written; applying again was refused ("matched 0 times"); a REJECTED trial was refused |

On the deployment:

| Feature | Where | Result |
| --- | --- | --- |
| Settings deployed | [Deploy edge run 41654525427](https://github.com/qlanfr/mata_Hackathon/actions/runs/41654525427): migration 0004 applied, check run with `muse-spark-1.3` | COMPLETED, VERIFIED 88.3% (A/A 6.7%) |
| Settings live | `aiops info` against the deployment ([PR workflow run 41664635685](https://github.com/qlanfr/mata_Hackathon/actions/runs/41664635685)) | `schedule 17:00 UTC  daily runs 30  daily tokens 1500000  noise retries 3 every 20 min  notifications off` |
| PR workflow, dry run | [run 41664635685](https://github.com/qlanfr/mata_Hackathon/actions/runs/41664635685) | picked the newest VERIFIED run (`01a1a1fe`, the deploy's check run), applied its patch, wrote the description (p95 31.4 → 3.3 ms, SQL 81.8 → 3.0, pairs 89.0 / 89.2 / 89.8%, golden 14/14), built the patched service, uploaded diff and description |

Not yet seen on the deployment:
- a scheduled run;
- a retry after noise;
- a notification: no webhook secret is set;
- an opened pull request: the workflow ran as a dry run.
