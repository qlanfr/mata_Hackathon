# Architecture

[한국어](./architecture.ko.md)

## 1. Deployment

The Worker only routes, authenticates, manages container lifetimes and relays bindings. The product logic lives in two Rust containers. Neither container reaches D1, R2 or the other container directly: every call goes through the Worker's outbound handlers.

```mermaid
flowchart LR
  B["Browser<br/>React SPA"]

  subgraph W["Cloudflare Worker · edge/ (TypeScript, no product logic)"]
    W1["Static assets<br/>web/dist"]
    W2["/api/* router<br/>writes need the admin token"]
    W3["Cron trigger<br/>every minute"]
    W4["Secrets<br/>META_API_KEY · ADMIN_TOKEN"]
    W5["outboundByHost<br/>runner · d1 · r2 .internal"]
  end

  subgraph A["aiopsd · Rust Axum · container (internet allowed)"]
    A1["API · SSE"]
    A2["Orchestrator<br/>tick · lease · CAS state transitions"]
    A3["Agent harness<br/>observer and experimenter roles · tools · budget"]
    A4["Evaluator<br/>the only place a verdict is made"]
    A5["Outbound clients<br/>D1 queries · R2 · runner jobs"]
  end

  subgraph R["aiops-runner · Rust · container standard-4 (no internet, no bindings, no secrets)"]
    R1["Fresh workspace · patch"] --> R2["cargo build --offline"] --> R3["Embedded PostgreSQL<br/>(the service's DB)"] --> R4["Golden · invariants · sample"] --> R5["k6, 3 alternating pairs"] --> R6["Process-group cleanup"]
  end

  D1[("Cloudflare D1<br/>runs · trials · jobs · run_events · artifacts · leases")]
  R2B[("Cloudflare R2<br/>reports above the inline limit")]
  M["Meta Model API<br/>Replay provider without a key"]

  B -->|"HTTPS"| W2
  W1 -->|"static files"| B
  W2 -->|"Durable Object fetch"| A1
  W3 -->|"/internal/tick"| A2
  A3 -->|"Responses API"| M
  A5 -->|"http://*.internal"| W5
  W5 -->|"env.DB · query / batch"| D1
  W5 -->|"env.ARTIFACTS"| R2B
  W5 -->|"Runner DO fetch<br/>results + boot_id"| R1
```

- **Two databases.** The control DB (runs, trials, jobs, events) is D1. The service under test keeps its own PostgreSQL inside the runner, because the target user runs APIs on PostgreSQL.
- **No interactive transactions in D1.** A state transition is an `UPDATE ... WHERE state_version = ?` inside a `batch()` (one transaction) together with its event and job rows. A guard row records `changes()` right after the UPDATE; if it is 0, a CHECK constraint fails and the whole batch rolls back. A transition that loses the race therefore writes nothing.
- **One orchestrator at a time.** Only the instance holding the D1 lease ticks (TTL 60 s, renewed by a heartbeat). On SIGTERM, aiopsd finishes its tick and releases the lease. This was added after a rollout where an old and a new instance advanced the same run.
- **The runner holds nothing.** It has no outbound handler, so it cannot reach D1, R2 or the Meta API, and it has no secrets. aiopsd polls it and stores the results.
- **Keep-alive.** Code running inside a container does not count as activity. aiopsd's polling keeps the runner awake, and the 1-minute cron keeps aiopsd awake and resumes runs from the DB if it restarted.
- **Artifacts.** Reports up to the inline limit are stored in D1; larger ones go to R2 with only the hash, size and key in D1. The deployed limit is 4 KB, so every report goes through R2.

## 2. Run states

