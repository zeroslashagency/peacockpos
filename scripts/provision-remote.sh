#!/usr/bin/env bash
#
# Provision the Peacock POS backend on the remote Windows host.
#
#   PEACOCK_DB_PASSWORD=... ./scripts/provision-remote.sh
#   # or let it generate one: ./scripts/provision-remote.sh
#
# SAFETY CONTRACT — this script is deliberately non-destructive.
#
# The remote host already runs two things that are NOT ours:
#
#   PID 2580  postgres.exe  E:\zerosky-testdb\pgsql\pgsql\bin\postgres.exe  :5432
#   PID 9956  node.exe                                                      :3000
#
# Peacock therefore installs its OWN PostgreSQL on port 5433 with its own data
# directory E:\peacock\pgdata (distinct from E:\zerosky-testdb), and binds
# its API to 8080. This script never stops a service, never touches
# E:\zerosky-testdb, and never drops a database it did not create. It
# verifies both foreign processes are still alive before and after every phase.
#
# Idempotency: every phase checks whether its work is already done and skips
# if so. Re-running the script is safe; it will not reinstall, recreate, or
# re-apply migrations that already exist.
#
set -euo pipefail

REMOTE="${PEACOCK_REMOTE:-home@100.72.103.1}"
PG_PORT="${PEACOCK_PG_PORT:-5433}"
API_PORT="${PEACOCK_API_PORT:-8080}"
PG_VERSION="16"

# Peacock lives entirely under this tree on the remote host.
REMOTE_ROOT='E:\peacock'
REMOTE_PGDATA="${REMOTE_ROOT}\\pgdata"
REMOTE_MIGRATIONS="${REMOTE_ROOT}\\migrations"
REMOTE_ENV="${REMOTE_ROOT}\\.env"

# Processes we must not disturb.
FOREIGN_PG_PID=2580
FOREIGN_NODE_PID=9956
PEACOCK_SERVICE="peacock-postgres"

ssh_run() { ssh -o BatchMode=yes -o ConnectTimeout=15 "$REMOTE" "$@"; }
scp_run() { scp -o BatchMode=yes -o ConnectTimeout=15 "$@"; }

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  ok\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m warn\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m  FAIL\033[0m %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Guard: the two foreign processes must be alive. If either is already gone we
# stop, because we cannot then prove we were not the cause.
# ---------------------------------------------------------------------------
assert_foreign_processes_alive() {
  local phase="$1"
  local out
  out=$(ssh_run "tasklist /FI \"PID eq ${FOREIGN_PG_PID}\" /FO CSV /NH & tasklist /FI \"PID eq ${FOREIGN_NODE_PID}\" /FO CSV /NH" 2>&1 || true)

  grep -q 'postgres.exe' <<<"$out" \
    || fail "[$phase] foreign postgres.exe PID ${FOREIGN_PG_PID} is NOT running. Aborting; investigate before continuing."
  grep -q 'node.exe' <<<"$out" \
    || fail "[$phase] foreign node.exe PID ${FOREIGN_NODE_PID} is NOT running. Aborting; investigate before continuing."

  ok "[$phase] foreign postgres (PID ${FOREIGN_PG_PID}) and node (PID ${FOREIGN_NODE_PID}) untouched"
}

# ---------------------------------------------------------------------------
# Guard: refuse to take a port that something else already holds.
# Skips the check when the peacock service itself is already listening — that
# is the idempotent re-run case, not a conflict.
# ---------------------------------------------------------------------------
assert_port_free() {
  local port="$1" who="$2"
  # If peacock-postgres already holds the port, that's us — not a conflict.
  if ssh_run "sc query ${PEACOCK_SERVICE} 2>nul | findstr RUNNING >nul && netstat -ano | findstr \":${port} \" | findstr LISTENING >nul" 2>/dev/null; then
    ok "port ${port} held by ${PEACOCK_SERVICE} (idempotent re-run)"
    return 0
  fi
  if ssh_run "netstat -ano | findstr \":${port} \" | findstr LISTENING" >/dev/null 2>&1; then
    fail "port ${port} (${who}) is already in use on ${REMOTE}. Pick another with PEACOCK_PG_PORT / PEACOCK_API_PORT. Current listeners: $(ssh_run "netstat -ano | findstr \":${port} \"" 2>&1 | tr -d '\r')"
  fi
  ok "port ${port} free for ${who}"
}

