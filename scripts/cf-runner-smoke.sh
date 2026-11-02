#!/usr/bin/env bash
# Runs the baseline job and the known-good trial on the deployed runner and prints a summary.
#
#   EDGE_URL=https://aiops-edge.<subdomain>.workers.dev ADMIN_TOKEN=... scripts/cf-runner-smoke.sh
#
# Polling every 5 seconds also keeps the runner container awake while a job runs.
set -euo pipefail

: "${EDGE_URL:?set EDGE_URL to the deployed Worker URL}"
: "${ADMIN_TOKEN:?set ADMIN_TOKEN}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-$ROOT/.work/cf}"
mkdir -p "$OUT"

api() { curl -fsS -H "authorization: Bearer $ADMIN_TOKEN" -H 'content-type: application/json' "$@"; }

run_job() { # $1 = name, $2 = JSON body
  local name=$1 body=$2 started job_id status
  started=$(date +%s)
  job_id=$(api -X POST "$EDGE_URL/runner/jobs" -d "$body" | python3 -c 'import json,sys; print(json.load(sys.stdin)["job_id"])')
  echo "[$name] job $job_id"
  while true; do
    api "$EDGE_URL/runner/jobs/$job_id" > "$OUT/$name.json"
    status=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d["status"], d["phase"])' "$OUT/$name.json")
    printf '\r[%s] %4ss %s          ' "$name" "$(( $(date +%s) - started ))" "$status"
    case "$status" in done*|cancelled*) echo; break ;; esac
    sleep 5
  done
}

echo "health: $(api "$EDGE_URL/runner/healthz")"
run_job baseline '{"kind":"baseline"}'
PATCH=$(cat "$ROOT/scenarios/n-plus-one/candidates/known-good-batched-join.json")
run_job trial-good "{\"kind\":\"trial\",\"patch\":$PATCH}"

python3 - "$OUT" <<'PY'
import json, sys, statistics
out = sys.argv[1]
base = json.load(open(f"{out}/baseline.json"))["result"]
trial = json.load(open(f"{out}/trial-good.json"))["result"]
print("\n== Cloudflare runner smoke result")
print("env:", base["env"])
print("baseline error:", base["execution_error"], "| build:", base["build"])
aa = [abs(p["baseline"]["p95_ms"] - p["candidate"]["p95_ms"]) / p["baseline"]["p95_ms"] for p in base["aa_pairs"] if p["baseline"]["p95_ms"]]
print("A/A noise per pair:", [f"{x:.1%}" for x in aa], "median:", f"{statistics.median(aa):.1%}" if aa else None)
print("trial error:", trial["execution_error"], "| candidate build ms:", (trial["build"] or {}).get("duration_ms"))
for p in trial["pairs"]:
    b, c = p["baseline"], p["candidate"]
    print(f"pair {p['index']}: baseline p95 {b['p95_ms']:.1f} ms, candidate p95 {c['p95_ms']:.1f} ms, dropped {b['dropped']}/{c['dropped']}")
print("timings:", trial["timings_ms"])
PY
echo "raw reports: $OUT"
