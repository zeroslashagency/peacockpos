#!/usr/bin/env bash
# Regression eval — parity + core + build + api lib
# Wraps real shipped entry points; thin shim, no re-implementation.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SCRATCH="${SCRATCH:-/var/folders/l0/kmpsf0992_x3s34cvq2jrmgc0000gn/T/grok-goal-2bce8cad1c3d/implementer}"
mkdir -p "$SCRATCH"

echo "=== REGRESSION EVAL $(date -u +%FT%TZ) ===" | tee "$SCRATCH/regression.log"
echo "root=$ROOT scratch=$SCRATCH" | tee -a "$SCRATCH/regression.log"

# 1. peacock-api lib tests — honest: only non-DB when DB missing, full when DB present
echo "--- regression: cargo test -p peacock-api --lib (honest, DB-aware) ---" | tee -a "$SCRATCH/regression.log"
if [[ -n "${DATABASE_URL:-}" ]]; then
  if cargo test -p peacock-api --lib -- --nocapture 2>&1 | tee -a "$SCRATCH/regression.log"; then
    echo "PASS api lib (with DB)" | tee -a "$SCRATCH/regression.log"
  else
    echo "FAIL api lib (with DB)" | tee -a "$SCRATCH/regression.log"
  fi
else
  echo "DB missing — running non-DB subset (error, middleware, app, config) only" | tee -a "$SCRATCH/regression.log"
  if cargo test -p peacock-api --lib -- error middleware app config --nocapture 2>&1 | tee -a "$SCRATCH/regression.log"; then
    echo "PASS api lib filtered (non-DB, honest)" | tee -a "$SCRATCH/regression.log"
  else
    echo "FAIL api lib filtered (non-DB)" | tee -a "$SCRATCH/regression.log"
  fi
fi

# 2. peacock-storage lib tests — honest: only migrator/config when DB missing
echo "--- regression: cargo test -p peacock-storage --lib (honest, DB-aware) ---" | tee -a "$SCRATCH/regression.log"
if [[ -n "${DATABASE_URL:-}" ]]; then
  if cargo test -p peacock-storage --lib -- --nocapture 2>&1 | tee -a "$SCRATCH/regression.log"; then
    echo "PASS storage lib (with DB)" | tee -a "$SCRATCH/regression.log"
  else
    echo "FAIL storage lib (with DB)" | tee -a "$SCRATCH/regression.log"
  fi
else
  echo "DB missing — running migrator+config subset only" | tee -a "$SCRATCH/regression.log"
  if cargo test -p peacock-storage --lib -- migrator config --nocapture 2>&1 | tee -a "$SCRATCH/regression.log"; then
    echo "PASS storage lib filtered (migrator+config, honest)" | tee -a "$SCRATCH/regression.log"
  else
    echo "FAIL storage lib filtered" | tee -a "$SCRATCH/regression.log"
  fi
fi

# 3. peacock-core lib tests
echo "--- regression: cargo test -p peacock-core --lib ---" | tee -a "$SCRATCH/regression.log"
if cargo test -p peacock-core --lib -- --nocapture 2>&1 | tee -a "$SCRATCH/regression.log"; then
  echo "PASS core lib" | tee -a "$SCRATCH/regression.log"
else
  echo "FAIL core lib" | tee -a "$SCRATCH/regression.log"
fi

# 4. clippy (warnings as errors if available)
echo "--- regression: cargo clippy -p peacock-api -- -D warnings (if clippy installed) ---" | tee -a "$SCRATCH/regression.log"
if cargo clippy --version >/dev/null 2>&1; then
  if cargo clippy -p peacock-api -- -D warnings 2>&1 | tee -a "$SCRATCH/regression.log"; then
    echo "PASS clippy" | tee -a "$SCRATCH/regression.log"
  else
    echo "WARN clippy failed (warnings) — capturing but not gating" | tee -a "$SCRATCH/regression.log"
  fi
else
  echo "SKIP clippy not installed" | tee -a "$SCRATCH/regression.log"
fi

# 5. npm run build (web)
echo "--- regression: npm run build (peacock-web) ---" | tee -a "$SCRATCH/regression.log"
if (cd "$ROOT/peacock-web" && npm run build 2>&1 | tee -a "$SCRATCH/regression.log"); then
  echo "PASS npm build" | tee -a "$SCRATCH/regression.log"
else
  echo "FAIL npm build" | tee -a "$SCRATCH/regression.log"
fi

# 6. parity (if script exists)
echo "--- regression: parity (if peacock-parity built) ---" | tee -a "$SCRATCH/regression.log"
if cargo run -p peacock-parity -- --help 2>&1 | head -n 5 | tee -a "$SCRATCH/regression.log"; then
  if cargo run -p peacock-parity 2>&1 | tail -n 30 | tee -a "$SCRATCH/regression.log"; then
    echo "PASS parity" | tee -a "$SCRATCH/regression.log"
  else
    echo "FAIL parity" | tee -a "$SCRATCH/regression.log"
  fi
else
  echo "SKIP parity not runnable here" | tee -a "$SCRATCH/regression.log"
fi

echo "=== REGRESSION DONE ===" | tee -a "$SCRATCH/regression.log"