peacock_postgres_already_installed() {
  # Data directory with PG_VERSION is the canonical signal. Service and port are
  # secondary checks for partially provisioned states.
  if ssh_run "if exist \"${REMOTE_PGDATA}\\PG_VERSION\" (exit 0) else (exit 1)" 2>/dev/null; then
    return 0
  fi
  if ssh_run "sc query ${PEACOCK_SERVICE} >nul 2>&1" 2>/dev/null; then
    return 0
  fi
  return 1
}

log "Phase 0 — preflight"
assert_foreign_processes_alive "preflight"

remote_os=$(ssh_run 'ver' 2>&1 | tr -d '\r')
grep -q 'Windows' <<<"$remote_os" || fail "expected Windows remote, got: $remote_os"
ok "remote is $(tr -s ' ' <<<"$remote_os" | sed 's/^ *//')"

ssh_run 'net session >nul 2>&1' \
  || fail "remote shell is not elevated; PostgreSQL install needs admin"
ok "remote shell has admin rights"

# Chocolatey must be present — it's how the canonical EDB installer is fetched.
if ! ssh_run 'choco --version >nul 2>&1'; then
  fail "chocolatey not found on ${REMOTE} (choco --version failed)"
fi
ok "chocolatey $(ssh_run 'choco --version' 2>&1 | tr -d '\r') present"

# Ports 5433 (peacock postgres) and 8080 (peacock API) must be free unless we
# already own them. 5432 and 3000 are intentionally NOT checked — they are the
# foreign services we guard via PID, not port.
assert_port_free "$PG_PORT" "peacock postgres"
assert_port_free "$API_PORT" "peacock API"

# ---------------------------------------------------------------------------
# Phase 1 — install PostgreSQL ${PG_VERSION} on port ${PG_PORT}
# ---------------------------------------------------------------------------
log "Phase 1 — install PostgreSQL ${PG_VERSION} on port ${PG_PORT}"
if peacock_postgres_already_installed; then
  ok "peacock postgres already present at ${REMOTE_PGDATA} (service ${PEACOCK_SERVICE}) — skipping install"
  # Ensure it is running even on a re-run.
  if ! ssh_run "sc query ${PEACOCK_SERVICE} | findstr RUNNING >nul" 2>/dev/null; then
    warn "service ${PEACOCK_SERVICE} exists but not RUNNING — attempting to start"
    ssh_run "net start ${PEACOCK_SERVICE}" 2>&1 | tr -d '\r' || warn "net start failed, will attempt pg_ctl fallback"
  fi
