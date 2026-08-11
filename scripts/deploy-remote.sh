#!/usr/bin/env bash
#
# Deploy the Peacock POS backend to the remote Windows host.
#
# This is the Wave 2 entry point defined in docs/MASTER_PLAN.md §4 W2.
# Wave 2-A (this lane) provisions the isolated Postgres 5433; Wave 2-B will
# add the API build/ship/run steps in the same script.
#
#   PEACOCK_DB_PASSWORD=... ./scripts/deploy-remote.sh          # provision only (W2-A)
#   PEACOCK_DB_PASSWORD=... ./scripts/deploy-remote.sh --deploy # provision + deploy (W2-B, when ready)
#
# Safety: identical to provision-remote.sh — never touches PID 2580/9956 or
# E:\zerosky-testdb, always uses 5433/8080, verifies isolation before/after.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REMOTE="${PEACOCK_REMOTE:-home@100.72.103.1}"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m warn\033[0m %s\n' "$*"; }

# ---------------------------------------------------------------------------
# Wave 2-A — provision
# ---------------------------------------------------------------------------
log "Wave 2-A — provision remote Postgres (5433) + peacock DB + migrations"
"${SCRIPT_DIR}/provision-remote.sh" "$@"

# ---------------------------------------------------------------------------
# Wave 2-B — deploy API (placeholder until W2-B lane lands)
# ---------------------------------------------------------------------------
if [[ "${1:-}" == "--deploy" ]]; then
  log "Wave 2-B — deploy API to ${REMOTE}:8080 (not yet implemented — see docs/DEPLOYMENT.md §Deploy the API)"
  warn "W2-B will: cargo build --release --target x86_64-pc-windows-msvc, scp to E:\\peacock\\bin\\peacock-api.exe, run on 8080, health-check /health and /health/ready"
  warn "For now, provision is complete. Run with --deploy after W2-B merges."
else
  log "Provision complete. Skipping W2-B deploy (pass --deploy when W2-B is ready)."
fi

cat <<EOF

  Next steps (W2-B, when implemented):
    ./scripts/deploy-remote.sh --deploy
    # then verify:
    curl http://100.72.103.1:8080/health | jq .
    ssh ${REMOTE} 'netstat -ano | findstr ":8080" | findstr LISTENING'
    ssh ${REMOTE} 'sc query peacock-api | findstr STATE'

EOF
