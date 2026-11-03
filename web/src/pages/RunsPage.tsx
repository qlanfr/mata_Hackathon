import { useState } from "react";
import { api, ApiError, TERMINAL } from "../api";
import { HarnessScorecard, LatestVerified, ModelResults } from "../components/Highlights";
import { ProviderBadge, StatePill } from "../components/Status";
import { ago, duration } from "../format";
import { useLoad } from "../hooks";
import { navigate } from "../router";

export function RunsPage({ askToken }: { askToken: () => Promise<boolean> }) {
  const { data, error, reload } = useLoad(() => api.runs(), [], 5000);
  const [starting, setStarting] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);

  const start = async () => {
    setStarting(true);
    setStartError(null);
    try {
      const { run_id } = await api.createRun();
      navigate(`/runs/${run_id}`);
    } catch (e) {
      if (e instanceof ApiError && e.status === 401 && (await askToken())) {
        setStarting(false);
        return start();
      }
      setStartError(e instanceof Error ? e.message : String(e));
    } finally {
      setStarting(false);
    }
  };

  const runs = data?.runs ?? [];
  return (
    <>
      <header className="page-head">
        <div className="row" style={{ justifyContent: "space-between" }}>
          <h1>Experiment runs</h1>
          <button className="btn primary" onClick={() => void start()} disabled={starting}>
            {starting ? "Starting…" : "Start a run"}
          </button>
        </div>
        <p className="lede">
          A Meta model reads the evidence and proposes one change at a time. A Rust harness builds each change in an isolated
          runner with no internet, checks that every response is unchanged, and measures p95 against the unchanged service. Only
          the Rust evaluator decides whether a change is accepted.
        </p>
        {startError && <p className="error">Could not start the run: {startError}</p>}
      </header>

      {runs.length > 0 && <LatestVerified runs={runs} />}
      {runs.length > 0 && (
        <div className="grid-two">
          <ModelResults runs={runs} />
          <HarnessScorecard />
        </div>
      )}

      <section className="card" aria-labelledby="runs-title">
        <div className="card-head">
          <h2 id="runs-title">Recent runs</h2>
          <span className="hint">Refreshes every 5 seconds</span>
        </div>
        {error && <p className="error">Could not load runs: {error}</p>}
        {data && runs.length === 0 && (
          <div className="empty">
            <p>No runs yet. Start one to measure the order-list API and look for a verified fix.</p>
          </div>
        )}
        {!data && !error && <p className="muted small">Loading…</p>}
        {runs.length > 0 && (
          <div className="table-wrap">
            <table className="data">
              <thead>
                <tr>
                  <th>State</th>
                  <th>Run</th>
                  <th>Model</th>
                  <th className="num">Candidates</th>
                  <th className="num">Duration</th>
                  <th className="num">Started</th>
                </tr>
              </thead>
              <tbody>
                {runs.map((r) => {
                  const end = r.finished_at ?? (TERMINAL.includes(r.state) ? r.updated_at : Date.now());
                  return (
                    <tr key={r.id} className="link" onClick={() => navigate(`/runs/${r.id}`)}>
                      <td>
                        <StatePill state={r.state} />
                      </td>
                      <td style={{ minWidth: 260 }}>
                        <a href={`/runs/${r.id}`} onClick={(e) => e.preventDefault()}>
                          {r.trigger?.kind === "alert" ? `Alert · ${r.trigger.fingerprint}` : "Manual run"}
                        </a>
                        <div className="muted small clip">{r.summary ?? r.goal}</div>
                      </td>
                      <td>
                        <ProviderBadge provider={r.provider} />
                      </td>
                      <td className="num">
                        {r.usage?.candidates ?? 0} / {r.budget?.max_candidates}
                      </td>
                      <td className="num">{duration(end - r.created_at)}</td>
                      <td className="num">{ago(r.created_at)}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
        <button className="btn small" onClick={() => void reload()} style={{ justifySelf: "start" }}>
          Refresh now
        </button>
      </section>
    </>
  );
}
