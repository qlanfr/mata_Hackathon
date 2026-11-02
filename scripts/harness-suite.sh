#!/usr/bin/env bash
# Runs every candidate listed in scenarios/n-plus-one/candidates/suite.json through the runner and
# checks each verdict, and the reason that decided it, against the expected ones. The candidates
# are hand-written harness checks, not model output: each one targets a single gate of the
# evaluator (policy, build, correctness, errors under load, improvement).
#
# Every trial starts cold, like a freshly started runner: the docker mode uses a new container per
# candidate, and the local mode a new work directory (only the cargo target directory is shared).
# A cold start once hid a stale-build bug, where the candidate's build returned the baseline
# binary; warm runs had passed.
#
#   scripts/harness-suite.sh                                  # local runner binary
#   RUNNER_IMAGE=aiops-runner:local scripts/harness-suite.sh  # inside the runner image, no network
#   ONLY=trap-slower scripts/harness-suite.sh                 # entries whose file name contains this
#
# Local mode needs what the runner needs: PostgreSQL server binaries, k6 (AIOPS_K6) and cargo.
# Exits non-zero when any verdict falls outside its expected set.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SUITE="scenarios/n-plus-one/candidates/suite.json"
OUT="${OUT:-$ROOT/.work/harness-suite}"
mkdir -p "$OUT"

run_trial() { # $1 = candidate path relative to the repository root, $2 = output JSON
  if [ -n "${RUNNER_IMAGE:-}" ]; then
    # The image carries its own copy of the scenario under /opt/aiops (its working directory).
    docker run --rm --network none "$RUNNER_IMAGE" aiops-runner trial --patch "$1" > "$2" 2> "${2%.json}.log"
  else
    local pg_user=() work="$OUT/work/$(basename "${2%.json}")"
    if [ "$(id -u)" = 0 ]; then pg_user=(--pg-user postgres); fi
    rm -rf "$work" && mkdir -p "$work" && chmod 777 "$work"
    local status=0
    AIOPS_WORK_DIR="$work" AIOPS_TARGET_DIR="${AIOPS_TARGET_DIR:-$ROOT/.work/target}" \
      "${RUNNER_BIN:-$ROOT/target/release/aiops-runner}" ${pg_user[@]+"${pg_user[@]}"} \
      trial --patch "$ROOT/$1" > "$2" 2> "${2%.json}.log" || status=$?
    rm -rf "$work"
    return $status
  fi
}

entries=$(python3 - "$ROOT/$SUITE" "${ONLY:-}" <<'PY'
import json, sys
for e in json.load(open(sys.argv[1])):
    if sys.argv[2] in e["file"]:
        # The reason is a phrase with spaces, so it goes last.
        print(e["file"], e["expect"], ",".join(e["tolerate"]) or "-", e["gate"], e["reason"])
PY
)

: > "$OUT/results.tsv"
while read -r file expect tolerate gate want_reason; do
  name="${file%.json}"
  started=$(date +%s)
  printf '%-34s ' "$name"
  if run_trial "scenarios/n-plus-one/candidates/$file" "$OUT/$name.json"; then
    verdict=$(python3 - "$OUT/$name.json" <<'PY'
import json, sys
e = json.load(open(sys.argv[1]))["evaluation"]
reason = (e["reasons"] or [""])[0].replace("\t", " ").replace("\n", " ")
print(e["outcome"] + "\t" + reason[:110])
PY
)
  else
    verdict=$'RUNNER_FAILED\tsee '"$OUT/$name.log"
  fi
  got="${verdict%%$'\t'*}"
  reason="${verdict#*$'\t'}"
  reason_ok=false  # want_reason may list alternatives separated by "|"
  IFS='|' read -ra wanted <<< "$want_reason"
  for w in "${wanted[@]}"; do [[ "$reason" == *"$w"* ]] && reason_ok=true; done
  if [ "$got" = "$expect" ] && $reason_ok; then
    status=PASS
  elif [ "$got" = "$expect" ]; then
    status=FAIL  # right verdict for the wrong reason: the intended gate did not fire
    reason="expected a reason containing '$want_reason'; got: $reason"
  elif [[ ",$tolerate," == *",$got,"* ]]; then
    status=TOLERATED  # e.g. a noisy machine made a timing-based verdict INCONCLUSIVE
  else
    status=FAIL
  fi
  secs=$(( $(date +%s) - started ))
  printf '%-10s %-16s %-16s %4ss  %s\n' "$status" "$expect" "$got" "$secs" "$reason"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$gate" "$expect" "$got" "$status" "$secs" "$reason" >> "$OUT/results.tsv"
done <<< "$entries"

python3 - "$OUT/results.tsv" "$OUT/summary.md" <<'PY'
import sys
rows = [line.rstrip("\n").split("\t") for line in open(sys.argv[1]) if line.strip()]
with open(sys.argv[2], "w") as f:
    f.write("| candidate | gate | expected | verdict | result | seconds | first reason |\n")
    f.write("| --- | --- | --- | --- | --- | ---: | --- |\n")
    for r in rows:
        f.write("| " + " | ".join(c.replace("|", "\\|") for c in r) + " |\n")
counts = {s: sum(1 for r in rows if r[4] == s) for s in ("PASS", "TOLERATED", "FAIL")}
print(f"\n{len(rows)} candidates: {counts['PASS']} pass, {counts['TOLERATED']} tolerated, {counts['FAIL']} fail")
print(f"table: {sys.argv[2]}")
sys.exit(1 if counts["FAIL"] else 0)
PY
