#!/usr/bin/env bash
# Starts one run on an aiopsd API, waits for it to finish and prints its events and verdicts.
# Works against local aiopsd, the Docker setup (scripts/container-e2e.sh) or the deployed Worker:
#
#   BASE_URL=https://aiops-edge.<subdomain>.workers.dev ADMIN_TOKEN=... scripts/drive-run.sh
#
# By default it exits non-zero unless the run reaches COMPLETED with a VERIFIED trial. With
# ACCEPT=finished, COMPLETED or EXHAUSTED both pass: a real model may legitimately finish without a
# verified change. EXPECT_PROVIDER (for example `meta:muse-spark-1.3` or `replay`) fails the run
# check when aiopsd used another provider. RUN_MODEL asks aiopsd for a specific Meta model for this
# run (the run's provider is then `meta:<RUN_MODEL>`). Polling also keeps the Cloudflare containers
# awake. After the run, every trial's report artifact is fetched back through the API, and the
# cron ticks aiopsd received during the run are reported.
set -euo pipefail

: "${BASE_URL:?set BASE_URL, e.g. http://127.0.0.1:18090}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-$ROOT/.work/drive-run}"
TIMEOUT_S="${TIMEOUT_S:-1800}"
STARTUP_S="${STARTUP_S:-600}"
POLL_S="${POLL_S:-15}"
BASE_URL="${BASE_URL%/}"
mkdir -p "$OUT"

auth=()
if [ -n "${ADMIN_TOKEN:-}" ]; then auth=(-H "authorization: Bearer $ADMIN_TOKEN"); fi
get() { curl -fsS -m 30 "$@"; }

# On Cloudflare the first request starts the aiopsd container, which can take minutes on a fresh
# deploy. Wait until the API answers before starting the run, so the POST is sent exactly once.
started=$(date +%s)
until get "$BASE_URL/api/v1/runs" > "$OUT/runs-before.json" 2> "$OUT/startup.err"; do
  if [ $(( $(date +%s) - started )) -gt "$STARTUP_S" ]; then
    echo "FAIL: $BASE_URL/api/v1/runs did not answer within ${STARTUP_S}s: $(cat "$OUT/startup.err")" >&2
    exit 1
  fi
  sleep 10
done
echo "API ready after $(( $(date +%s) - started ))s"

# Right after a deploy the old aiopsd may still answer. With EXPECT_PROVIDER set, wait until the
# instance answering is the one configured for it, so the run is not created on the old one.
# With RUN_MODEL, the instance must be on the Meta provider (any default model).
WAIT_FOR="${EXPECT_PROVIDER:-}"
if [ -n "${RUN_MODEL:-}" ]; then WAIT_FOR="meta:"; fi
if [ -n "$WAIT_FOR" ]; then
  until get "$BASE_URL/api/v1/info" > "$OUT/info.json" 2> /dev/null \
    && python3 -c 'import json,sys; p=json.load(open(sys.argv[1])).get("provider",""); sys.exit(0 if p == sys.argv[2] or (sys.argv[2].endswith(":") and p.startswith(sys.argv[2])) else 1)' \
      "$OUT/info.json" "$WAIT_FOR"; do
    if [ $(( $(date +%s) - started )) -gt "$STARTUP_S" ]; then
      echo "FAIL: aiopsd still reports $(cat "$OUT/info.json" 2> /dev/null || echo nothing), expected provider $WAIT_FOR" >&2
      exit 1
    fi
    sleep 10
  done
  echo "aiopsd reports: $(cat "$OUT/info.json")"
fi
cron_before=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("cron_ticks", 0))' "$OUT/info.json" 2> /dev/null || echo 0)

body='{}'
if [ -n "${RUN_MODEL:-}" ]; then
  body=$(python3 -c 'import json,sys; print(json.dumps({"model": sys.argv[1]}))' "$RUN_MODEL")
  EXPECT_PROVIDER="${EXPECT_PROVIDER:-meta:$RUN_MODEL}"
fi

# One more run is started in exactly two cases, both about the environment rather than the change:
# - Right after a deploy, old containers can still serve while new ones roll out. If the rollout
#   changed the scenario, aiopsd stops the run ("scenario changed during the run").
# - Every trial was INCONCLUSIVE (the machine was too noisy to judge) and the run ran out of
#   candidates. A second noisy run still fails.
for attempt in 1 2; do
  run_id=$(curl -fsS -m 60 -X POST "$BASE_URL/api/v1/runs" ${auth[@]+"${auth[@]}"} \
    -H 'content-type: application/json' -d "$body" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["run_id"])')
  echo "run $run_id  ($BASE_URL/runs/$run_id)"

  started=$(date +%s)
  last=""
  while true; do
    elapsed=$(( $(date +%s) - started ))
    if get "$BASE_URL/api/v1/runs/$run_id" > "$OUT/run.json.tmp" 2> "$OUT/poll.err"; then
      mv "$OUT/run.json.tmp" "$OUT/run.json"
      state=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["run"]["state"])' "$OUT/run.json")
      if [ "$state" != "$last" ]; then echo "[${elapsed}s] $state"; last=$state; fi
      case "$state" in COMPLETED|EXHAUSTED|FAILED|CANCELLED) break ;; esac
    else
      echo "[${elapsed}s] poll failed: $(cat "$OUT/poll.err")"
    fi
    if [ "$elapsed" -gt "$TIMEOUT_S" ]; then echo "FAIL: timed out in state $last" >&2; exit 1; fi
    sleep "$POLL_S"
  done

  if [ "$attempt" = 1 ]; then
    retry=$(python3 - "$OUT/run.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
run, trials = d["run"], d["trials"]
if run["state"] == "FAILED" and "scenario changed during the run" in (run["error"] or ""):
    print("the scenario changed during the rollout")
elif run["state"] == "EXHAUSTED" and trials and all(t["outcome"] == "INCONCLUSIVE" for t in trials):
    print("every trial was INCONCLUSIVE (noisy machine)")
PY
)
    if [ -n "$retry" ]; then
      echo "$retry; starting one more run"
      cp "$OUT/run.json" "$OUT/run-attempt1.json"
      sleep 30
      continue
    fi
  fi
  break
