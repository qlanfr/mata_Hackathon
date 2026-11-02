import { TERMINAL, type Outcome, type RunState } from "../api";

export function stateTone(state: RunState): string {
  if (state === "COMPLETED") return "s-ok";
  if (state === "FAILED") return "s-bad";
  if (state === "EXHAUSTED" || state === "CANCELLED" || state === "CANCELLING") return "s-warn";
  return TERMINAL.includes(state) ? "s-muted" : "s-active";
}

export function outcomeTone(outcome: Outcome | null | undefined): string {
  switch (outcome) {
    case "VERIFIED":
      return "s-ok";
    case "REJECTED":
    case "POLICY_REJECTED":
      return "s-bad";
    case "INCONCLUSIVE":
      return "s-warn";
    case "EXECUTION_ERROR":
      return "s-muted";
    default:
      return "s-active";
  }
}

export function StatePill({ state, large }: { state: RunState; large?: boolean }) {
  return <span className={`pill ${stateTone(state)}${large ? " lg" : ""}`}>{state}</span>;
}

export function OutcomePill({ outcome, large }: { outcome: Outcome | null | undefined; large?: boolean }) {
  return <span className={`pill ${outcomeTone(outcome)}${large ? " lg" : ""}`}>{outcome ?? "RUNNING"}</span>;
}

/** Replay runs must never look like real model runs. */
export function ProviderBadge({ provider }: { provider: string }) {
  if (provider === "replay") {
    return <span className="badge replay" title="Scripted model output for development and demos. Measurements and verdicts are real.">REPLAY</span>;
  }
  if (provider.startsWith("meta:")) {
    return <span className="badge meta" title="Meta model">{provider.replace("meta:", "META · ")}</span>;
  }
  return <span className="badge plain">{provider}</span>;
}
