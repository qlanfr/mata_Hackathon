import type { ReactNode } from "react";
import type { RunEvent } from "../api";
import { hash, ms, offset, pct, short } from "../format";
import { Link } from "../router";
import { OutcomePill } from "./Status";

type Actor = "control" | "model" | "runner";

interface Rendered {
  actor: Actor;
  tone?: string;
  who: string;
  title: ReactNode;
  detail?: ReactNode;
  compact?: boolean;
}

const ROLE = (r: string) => (r === "observer" ? "Observer" : r === "experimenter" ? "Experimenter" : r);

function render(e: RunEvent): Rendered | null {
  const p = e.payload ?? {};
  switch (e.kind) {
    case "run_created":
      return {
        actor: "control",
        who: "control plane",
        title: "Run created",
        detail: `${p.trigger?.kind === "alert" ? `Alert ${p.trigger.fingerprint}` : "Started manually"} · budget ${p.budget?.max_candidates} candidates, ${p.budget?.max_model_turns} model turns · target p95 −${pct(p.contract?.min_improvement, 0)}`,
      };
    case "alert_merged":
      return { actor: "control", who: "control plane", title: "Duplicate alert merged into this run", detail: p.trigger?.summary ?? p.trigger?.fingerprint, compact: true };
    case "state": {
      if (p.to === "EVALUATING" && p.outcome) {
        const tone = p.outcome === "VERIFIED" ? "verdict-ok" : p.outcome === "INCONCLUSIVE" ? "verdict-warn" : "verdict-bad";
        return {
          actor: "control",
          tone,
          who: "evaluator",
          title: (
            <>
              Trial {p.trial} judged <OutcomePill outcome={p.outcome} /> {p.trial_id && <Link to={`/trials/${p.trial_id}`}>results →</Link>}
            </>
          ),
          detail: (
            <>
              {p.reasons?.[0]}
              {p.median_improvement != null && <> · median p95 change {pct(p.median_improvement)}</>}
              {p.sql_per_request?.candidate != null && <> · SQL/request {p.sql_per_request.baseline?.toFixed(1)} → {p.sql_per_request.candidate.toFixed(1)}</>}
            </>
          ),
        };
      }
      if (p.to === "PREPARING" && p.plan) {
        return {
          actor: "model",
          who: "experimenter → control plane",
          title: `Experiment ${p.trial} planned: ${p.patch_summary ?? ""}`,
          detail: (
            <>
              <b>Hypothesis:</b> {p.plan.hypothesis}
              <br />
              <b>Falsified if:</b> {p.plan.falsified_if} · files {p.files?.join(", ")} · patch {hash(p.patch_hash)}
            </>
          ),
        };
      }
      if (p.remeasure) {
        return { actor: "control", tone: "problem", who: "control plane", title: "Inconclusive measurement: measuring the same patch once more", detail: p.reasons?.[0] };
      }
      const terminal = ["COMPLETED", "EXHAUSTED", "FAILED", "CANCELLED"].includes(p.to);
      const tone = p.to === "COMPLETED" ? "verdict-ok" : p.to === "FAILED" ? "verdict-bad" : terminal ? "verdict-warn" : undefined;
      const why = p.reason ?? p.cleanup ?? (p.runner_phase ? `runner phase: ${p.runner_phase}` : p.after ? `after ${p.after}` : undefined);
      return {
        actor: p.runner_phase ? "runner" : "control",
        tone,
        who: "state",
        title: (
          <>
            {p.from} → {p.to}
          </>
        ),
        detail: why,
        compact: !terminal,
      };
    }
    case "job_submitted":
      return { actor: "runner", who: "runner", title: `${p.kind === "baseline" ? "Baseline" : "Trial"} job started${p.attempt > 1 ? ` (attempt ${p.attempt})` : ""}`, detail: `runner boot ${short(p.runner_boot_id)}`, compact: true };
    case "job_waiting":
      return { actor: "runner", tone: "problem", who: "runner", title: "Waiting for the runner", detail: p.problem, compact: true };
    case "job_lost":
      return { actor: "runner", tone: "problem", who: "runner", title: p.retrying ? "Runner job lost; retrying once" : "Runner job lost", detail: p.reason };
    case "baseline_measured":
      return {
        actor: "runner",
        who: "runner",
        title: "Baseline measured",
        detail: `A/A noise ${pct(p.aa_noise)} · p95 ${p.aa_pairs?.map((x: { a_p95_ms: number }) => ms(x.a_p95_ms)).join(" / ")} · ${p.logs?.avg_sql_count?.toFixed(1)} SQL per request · golden ${p.correctness?.passed}/${p.correctness?.total}`,
      };
    case "agent_started":
      return { actor: "model", who: "Meta model", title: `${ROLE(p.role)} started${p.round ? ` (round ${p.round + 1})` : ""}`, compact: true };
    case "model_call":
      return {
        actor: "model",
        who: ROLE(p.role),
        title: p.answered ? `${ROLE(p.role)} answered` : `${ROLE(p.role)} requested ${p.tool_calls.join(", ")}`,
        detail: p.usage?.input_tokens ? `${p.usage.input_tokens} in / ${p.usage.output_tokens} out tokens` : undefined,
        compact: true,
      };
    case "tool_call":
      return {
        actor: "model",
        tone: p.error ? "problem" : undefined,
        who: "tool",
        title: (
          <>
            <code>{p.name}</code>
            <span className="muted small">{Object.keys(p.arguments ?? {}).length ? JSON.stringify(p.arguments) : ""}</span>
          </>
        ),
        detail: p.error ? `error: ${p.error}` : `${p.output_bytes} bytes returned`,
        compact: true,
      };
    case "agent_repair":
      return { actor: "model", tone: "problem", who: ROLE(p.role), title: "Answer rejected; one repair allowed", detail: p.problem };
    case "agent_wrap_up":
      return { actor: "model", who: ROLE(p.role), title: "Tools withdrawn; answer requested", detail: p.reason };
    case "observation":
      return {
        actor: "model",
        who: "observer",
        title: `Observation: ${p.priority ?? ""}`,
        detail: `${p.facts?.length ?? 0} facts with evidence · ${p.hypotheses?.length ?? 0} hypotheses · affected ${p.affected_path}`,
      };
    case "plan":
      return { actor: "model", who: "experimenter", title: `Experimenter chose to ${p.action}`, detail: p.hypothesis };
    case "patch_refused":
      return { actor: "control", tone: "verdict-bad", who: "policy", title: "Patch refused before running", detail: p.problem };
    case "duplicate_patch":
      return { actor: "control", tone: "problem", who: "policy", title: "Same patch proposed again; not re-run", detail: p.summary };
    case "cancel_requested":
      return { actor: "control", tone: "problem", who: "control plane", title: "Cancel requested", compact: true };
    case "cancel_sent":
      return { actor: "runner", tone: "problem", who: "runner", title: "Runner asked to stop the job", compact: true };
    case "step_error":
      return { actor: "control", tone: "problem", who: "control plane", title: "Step failed; retrying", detail: p.error, compact: true };
    default:
      return { actor: "control", who: e.kind, title: e.kind, detail: JSON.stringify(p).slice(0, 200), compact: true };
  }
}

export function Timeline({ events, startedAt }: { events: RunEvent[]; startedAt: number }) {
  if (!events.length) {
    return <p className="muted small">No events yet.</p>;
  }
  return (
    <div className="timeline">
      {events.map((e) => {
        const r = render(e);
        if (!r) return null;
        return (
          <div key={e.seq} className={`ev ${r.actor} ${r.tone ?? ""} ${r.compact ? "compact" : ""}`}>
            <span className="t">+{offset(e.ts - startedAt)}</span>
            <span className="rail" aria-hidden="true" />
            <div className="body">
              <span className="who">{r.who}</span>
              <div className="title">{r.title}</div>
              {r.detail && <div className="detail">{r.detail}</div>}
            </div>
          </div>
        );
      })}
    </div>
  );
}
