#!/usr/bin/env bash
# Probe: does a real, documented N+1 bug reproduce in a setup like the runner's (service and
# PostgreSQL on one machine)? The bug is Vaultwarden issue #1402, "N+1 query problem on
# api/sync", fixed by PR #2429 (commit 3ca8502, merged 2022-05-11). This builds the commit before
# the fix and the fix commit, seeds the same vault into each through the public API, and measures
# GET /api/sync: SQL statements per request (pg_stat_statements) and latency.
#
# Runs in GitHub Actions (.github/workflows/real-bug-probe.yml); needs Docker and network access.
# Vaultwarden is GPL-3.0 and is cloned at build time, never copied into this repository.
set -euo pipefail

BASE_SHA=3abf173d8954c982383285b29b39d15043e974cb   # parent of the fix: has the N+1
FIX_SHA=3ca85028ea99ed43c99847b18e222f25ca47374e    # "Improve sync speed and updated dep. versions"
SIZES="${SIZES:-100 1000}"                           # ciphers in the vault
FOLDERS="${FOLDERS:-20}"
REPEAT="${REPEAT:-10}"                               # measured syncs per size
OUT="${OUT:-$PWD/.work/real-bug}"
mkdir -p "$OUT"
WORK="$(mktemp -d)"

log() { echo "[$(date -u +%H:%M:%S)] $*"; }

