#!/usr/bin/env bash
# Builds both container images and drives one full run through them with Docker.
#
#   scripts/container-e2e.sh            # build, then run
#   SKIP_BUILD=1 scripts/container-e2e.sh
#
# The setup mirrors Cloudflare as closely as plain Docker can:
# - The runner sits on an internal network with no route out (enableInternet=false).
# - aiopsd reaches the runner by name, as it does through the runner.internal relay.
# - SQLite and a local directory stand in for D1 and R2, which only exist behind the Worker.
# - With no META_API_KEY the run uses the replay provider.
#
# It exits non-zero unless the run reaches COMPLETED with a VERIFIED trial.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${PORT:-18190}"
TIMEOUT_S="${TIMEOUT_S:-1800}"
OUT="${OUT:-$ROOT/.work/container-e2e}"
NET=aiops-e2e-net
RUNNER=aiops-e2e-runner
AIOPSD=aiops-e2e-aiopsd
mkdir -p "$OUT"

if [ "${SKIP_BUILD:-}" != 1 ]; then
  for image in runner aiopsd; do
    started=$(date +%s)
    docker build -f "$ROOT/deploy/Dockerfile.$image" -t "aiops-$image:local" "$ROOT"
    echo "built aiops-$image in $(( $(date +%s) - started ))s"
  done
fi
docker image ls --format '{{.Repository}}:{{.Tag}} {{.Size}}' | grep '^aiops-' || true

cleanup() {
  docker logs "$RUNNER" > "$OUT/runner.log" 2>&1 || true
  docker logs "$AIOPSD" > "$OUT/aiopsd.log" 2>&1 || true
  docker rm -f "$RUNNER" "$AIOPSD" > /dev/null 2>&1 || true
  docker network rm "$NET" > /dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

docker network create --internal "$NET" > /dev/null
docker run -d --name "$RUNNER" --network "$NET" aiops-runner:local > /dev/null
docker run -d --name "$AIOPSD" -p "127.0.0.1:$PORT:8080" \
  -e AIOPS_DB=sqlite:/tmp/aiops/control.db \
  -e AIOPS_ARTIFACTS=fs:/tmp/aiops/artifacts \
  -e AIOPS_RUNNER_URL="http://$RUNNER:8080" \
  aiops-aiopsd:local > /dev/null
docker network connect "$NET" "$AIOPSD"

if docker exec "$RUNNER" curl -sS -m 5 -o /dev/null https://github.com 2> /dev/null; then
  echo "FAIL: the runner container reached the internet" >&2
  exit 1
fi
echo "ok: the runner has no internet access"

base="http://127.0.0.1:$PORT"
for _ in $(seq 60); do
  if curl -fsS "$base/healthz" > "$OUT/healthz.json" 2> /dev/null; then break; fi
  sleep 1
done
echo "aiopsd health: $(cat "$OUT/healthz.json")"

run_id=$(curl -fsS -X POST "$base/api/v1/runs" -H 'content-type: application/json' -d '{}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["run_id"])')
echo "run $run_id"

started=$(date +%s)
state=""
while true; do
  curl -fsS "$base/api/v1/runs/$run_id" > "$OUT/run.json"
  state=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["run"]["state"])' "$OUT/run.json")
  elapsed=$(( $(date +%s) - started ))
  echo "[${elapsed}s] $state"
  case "$state" in COMPLETED|EXHAUSTED|FAILED|CANCELLED) break ;; esac
  if [ "$elapsed" -gt "$TIMEOUT_S" ]; then echo "FAIL: timed out in $state" >&2; exit 1; fi
  sleep 15
done

curl -fsS "$base/api/v1/runs/$run_id/events?after_seq=0" > "$OUT/events.json"
python3 - "$OUT" <<'PY'
import json, sys
out = sys.argv[1]
detail = json.load(open(f"{out}/run.json"))
events = json.load(open(f"{out}/events.json"))["events"]
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
ok = run["state"] == "COMPLETED" and any(t["outcome"] == "VERIFIED" for t in detail["trials"])
print("\nRESULT:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
PY
