# Adding a service

What it takes today to point the harness at another service, and what it cannot do yet. Everything below matches the current code; nothing here is planned behaviour unless it says so.

## What the harness needs from the service

The runner builds the service from source and talks to it over HTTP. The service must:

| Requirement | Why | Where it is used |
| --- | --- | --- |
| Be a Cargo package that builds with `cargo build --release --offline` | The runner has no internet; dependencies are compiled into the image beforehand | `apps/aiops-runner/src/builder.rs` |
| Read `DATABASE_URL`, `PORT` and `POOL_SIZE` from the environment | The runner starts its own PostgreSQL and picks the port | `apps/aiops-runner/src/service.rs` |
| Answer `GET /healthz` when ready | The runner waits for it before any request | `apps/aiops-runner/src/service.rs` |
| Print one JSON line per request to stdout with `request_id`, `uri`, `status`, `latency_ms`, `sql_count` and `db_ms` | This is the observation evidence the model reads (slowest requests, SQL per request) | `apps/aiops-runner/src/logs.rs` |
| Be deterministic for the same database state | Golden answers are recorded once from the original and compared as JSON values | `apps/aiops-runner/src/correctness.rs` |

The SQL count comes from the service itself. `fixtures/orders-api/src/tracker.rs` is a 33-line example that wraps each query and is protected from edits, so a patch cannot fake the evidence. Without it the harness still works, but the model only sees latency.

## The five files of a scenario

Copy `scenarios/n-plus-one/` and change these.

1. **`scenario.toml`**: the contract. No model can edit any file in this directory.
   - `[target]`: `source_dir`, `binary`, the files a patch may touch (`allowed_paths`), the files it may never touch (`denied_paths`: put the tracker, `Cargo.toml`, `Cargo.lock` and `build.rs` here), and `max_edits`.
   - `[database]`: schema and seed SQL. The seed should be large enough that the slow path is visible under load.
   - `[correctness]`: the requests that become golden cases, an optional `sample` (a path with `{user}`, filled from the load script's user sequence), and optional `invariants`.
   - `[load]`: arrival rate, warm-up, duration, VUs, timeout, number of distinct users.
   - `[contract]`: pairs, minimum improvement, maximum noise, allowed error-rate increase.
   - `[budget]`: candidates, model turns, tool calls, minutes, build timeout.
2. **`schema.sql`** and **`seed.sql`**: loaded into the runner's PostgreSQL before every measurement.
3. **`load.js`**: a k6 script. The runner passes `TARGET`, `RATE`, `DURATION_S`, `PRE_VUS`, `MAX_VUS`, `TIMEOUT_MS`, `USERS` and `SUMMARY_PATH` as environment variables, so baseline and candidate always get the same plan. Use a `constant-arrival-rate` executor: with a closed loop, a slow service simply receives fewer requests and p95 hides the problem.
4. **`cases/*.json`**: golden answers. Do not write them by hand. Generate them from the unchanged service:

   ```bash
   AIOPS_SCENARIO=scenarios/<id>/scenario.toml ./target/release/aiops-runner --pg-user postgres golden
   ```

5. **`candidates/`** (recommended): a known-good patch and a few trap patches with their expected verdicts in `suite.json`. `scripts/harness-suite.sh` then shows that every gate works on this service before any model touches it.

## Wiring it in

- **Locally:** pass `--scenario scenarios/<id>/scenario.toml` to aiopsd and set `AIOPS_SCENARIO` for the runner. Both must load the same file: aiopsd stops a run whose runner reports a different scenario hash.
- **Runner image:** add the service to `deploy/Dockerfile.runner` and warm the shared target directory with its dependencies, as the image already does for `fixtures/orders-api`. Candidate builds then compile only the service crate.
- **Cloudflare:** set `AIOPS_SCENARIO` for both containers (aiopsd's `envVars` in `edge/src/index.ts`, the runner's `ENV` in `deploy/Dockerfile.runner`) and deploy.

## What does not work yet

- **One scenario per deployment.** aiopsd and the runner each load one scenario at start-up. Choosing the scenario per run is the next change; the run already records its scenario hash.
- **Invariants are code.** The only built-in invariant set is `orders-page` (Rust, in `apps/aiops-runner/src/invariants.rs`). A new service either writes its own set or runs with golden cases and the sample comparison only.
- **Rust and PostgreSQL only.** Building, starting and measuring assume a Cargo package and a PostgreSQL database. Another language needs another builder; another database needs another way to start and seed it.
- **The service must be self-contained.** No calls to other services or the internet during a measurement. A service with dependencies needs them stubbed inside the runner.
- **No repository connection.** The source is copied into the image. Pulling a commit from GitHub and opening a pull request with the evidence are not built.
- **Golden answers mean "same as before".** A change that intentionally alters a response cannot pass. That is the right default for performance work, not for behaviour changes.

## Next steps, in order

1. Scenario chosen per run, with each scenario's dependencies warmed in the runner image.
2. A second scenario of a different kind (for example a missing index or a hot loop without a database), to show that the gates do not depend on N+1.
3. A "command" measurement mode (run a benchmark command instead of HTTP load, and the repository's tests as the correctness gate), which would let the harness run real optimization tasks from public benchmarks.
4. Repository connection: check out a commit, and open a pull request with the verdict, the measurements and the diff when a patch is VERIFIED.
5. Golden cases generated from recorded production requests instead of a hand-picked list.
