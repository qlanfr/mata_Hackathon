// The runs page's first screen: the latest verified change with its before/after numbers, the
// model results computed from the runs themselves, and how the harness itself was tested.
import { api, type Run, type TrialReport } from "../api";
import { ago, ms, pct, short } from "../format";
import { useLoad } from "../hooks";
import { Link } from "../router";
import { REAL_BUG, SCORECARD, SCORECARD_AS_OF } from "../scorecard";
import { ProviderBadge } from "./Status";

const mean = (xs: number[]) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : null);

/** Mean p95 of each side over the measured pairs. */
export function p95s(report: TrialReport | null | undefined) {
  const pairs = report?.pairs ?? [];
  return {
    baseline: mean(pairs.map((p) => p.baseline.p95_ms)),
    candidate: mean(pairs.map((p) => p.candidate.p95_ms)),
  };
}

export function checksLine(report: TrialReport | null | undefined): string {
  if (!report) return "—";
  const part = (name: string, c: { passed: number; total: number } | null | undefined) =>
    c ? `${name} ${c.passed}/${c.total}` : null;
  return [part("golden", report.correctness), part("invariants", report.invariants), part("sample", report.sample)]
    .filter(Boolean)
    .join(" · ");
}

export function BeforeAfter({ from, to, unit }: { from: string; to: string; unit?: string }) {
  return (
    <span className="v ba">
      <span className="from">{from}</span>
      <span className="to-arrow" aria-label="to">
        →
      </span>
      <span>{to}</span>
      {unit && <span className="unit">{unit}</span>}
    </span>
  );
}

function pickLatest(runs: Run[]): Run | undefined {
  const done = runs.filter((r) => r.state === "COMPLETED");
  return done.find((r) => r.provider.startsWith("meta:")) ?? done[0];
}

export function LatestVerified({ runs }: { runs: Run[] }) {
  const latest = pickLatest(runs);
  const { data, error } = useLoad(
    async () => {
      if (!latest) return null;
      const detail = await api.run(latest.id);
      const trial = [...detail.trials].reverse().find((t) => t.outcome === "VERIFIED");
      if (!trial) return { trial: null, report: null };
      const { report } = await api.trial(trial.id);
      return { trial, report };
    },
    [latest?.id],
    0,
  );

  if (!latest) return null;
  const trial = data?.trial ?? null;
  const report = data?.report;
  const p95 = p95s(report);
  const med = trial?.evaluation?.median_improvement ?? null;
  const fmt1 = (x: number | null | undefined) => (x == null ? "—" : x.toFixed(1));

  return (
    <section className="card highlight" aria-labelledby="latest-title">
      <div className="card-head">
        <h2 id="latest-title">Latest verified change</h2>
        <span className="row-inline">
          <ProviderBadge provider={latest.provider} />
          <span className="hint">{ago(latest.created_at)}</span>
        </span>
      </div>
      {error && <p className="error">Could not load the result: {error}</p>}
      {!data && !error && <p className="muted small">Loading…</p>}
      {trial && (
        <>
          <p className="clip">
            <span className="muted">Hypothesis · </span>
            {trial.hypothesis}
          </p>
          <div className="tiles wide">
            <div className="tile">
              <span className="k">p95 latency</span>
              <BeforeAfter from={ms(p95.baseline)} to={ms(p95.candidate)} />
              <span className="s">
                median −{pct(med)} across {report?.pairs.length ?? 0} measured pairs
              </span>
            </div>
            <div className="tile">
              <span className="k">SQL per request</span>
              <BeforeAfter from={fmt1(report?.baseline_logs?.avg_sql_count)} to={fmt1(report?.candidate_logs?.avg_sql_count)} />
              <span className="s">from the service's own request logs</span>
            </div>
            <div className="tile">
              <span className="k">Responses unchanged</span>
              <span className="v">{report?.correctness ? `${report.correctness.passed}/${report.correctness.total}` : "—"}</span>
              <span className="s">{checksLine(report)}</span>
            </div>
          </div>
          <div className="row-inline">
            <Link to={`/trials/${trial.id}`} className="btn small primary">
              Open the verdict
            </Link>
            <Link to={`/runs/${latest.id}`} className="small">
              Timeline of run {short(latest.id, 12)}
            </Link>
          </div>
        </>
      )}
      {data && !trial && <p className="muted small">The latest completed run has no verified trial to show.</p>}
    </section>
  );
}

