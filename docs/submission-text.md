# Submission text

Ready to paste. Every number comes from a run on the deployed system or a CI run linked in the README. Trim from the bottom if the form has a length limit.

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

- 19 hand-written trap patches, each built to fail exactly one gate:
  - wrong item order, leaked orders, rounded totals;
  - edited golden answers, slower code, a crash on one product, a race under load, a build error;
  - four that add dangerous code, which the security gate refuses.

  All 19 got the expected verdict for the expected reason in CI.
- 36 rule-generated mutants of the handler: the correctness gates caught 30. Of the rest, 4 cannot change any response and 2 give identical results on this data.
- Building the traps and mutants exposed real gaps in our own harness, including golden cases that let a wrong item order pass and a build-cache bug that silently measured the original binary. All are fixed and recorded.

## Beyond performance

- **Security gate.** Before anything is built, a patch that adds any of the following is refused:
  - `unsafe` code or SQL built from strings;
  - a process, a network call, a secret read or a file write;
  - an edit to auth or crypto files.
- **Vulnerability fixes.** The model fixes a reported vulnerability. The harness accepts the fix only if every attack request worked on the original and fails on the fix, and every golden response is unchanged. The attacks include held-out variants the model never sees, so filtering the reported strings fails.
  - On a SQL injection in product search, 5 of 5 Meta runs were VERIFIED on the first candidate: 3 with Muse Spark 1.3 and 2 with Wonder 1.0.
  - Every one bound the query as a parameter: 4 of 4 attacks blocked, golden 12/12.
  - Five hand-written fixes, the shortcuts among them, got the expected verdicts in CI.
- **Unattended operation.** The deployment can run on its own:
  - a daily scheduled run;
  - a noisy measurement is retried later instead of failing;
  - daily caps on runs and model tokens;
  - Slack, Discord or webhook notifications;
  - a workflow that turns a VERIFIED patch into a pull request with its evidence.

## Honest scope

Both scenarios are N+1 problems, one hand-written and one real. What the project contributes is the loop around the model: the gates know nothing about N+1 and judge any patch the same way, but other kinds of performance problems have not been tried. The runner targets Rust HTTP services on PostgreSQL; adding one is a scenario file, a seed and a load script, as the Vaultwarden scenario shows. The real-bug seed has no organizations or attachments, so those branches of the model's patch were not exercised.

## What's next

Choose the scenario per run so the real-bug scenario can run on the deployment, try other kinds of performance problems, open a pull request with the evidence when a patch is VERIFIED, and generate golden cases from recorded production traffic.

## Built with

Rust (Axum, Tokio, SQLx), React, Cloudflare Workers, Containers, D1 and R2, k6, PostgreSQL, and the Meta Model API (Muse Spark 1.3, Muse Wonder 1.0).

**Live demo:** https://aiops-edge.yh9365.workers.dev
**Code:** https://github.com/qlanfr/mata_Hackathon