```mermaid
stateDiagram-v2
  [*] --> QUEUED
  QUEUED --> COLLECTING: baseline job submitted · budget reserved
  COLLECTING --> BASELINING: baseline built · request logs collected
  BASELINING --> PLANNING: A/A noise within limit
  BASELINING --> FAILED: A/A noise above limit (too noisy to judge)
  PLANNING --> PREPARING: observer writes facts · experimenter proposes · policy check passes
  PLANNING --> PLANNING: policy refusal or invalid answer · one repair turn
  PLANNING --> EXHAUSTED: model stops or budget spent
  PREPARING --> BUILDING
  BUILDING --> CHECKING: build succeeded
  CHECKING --> BENCHMARKING: all correctness checks passed
  CHECKING --> EVALUATING: a check failed (no load test)
  BENCHMARKING --> EVALUATING: 3 alternating pairs
  EVALUATING --> COMPLETED: VERIFIED
  EVALUATING --> PREPARING: INCONCLUSIVE · same patch measured once more
  EVALUATING --> PLANNING: REJECTED or still INCONCLUSIVE · budget left
  EVALUATING --> EXHAUSTED: no budget left
  CANCELLING --> CANCELLED: runner confirmed its processes are gone
  COMPLETED --> [*]
  EXHAUSTED --> [*]
  CANCELLED --> [*]
  FAILED --> [*]
```

- A cancel request is stored first and moves the run to `CANCELLING` from any state. It becomes `CANCELLED` only after the runner confirms cleanup.
- If the runner's `boot_id` changes, or the runner no longer knows a job, the job is recorded as `lost` and submitted once more. A restarted runner has already lost that job's processes and workspace, so nothing runs twice. A runner that is shutting down refuses new jobs with 503.
- If the runner's report carries a different scenario hash than the run, the run stops with "scenario changed during the run". This catches a rollout that replaced the runner mid-run.

## 3. One trial

```mermaid
sequenceDiagram
  autonumber
  participant O as aiopsd orchestrator
  participant L as Meta model (experimenter)
  participant E as Evaluator (aiops-core)
  participant W as Worker outbound
  participant R as aiops-runner

  O->>L: evidence IDs · past trials · remaining budget · allowed paths
  L-->>O: ExperimentPlan + patch ({path, search, replace}[])
  O->>O: policy check (paths, protected files, exact single match, duplicate patch hash)
  O->>W: POST http://runner.internal/jobs
  W->>R: Runner Durable Object fetch
  R-->>O: 202 · job_id · boot_id
  loop every few seconds
    O->>W: GET /jobs/{id}
    W->>R: status (also keeps the runner awake)
    R-->>O: stage history · boot_id
  end
  R-->>O: build result · correctness results · 6 k6 summaries · git diff
  O->>E: raw measurements + the fixed contract
  E-->>O: verdict + reasons
  O->>L: the verdict and reasons, if budget is left
```

## 4. Gates

The first gate that fails decides the verdict. All thresholds come from the scenario contract (`scenarios/n-plus-one/scenario.toml`), which no model can edit. The 20% target and 3 pairs are policy values chosen for the demo, not measured results.

| Order | Condition | Verdict |
| --- | --- | --- |
| 1 | Patch refused by policy | POLICY_REJECTED |
| 1 | Runner error, build failure, the baseline itself breaks an invariant | EXECUTION_ERROR |
| 2 | A golden case, an invariant or a sampled response differs, or the candidate process dies | REJECTED |
| 3 | Fewer than 3 pairs completed, A/A noise or this trial's baseline spread above 15%, dropped iterations in the baseline | INCONCLUSIVE |
| 3 | The candidate falls behind the arrival rate while the baseline keeps up, and is slower | REJECTED |
| 4 | Error or timeout rate higher than the baseline's | REJECTED |
| 5 | Median pairwise p95 improvement ≥ 20% and every pair improved | VERIFIED |
| 6 | The candidate is slower, or every pair misses 20% by more than the measured noise | REJECTED |
| 7 | Anything else (pairs disagree, or close to the target) | INCONCLUSIVE |

The pairwise improvement is `(baseline p95 − candidate p95) / baseline p95`, with p95 taken over successful responses only. Failed requests are judged by gate 4.