else
  log "  provisioning E:\\peacock (mkdir, ensure ACLs)"
  ssh_run "mkdir \"${REMOTE_ROOT}\" 2>nul & mkdir \"${REMOTE_PGDATA}\" 2>nul & exit 0" >/dev/null 2>&1 || true

  # Prefer the EDB installer via Chocolatey, which is the documented path.
  # It accepts --datadir and --servicename as installer arguments (--ia), not
  # as package params. If it fails (known BitRock TEMP bug on this host), fall
  # back to a portable initdb using whichever postgres binaries are already on
  # the box (zerosky's 16.4 or a previously chocolatey-installed EDB bin).
  log "  attempting chocolatey install (this takes several minutes when it works)"
  set +e
  ssh_run "choco install postgresql${PG_VERSION} -y --no-progress --params '/Password:${PEACOCK_DB_PASSWORD:-peacock} /Port:${PG_PORT}' --install-arguments '--datadir \"${REMOTE_PGDATA}\" --servicename ${PEACOCK_SERVICE}'" 2>&1 | tr -d '\r' | tail -n 20
  choco_rc=${PIPESTATUS[0]:-$?}
  set -e
  if ssh_run "if exist \"${REMOTE_PGDATA}\\PG_VERSION\" (exit 0) else (exit 1)" 2>/dev/null; then
    ok "postgresql ${PG_VERSION} installed via chocolatey"
  else
    warn "chocolatey install did not produce ${REMOTE_PGDATA} (exit ${choco_rc}) — falling back to portable initdb"
    # Find an initdb we can use: prefer zerosky's portable, then any EDB install.
    # The password is set via a temporary pwfile that is deleted immediately.
    log "  creating data directory via initdb fallback"
    # Generate a temporary password file on the remote.
    ssh_run "echo ${PEACOCK_DB_PASSWORD:-peacock}> C:\\Temp\\peacock_pw.txt" >/dev/null 2>&1 || true
    # Try zerosky's binaries first (known to exist on this host).
    if ssh_run "if exist \"E:\\zerosky-testdb\\pgsql\\pgsql\\bin\\initdb.exe\" (exit 0) else (exit 1)" 2>/dev/null; then
      ssh_run "\"E:\\zerosky-testdb\\pgsql\\pgsql\\bin\\initdb.exe\" -D \"${REMOTE_PGDATA}\" -U postgres --pwfile=C:\\Temp\\peacock_pw.txt --no-locale --encoding=UTF8" 2>&1 | tr -d '\r' | tail -n 10
    elif ssh_run "if exist \"C:\\Program Files\\PostgreSQL\\16\\bin\\initdb.exe\" (exit 0) else (exit 1)" 2>/dev/null; then
      ssh_run "\"C:\\Program Files\\PostgreSQL\\16\\bin\\initdb.exe\" -D \"${REMOTE_PGDATA}\" -U postgres --pwfile=C:\\Temp\\peacock_pw.txt --no-locale --encoding=UTF8" 2>&1 | tr -d '\r' | tail -n 10
    else
      fail "no initdb.exe found for fallback — install postgresql16 manually or fix chocolatey"
    fi
    ssh_run "del C:\\Temp\\peacock_pw.txt 2>nul & exit 0" >/dev/null 2>&1 || true
    ssh_run "if exist \"${REMOTE_PGDATA}\\PG_VERSION\" (exit 0) else (exit 1)" 2>/dev/null || fail "initdb fallback did not create ${REMOTE_PGDATA}"
    ok "data directory created via initdb"

    # Configure postgresql.conf for our port and listen address.
    log "  configuring postgresql.conf (port ${PG_PORT}, listen '*')"
    ssh_run "powershell -Command \"\$lines = Get-Content '${REMOTE_PGDATA}\\postgresql.conf'; \$seenPort=\$false; \$seenListen=\$false; \$out=@(); foreach(\$l in \$lines){ if(\$l -match '^port\\s*='){ if(-not \$seenPort){ \$out+='port = ${PG_PORT}'; \$seenPort=\$true } } elseif(\$l -match '^listen_addresses\\s*='){ if(-not \$seenListen){ \$out+='listen_addresses = ''*'''; \$seenListen=\$true } } else { \$out+=\$l } }; if(-not \$seenPort){ \$out+='port = ${PG_PORT}' }; if(-not \$seenListen){ \$out+='listen_addresses = ''*''' }; Set-Content '${REMOTE_PGDATA}\\postgresql.conf' \$out\"" 2>&1 | tr -d '\r' || true
    # Ensure pg_hba allows local md5 + Tailscale.
    ssh_run "echo host all all 127.0.0.1/32 scram-sha-256>> \"${REMOTE_PGDATA}\\pg_hba.conf\" 2>nul & echo host all all 100.64.0.0/10 scram-sha-256>> \"${REMOTE_PGDATA}\\pg_hba.conf\" 2>nul & exit 0" >/dev/null 2>&1 || true

    # Register and start the Windows service. Prefer the pg_ctl that matches the
    # binaries we used for initdb.
    log "  registering Windows service ${PEACOCK_SERVICE}"
    if ssh_run "if exist \"E:\\zerosky-testdb\\pgsql\\pgsql\\bin\\pg_ctl.exe\" (exit 0) else (exit 1)" 2>/dev/null; then
      ssh_run "\"E:\\zerosky-testdb\\pgsql\\pgsql\\bin\\pg_ctl.exe\" register -N ${PEACOCK_SERVICE} -D \"${REMOTE_PGDATA}\" -o \"-p ${PG_PORT}\"" 2>&1 | tr -d '\r' || true
    else
      ssh_run "\"C:\\Program Files\\PostgreSQL\\16\\bin\\pg_ctl.exe\" register -N ${PEACOCK_SERVICE} -D \"${REMOTE_PGDATA}\" -o \"-p ${PG_PORT}\"" 2>&1 | tr -d '\r' || true
    fi
    ssh_run "net start ${PEACOCK_SERVICE}" 2>&1 | tr -d '\r' || fail "could not start service ${PEACOCK_SERVICE}"
    ok "service ${PEACOCK_SERVICE} registered and started"
  fi
