import type { ReactNode } from "react";
import { api, type Contract, type Evaluation, type LoadStats, type TrialReport } from "../api";
import { DiffView } from "../components/DiffView";
import { BeforeAfter, checksLine, p95s } from "../components/Highlights";
import { PairsChart } from "../components/PairsChart";
import { OutcomePill } from "../components/Status";
import { duration, hash, ms, pct, short } from "../format";
import { useLoad } from "../hooks";
import { Link } from "../router";

export function TrialPage({ id }: { id: string }) {
  const { data, error } = useLoad(() => api.trial(id), [id], 0);
  if (error) return <p className="error">Could not load the trial: {error}</p>;
  if (!data) return <p className="muted">Loading…</p>;
  const { trial, report, contract } = data;
  const ev = trial.evaluation;
  const med = ev?.median_improvement ?? null;
  const tone = trial.outcome === "VERIFIED" ? "ok" : trial.outcome === "INCONCLUSIVE" ? "warn" : "bad";

  return (
    <>
      <header className="page-head">
        <span className="crumbs">
          <Link to="/">Runs</Link> / <Link to={`/runs/${trial.run_id}`}>{short(trial.run_id, 12)}</Link> / trial {trial.seq}
        </span>
        <div className="row">
          <h1>Trial {trial.seq}</h1>
          <OutcomePill outcome={trial.outcome} large />
        </div>
        <p className="lede">{trial.patch_summary}</p>
      </header>

      {report && report.pairs.length > 0 && <ResultStrip report={report} median={med} />}

      <div className="grid-main">
        <div className="stack">
          <section className="card" aria-labelledby="verdict-title">
            <div className="card-head">
              <h2 id="verdict-title">Verdict</h2>
              <span className="hint">decided by the Rust evaluator, not the model</span>
            </div>
            <div className="headline">
              <span className={`big ${med == null ? "" : tone}`}>{med == null ? "—" : `${med >= 0 ? "−" : "+"}${pct(Math.abs(med))}`}</span>
              <span className="muted">median p95 change · target −{pct(contract.min_improvement, 0)} in every pair</span>
            </div>
            {ev?.reasons?.[0] && <p>{ev.reasons[0]}</p>}
            <Gates report={report} evaluation={ev} contract={contract} />
          </section>

          <section className="card" aria-labelledby="chart-title">
            <div className="card-head">
              <h2 id="chart-title">p95 latency per pair</h2>
              <span className="hint">{report?.pairs[0] ? `${report.pairs[0].baseline.rate} req/s for ${report.pairs[0].baseline.duration_s}s per run` : ""}</span>
            </div>
            <PairsChart pairs={report?.pairs ?? []} />
          </section>

          <section className="card" aria-labelledby="diff-title">
            <div className="card-head">
              <h2 id="diff-title">Code change</h2>
              <span className="hint mono">patch {hash(trial.patch_hash)}</span>
            </div>
            <DiffView diff={report?.diff ?? ""} />
          </section>
        </div>

        <div className="stack">
          <section className="card" aria-labelledby="hyp-title">
            <h2 id="hyp-title">Hypothesis</h2>
            <div className="quote">
              <span className="k">Experimenter proposed</span>
              <span>{trial.hypothesis}</span>
              {trial.plan?.expected_effect && (
                <span className="small">
                  <b>Expected:</b> {trial.plan.expected_effect}
                </span>
              )}
              {trial.plan?.falsified_if && (
                <span className="small">
                  <b>Wrong if:</b> {trial.plan.falsified_if}
                </span>
              )}
              {!!trial.plan?.evidence?.length && (
                <span className="chips">
                  {trial.plan.evidence.map((e) => (
                    <span className="chip" key={e}>
                      {e}
                    </span>
                  ))}
                </span>
              )}
            </div>
          </section>

          {report && <Metrics report={report} />}

          {report?.correctness && (
            <section className="card" aria-labelledby="golden-title">
              <div className="card-head">
                <h2 id="golden-title">Golden cases</h2>
                <span className="hint">
                  {report.correctness.passed}/{report.correctness.total} identical
                </span>
              </div>
              <ul className="list">
                {report.correctness.cases.map((c) => (
                  <li key={c.name}>
                    <span className="small">
                      <span style={{ color: c.passed ? "var(--ok)" : "var(--bad)" }}>{c.passed ? "✓" : "✗"}</span> <code>{c.name}</code>
                    </span>
                    {c.detail && <span className="error">{c.detail}</span>}
                  </li>
                ))}
              </ul>
            </section>
          )}

          {report && (
            <section className="card" aria-labelledby="repro-title">
              <h2 id="repro-title">Reproduce</h2>
              <dl className="kv">
                <dt>Scenario</dt>
                <dd>
                  {report.scenario_id} {hash(report.scenario_hash)}
                </dd>
                <dt>Patch</dt>
                <dd>{hash(report.patch_hash)}</dd>
                <dt>Runner</dt>
                <dd>
                  boot {short(report.env.runner_boot_id)} · {report.env.cpus} vCPU
                </dd>
                <dt>Toolchain</dt>
                <dd>
                  {report.env.rustc} · {report.env.postgres} · {report.env.k6.split(" ").slice(0, 2).join(" ")}
                </dd>
                <dt>Timings</dt>
                <dd>
                  {Object.entries(report.timings_ms)
                    .filter(([, v]) => v > 0)
                    .map(([k, v]) => `${k} ${duration(v)}`)
                    .join(" · ")}
                </dd>
              </dl>
              {trial.report_artifact_id && (
                <a href={api.artifactUrl(trial.report_artifact_id)} target="_blank" rel="noreferrer" className="small">
                  Raw trial report (JSON) →
                </a>
              )}
            </section>
          )}
        </div>
      </div>
    </>
  );
}