/** Per-model results, counted from the runs list (the API returns the latest 50 runs). */
export function ModelResults({ runs }: { runs: Run[] }) {
  const byModel = new Map<string, Run[]>();
  for (const r of runs) {
    if (!r.provider.startsWith("meta:")) continue;
    byModel.set(r.provider, [...(byModel.get(r.provider) ?? []), r]);
  }
  if (byModel.size === 0) return null;
  const rows = [...byModel.entries()].sort((a, b) => b[1].length - a[1].length);
  const unfinished = runs.filter((r) => r.provider.startsWith("meta:") && (r.state === "FAILED" || r.state === "EXHAUSTED"));

  return (
    <section className="card" aria-labelledby="models-title">
      <div className="card-head">
        <h2 id="models-title">Meta model runs</h2>
        <span className="hint">counted live from the recorded runs</span>
      </div>
      <div className="table-wrap">
        <table className="data">
          <thead>
            <tr>
              <th>Model</th>
              <th className="num">Runs</th>
              <th className="num">Verified</th>
              <th className="num">Not verified</th>
              <th className="num">Failed</th>
            </tr>
          </thead>
          <tbody>
            {rows.map(([provider, list]) => {
              const count = (s: string) => list.filter((r) => r.state === s).length;
              return (
                <tr key={provider}>
                  <td>
                    <ProviderBadge provider={provider} />
                  </td>
                  <td className="num">{list.length}</td>
                  <td className={`num${count("COMPLETED") > 0 ? " ok-text" : ""}`}>{count("COMPLETED")}</td>
                  <td className="num">{count("EXHAUSTED")}</td>
                  <td className="num">{count("FAILED")}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      <p className="muted small">
        Verified: the run ended with a change the evaluator VERIFIED. Not verified: the model stopped or ran out of budget
        without one. Runs still in progress count in Runs only.
      </p>
      {unfinished.length > 0 && (
        <details className="small">
          <summary>Why {unfinished.length === 1 ? "one run" : `${unfinished.length} runs`} ended without a verified change</summary>
          <ul className="list reasons">
            {unfinished.map((r) => (
              <li key={r.id}>
                <Link to={`/runs/${r.id}`} className="mono">
                  {short(r.id, 12)}
                </Link>{" "}
                <span className="muted">
                  {r.provider.replace("meta:", "")} · {r.state}
                </span>
                <span className="clip">{r.error ?? r.summary ?? "—"}</span>
              </li>
            ))}
          </ul>
        </details>
      )}
    </section>
  );
}

export function HarnessScorecard() {
  return (
    <section className="card" aria-labelledby="harness-title">
      <div className="card-head">
        <h2 id="harness-title">Is the harness a rubber stamp?</h2>
        <span className="hint">tested before any model touched it · as of {SCORECARD_AS_OF}</span>
      </div>
      <div className="scores">
        {SCORECARD.map((s) => (
          <div className="score" key={s.label}>
            <span className="k">{s.label}</span>
            <span className="v">{s.value}</span>
            <span className="s">{s.detail}</span>
            <a href={s.href} target="_blank" rel="noreferrer" className="small">
              {s.source}
            </a>
          </div>
        ))}
      </div>
    </section>
  );
}

/** The loop on a real open-source bug; the numbers come from a GitHub Actions run, not this deployment. */
export function RealBug() {
  const r = REAL_BUG;
  return (
    <section className="card" aria-labelledby="realbug-title">
      <div className="card-head">
        <h2 id="realbug-title">On a real bug</h2>
        <span className="row-inline">
          <span className="pill s-ok">VERIFIED · {r.improvement}</span>
        </span>
      </div>
      <p>
        <b>{r.title}.</b> <span className="muted">{r.project}.</span>
      </p>
      <div className="tiles wide">
        <div className="tile">
          <span className="k">SQL per request</span>
          <BeforeAfter from={r.sqlBefore} to={r.sqlAfter} />
          <span className="s">counted in PostgreSQL during the measured runs</span>
        </div>
        <div className="tile">
          <span className="k">p95 latency</span>
          <BeforeAfter from={r.p95Before} to={r.p95After} />
          <span className="s">median improvement {r.improvement} across 3 pairs</span>
        </div>
      </div>
      <p className="small">{r.summary}</p>
      <p className="muted small">{r.caveat}</p>
      <div className="row-inline small">
        <a href={r.runHref} target="_blank" rel="noreferrer">
          The run on GitHub Actions
        </a>
        <a href={r.docHref} target="_blank" rel="noreferrer">
          Every run, including the rejected ones
        </a>
      </div>
    </section>
  );
}