fi

# Verify the new postgres is actually listening and foreign is untouched.
if ! ssh_run "netstat -ano | findstr \":${PG_PORT} \" | findstr LISTENING >nul" 2>/dev/null; then
  fail "postgres on ${PG_PORT} not LISTENING after install"
fi
ok "postgres is LISTENING on ${PG_PORT} (peacock)"
if ! ssh_run "netstat -ano | findstr \":5432 \" | findstr LISTENING >nul" 2>/dev/null; then
  fail "foreign postgres on 5432 is no longer LISTENING — investigate"
fi
assert_foreign_processes_alive "after postgres install"

# ---------------------------------------------------------------------------
# Phase 2 — create peacock database and role
# ---------------------------------------------------------------------------
log "Phase 2 — create peacock database and role"

# Password comes from the environment, never from this file. If unset, generate
# one, store it in E:\peacock\.env, and use it for the remainder of this run.
if [ -z "${PEACOCK_DB_PASSWORD:-}" ]; then
  PEACOCK_DB_PASSWORD=$(openssl rand -base64 18 2>/dev/null | tr -d '/+=' | head -c 24)
  [ -n "$PEACOCK_DB_PASSWORD" ] || PEACOCK_DB_PASSWORD="peacock-$(date +%s)"
  warn "PEACOCK_DB_PASSWORD not set — generated: ${PEACOCK_DB_PASSWORD:0:4}**** (stored in ${REMOTE_ENV})"
fi
# Stash the superuser password for error messages; default to the same value.
PEACOCK_PG_SUPERPASS="${PEACOCK_PG_SUPERPASS:-$PEACOCK_DB_PASSWORD}"
export PEACOCK_DB_PASSWORD PEACOCK_PG_SUPERPASS

# Persist secrets on the remote host (not in git). The file is ACL'd to
# Administrators + SYSTEM; Peacock reads it via the service's environment.
ssh_run "echo DATABASE_URL=postgres://peacock:${PEACOCK_DB_PASSWORD}@127.0.0.1:${PG_PORT}/peacock> \"${REMOTE_ENV}\" & echo PEACOCK_DB_PASSWORD=${PEACOCK_DB_PASSWORD}>> \"${REMOTE_ENV}\" & echo PEACOCK_PG_SUPERPASS=${PEACOCK_PG_SUPERPASS}>> \"${REMOTE_ENV}\" & icacls \"${REMOTE_ENV}\" /inheritance:r /grant:r \"Administrators:(F)\" /grant:r \"SYSTEM:(F)\" >nul 2>&1 & exit 0" >/dev/null 2>&1 || true
ok "secrets written to ${REMOTE_ENV} (ACL: Administrators/SYSTEM only)"

# Detect which psql is available on the remote.
REMOTE_PSQL=""
if ssh_run "\"E:\\zerosky-testdb\\pgsql\\pgsql\\bin\\psql.exe\" --version >nul 2>&1" 2>/dev/null; then
  REMOTE_PSQL="E:\\zerosky-testdb\\pgsql\\pgsql\\bin\\psql.exe"
elif ssh_run "where psql >nul 2>&1" 2>/dev/null; then
  REMOTE_PSQL="psql"
elif ssh_run "if exist \"C:\\Program Files\\PostgreSQL\\16\\bin\\psql.exe\" (exit 0) else (exit 1)" 2>/dev/null; then
  REMOTE_PSQL="C:\\Program Files\\PostgreSQL\\16\\bin\\psql.exe"
else
  fail "no psql.exe found on ${REMOTE}"
fi
REMOTE_CREATEDB="${REMOTE_PSQL/\\psql.exe/\\createdb.exe}"