# Rust 1.60 matches the toolchain of the time; newer compilers reject some of the locked crates.
log "build image"
docker build -q -t vw-build - <<'EOF' > /dev/null
FROM rust:1.60-bullseye
# The buildpack-deps base already ships libpq. Debian 11 has moved to archive.debian.org, so
# install from there only if it is missing.
RUN test -e /usr/include/postgresql/libpq-fe.h || ( \
      printf 'deb http://archive.debian.org/debian bullseye main\ndeb http://archive.debian.org/debian-security bullseye-security main\n' > /etc/apt/sources.list \
   && apt-get -o Acquire::Check-Valid-Until=false update \
   && apt-get install -y --no-install-recommends libpq-dev && rm -rf /var/lib/apt/lists/* )
EOF

git clone -q https://github.com/dani-garcia/vaultwarden "$WORK/vw"
for v in base fix; do
  sha=BASE_SHA; [ "$v" = fix ] && sha=FIX_SHA
  git -C "$WORK/vw" worktree add -q "$WORK/$v" "${!sha}"
  log "cargo build $v (${!sha:0:12})"
  # The repository's rust-toolchain file says "stable", which would pull today's compiler; the
  # environment variable takes precedence and keeps the 1.60 compiler of the image.
  docker run --rm -e RUSTUP_TOOLCHAIN=1.60.0 -v "$WORK/$v:/src" -v vw-cargo:/usr/local/cargo/registry -w /src vw-build \
    cargo build --release --locked --features postgresql > "$OUT/build-$v.log" 2>&1 \
    || { tail -40 "$OUT/build-$v.log"; exit 1; }
  log "built $v: $(tail -1 "$OUT/build-$v.log")"
done

docker network create vw > /dev/null
docker run -d --name pg --network vw -e POSTGRES_PASSWORD=pw postgres:15 \
  -c shared_preload_libraries=pg_stat_statements -c pg_stat_statements.track=all > /dev/null
until docker exec pg pg_isready -U postgres -q; do sleep 1; done
sleep 2
psql() { docker exec -i pg psql -U postgres -qAt "$@"; }
psql -c "CREATE EXTENSION pg_stat_statements" > /dev/null

echo "| version | ciphers | SQL per sync | median sync ms | p95 sync ms | ciphers returned |" > "$OUT/summary.md"
echo "| --- | --- | --- | --- | --- | --- |" >> "$OUT/summary.md"

port=8100
for v in base fix; do
  for n in $SIZES; do
    port=$((port + 1))
    db="vw_${v}_${n}"
    psql -c "CREATE DATABASE $db" > /dev/null
    name="vw-$v-$n"
    docker run -d --name "$name" --network vw -p "$port:80" -v "$WORK/$v:/src:ro" vw-build bash -c \
      "mkdir -p /data && cd /tmp && exec env DATABASE_URL=postgresql://postgres:pw@pg/$db ROCKET_ADDRESS=0.0.0.0 ROCKET_PORT=80 \
       WEB_VAULT_ENABLED=false DATA_FOLDER=/data LOGIN_RATELIMIT_MAX_BURST=100 DATABASE_MAX_CONNS=4 /src/target/release/vaultwarden" > /dev/null
    url="http://127.0.0.1:$port"
    for _ in $(seq 60); do curl -fsS -o /dev/null "$url/alive" 2> /dev/null && break; sleep 1; done
    curl -fsS -o /dev/null "$url/alive" || { docker logs "$name" | tail -30; exit 1; }

    # The server stores whatever the client sends as the master-password hash and the encrypted
    # fields; it never decrypts them. Fixed strings therefore give a valid, deterministic vault.
    enc='2.AAAAAAAAAAAAAAAAAAAAAA==|AAAAAAAAAAAAAAAAAAAAAA==|AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA='
    curl -fsS -o /dev/null "$url/api/accounts/register" -H 'content-type: application/json' -d @- <<EOF
{"email":"probe@example.com","name":"probe","masterPasswordHash":"probe-hash","key":"$enc","kdf":0,"kdfIterations":100000,
 "keys":{"encryptedPrivateKey":"$enc","publicKey":"probe-public-key"}}
EOF
    token=$(curl -fsS "$url/identity/connect/token" \
      --data-urlencode grant_type=password --data-urlencode username=probe@example.com --data-urlencode password=probe-hash \
      --data-urlencode "scope=api offline_access" --data-urlencode client_id=web --data-urlencode device_type=8 \
      --data-urlencode device_identifier=00000000-0000-4000-8000-000000000001 --data-urlencode device_name=probe \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')
    auth=(-H "authorization: Bearer $token")

    folders=()
    for i in $(seq "$FOLDERS"); do
      folders+=("$(curl -fsS "${auth[@]}" "$url/api/folders" -H 'content-type: application/json' -d "{\"name\":\"$enc\"}" \
        | python3 -c 'import json,sys; print(json.load(sys.stdin)["Id"])')")
    done
    log "$v n=$n: seeding"
    for i in $(seq "$n"); do
      folder="${folders[$((i % FOLDERS))]}"
      fav=false; [ $((i % 7)) = 0 ] && fav=true
      printf '{"type":1,"name":"%s","notes":null,"favorite":%s,"folderId":"%s","reprompt":0,"login":{"username":"%s","password":"%s","uris":[{"uri":"%s","match":null}]}}' \
        "$enc" "$fav" "$folder" "$enc" "$enc" "$enc"
      echo
    done | xargs -P 8 -d '\n' -I{} curl -fsS -o /dev/null "${auth[@]}" "$url/api/ciphers" -H 'content-type: application/json' -d '{}'

    curl -fsS -o /dev/null "${auth[@]}" "$url/api/sync?excludeDomains=true"   # warm-up
    psql -c "SELECT pg_stat_statements_reset()" > /dev/null
    : > "$OUT/times-$v-$n.txt"
    for _ in $(seq "$REPEAT"); do
      curl -fsS -o "$OUT/sync-$v-$n.json" -w '%{time_total}\n' "${auth[@]}" "$url/api/sync?excludeDomains=true" >> "$OUT/times-$v-$n.txt"
    done
    calls=$(psql -c "SELECT COALESCE(SUM(s.calls),0) FROM pg_stat_statements s JOIN pg_database d ON d.oid = s.dbid WHERE d.datname = '$db'")
    psql -c "SELECT s.calls, left(regexp_replace(s.query, '\s+', ' ', 'g'), 160) FROM pg_stat_statements s JOIN pg_database d ON d.oid = s.dbid WHERE d.datname = '$db' ORDER BY s.calls DESC LIMIT 12" > "$OUT/top-queries-$v-$n.txt"
    python3 - "$OUT" "$v" "$n" "$calls" "$REPEAT" >> "$OUT/summary.md" <<'PY'
import json, statistics, sys
out, v, n, calls, repeat = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
times = sorted(float(x) * 1000 for x in open(f"{out}/times-{v}-{n}.txt").read().split())
p95 = times[min(len(times) - 1, int(round(0.95 * (len(times) - 1))))]
returned = len(json.load(open(f"{out}/sync-{v}-{n}.json"))["Ciphers"])
print(f"| {v} | {n} | {calls / repeat:.0f} | {statistics.median(times):.1f} | {p95:.1f} | {returned} |")
PY
    log "$(tail -1 "$OUT/summary.md")"
    docker rm -f "$name" > /dev/null
  done
done

cat "$OUT/summary.md"
