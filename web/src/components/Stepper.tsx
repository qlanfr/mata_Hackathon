import { FLOW, type RunState } from "../api";

const WHO: Record<string, string> = {
  QUEUED: "control",
  COLLECTING: "runner",
  BASELINING: "runner",
  PLANNING: "Meta model",
  PREPARING: "runner",
  BUILDING: "runner",
  CHECKING: "runner",
  BENCHMARKING: "runner",
  EVALUATING: "evaluator",
};

/** The run's path through the state machine. `visited` comes from the event timeline. */
export function Stepper({ state, visited }: { state: RunState; visited: Set<string> }) {
  const terminal = !FLOW.includes(state) && state !== "CANCELLING";
  const endTone = state === "COMPLETED" ? "end-ok" : state === "FAILED" ? "end-bad" : "end-warn";
  return (
    <div className="stepper" role="list" aria-label="Run states">
      {FLOW.map((s) => {
        const cls = s === state ? "current" : visited.has(s) ? "done" : "";
        return (
          <div key={s} role="listitem" className={`step ${cls}`} aria-current={s === state ? "step" : undefined}>
            {s}
            <span className="who">{WHO[s]}</span>
          </div>
        );
      })}
      {(terminal || state === "CANCELLING") && (
        <div role="listitem" className={`step ${state === "CANCELLING" ? "current" : endTone}`}>
          {state}
          <span className="who">{state === "CANCELLING" ? "waiting for cleanup" : "final"}</span>
        </div>
      )}
    </div>
  );
}
