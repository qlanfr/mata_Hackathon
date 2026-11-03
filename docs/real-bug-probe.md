# Real bug probe: Vaultwarden's N+1 on /api/sync

The N+1 scenario in this repository is hand-written. Before pointing the harness at a real bug, we checked that a real, documented one reproduces in a setup like the runner's: the service and PostgreSQL on one machine, no remote database.

## The bug

- **Project:** [Vaultwarden](https://github.com/dani-garcia/vaultwarden), a Rust (Rocket, Diesel) implementation of the Bitwarden server. GPL-3.0; it is cloned at build time and never copied into this repository.
- **Issue:** #1402, "N+1 query problem on api/sync" (2021). The reporter saw syncs of about 50 to 60 seconds with a remote PostgreSQL, and wrote that local databases seemed hardly affected.
- **Fix:** PR #2429, "Improve sync speed and updated dep. versions", merged 2022-05-11 (commit `3ca85028`, parent `3abf173d`).
- **Cause:** `GET /api/sync` loads the user's ciphers, then calls `to_json` for each one. Each call runs its own queries for attachments, favorites, folder and collections.

## Method

[`scripts/real-bug/vaultwarden-probe.sh`](../scripts/real-bug/vaultwarden-probe.sh), run by the **Real bug probe** workflow:

1. Build the commit before the fix and the fix commit with Rust 1.60, the toolchain of the time.
2. For each version and vault size, start the server on an empty PostgreSQL 15 database and register one user. Create 20 folders and N ciphers through the public API; every 7th cipher is a favorite.
3. After one warm-up, call `GET /api/sync?excludeDomains=true` 10 times. Count SQL statements with `pg_stat_statements` and time each call.

## Result

[Real bug probe run 41592017741](https://github.com/qlanfr/mata_Hackathon/actions/runs/41592017741), GitHub-hosted runner (4 vCPU):

| Version | Ciphers | SQL statements per sync | Median sync time | Ciphers returned |
| --- | --- | --- | --- | --- |
| Before the fix | 100 | 411 | 28.1 ms | 100 |
| Before the fix | 1000 | 4,011 | 249.6 ms | 1000 |
| Fix (PR #2429) | 100 | 17 | 6.8 ms | 100 |
| Fix (PR #2429) | 1000 | 17 | 44.0 ms | 1000 |

- The original runs 4 queries per cipher plus 11 per sync. Each of the four per-cipher queries ran exactly 10,000 times over 10 syncs of 1000 ciphers.
- The fix runs a constant 17 queries per sync, independent of vault size.
- With the database on the same machine, the median sync at 1000 ciphers was 82% faster with the fix. Local databases suffer far less than the remote one in the issue, but the effect is still large enough to measure without simulating network latency.
- Each sample is 10 requests, so treat the times as indicative; the query counts are exact.
- Both builds together took about 10 minutes in GitHub Actions.

## What this does and does not show

- **Shows:** a real N+1 bug from an open-source Rust project reproduces in a runner-like setup, with a large and measurable effect. It is a usable second scenario.
- **Does not show:** that the Meta model finds or fixes it. No model has been run on it yet.

## Running the model loop on it

What the harness would need, from [adding-a-service.md](adding-a-service.md):

- **Runner image:** Rust 1.60 next to the current toolchain, with Vaultwarden's dependencies fetched at image build time.
- **Service start:** per-scenario environment (`ROCKET_PORT`, `WEB_VAULT_ENABLED=false`, `DATA_FOLDER`), a configurable health path (`/alive`), and `--features postgresql` for the build.
- **Authentication:** a login step whose token is sent with golden, sampled and load requests.
- **Evidence:** SQL counts from `pg_stat_statements` instead of the service's own request log, which Vaultwarden does not write. This would also work for any other service.
- **Comparison:** arrays compared without regard to order, if a correct patch may return ciphers in another order.
- **Seed:** a database dump taken after Vaultwarden's own migrations and the API seeding above.