done

# Collect every event page.
# Requests go through curl like the rest of this script: on workers.dev, Cloudflare answered
# Python's default urllib client with 403.
python3 - "$BASE_URL" "$run_id" "$OUT" <<'PY'
import json, subprocess, sys
base, run_id, out = sys.argv[1:4]
events, after = [], 0
while True:
    url = f"{base}/api/v1/runs/{run_id}/events?after_seq={after}"
    raw = subprocess.run(["curl", "-fsS", "-m", "30", url], check=True, capture_output=True, text=True).stdout
    page = json.loads(raw)["events"]
    if not page:
        break
    events += page
    after = page[-1]["seq"]
json.dump(events, open(f"{out}/events.json", "w"), ensure_ascii=False, indent=1)
PY

# Fetch every trial's report back through the API: on Cloudflare this reads D1 or R2, depending
# on the artifact's size and the configured inline limit.
for id in $(python3 -c 'import json,sys; [print(t["report_artifact_id"]) for t in json.load(open(sys.argv[1]))["trials"] if t.get("report_artifact_id")]' "$OUT/run.json"); do
  if get "$BASE_URL/api/v1/artifacts/$id" > "$OUT/artifact-$id.json" 2> "$OUT/artifact.err" \
    && python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert d["trial_id"]' "$OUT/artifact-$id.json" 2> /dev/null; then
    echo "artifact $id: fetched ($(wc -c < "$OUT/artifact-$id.json") bytes)"
  else
    echo "artifact $id: FETCH FAILED: $(cat "$OUT/artifact.err")" | tee "$OUT/artifact-failed"
  fi
done
get "$BASE_URL/api/v1/info" > "$OUT/info-after.json" 2> /dev/null || echo '{}' > "$OUT/info-after.json"

ACCEPT="${ACCEPT:-verified}" EXPECT_PROVIDER="${EXPECT_PROVIDER:-}" CRON_BEFORE="$cron_before" python3 - "$OUT" <<'PY'
import json, os, sys
out = sys.argv[1]
detail = json.load(open(f"{out}/run.json"))
events = json.load(open(f"{out}/events.json"))
run = detail["run"]
print("\n== events")
for e in events:
    payload = json.dumps(e["payload"], ensure_ascii=False)
    print(f"{e['seq']:>3} {e['kind']:<22} {payload[:160]}")
print("\n== run")
print("state:", run["state"], "| provider:", run["provider"], "| aa_noise:", run["aa_noise"])
print("summary:", run["summary"], "| error:", run["error"])
for t in detail["trials"]:
    ev = t["evaluation"] or {}
    imp = [f"{x:.1%}" for x in ev.get("improvements") or []]
    print(f"trial {t['seq']}: {t['status']} {t['outcome']} improvements={imp} reasons={ev.get('reasons')}")
verified = run["state"] == "COMPLETED" and any(t["outcome"] == "VERIFIED" for t in detail["trials"])
if os.environ["ACCEPT"] == "finished":
    ok = run["state"] in ("COMPLETED", "EXHAUSTED")
else:
    ok = verified
want = os.environ["EXPECT_PROVIDER"]
if want and run["provider"] != want:
    print(f"\nprovider is {run['provider']}, expected {want}")
    ok = False
if os.path.exists(f"{out}/artifact-failed"):
    print("\na trial report could not be fetched back through the API")
    ok = False
info = json.load(open(f"{out}/info-after.json"))
cron = info.get("cron_ticks", 0) - int(os.environ["CRON_BEFORE"])
usage = run.get("usage") or {}
seconds = ((run.get("finished_at") or run["updated_at"]) - run["created_at"]) / 1000
summary = (f"{run['provider']} | {run['state']} | trials: " +
           ", ".join(f"{t['seq']}:{t['outcome']}" for t in detail["trials"]) +
           f" | candidates {usage.get('candidates')} | turns {usage.get('model_turns')} | tool calls {usage.get('tool_calls')}"
           f" | tokens in {usage.get('input_tokens')} out {usage.get('output_tokens')} | {seconds:.0f}s | cron ticks during run: {cron}")
print("\nSUMMARY:", summary)
with open(f"{out}/summary.txt", "w") as f:
    f.write(summary + "\n")
print("RESULT:", "PASS" if ok else "FAIL", f"(accept={os.environ['ACCEPT']}, verified={verified})")
sys.exit(0 if ok else 1)
PY
