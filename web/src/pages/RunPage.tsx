import { useEffect, useMemo, useState } from "react";
import { api, ApiError, FLOW, isHunt, runTitle, TERMINAL, type Finding, type RunState } from "../api";
import { Stepper } from "../components/Stepper";
import { OutcomePill, ProviderBadge, StatePill } from "../components/Status";
import { Timeline } from "../components/Timeline";
import { clock, duration, hash, ms, pct, short } from "../format";
import { useLoad, useRunEvents } from "../hooks";
import { Link } from "../router";

export function RunPage({ id, askToken }: { id: string; askToken: () => Promise<boolean> }) {
  const { data, error, reload } = useLoad(() => api.run(id), [id]);
  const { events, live } = useRunEvents(id);
  const [cancelError, setCancelError] = useState<string | null>(null);

  // Reload the run whenever its state changes in the stream.
  const stateEvents = events.filter((e) => e.kind === "state" || e.kind === "observation").length;
  useEffect(() => {
    if (stateEvents) void reload();
  }, [stateEvents, reload]);

  const visited = useMemo(() => {
    const set = new Set<string>(["QUEUED"]);
    events.forEach((e) => e.kind === "state" && set.add(e.payload.from) && set.add(e.payload.to));
    return set;
  }, [events]);

  const baseline = [...events].reverse().find((e) => e.kind === "baseline_measured")?.payload;

  if (error) return <p className="error">Could not load the run: {error}</p>;
  if (!data) return <p className="muted">Loading…</p>;
  const { run, trials } = data;
  const active = !TERMINAL.includes(run.state);
  const end = run.finished_at ?? Date.now();

  const cancel = async () => {
    setCancelError(null);
    try {
      await api.cancelRun(id);
      void reload();
    } catch (e) {
      if (e instanceof ApiError && e.status === 401 && (await askToken())) return cancel();
      setCancelError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <>
      <header className="page-head">
        <span className="crumbs">
          <Link to="/">Runs</Link> / {short(run.id, 12)}
        </span>
        <div className="row">
          <h1>{runTitle(run)}</h1>
          <StatePill state={run.state} large />
          <ProviderBadge provider={run.provider} />
          {live && <span className="live">live</span>}
          {active && (
            <button className="btn danger small" onClick={() => void cancel()} disabled={run.state === "CANCELLING" || run.cancel_requested}>
              {run.cancel_requested ? "Cancelling…" : "Cancel run"}
            </button>
          )}
        </div>
        <p className="lede">{run.goal}</p>
        <p className="muted small">
          Started {clock(run.created_at)} · {duration(end - run.created_at)} · scenario {run.scenario_id} <code>{hash(run.scenario_hash)}</code>
        </p>
        {cancelError && <p className="error">Could not cancel: {cancelError}</p>}
      </header>

      {run.summary && <Summary state={run.state} text={run.summary} hunt={isHunt(run)} />}
      {run.provider === "replay" && (
        <div className="banner warn">
          <span className="label">Replay model</span>
          <span className="small">
            {isHunt(run)
              ? "The hunter's answer in this run is scripted. The requests sent to the service and the harness's verdicts are real."
              : "The observer and experimenter answers in this run are scripted. Baseline, build, golden checks, load and the verdict are real measurements."}
          </span>
        </div>
      )}

      {isHunt(run) ? <Findings findings={run.findings ?? []} state={run.state} /> : <Stepper state={run.state as RunState} visited={visited} />}

      <div className="grid-main">
        <section className="card" aria-labelledby="timeline-title">
          <div className="card-head">
            <h2 id="timeline-title">Timeline</h2>
            <span className="hint">{events.length} events · control plane, model, runner</span>
          </div>
          <Timeline events={events} startedAt={run.created_at} />
        </section>

        <div className="stack">
          {!isHunt(run) && (
            <>
          <section className="card" aria-labelledby="trials-title">
            <h2 id="trials-title">Experiments</h2>
            {trials.length === 0 && <p className="muted small">No experiment yet. The experimenter plans one after the baseline and the observation.</p>}
            <ul className="list">
              {trials.map((t) => (
                <li key={t.id}>
                  <div className="row" style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                    <Link to={`/trials/${t.id}`}>Trial {t.seq}</Link>
                    <OutcomePill outcome={t.outcome} />
                    {t.evaluation?.median_improvement != null && <span className="mono small">p95 {pct(-t.evaluation.median_improvement)}</span>}
                  </div>
                  <span className="small">{t.patch_summary}</span>
                  {t.evaluation?.reasons?.[0] && <span className="muted small">{t.evaluation.reasons[0]}</span>}
                </li>
              ))}
            </ul>
          </section>

          <section className="card" aria-labelledby="baseline-title">
            <h2 id="baseline-title">Baseline</h2>
            {baseline ? (
              <div className="tiles">
                <Tile k="p95 (A/A)" v={ms(median(baseline.aa_pairs?.map((p: { a_p95_ms: number }) => p.a_p95_ms) ?? []))} s={`${baseline.aa_pairs?.length ?? 0} pairs`} />
                <Tile k="SQL / request" v={baseline.logs?.avg_sql_count?.toFixed(1) ?? "—"} s={`max ${baseline.logs?.max_sql_count ?? "—"}`} />
                <Tile k="A/A noise" v={pct(baseline.aa_noise)} s={`limit ${pct(data.contract.max_noise, 0)}`} />
                <Tile k="Golden" v={`${baseline.correctness?.passed ?? "—"}/${baseline.correctness?.total ?? "—"}`} s="cases match" />
              </div>
            ) : (
              <p className="muted small">{FLOW.indexOf(run.state) <= FLOW.indexOf("BASELINING") ? "Measuring the unchanged service…" : "No baseline recorded."}</p>
            )}
          </section>
            </>
          )}

          {run.observation && (
            <section className="card" aria-labelledby="obs-title">
              <div className="card-head">
                <h2 id="obs-title">Observation</h2>
                <span className="hint">facts and hypotheses kept apart</span>
              </div>
              <p className="small">
                <b>Priority:</b> {run.observation.priority}
              </p>
              <ul className="list">
                {run.observation.facts.map((f, i) => (
                  <li key={i}>
                    <span className="small">{f.statement}</span>
                    <span className="chips">
                      {f.evidence.map((ev) => (
                        <span className="chip" key={ev}>
                          {ev}
                        </span>
                      ))}
                    </span>
                  </li>
                ))}
              </ul>
              <h3>Hypotheses</h3>
              <ul className="list">
                {run.observation.hypotheses.map((h, i) => (
                  <li key={i}>
                    <span className={`conf ${h.confidence}`}>{h.confidence} confidence</span>
                    <span className="small">{h.statement}</span>
                  </li>
                ))}
              </ul>
            </section>
          )}

          <section className="card" aria-labelledby="budget-title">
            <h2 id="budget-title">Budget</h2>
            <Meter label="Candidates" used={run.usage.candidates} max={run.budget.max_candidates} />
            <Meter label="Model turns" used={run.usage.model_turns} max={run.budget.max_model_turns} />
            <Meter label="Tool calls" used={run.usage.tool_calls} max={run.budget.max_tool_calls} />
            {run.usage.input_tokens > 0 && (
              <p className="muted small mono">
                {run.usage.input_tokens.toLocaleString()} input / {run.usage.output_tokens.toLocaleString()} output tokens
              </p>
            )}
          </section>
        </div>
      </div>
    </>
  );
}

function Summary({ state, text, hunt }: { state: RunState; text: string; hunt?: boolean }) {
  const tone = hunt && state === "COMPLETED" ? "info" : state === "COMPLETED" ? "ok" : state === "FAILED" ? "bad" : TERMINAL.includes(state) ? "warn" : "info";
  const label = hunt && state === "COMPLETED" ? "Hunt finished" : state === "COMPLETED" ? "Verified change found" : state === "EXHAUSTED" ? "No verified change" : state === "FAILED" ? "Run failed" : state === "CANCELLED" ? "Cancelled" : "Status";
  return (
    <div className={`banner ${tone}`} role="status">
      <span className="label">{label}</span>
      <span>{text}</span>
    </div>
  );
}

function Tile({ k, v, s }: { k: string; v: string; s?: string }) {
  return (
    <div className="tile">
      <span className="k">{k}</span>
      <span className="v">{v}</span>
      {s && <span className="s">{s}</span>}
    </div>
  );
}

function Meter({ label, used, max }: { label: string; used: number; max: number }) {
  const ratio = max > 0 ? Math.min(1, used / max) : 0;
  return (
    <div className="meter">
      <div className="row">
        <span>{label}</span>
        <span>
          {used} / {max}
        </span>
      </div>
      <div className="bar" role="progressbar" aria-valuenow={used} aria-valuemin={0} aria-valuemax={max} aria-label={label}>
        <i className={ratio >= 0.8 ? "hot" : ""} style={{ width: `${ratio * 100}%` }} />
      </div>
    </div>
  );
}

function median(xs: number[]): number | null {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
}

/** A vulnerability hunt's suspects and, once probed, what the harness found for each. */
function Findings({ findings, state }: { findings: Finding[]; state: string }) {
  const tone = (v?: string) => (v === "REPRODUCED" ? "s-bad" : v === "SERVER_ERROR" ? "s-warn" : v ? "s-ok" : "");
  return (
    <section className="card" aria-labelledby="findings-title">
      <div className="card-head">
        <h2 id="findings-title">Findings</h2>
        <span className="hint">the model suspects; the harness sends each attack to the unchanged service in the sandbox</span>
      </div>
      {findings.length === 0 ? (
        <p className="muted small">{state === "HUNTING" ? "The model is reading the code…" : "No suspected vulnerabilities."}</p>
      ) : (
        <div className="table-wrap">
          <table className="data findings">
            <thead>
              <tr>
                <th>Suspected</th>
                <th>Harness</th>
              </tr>
            </thead>
            <tbody>
              {findings.map((f, i) => (
                <tr key={i}>
                  <td>
                    <b>{f.title}</b> <span className="tag">{f.class}</span>
                    <br />
                    <span className="muted small">{f.location}</span>
                    <br />
                    <span className="small">{f.explanation}</span>
                    <br />
                    <span className="mono small attack-path">attack: GET {safeDecode(f.poc_path)}</span>
                    <br />
                    <span className="mono small attack-path">control: GET {safeDecode(f.control_path)} · evidence “{f.evidence}”</span>
                  </td>
                  <td>
                    <span className={`pill ${tone(f.verdict)}`}>{f.verdict ?? (state === "PROBING" ? "PROBING" : "SUSPECTED")}</span>
                    {f.reason && <div className="small">{f.reason}</div>}
                    {f.poc_excerpt && f.verdict === "REPRODUCED" && <div className="mono small attack-path muted">{f.poc_excerpt.slice(0, 200)}</div>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

function safeDecode(path: string): string {
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}
