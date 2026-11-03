#!/usr/bin/env bash
# Prepares scenarios/vaultwarden-sync on this machine:
#   1. clones Vaultwarden at the commit before the fix of issue #1402 into external/vaultwarden
#      (GPL-3.0, never committed here) and fetches its dependencies, so builds can run offline;
#   2. builds it the way the runner will (same target directory, toolchain, profile, features),
#      so the runner's own baseline build only recompiles the Vaultwarden crate;
#   3. starts it on a scratch PostgreSQL, registers one user and creates folders and vault items
#      through the public API, and dumps that database to external/vaultwarden-seed.sql, the
#      state every measurement starts from.
#
# Environment: AIOPS_TARGET_DIR (the runner's cargo target directory), AIOPS_PG_BIN (PostgreSQL
# server binaries), CIPHERS (default 500), FOLDERS (default 20).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
SHA=3abf173d8954c982383285b29b39d15043e974cb
SRC="$ROOT/external/vaultwarden"
TARGET_DIR="${AIOPS_TARGET_DIR:-$ROOT/.work/target}"
PG_BIN="${AIOPS_PG_BIN:-$(pg_config --bindir)}"
CIPHERS="${CIPHERS:-500}"
FOLDERS="${FOLDERS:-20}"
PG_PORT=55499
VW_PORT=8199
# Must match build_env and cargo_args in scenarios/vaultwarden-sync/scenario.toml.
export RUSTUP_TOOLCHAIN=1.60.0 CARGO_PROFILE_RELEASE_LTO=false

log() { echo "[$(date -u +%H:%M:%S)] $*"; }

mkdir -p "$ROOT/external"
if [ ! -d "$SRC/.git" ]; then
  git clone -q https://github.com/dani-garcia/vaultwarden "$SRC"
fi
git -C "$SRC" checkout -q "$SHA"
log "fetching dependencies"
(cd "$SRC" && cargo fetch --locked)
log "building vaultwarden $(git -C "$SRC" rev-parse --short HEAD) with $(rustc --version)"
CARGO_TARGET_DIR="$TARGET_DIR" cargo build --release --locked --offline --features postgresql \
  --manifest-path "$SRC/Cargo.toml"
BIN="$TARGET_DIR/release/vaultwarden"

TMP="$(mktemp -d)"
cleanup() {
  [ -n "${VW_PID:-}" ] && kill "$VW_PID" 2> /dev/null || true
  "$PG_BIN/pg_ctl" -D "$TMP/pg" -m fast -w stop > /dev/null 2>&1 || true
}
trap cleanup EXIT

log "seeding: $FOLDERS folders, $CIPHERS vault items"
"$PG_BIN/initdb" -D "$TMP/pg" -U aiops --auth=trust --no-locale -E UTF8 > "$TMP/initdb.log"
"$PG_BIN/pg_ctl" -D "$TMP/pg" -l "$TMP/pg.log" -w \
  -o "-p $PG_PORT -c listen_addresses=127.0.0.1 -c unix_socket_directories=$TMP" start > /dev/null
psql() { "$PG_BIN/psql" -X -q -v ON_ERROR_STOP=1 -h 127.0.0.1 -p "$PG_PORT" -U aiops "$@"; }
psql -d postgres -c "CREATE DATABASE vaultwarden"
mkdir -p "$TMP/data"
env -i PATH="$PATH" DATABASE_URL="postgresql://aiops@127.0.0.1:$PG_PORT/vaultwarden" \
  ROCKET_ADDRESS=127.0.0.1 ROCKET_PORT=$VW_PORT DATA_FOLDER="$TMP/data" ENV_FILE="$TMP/none.env" \
  WEB_VAULT_ENABLED=false LOG_LEVEL=warn LOGIN_RATELIMIT_MAX_BURST=100 \
  "$BIN" > "$TMP/vaultwarden.log" 2>&1 &
VW_PID=$!
url="http://127.0.0.1:$VW_PORT"
for _ in $(seq 60); do curl -fsS -o /dev/null "$url/alive" 2> /dev/null && break; sleep 1; done
curl -fsS -o /dev/null "$url/alive" || { tail -30 "$TMP/vaultwarden.log"; exit 1; }

# The server stores whatever the client sends as the master-password hash and as the encrypted
# fields; it never decrypts them. Fixed strings therefore give a valid, deterministic vault.
enc='2.AAAAAAAAAAAAAAAAAAAAAA==|AAAAAAAAAAAAAAAAAAAAAA==|AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA='
curl -fsS -o /dev/null "$url/api/accounts/register" -H 'content-type: application/json' -d @- << EOF
{"email":"probe@example.com","name":"probe","masterPasswordHash":"probe-hash","key":"$enc","kdf":0,"kdfIterations":100000,
 "keys":{"encryptedPrivateKey":"$enc","publicKey":"probe-public-key"}}
EOF
token=$(curl -fsS "$url/identity/connect/token" \
  --data-urlencode grant_type=password --data-urlencode username=probe@example.com --data-urlencode password=probe-hash \
  --data-urlencode "scope=api offline_access" --data-urlencode client_id=web --data-urlencode device_type=8 \
  --data-urlencode device_identifier=00000000-0000-4000-8000-000000000001 --data-urlencode device_name=seed \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')
auth=(-H "authorization: Bearer $token")
folders=()
for _ in $(seq "$FOLDERS"); do
  folders+=("$(curl -fsS "${auth[@]}" "$url/api/folders" -H 'content-type: application/json' -d "{\"name\":\"$enc\"}" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["Id"])')")
done
for i in $(seq "$CIPHERS"); do
  folder="${folders[$((i % FOLDERS))]}"
  fav=false; [ $((i % 7)) = 0 ] && fav=true
  printf '{"type":1,"name":"%s","notes":null,"favorite":%s,"folderId":"%s","reprompt":0,"login":{"username":"%s","password":"%s","uris":[{"uri":"%s","match":null}]}}\n' \
    "$enc" "$fav" "$folder" "$enc" "$enc" "$enc"
done | xargs -P 8 -d '\n' -I{} curl -fsS -o /dev/null "${auth[@]}" "$url/api/ciphers" -H 'content-type: application/json' -d '{}'
count=$(curl -fsS "${auth[@]}" "$url/api/sync?excludeDomains=true" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["Ciphers"]))')
[ "$count" = "$CIPHERS" ] || { echo "seeded $count vault items, expected $CIPHERS"; exit 1; }
kill "$VW_PID"; wait "$VW_PID" 2> /dev/null || true; VW_PID=

"$PG_BIN/pg_dump" -h 127.0.0.1 -p "$PG_PORT" -U aiops --no-owner --no-privileges --no-comments vaultwarden \
  > "$ROOT/external/vaultwarden-seed.sql"
log "seed: $(wc -c < "$ROOT/external/vaultwarden-seed.sql") bytes, $count vault items"