type GateState = "pass" | "fail" | "hold" | "skip";

function Gates({ report, evaluation, contract }: { report: TrialReport | null; evaluation: Evaluation | null; contract: Contract }) {
  const gates: { name: string; why: ReactNode; value: string; state: GateState }[] = [];
  const pairs = report?.pairs ?? [];
  const dropped = pairs.reduce((n, p) => n + p.baseline.dropped + p.candidate.dropped, 0);
  const errors = (s: LoadStats) => s.bad_status + s.timeouts + s.net_errors;
  const candErrors = pairs.reduce((n, p) => n + errors(p.candidate), 0);
  const baseErrors = pairs.reduce((n, p) => n + errors(p.baseline), 0);
  const improvements = evaluation?.improvements ?? [];
  const allBetter = improvements.length > 0 && improvements.every((x) => x > 0);

  gates.push({ name: "Patch policy", why: "Only editable files; every search text matched once", value: report?.patch_error ? "refused" : "ok", state: report?.patch_error ? "fail" : "pass" });
  gates.push({
    name: "Build",
    why: "Fixed release profile, offline dependencies",
    value: report?.build ? (report.build.ok ? duration(report.build.duration_ms) : "failed") : "—",
    state: report?.build ? (report.build.ok ? "pass" : "fail") : "skip",
  });
  const check = (c: { passed: number; total: number } | null | undefined): GateState =>
    c ? (c.passed === c.total ? "pass" : "fail") : "skip";
  gates.push({
    name: "Golden responses",
    why: "Every case must be identical; the candidate cannot see or change them",
    value: report?.correctness ? `${report.correctness.passed}/${report.correctness.total}` : "—",
    state: check(report?.correctness),
  });
  if (report?.invariants) {
    gates.push({
      name: "Response invariants",
      why: "Rules every response must follow, checked on the original too",
      value: `${report.invariants.passed}/${report.invariants.total}`,
      state: check(report.invariants),
    });
  }
  if (report?.sample) {
    gates.push({
      name: "Sampled comparison",
      why: "More requests answered by both services and compared",
      value: `${report.sample.passed}/${report.sample.total}`,
      state: check(report.sample),
    });
  }
  const noise = Math.max(evaluation?.aa_noise ?? 0, evaluation?.baseline_spread ?? 0);
  gates.push({
    name: "Measurement trust",
    why: `A/A noise and baseline spread within ${pct(contract.max_noise, 0)}, no dropped iterations`,
    value: pairs.length ? `${pct(evaluation?.aa_noise)} / ${pct(evaluation?.baseline_spread)} · ${dropped} dropped` : "—",
    state: !pairs.length ? "skip" : noise > contract.max_noise || dropped > 0 ? "hold" : "pass",
  });
  gates.push({
    name: "No new errors",
    why: "Errors, timeouts and crashes must not increase",
    value: pairs.length ? `${candErrors} vs ${baseErrors}` : "—",
    state: !pairs.length ? "skip" : report?.candidate_crashed || candErrors > baseErrors ? "fail" : "pass",
  });
  gates.push({
    name: "Improvement",
    why: `Median p95 change at least ${pct(contract.min_improvement, 0)}, every pair better`,
    value: improvements.length ? improvements.map((x) => pct(x, 0)).join(" · ") : "—",
    state: !improvements.length ? "skip" : allBetter && (evaluation?.median_improvement ?? 0) >= contract.min_improvement ? "pass" : allBetter ? "fail" : "hold",
  });

  const mark: Record<GateState, string> = { pass: "✓", fail: "✗", hold: "?", skip: "–" };
  return (
    <ol className="gates">
      {gates.map((g) => (
        <li key={g.name} className={g.state}>
          <span className="mark" aria-label={g.state}>
            {mark[g.state]}
          </span>
          <span>
            <span className="name">{g.name}</span>
            <br />
            <span className="why">{g.why}</span>
          </span>
          <span className="val">{g.value}</span>
        </li>
      ))}
    </ol>
  );
}