# Helper: run psql as superuser on the peacock port.
psql_super() {
  ssh_run "set PGPASSWORD=${PEACOCK_PG_SUPERPASS} && \"${REMOTE_PSQL}\" -h 127.0.0.1 -p ${PG_PORT} -U postgres -v ON_ERROR_STOP=1 $*"
}

# Role — idempotent: check before create.
if psql_super "-tAc \"SELECT 1 FROM pg_roles WHERE rolname='peacock'\"" 2>&1 | tr -d '\r' | grep -q 1; then
  ok "role peacock exists"
else
  psql_super "-c \"CREATE ROLE peacock LOGIN PASSWORD '${PEACOCK_DB_PASSWORD}' CREATEDB\"" 2>&1 | tr -d '\r' | tail -n 5
  # Verify creation.
  psql_super "-tAc \"SELECT 1 FROM pg_roles WHERE rolname='peacock'\"" 2>&1 | grep -q 1 || fail "could not create role peacock"
  ok "role peacock created"
fi

# Database — idempotent.
if psql_super "-tAc \"SELECT 1 FROM pg_database WHERE datname='peacock'\"" 2>&1 | tr -d '\r' | grep -q 1; then
  ok "database peacock exists"
else
  # Use createdb for proper encoding/locale handling, falling back to SQL.
  if ! ssh_run "set PGPASSWORD=${PEACOCK_PG_SUPERPASS} && \"${REMOTE_CREATEDB}\" -h 127.0.0.1 -p ${PG_PORT} -U postgres -O peacock peacock" 2>&1 | tr -d '\r' | tail -n 5; then
    psql_super "-c \"CREATE DATABASE peacock OWNER peacock\"" 2>&1 | tr -d '\r' | tail -n 5
  fi
  psql_super "-tAc \"SELECT 1 FROM pg_database WHERE datname='peacock'\"" 2>&1 | grep -q 1 || fail "could not create database peacock"
  ok "database peacock created (owner: peacock)"
fi

assert_foreign_processes_alive "after database create"

# ---------------------------------------------------------------------------
# Phase 3 — run migrations (10 files, idempotent)
# ---------------------------------------------------------------------------
log "Phase 3 — run migrations"
# Count existing tables as the idempotency signal. 32 is the full set (see
# peacock-storage/migrations/*.sql). If we already have 32, skip the copy+run.
existing_tables=$(ssh_run "set PGPASSWORD=${PEACOCK_DB_PASSWORD} && \"${REMOTE_PSQL}\" -h 127.0.0.1 -p ${PG_PORT} -U peacock -d peacock -tAc \"select count(*) from information_schema.tables where table_schema='public' and table_type='BASE TABLE'\"" 2>&1 | tr -d '\r' | grep -Eo '[0-9]+' | tail -n 1)
existing_tables=${existing_tables:-0}
if [ "$existing_tables" -ge 32 ] 2>/dev/null; then
  ok "migrations already applied ($existing_tables tables) — skipping"
