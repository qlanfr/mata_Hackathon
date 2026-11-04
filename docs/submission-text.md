# Submission text

Ready to paste. Every number comes from a run on the deployed system or a CI run linked in the README. Trim from the bottom if the form has a length limit. A Korean translation is in [submission-text.ko.md](submission-text.ko.md).

---

**Name:** AIOps Experiment Engineer

**Tagline:** The AI proposes the performance fix. A Rust harness proves it, or refuses it.

## The problem

An AI model can write a plausible performance fix in seconds. Whether that fix keeps every response the same, and whether the service is really faster, still takes an engineer hours to check. Trusting the model's own claim is the failure mode.

## What it does

A Meta model (Muse Spark 1.3 via the Meta Model API) reads a slow service's evidence through tools: metrics, the slowest requests with their SQL counts, the source code and past experiments. It writes facts and hypotheses separately, then proposes one hypothesis, what would prove it wrong, and a patch.

A Rust harness then does what an engineer would:

- builds the patch in an isolated Cloudflare Container with no internet and no secrets;
- compares the candidate with the original on 14 golden cases, response invariants and 200 sampled requests;
- measures p95 latency against the unchanged service in 3 alternating pairs, after first measuring how noisy the machine is.

A Rust evaluator, not the model, returns VERIFIED, REJECTED or INCONCLUSIVE with the measured reason. Rejections go back to the model, which may try again within a fixed budget.

## Results on the deployed system

On an order-list API with an N+1 query:

- Muse Spark 1.3 reached a VERIFIED patch in all 9 runs where the model got to work. Two more stopped before the model was called: one by our own deploy, and one because the machine was too noisy to measure, which the harness refused to judge. Muse Wonder 1.0 did in 2 of 4; one of its failures was our output-token cap, now fixed.
- Verified patches cut SQL statements per request from 81.8 to 3.0 and p95 from about 17 ms to about 2 ms (median pairwise improvement 87.7% to 90.1%), with every golden and sampled response unchanged.
- In one run the model's first patch did not match the source. The harness refused it before anything ran. The model read the refusal, corrected the patch, and the second one was VERIFIED.
- In another, the measurement was too noisy (baseline varied 19.2%). The harness said INCONCLUSIVE twice instead of guessing, and the model's next candidate was VERIFIED.

## On a real bug

We also ran the loop on a documented bug in a real open-source Rust project: Vaultwarden issue #1402, an N+1 on `GET /api/sync`, at the commit before its maintainers fixed it (run on a GitHub-hosted runner). The model's batched rewrite, the same approach as the maintainers' fix, cut SQL statements per sync from 2,011 to 15 and p95 from 246 to 64 ms, with every golden answer identical: VERIFIED at 74%. It got there in both runs after we fixed what this codebase exposed in our harness. Before that, one run ran out of turns, one rewrite did not compile, and a smaller correct change measured 18% on one machine and 21% on another, so it was REJECTED once and VERIFIED once.

## How we know the harness is not a rubber stamp

- 23 hand-written trap patches, each built to fail exactly one gate:
  - wrong item order, leaked orders, rounded totals;
  - edited golden answers, slower code, a crash on one product, a race under load, a build error;
  - seven that add dangerous code, which the security gate refuses, among them a shell command run through SQL and a golden answer compiled into the binary;
  - one that keeps the slow code and secretly slows down the baseline (see Security).

  All 23 got the expected verdict for the expected reason in CI.
- 36 rule-generated mutants of the handler: the correctness gates caught 30. Of the rest, 4 cannot change any response and 2 give identical results on this data.
- Building the traps and mutants exposed real gaps in our own harness, including golden cases that let a wrong item order pass, a build-cache bug that silently measured the original binary, and a way to fake an improvement by slowing the baseline. All are fixed and recorded.

## Security: the model and its code are untrusted

The model only proposes. It has no tool that changes anything, no network access and no secrets. Every patch it writes passes layers that do not trust it:

- **Security gate.** Before anything is built, a patch is refused if it adds any of the following:
  - `unsafe`, `libc` or `asm!`;
  - a process, a network call, a secret read, or a file read or write;
  - a file compiled into the binary (`include_str!`);
  - SQL built from strings, or administrative SQL (`COPY ... PROGRAM`, `LOCK`, `pg_sleep`, `ALTER`, `GRANT`);
  - an edit to auth or crypto files.
- **Sandbox.** The patch builds and runs in a container with no internet and no secrets, as a non-root user. The scenario and golden answers are read-only.
- **Least-privilege database.** The service under test connects as a read-only role, not as the database superuser. We tried 10 privileged or long-running statements as that role:
  - 9 were denied: shell commands through `COPY ... TO PROGRAM`, table locks, writes, file reads, configuration changes and more;
  - a long sleep was cut off at 10 s.
