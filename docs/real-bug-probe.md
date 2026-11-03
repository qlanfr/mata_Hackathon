# Real bug: Vaultwarden's N+1 on /api/sync

The N+1 scenario in this repository is hand-written. To see whether the loop works on code nobody wrote for it, we took a real, documented performance bug from an open-source Rust project, reproduced it, and ran the whole loop on it: the Meta model, the runner and the evaluator, unchanged except for the opt-in options listed at the end.

## The bug

- **Project:** [Vaultwarden](https://github.com/dani-garcia/vaultwarden), a Rust (Rocket, Diesel) implementation of the Bitwarden server. GPL-3.0; it is cloned at build time and never copied into this repository.
- **Issue:** #1402, "N+1 query problem on api/sync" (2021). The reporter saw syncs of about 50 to 60 seconds with a remote PostgreSQL, and wrote that local databases seemed hardly affected.
- **Fix by the maintainers:** PR #2429, "Improve sync speed and updated dep. versions", merged 2022-05-11 (commit `3ca85028`, parent `3abf173d`).
- **Cause:** `GET /api/sync` loads the user's vault items ("ciphers"), then calls `to_json` for each one. Each call runs its own queries for attachments, favorites, folder and collections.

## Step 1: does it reproduce?

[`scripts/real-bug/vaultwarden-probe.sh`](../scripts/real-bug/vaultwarden-probe.sh) builds the commit before the fix and the fix commit with Rust 1.60, seeds the same vault into each through the public API (20 folders, N items, every 7th a favorite), and calls `GET /api/sync?excludeDomains=true` 10 times, counting SQL statements with `pg_stat_statements`. PostgreSQL runs on the same machine. [Run 41592017741](https://github.com/qlanfr/mata_Hackathon/actions/runs/41592017741):

| Version | Items | SQL statements per sync | Median sync time |
| --- | --- | --- | --- |
| Before the fix | 100 | 411 | 28.1 ms |
| Before the fix | 1000 | 4,011 | 249.6 ms |
| Maintainers' fix | 100 | 17 | 6.8 ms |
| Maintainers' fix | 1000 | 17 | 44.0 ms |

The original runs 4 queries per item plus 11 per sync. The fix runs a constant 17. Query counts are exact; times come from 10 requests each and are indicative.

## Step 2: the loop on it

Scenario: [`scenarios/vaultwarden-sync`](../scenarios/vaultwarden-sync/scenario.toml). The commit before the fix, a vault of 20 folders and 500 items, `GET /api/sync` at 8 requests per second, the same contract as the N+1 scenario (3 alternating pairs, median p95 improvement of at least 20%, every pair better, A/A noise under 15%). Golden answers for `/api/sync`, `/api/ciphers`, `/api/folders` and `/api/accounts/profile` are recorded from the unchanged service; arrays are compared without regard to order, since Bitwarden clients sort items themselves. The model is Muse Spark 1.3.

The **Real bug run** workflow runs everything on a GitHub-hosted runner (4 vCPU) as local processes, not on the Cloudflare deployment. Candidate builds run offline with the dependencies fetched beforehand.

| Run | Candidates and verdicts | SQL per request | p95 | Per-pair p95 improvement |
| --- | --- | --- | --- | --- |
| [41594977758](https://github.com/qlanfr/mata_Hackathon/actions/runs/41594977758) | none: the experimenter spent the run's 30 model turns reading files | | | |
| [41597208627](https://github.com/qlanfr/mata_Hackathon/actions/runs/41597208627) | 1. POLICY_REJECTED: a search text matched twice. 2. **REJECTED**: skip the collections query for personal items. Then a model request timed out and the run stopped | 2,011 → 1,511 | 148.3 → 121.2 ms | 17.9 / 17.9 / 18.9% |
| [41600456007](https://github.com/qlanfr/mata_Hackathon/actions/runs/41600456007) | 1. EXECUTION_ERROR: a batched rewrite did not compile. 2. **VERIFIED**: the same collections skip | 2,011 → 1,511 | 187.7 → 148.5 ms | 21.0 / 21.3 / 20.4% |
| [41604035655](https://github.com/qlanfr/mata_Hackathon/actions/runs/41604035655) | 1. **VERIFIED**: batched rewrite, one query per relation | 2,011 → **15** | 246.4 → **63.6 ms** | **74.3 / 74.0 / 74.2%** |
| [41607113192](https://github.com/qlanfr/mata_Hackathon/actions/runs/41607113192) | 1. POLICY_REJECTED: a search text matched twice. 2. **VERIFIED**: batched rewrite, written differently | 2,011 → **15** | 237.6 → **61.4 ms** | **73.8 / 74.5 / 74.2%** |

A/A noise stayed under 1% in every run. Golden answers were identical (4/4) for every candidate that built.

### What the verified rewrites do

In both of the last two runs the model replaced the four queries per item with one `IN (…)` query per relation (attachments, favorites, folder links, collection links) for all of the user's items, then built each item's JSON from those maps. The two patches are written differently: one adds `Cipher::to_json_batch`, the other adds batch loaders to the attachment, favorite and folder models and a `to_json_prefetched` method. Both are the approach of the maintainers' fix: 15 statements per request against 17 per sync for the official fix in step 1, constant in the number of items. The diffs are in the runs' logs.

## What this shows

- **The loop works on code nobody wrote for it.** The same gates refused a patch that did not match the source, recorded a build failure, rejected a correct but too-small change and verified the real fix.
- **The model found the maintainers' approach** in both runs after the harness fixes below, at 74% each. Before those fixes it ran out of turns, or settled for a smaller, correct change.
- **A verdict near the threshold depends on the machine.** The collections skip measured 18% on one runner and 21% on another, so it was REJECTED once and VERIFIED once. Each run's A/A noise was under 1%, so each verdict is right for its own measurement; a borderline result should be re-measured on other machines before anyone acts on it. The full fix, at 74%, is far from that line.

## What it does not show

- **The seed has no organizations, collections or attachments.** Those branches of the verified rewrite produced no data, so the golden comparison could not check them. A reviewer still has to read that part of the diff.
- **Not on Cloudflare.** These runs used a GitHub-hosted runner, without the network isolation of the deployed runner container.
- **Small sample.** Five runs with one model, on one bug; two of them after the last harness change.

## Harness changes these runs led to

Each was a problem the real codebase exposed. All are generic and opt-in, and the N+1 scenario's hash and behaviour are unchanged:
- **Service options:** build features and a pinned toolchain, environment with placeholders, health path, a login whose token is sent with every check and load request, order-insensitive comparison, and SQL counts from `pg_stat_statements` for services that do not log them.
- **Evidence:** the most frequent statements of a baseline run as evidence (`sql:N`), and `read_source` that can continue a long file from a given line.
- **Agent:** after 12 turns with tools in one step, or 2 turns before the run's budget ends, the model is asked for its answer without tools. A repeated read returns a pointer instead of the file again.
- **Meta provider:** a 420-second request timeout with one retry, for answers that carry a large patch.
- **Evaluator:** a failed build's reason now carries the compiler's error lines, so the model can fix its patch.
- **Prompt:** candidates are applied to the original source, so building on an earlier one means repeating its edits.

## Reproduce

Run the **Real bug probe** workflow for step 1, and the **Real bug run** workflow for step 2 (it needs the `META_API_KEY` repository secret). Locally, `scripts/real-bug/vaultwarden-setup.sh` prepares the scenario, after which `aiops-runner golden` and the usual `aiopsd` and `aiops-runner serve` commands run it with `AIOPS_SCENARIO=scenarios/vaultwarden-sync/scenario.toml`.
