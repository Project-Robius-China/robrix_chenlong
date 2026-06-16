#!/usr/bin/env bash
# ============================================================
# Robrix + Palpo + Octos — Start Services (native binaries)
# ============================================================
# Runs palpo and octos as background processes.
# Postgres must already be running before calling this script.
# Run setup.sh first if this is a fresh checkout.
#
# Usage:  ./start.sh
# Stop:   Ctrl+C  (sends SIGTERM to all children)
# Logs:   logs/palpo.log  logs/octos.log
# ============================================================
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"

# ---- Load .env ----
if [ ! -f "$DIR/.env" ]; then
  echo "ERROR: .env not found. Run ./setup.sh first."
  exit 1
fi
set -a; source "$DIR/.env"; set +a

mkdir -p "$DIR/logs" "$DIR/data/octos/profiles"

# ---- Cleanup on Ctrl+C / SIGTERM ----
cleanup() {
  echo ""
  echo "==> Stopping services..."
  if [ -f "$DIR/.palpo.pid" ]; then
    kill "$(cat "$DIR/.palpo.pid")" 2>/dev/null || true
    rm -f "$DIR/.palpo.pid"
  fi
  if [ -f "$DIR/.octos.pid" ]; then
    kill "$(cat "$DIR/.octos.pid")" 2>/dev/null || true
    rm -f "$DIR/.octos.pid"
  fi
  # Kill any stray octos gateway processes (releases redb lock)
  pkill -f "octos gateway" 2>/dev/null || true
  echo "    Done."
}
trap cleanup INT TERM

# Kill any leftover octos from a previous run so redb lock is free
pkill -f "octos gateway" 2>/dev/null || true
sleep 0.5

# ---- Generate native palpo config ----
# Patches Docker-specific values from palpo.toml:
#   appservice_registration_dir  /var/palpo/appservices → absolute local path
#   db.url hostname              palpo_postgres          → localhost
#   listener port                0.0.0.0:8008            → 0.0.0.0:8128
PALPO_NATIVE_CONFIG="$DIR/palpo-native.toml"
sed \
  -e "s|/var/palpo/appservices|$DIR/appservices|g" \
  -e "s|@palpo_postgres:|@localhost:|g" \
  -e "s|0\.0\.0\.0:8008|0.0.0.0:8128|g" \
  "$DIR/palpo.toml" > "$PALPO_NATIVE_CONFIG"

# ---- Palpo ----
echo "==> Starting palpo (log: logs/palpo.log)..."
NO_PROXY="127.0.0.1,localhost" no_proxy="127.0.0.1,localhost" \
"$DIR/palpo" -c "$PALPO_NATIVE_CONFIG" >> "$DIR/logs/palpo.log" 2>&1 &
echo $! > "$DIR/.palpo.pid"
echo "    PID $(cat "$DIR/.palpo.pid")"

# ---- Octos ----
OCTOS_BIN="$DIR/octos"
if [ ! -f "$OCTOS_BIN" ]; then
  echo "==> Building octos with matrix support (first run, this may take a few minutes)..."
  cargo build --release --bin octos -p octos-cli --features matrix \
    --manifest-path "$DIR/repos/octos/Cargo.toml" \
    2>&1 | tee "$DIR/logs/octos-build.log"
  cp "$DIR/repos/octos/target/release/octos" "$OCTOS_BIN"
fi

echo "==> Starting octos gateway (log: logs/octos.log)..."
DEEPSEEK_API_KEY="${DEEPSEEK_API_KEY:-}" \
RUST_LOG="${RUST_LOG:-octos=debug,info}" \
NO_PROXY="127.0.0.1,localhost" no_proxy="127.0.0.1,localhost" \
HTTP_PROXY="" \
HTTPS_PROXY="" \
"$OCTOS_BIN" gateway \
  --profile "$DIR/config/botfather.json" \
  --data-dir "$DIR/data/octos" \
  >> "$DIR/logs/octos.log" 2>&1 &
echo $! > "$DIR/.octos.pid"
echo "    PID $(cat "$DIR/.octos.pid")"

echo ""
echo "All services running."
echo "  Palpo (Matrix):  http://localhost:8128"
echo "  Logs:            logs/palpo.log | logs/octos.log"
echo ""
echo "Press Ctrl+C to stop all services."
wait