function Metrics({ report }: { report: TrialReport }) {
  const avg = (pick: (s: LoadStats) => number, side: "baseline" | "candidate") =>
    report.pairs.length ? report.pairs.reduce((n, p) => n + pick(p[side]), 0) / report.pairs.length : null;
  const rows: { k: string; b: string; c: string }[] = [
    { k: "p50", b: ms(avg((s) => s.p50_ms, "baseline")), c: ms(avg((s) => s.p50_ms, "candidate")) },
    { k: "p95", b: ms(avg((s) => s.p95_ms, "baseline")), c: ms(avg((s) => s.p95_ms, "candidate")) },
    { k: "p99", b: ms(avg((s) => s.p99_ms, "baseline")), c: ms(avg((s) => s.p99_ms, "candidate")) },
    {
      k: "Successful / offered",
      b: pct(avg((s) => (s.offered ? s.ok / s.offered : 0), "baseline")),
      c: pct(avg((s) => (s.offered ? s.ok / s.offered : 0), "candidate")),
    },
    { k: "SQL per request", b: report.baseline_logs?.avg_sql_count.toFixed(1) ?? "—", c: report.candidate_logs?.avg_sql_count.toFixed(1) ?? "—" },
    { k: "DB time per request", b: ms(report.baseline_logs?.avg_db_ms), c: ms(report.candidate_logs?.avg_db_ms) },
  ];
  return (
    <section className="card" aria-labelledby="metrics-title">
      <div className="card-head">
        <h2 id="metrics-title">Baseline vs candidate</h2>
        <span className="hint">mean of {report.pairs.length} pairs</span>
      </div>
      <div className="table-wrap">
        <table className="data">
          <thead>
            <tr>
              <th>Metric</th>
              <th className="num">Baseline</th>
              <th className="num">Candidate</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.k}>
                <td>{r.k}</td>
                <td className="num">{r.b}</td>
                <td className="num">{r.c}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

/** The before/after numbers first: what changed, measured, and what stayed the same. */
function ResultStrip({ report, median }: { report: TrialReport; median: number | null }) {
  const p95 = p95s(report);
  const fmt1 = (x: number | null | undefined) => (x == null ? "—" : x.toFixed(1));
  return (
    <section className="card result-strip" aria-label="Baseline and candidate">
      <div className="tiles">
        <div className="tile">
          <span className="k">p95 latency</span>
          <BeforeAfter from={ms(p95.baseline)} to={ms(p95.candidate)} />
          <span className="s">
            median change {median == null ? "—" : `${median >= 0 ? "−" : "+"}${pct(Math.abs(median))}`} over {report.pairs.length} pairs
          </span>
        </div>
        <div className="tile">
          <span className="k">SQL per request</span>
          <BeforeAfter from={fmt1(report.baseline_logs?.avg_sql_count)} to={fmt1(report.candidate_logs?.avg_sql_count)} />
          <span className="s">from the service's request logs</span>
        </div>
        <div className="tile">
          <span className="k">DB time per request</span>
          <BeforeAfter from={ms(report.baseline_logs?.avg_db_ms)} to={ms(report.candidate_logs?.avg_db_ms)} />
          <span className="s">mean over all measured requests</span>
        </div>
        <div className="tile">
          <span className="k">Responses unchanged</span>
          <span className="v">{report.correctness ? `${report.correctness.passed}/${report.correctness.total}` : "—"}</span>
          <span className="s">{checksLine(report)}</span>
        </div>
      </div>
    </section>
  );
}
