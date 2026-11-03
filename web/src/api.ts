// Types and calls for the aiopsd API. Reads are public; writes send the admin token, which
// the edge Worker checks.

export type RunState =
  | "QUEUED" | "COLLECTING" | "BASELINING" | "PLANNING" | "PREPARING" | "BUILDING"
  | "CHECKING" | "BENCHMARKING" | "EVALUATING" | "CANCELLING"
  | "COMPLETED" | "EXHAUSTED" | "FAILED" | "CANCELLED";

export type Outcome = "VERIFIED" | "REJECTED" | "INCONCLUSIVE" | "EXECUTION_ERROR" | "POLICY_REJECTED";

export const TERMINAL: RunState[] = ["COMPLETED", "EXHAUSTED", "FAILED", "CANCELLED"];
export const FLOW: RunState[] = ["QUEUED", "COLLECTING", "BASELINING", "PLANNING", "PREPARING", "BUILDING", "CHECKING", "BENCHMARKING", "EVALUATING"];

export interface Budget { max_candidates: number; max_model_turns: number; max_tool_calls: number; max_run_minutes: number; build_timeout_s: number }
export interface Usage { model_turns: number; tool_calls: number; candidates: number; input_tokens: number; output_tokens: number }
export interface Contract { pairs: number; min_improvement: number; max_noise: number; max_error_rate_increase: number; max_baseline_error_rate: number }

export interface Observation {
  facts: { statement: string; evidence: string[] }[];
  hypotheses: { statement: string; confidence: string }[];
  affected_path: string;
  missing_information: string[];
  priority: string;
}

export interface Run {
  id: string;
  state: RunState;
  goal: string;
  provider: string;
  scenario_id: string;
  scenario_hash: string;
  trigger: { kind: string; fingerprint?: string; service?: string; summary?: string };
  budget: Budget;
  usage: Usage;
  aa_noise: number | null;
  observation: Observation | null;
  summary: string | null;
  error: string | null;
  cancel_requested: boolean;
  baseline_artifact_id: string | null;
  created_at: number;
  updated_at: number;
  finished_at: number | null;
}

export interface Evaluation {
  outcome: Outcome;
  reasons: string[];
  improvements: number[];
  median_improvement: number | null;
  aa_noise: number | null;
  baseline_spread: number | null;
}

export interface Trial {
  id: string;
  run_id: string;
  seq: number;
  hypothesis: string;
  plan: { hypothesis: string; evidence: string[]; expected_effect: string; falsified_if: string; action: string } | null;
  patch_summary: string | null;
  files: string[] | null;
  patch_hash: string;
  status: string;
  outcome: Outcome | null;
  evaluation: Evaluation | null;
  report_artifact_id: string | null;
  created_at: number;
  updated_at: number;
}

export interface LoadStats {
  label: string; rate: number; duration_s: number; planned: number; offered: number; iterations: number; dropped: number;
  ok: number; bad_status: number; timeouts: number; net_errors: number;
  p50_ms: number; p95_ms: number; p99_ms: number; max_ms: number;
}
export interface Pair { index: number; order: string; baseline: LoadStats; candidate: LoadStats }
export interface CaseResult { name: string; passed: boolean; detail: string | null }
export interface Correctness { total: number; passed: number; cases: CaseResult[] }
export interface LogSummary { requests: number; avg_sql_count: number; max_sql_count: number; avg_db_ms: number; avg_latency_ms: number }
export interface Env { runner_boot_id: string; hostname: string; cpus: number; rustc: string; postgres: string; k6: string }

export interface TrialReport {
  trial_id: string;
  scenario_id: string;
  scenario_hash: string;
  patch_hash: string;
  patch_error: string | null;
  diff: string;
  build: { ok: boolean; cached: boolean; duration_ms: number; log_sha256: string; log_tail: string } | null;
  correctness: Correctness | null;
  baseline_correctness: Correctness | null;
  invariants?: Correctness | null;
  baseline_invariants?: Correctness | null;
  sample?: Correctness | null;
  pairs: Pair[];
  candidate_crashed: string | null;
  baseline_logs: LogSummary | null;
  candidate_logs: LogSummary | null;
  execution_error: string | null;
  env: Env;
  timings_ms: Record<string, number>;
}

export interface RunEvent {
  seq: number;
  ts: number;
  kind: string;
  // Payload shapes differ per kind; the timeline narrows them where it renders them.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  payload: any;
}

export interface Job { id: string; kind: string; status: string; attempt: number; runner_boot_id: string | null; runner_phase: string | null; error: string | null }

const TOKEN_KEY = "aiops-admin-token";

export function getToken(): string {
  try {
    return localStorage.getItem(TOKEN_KEY) ?? "";
  } catch {
    return "";
  }
}

export function setToken(token: string): void {
  try {
    if (token) localStorage.setItem(TOKEN_KEY, token);
    else localStorage.removeItem(TOKEN_KEY);
  } catch {
    // Storage can be unavailable (private windows); the token then lasts for this page only.
  }
  memoryToken = token;
}

let memoryToken = "";

function token(): string {
  return getToken() || memoryToken;
}

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers);
  if (init?.method && init.method !== "GET") {
    headers.set("content-type", "application/json");
    const t = token();
    if (t) headers.set("authorization", `Bearer ${t}`);
  }
  const res = await fetch(path, { ...init, headers });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) {
    const message = typeof body?.error === "string" ? body.error : `${res.status} ${res.statusText}`;
    throw new ApiError(res.status, message);
  }
  return body as T;
}

export const api = {
  runs: () => request<{ runs: Run[] }>("/api/v1/runs"),
  run: (id: string) => request<{ run: Run; trials: Trial[]; jobs: Job[]; contract: Contract }>(`/api/v1/runs/${id}`),
  events: (id: string, afterSeq: number) => request<{ events: RunEvent[] }>(`/api/v1/runs/${id}/events?after_seq=${afterSeq}`),
  trial: (id: string) => request<{ trial: Trial; patch: { summary: string; edits: { path: string }[] } | null; report: TrialReport | null; contract: Contract }>(`/api/v1/trials/${id}`),
  createRun: (goal?: string) => request<{ run_id: string }>("/api/v1/runs", { method: "POST", body: JSON.stringify(goal ? { goal } : {}) }),
  cancelRun: (id: string) => request<{ state: RunState }>(`/api/v1/runs/${id}/cancel`, { method: "POST", body: "{}" }),
  artifactUrl: (id: string) => `/api/v1/artifacts/${id}`,
};