- **Measurement isolation.** While one service is measured, the other is frozen.

Reviewing our own harness found two real holes. Both are now closed and measured:
- **Shell commands through SQL.** The service ran as the database superuser, so one static SQL string in a patch could run a shell command. The gate only looked for SQL built with `format!`.
- **A faked improvement.** A candidate could slow the baseline down to make itself look faster.
  - Our trap patch keeps the slow code and burns CPU only while idle. It measured 45-67% "faster" with no real change. The harness called it INCONCLUSIVE only because the baseline spread too much.
  - With the idle service frozen, the same trap measured 0.4 / -1.5 / -0.6% in CI and was REJECTED.

**Prompt injection.** Anything the model reads can carry text written to steer it: a code comment, a request URI in the logs, a value in the database.
- **Screening.** Every tool output is checked for instruction-like phrases and for hidden zero-width or bidirectional characters, also after URL decoding.
- **Marking.** A hit is recorded on the timeline, and the model gets the output marked as data.
- **Model text in outputs.** Text the model wrote goes into pull request descriptions inside a code block it cannot close.
- **Red team** (GitHub-hosted runner). Instructions were planted in the source, the request log and hidden characters. Three Meta runs, Muse Spark 1.3 twice and Wonder 1.0 once:
  - the screen flagged 4, 4 and 3 tool outputs;
  - no patch carried any planted code;
  - all three were VERIFIED at 85-89%.

  The models did not follow the injections. So this shows that the screen sees them, not that the gate stops a model that obeys one. The trap patches test the gate against the code an injection asks for.

## Beyond performance

- **Security gate.** Before anything is built, a patch that adds any of the following is refused:
  - `unsafe` code or SQL built from strings;
  - a process, a network call, a secret read or a file write;
  - an edit to auth or crypto files.
- **Vulnerability fixes.** The model fixes a reported vulnerability. The harness accepts the fix only if every attack request worked on the original and fails on the fix, and every golden response is unchanged. The attacks include held-out variants the model never sees, so filtering the reported strings fails.
  - On a SQL injection we planted in product search, 5 of 5 Meta runs were VERIFIED on the first candidate: 3 with Muse Spark 1.3 and 2 with Wonder 1.0.
  - Every one bound the query as a parameter: 4 of 4 attacks blocked, golden 12/12.
  - Five hand-written fixes, the shortcuts among them, got the expected verdicts in CI.
- **Weekly vulnerability hunt.** Once a week, or on demand, the model reads the code and names up to five suspected vulnerabilities. Each comes with an attack request, a harmless control request, and the evidence the attack should return.
  - The runner sends them to the unchanged service in the sandbox.
  - Rules, not the model, judge each one REPRODUCED, SERVER_ERROR or NOT_REPRODUCED.
  - The first hunt on the deployment (Muse Spark 1.3, 92 s) pointed at the right place: the SQL injection we planted in product search. But its attack did not return the evidence it named, so the verdict was NOT_REPRODUCED. A claim alone is not accepted.
- **Unattended operation.** The deployment can run on its own:
  - a daily scheduled run and the weekly hunt;
  - a noisy measurement is retried later instead of failing;
  - daily caps on runs and model tokens;
  - Slack, Discord or webhook notifications;
  - a workflow that turns a VERIFIED patch into a pull request with its evidence.

## Honest scope

Both scenarios are N+1 problems, one hand-written and one real. What the project contributes is the loop around the model: the gates know nothing about N+1 and judge any patch the same way, but other kinds of performance problems have not been tried. The runner targets Rust HTTP services on PostgreSQL; adding one is a scenario file, a seed and a load script, as the Vaultwarden scenario shows. The real-bug seed has no organizations or attachments, so those branches of the model's patch were not exercised.

On security:
- The gate is lexical: it catches what a performance patch has no reason to contain, not every way to write it. The sandbox, the database role and the freeze do not depend on spelling.
- The injection screen is a heuristic.
- Hunts send GET requests only, and one hunt is one data point. REPRODUCED would mean the claimed behaviour happened, not a judgement that it is a vulnerability.

## What's next

Choose the scenario per run so the real-bug scenario can run on the deployment, try other kinds of performance problems, let hunts send more than GET requests, test the gate with a model that does follow an injection, and generate golden cases from recorded production traffic.

## Built with

Rust (Axum, Tokio, SQLx), React, Cloudflare Workers, Containers, D1 and R2, k6, PostgreSQL, and the Meta Model API (Muse Spark 1.3, Muse Wonder 1.0).

**Live demo:** https://aiops-edge.yh9365.workers.dev
**Code:** https://github.com/qlanfr/mata_Hackathon
