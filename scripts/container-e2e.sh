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
# scripts/drive-run.sh starts the run and exits non-zero unless it reaches COMPLETED with a
# VERIFIED trial.
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

BASE_URL="http://127.0.0.1:$PORT" OUT="$OUT" TIMEOUT_S="$TIMEOUT_S" STARTUP_S=120 POLL_S=10 \
  "$ROOT/scripts/drive-run.sh"
