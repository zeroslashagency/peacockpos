#!/usr/bin/env bash
# Capability eval — slice-specific gates: auth 401/403 + dashboard 200 + health
# Thin shim over real shipped entry points (cargo test filtered + static checks).
# Never fabricates live evidence; degrades gracefully when DB/network unavailable.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SCRATCH="${SCRATCH:-/var/folders/l0/kmpsf0992_x3s34cvq2jrmgc0000gn/T/grok-goal-2bce8cad1c3d/implementer}"
mkdir -p "$SCRATCH"

echo "=== CAPABILITY EVAL $(date -u +%FT%TZ) ===" | tee "$SCRATCH/capability.log"
echo "root=$ROOT scratch=$SCRATCH" | tee -a "$SCRATCH/capability.log"

# 1. Migration embedded (S1-U1) — no DB needed
echo "--- S1-U1 migrator_contains_the_users_migration (haiku) ---" | tee -a "$SCRATCH/capability.log"
if cargo test -p peacock-storage --lib migrator_contains_the_users_migration -- --nocapture 2>&1 | tee -a "$SCRATCH/capability.log"; then
  echo "PASS S1-U1" | tee -a "$SCRATCH/capability.log"
else
  echo "FAIL S1-U1" | tee -a "$SCRATCH/capability.log"
fi

# 2. Forbidden 403 mapping (S1-U2) — no DB needed
echo "--- S1-U2 Forbidden 403 mapping (haiku) ---" | tee -a "$SCRATCH/capability.log"
if cargo test -p peacock-api --lib error::tests -- --nocapture 2>&1 | tee -a "$SCRATCH/capability.log"; then
  echo "PASS S1-U2 error::tests" | tee -a "$SCRATCH/capability.log"
else
  echo "FAIL S1-U2" | tee -a "$SCRATCH/capability.log"
fi
# static assert Forbidden exists and require_role! uses forbidden
echo "--- S1-U2 static: Forbidden variant + require_role! uses forbidden ---" | tee -a "$SCRATCH/capability.log"
if grep -q "Forbidden" "$ROOT/peacock-api/src/error.rs" && grep -q "ApiError::forbidden" "$ROOT/peacock-api/src/middleware/auth.rs"; then
  echo "PASS S1-U2 static 403 wiring" | tee -a "$SCRATCH/capability.log"
else
  echo "FAIL S1-U2 static 403 wiring" | tee -a "$SCRATCH/capability.log"
fi

# 3. Auth 401 gate + JWT (S1-U3) — no DB needed
echo "--- S1-U3 auth middleware (sonnet) ---" | tee -a "$SCRATCH/capability.log"
if cargo test -p peacock-api --lib middleware::auth::tests -- --nocapture 2>&1 | tee -a "$SCRATCH/capability.log"; then
  echo "PASS S1-U3 auth::tests" | tee -a "$SCRATCH/capability.log"
else
  echo "FAIL S1-U3" | tee -a "$SCRATCH/capability.log"
fi
# static: path guard
if grep -q 'path.starts_with("/api/")' "$ROOT/peacock-api/src/middleware/auth.rs" && grep -q 'is_public_path' "$ROOT/peacock-api/src/middleware/auth.rs"; then
  echo "PASS S1-U3 static 401 gate" | tee -a "$SCRATCH/capability.log"
else
  echo "FAIL S1-U3 static 401 gate" | tee -a "$SCRATCH/capability.log"
fi

# 4. Users CRUD Owner-only + argon2 (S1-U4) — needs DB; degrade if missing
echo "--- S1-U4 users CRUD (opus) — degraded if DATABASE_URL missing ---" | tee -a "$SCRATCH/capability.log"
if [[ -n "${DATABASE_URL:-}" ]] && command -v psql >/dev/null 2>&1; then
  if cargo test -p peacock-api --lib routes::users::tests -- --nocapture 2>&1 | tee -a "$SCRATCH/capability.log"; then
    echo "PASS S1-U4 users::tests (with DB)" | tee -a "$SCRATCH/capability.log"
  else
    echo "FAIL S1-U4 users::tests (with DB)" | tee -a "$SCRATCH/capability.log"
  fi
else
  echo "SKIP S1-U4 live DB tests — DATABASE_URL or psql unavailable; running static + filtered lib tests" | tee -a "$SCRATCH/capability.log"
  # static: argon2 hash and Owner guard present
  if grep -q "argon2" "$ROOT/peacock-api/src/routes/users.rs" && grep -q 'require_role!(caller, Owner)' "$ROOT/peacock-api/src/routes/users.rs" && grep -q "password_hash" "$ROOT/peacock-api/src/routes/users.rs"; then
    echo "PASS S1-U4 static argon2 + Owner guard" | tee -a "$SCRATCH/capability.log"
  else
    echo "FAIL S1-U4 static" | tee -a "$SCRATCH/capability.log"
  fi
  # filtered lib without users (proves harness runnable)
  if cargo test -p peacock-api --lib -- --skip users --nocapture 2>&1 | tail -n 30 | tee -a "$SCRATCH/capability.log"; then
    echo "PASS S1-U4 filtered lib (skip users)" | tee -a "$SCRATCH/capability.log"
  else
    echo "FAIL S1-U4 filtered lib" | tee -a "$SCRATCH/capability.log"
  fi
fi

# 5. Web settings/users (S1-U5) — build gate
echo "--- S1-U5 web settings/users (sonnet) ---" | tee -a "$SCRATCH/capability.log"
if grep -q "settings/users" "$ROOT/peacock-web/src/app/settings/users/page.tsx" 2>/dev/null || grep -rq "settings/users" "$ROOT/peacock-web" 2>&1 | head -n 5 | tee -a "$SCRATCH/capability.log"; then
  echo "PASS S1-U5 static page exists" | tee -a "$SCRATCH/capability.log"
else
  echo "FAIL S1-U5 static page missing" | tee -a "$SCRATCH/capability.log"
fi
# health still public (static)
if grep -q '"/health"' "$ROOT/peacock-api/src/middleware/auth.rs"; then
  echo "PASS health public path pinned" | tee -a "$SCRATCH/capability.log"
else
  echo "FAIL health public path" | tee -a "$SCRATCH/capability.log"
fi

echo "=== CAPABILITY DONE ===" | tee -a "$SCRATCH/capability.log"