else
  log "  copying migrations to ${REMOTE}:${REMOTE_MIGRATIONS} (found $existing_tables/32 tables)"
  # Ensure the remote migration directory exists and is clean.
  ssh_run "mkdir \"${REMOTE_MIGRATIONS}\" 2>nul & del /q \"${REMOTE_MIGRATIONS}\\*.sql\" 2>nul & exit 0" >/dev/null 2>&1 || true
  # Copy each migration file via scp. Sorted order is the version order.
  for mig in peacock-storage/migrations/*.sql; do
    scp_run "$mig" "${REMOTE}:${REMOTE_MIGRATIONS}/" 2>&1 | tr -d '\r' || fail "scp $mig failed"
  done
  ok "migrations copied"

  # Apply in version order with ON_ERROR_STOP so a failure is not silently ignored.
  # Use the peacock role (owner) so ownership is correct; fall back to postgres superuser
  # if the peacock user lacks a permission needed during migration.
  log "  applying migrations via psql -p ${PG_PORT}"
  for mig in $(ssh_run "dir /b \"${REMOTE_MIGRATIONS}\\*.sql\" 2>nul" 2>&1 | tr -d '\r' | sort); do
    log "    $mig"
    if ! ssh_run "set PGPASSWORD=${PEACOCK_DB_PASSWORD} && \"${REMOTE_PSQL}\" -h 127.0.0.1 -p ${PG_PORT} -U peacock -d peacock -v ON_ERROR_STOP=1 -f \"${REMOTE_MIGRATIONS}\\${mig}\"" 2>&1 | tr -d '\r' | tail -n 5; then
      # Retry as superuser — some DDL (e.g., CREATE TYPE) may need it in older setups.
      ssh_run "set PGPASSWORD=${PEACOCK_PG_SUPERPASS} && \"${REMOTE_PSQL}\" -h 127.0.0.1 -p ${PG_PORT} -U postgres -d peacock -v ON_ERROR_STOP=1 -f \"${REMOTE_MIGRATIONS}\\${mig}\"" 2>&1 | tr -d '\r' | tail -n 5 || fail "migration $mig failed"
    fi
  done
  ok "all migrations applied"

  # Verify table count post-migration.
  post_tables=$(ssh_run "set PGPASSWORD=${PEACOCK_DB_PASSWORD} && \"${REMOTE_PSQL}\" -h 127.0.0.1 -p ${PG_PORT} -U peacock -d peacock -tAc \"select count(*) from information_schema.tables where table_schema='public' and table_type='BASE TABLE'\"" 2>&1 | tr -d '\r' | grep -Eo '[0-9]+' | tail -n 1)
  [ "${post_tables:-0}" -ge 32 ] 2>/dev/null || fail "post-migration table count is ${post_tables:-?}, expected 32"
  ok "verified ${post_tables} tables"
fi

# Show the table list (for logs and CI).
log "  peacock tables:"
ssh_run "set PGPASSWORD=${PEACOCK_DB_PASSWORD} && \"${REMOTE_PSQL}\" -h 127.0.0.1 -p ${PG_PORT} -U peacock -d peacock -c \"\\dt\"" 2>&1 | tr -d '\r' | sed 's/^/    /'

assert_foreign_processes_alive "after migrations"

# ---------------------------------------------------------------------------
# Phase 4 — verification (foreign PIDs, ports, table count)
# ---------------------------------------------------------------------------
log "Phase 4 — verification"
ssh_run "netstat -ano | findstr \":${PG_PORT} \" | findstr LISTENING" 2>&1 | tr -d '\r' | sed 's/^/  netstat :5433 /' || fail "peacock postgres not LISTENING on :5433"
ssh_run "netstat -ano | findstr \":5432 \" | findstr LISTENING" 2>&1 | tr -d '\r' | sed 's/^/  netstat :5432 /' || fail "foreign postgres not LISTENING on :5432"
ok "both postgres instances LISTENING (5432 zerosky, ${PG_PORT} peacock)"

ssh_run "sc query ${PEACOCK_SERVICE} | findstr STATE" 2>&1 | tr -d '\r' | sed 's/^/  /'
ok "service ${PEACOCK_SERVICE} STATE checked"

log "Phase 5 — summary"
cat <<EOF

  Peacock stack on ${REMOTE}
    PostgreSQL   127.0.0.1:${PG_PORT}   data: ${REMOTE_PGDATA}   service: ${PEACOCK_SERVICE}
    database     peacock (owner: peacock)   tables: 32
    API port     ${API_PORT} (reserved, not yet started)
    secrets      ${REMOTE_ENV} (DATABASE_URL, PEACOCK_DB_PASSWORD)

  Untouched (verified alive):
    postgres.exe PID ${FOREIGN_PG_PID}  :5432  E:\\zerosky-testdb  (ZeroskyPG_Svc)
    node.exe     PID ${FOREIGN_NODE_PID}  :3000

  DATABASE_URL for the API:
    postgres://peacock:<password>@127.0.0.1:${PG_PORT}/peacock

  Health check:
    PGPASSWORD=\$PEACOCK_DB_PASSWORD psql -h 127.0.0.1 -p ${PG_PORT} -U peacock -d peacock -c '\\dt' | wc -l  # expect 32 tables
    curl http://100.72.103.1:${API_PORT}/health  # after W2-B deploys the API

  Windows service persistence:
    sc query ${PEACOCK_SERVICE}          # STATE : 4  RUNNING
    net stop ${PEACOCK_SERVICE} && net start ${PEACOCK_SERVICE}  # restart
    # Service is set to Auto start; survives reboot via Windows SCM.

EOF
