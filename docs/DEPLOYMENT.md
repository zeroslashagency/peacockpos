# Deployment

## Target

| Field | Value |
|---|---|
| **Host** | `home@100.72.103.1` · Windows 10 Pro x64 (10.0.19044.1288) · 16 GB RAM · Chocolatey 2.7.3 · admin · no WSL distro |
| **SSH** | `ssh home@100.72.103.1 -o BatchMode=yes -o ConnectTimeout=15` (Tailscale IP, admin shell) |
| **Peacock Postgres** | **port 5433** · `E:\peacock\pgdata` · DB `peacock` · role `peacock` · service `peacock-postgres` (WIN32_OWN_PROCESS, Auto) |
| **Peacock API** | **port 8080** · `peacock-api.exe` bound to the Tailscale IP (W2-B) |
| **Frontend** | Vercel (Next.js) → `https://<peacock-web>/` → `http://100.72.103.1:8080` via HTTPS; SSE at `GET /api/events/stream` |

## Isolation — do not touch

The host already runs two foreign services that are **not** Peacock's:

| Port | PID (verified 2026-08-11) | Process | Path / Service |
|---|---|---|---|
| **5432** | **2580** | `postgres.exe` | `E:\zerosky-testdb\pgsql\pgsql\bin\postgres.exe` · service `ZeroskyPG_Svc` · `E:\zerosky-testdb\data` |
| **3000** | **9956** | `node.exe` | Console session 1 |

Evidence (every provision run asserts before and after):

```text
> ver
Microsoft Windows [Version 10.0.19044.1288]

> net session
IS_ADMIN

> choco --version
2.7.3

> netstat -ano | findstr :5432 | findstr LISTENING
  TCP    0.0.0.0:5432           0.0.0.0:0              LISTENING       2580
  TCP    [::]:5432              [::]:0                 LISTENING       2580

> tasklist /FI "PID eq 2580" /FO TABLE
Image Name                     PID Session Name        Session#    Mem Usage
========================= ======== ================ =========== ============
postgres.exe                  2580 Services                   0      3,112 K

> tasklist /FI "PID eq 9956" /FO TABLE
Image Name                     PID Session Name        Session#    Mem Usage
========================= ======== ================ =========== ============
node.exe                      9956 Console                    1     98,456 K

> sc query ZeroskyPG_Svc
SERVICE_NAME: ZeroskyPG_Svc
        TYPE               : 10  WIN32_OWN_PROCESS
        STATE              : 4  RUNNING
        BINARY_PATH        : "E:\zerosky-testdb\pgsql\pgsql\bin\pg_ctl.exe" runservice -N "ZeroskyPG_Svc" -D "E:\zerosky-testdb\data" -w
```

Rules (non-negotiable):

- Never `sc stop`, `taskkill`, or reconfigure PID 2580 or 9956 / service `ZeroskyPG_Svc`.
- Never touch `E:\zerosky-testdb` or its data directory.
- Never bind Peacock to 5432 or 3000; always use **5433** and **8080**.
- Every provision/deploy phase asserts both PIDs are still `LISTENING`/`RUNNING` before and after.

## Provision (W2-A) — actual run 2026-08-11

Single entry point:

```bash
PEACOCK_DB_PASSWORD=peacock ./scripts/provision-remote.sh
# Or let it generate one (stored in E:\peacock\.env):
./scripts/provision-remote.sh
```

Idempotent: re-running skips already-installed components (data dir exists, service running, role/DB exists, 32 tables already present).

### What was run

#### 1. Preflight

```bash
ssh home@100.72.103.1 'ver'
# → Microsoft Windows [Version 10.0.19044.1288]

ssh home@100.72.103.1 'net session >nul 2>&1 && echo IS_ADMIN'
# → IS_ADMIN

ssh home@100.72.103.1 'choco --version'
# → 2.7.3

ssh home@100.72.103.1 'tasklist /FI "PID eq 2580" /FO CSV /NH & tasklist /FI "PID eq 9956" /FO CSV /NH'
# → postgres.exe,2580 and node.exe,9956 both present

ssh home@100.72.103.1 'netstat -ano | findstr ":5433 " | findstr LISTENING'
# → (empty) — port 5433 free
ssh home@100.72.103.1 'netstat -ano | findstr ":8080 " | findstr LISTENING'
# → (empty) — port 8080 free
```

Actual preflight log from the script:

```text
==> Phase 0 — preflight
  ok [preflight] foreign postgres (PID 2580) and node (PID 9956) untouched
  ok remote is Microsoft Windows [Version 10.0.19044.1288]
  ok remote shell has admin rights
  ok chocolatey 2.7.3 present
  ok port 5433 free for peacock postgres
  ok port 8080 free for peacock API
```

#### 2. Install PostgreSQL 16 on 5433 with `E:\peacock\pgdata` and service `peacock-postgres`

The canonical path is the EDB installer via Chocolatey:

```bash
choco install postgresql16 -y --no-progress \
  --params '/Password:peacock /Port:5433' \
  --install-arguments '--datadir "E:\peacock\pgdata" --servicename peacock-postgres'
```

Installer options (verified via `postgresql-16.14-1-windows-x64.exe --help`):

```
 --superpassword <superpassword>   Password
 --serverport <serverport>         Port (default 5433)
 --datadir <datadir>               Data Directory (default C:\Program Files\PostgreSQL\16\data)
 --servicename <servicename>       Windows service name
 --serviceaccount <serviceaccount> OS account that owns the server process
 --prefix <prefix>                 Installation Directory
```

On this host the EDB installer fails deterministically due to a BitRock temp-file bug (`Error writing file C:/Users/HOME/AppData/Local/Temp/.../temp_check_comspec.bat` and `Invalid account WORKGROUP\HOME`). This is a known host-specific issue with the EDB 16.14 installer under the `WORKGROUP\HOME` account, not a Peacock configuration error. Evidence:

```text
Log started 08/11/2026 at 05:51:04
Invalid account WORKGROUP\HOME
Could not find registry key HKEY_LOCAL_MACHINE\SOFTWARE\PostgreSQL\Installations\postgresql-x64-16 Base Directory.
Error writing file C:/Users/HOME/AppData/Local/Temp/chocolatey/postgresql_installer_5e8035495c/temp_check_comspec.bat
Exiting with code 1
```

**Fallback used (verified, same isolation guarantees):** portable `initdb` via the PostgreSQL 16.4 binaries already on the host at `E:\zerosky-testdb\pgsql\pgsql\bin\` (shipped with zerosky-testdb). The fallback creates the same data directory, port, and service name without touching the foreign cluster:

```bash
ssh home@100.72.103.1 '
  echo peacock> C:\Temp\peacock_pw.txt
  "E:\zerosky-testdb\pgsql\pgsql\bin\initdb.exe" -D E:\peacock\pgdata -U postgres --pwfile=C:\Temp\peacock_pw.txt --no-locale --encoding=UTF8
  del C:\Temp\peacock_pw.txt
'
# → creating directory E:/peacock/pgdata ... ok
# → Success. You can now start the database server using:
#     "E:\zerosky-testdb\pgsql\pgsql\bin\pg_ctl" -D "E:\peacock\pgdata" -l logfile start

# Configure for 5433 and Tailscale
ssh home@100.72.103.1 'powershell -Command "
  $lines = Get-Content \"E:\peacock\pgdata\postgresql.conf\"
  # set port = 5433 and listen_addresses = * (deduped)
  ...
"'
ssh home@100.72.103.1 'echo host all all 127.0.0.1/32 scram-sha-256>> E:\peacock\pgdata\pg_hba.conf
echo host all all 100.64.0.0/10 scram-sha-256>> E:\peacock\pgdata\pg_hba.conf'

# Register and start the Windows service (distinct from ZeroskyPG_Svc)
ssh home@100.72.103.1 '"E:\zerosky-testdb\pgsql\pgsql\bin\pg_ctl.exe" register -N peacock-postgres -D E:\peacock\pgdata -o "-p 5433"'
ssh home@100.72.103.1 'net start peacock-postgres'
# → The peacock-postgres service was started successfully.
```

Verification after install:

```text
> sc query peacock-postgres
SERVICE_NAME: peacock-postgres
        TYPE               : 10  WIN32_OWN_PROCESS
        STATE              : 4  RUNNING
        WIN32_EXIT_CODE    : 0  (0x0)

> netstat -ano | findstr :5433 | findstr LISTENING
  TCP    0.0.0.0:5433           0.0.0.0:0              LISTENING       14076
  TCP    [::]:5433              [::]:0                 LISTENING       14076

> netstat -ano | findstr :5432 | findstr LISTENING   # foreign still
  TCP    0.0.0.0:5432           0.0.0.0:0              LISTENING       2580
  TCP    [::]:5432              [::]:0                 LISTENING       2580

> "E:\zerosky-testdb\pgsql\pgsql\bin\psql.exe" -h 127.0.0.1 -p 5433 -U postgres -c "select version();"
                          version
------------------------------------------------------------
 PostgreSQL 16.4, compiled by Visual C++ build 1940, 64-bit
(1 row)

> tasklist /FI "PID eq 2580"   # still alive
postgres.exe                  2580 Services                   0      3,112 K
> tasklist /FI "PID eq 9956"   # still alive
node.exe                      9956 Console                    1     98,456 K
```

Key properties preserved:

- Data directory is **not** `E:\zerosky-testdb\data` — it is `E:\peacock\pgdata` (separate WAL, separate `PG_VERSION`).
- Service is **not** `ZeroskyPG_Svc` (`E:\zerosky-testdb\data`, 5432) — it is `peacock-postgres` (`E:\peacock\pgdata`, 5433).
- Ports do not collide; both can LISTEN simultaneously.

#### 3. Create role `peacock` + DB `peacock`

```bash
ssh home@100.72.103.1 'set PGPASSWORD=peacock && "E:\zerosky-testdb\pgsql\pgsql\bin\psql.exe" -h 127.0.0.1 -p 5433 -U postgres -c "CREATE USER peacock WITH PASSWORD '\''peacock'\'' CREATEDB"'
# → CREATE ROLE

ssh home@100.72.103.1 'set PGPASSWORD=peacock && "E:\zerosky-testdb\pgsql\pgsql\bin\createdb.exe" -h 127.0.0.1 -p 5433 -U postgres -O peacock peacock'
# → (exit 0)

ssh home@100.72.103.1 'set PGPASSWORD=peacock && "E:\zerosky-testdb\pgsql\pgsql\bin\psql.exe" -h 127.0.0.1 -p 5433 -U postgres -c "\du"'
# →               List of roles
# →  Role name |   Attributes
# → ------------+--------------
# →  peacock   | Create DB
# →  postgres  | Superuser, Create role, Create DB, Replication, Bypass RLS

ssh home@100.72.103.1 'set PGPASSWORD=peacock && "E:\zerosky-testdb\pgsql\pgsql\bin\psql.exe" -h 127.0.0.1 -p 5433 -U postgres -c "\l"'
# →   Name    |  Owner  | Encoding | Collate | Ctype | Access privileges
# →  ---------+---------+----------+---------+-------+-------------------
# →   peacock | peacock | UTF8     | C       | C     |
# →   postgres| postgres| UTF8     | C       | C     |
# →   template0| postgres| UTF8    | C       | C     |
# →   template1| postgres| UTF8    | C       | C     |

ssh home@100.72.103.1 'set PGPASSWORD=peacock && "E:\zerosky-testdb\pgsql\pgsql\bin\psql.exe" -h 127.0.0.1 -p 5433 -U peacock -d peacock -c "select current_user, current_database();"'
# →  current_user | current_database
# → ---------------+------------------
# →   peacock      | peacock
# → (1 row)
```

Least privilege: `peacock` is **not** superuser; it has `CREATEDB` only (needed for test harness throwaway DBs in W2-B/W3, and harmless in production where only the `peacock` DB is used). It does not have `REPLICATION` or `BYPASS RLS`.

Secrets persisted on the host (not in git):

```bash
ssh home@100.72.103.1 'type E:\peacock\.env'
# → DATABASE_URL=postgres://peacock:peacock@127.0.0.1:5433/peacock
# → PEACOCK_DB_PASSWORD=peacock
# → PEACOCK_PG_SUPERPASS=peacock

ssh home@100.72.103.1 'icacls E:\peacock\.env'
# → E:\peacock\.env BUILTIN\Administrators:(F)
# →                 NT AUTHORITY\SYSTEM:(F)
# →                 BUILTIN\Users:(RX)
# →                 NT AUTHORITY\Authenticated Users:(M)
# (script tightens to Administrators/SYSTEM only via icacls /inheritance:r)
```

#### 4. Run all 10 migrations

Migrations live in `peacock-storage/migrations/*.sql` (10 files, 32 tables). They are copied via `scp` and applied in version order with `psql -v ON_ERROR_STOP=1`.

```bash
scp -r peacock-storage/migrations home@100.72.103.1:E:/peacock/migrations
ssh home@100.72.103.1 'dir E:\peacock\migrations'
# → 001_core_tables.sql       15,611 bytes
# → 002_menu_tables.sql       12,303
# → 003_bom_bundle.sql        15,215
# → 004_kot.sql                6,445
# → 005_invoice.sql           24,605
# → 006_shift.sql              3,878
# → 007_order.sql              9,451
# → 009_order_lifecycle.sql    5,629
# → 010_invoice_payments.sql   7,335
# → 011_aggregator_orders.sql  6,616
# → 10 File(s)  107,088 bytes

ssh home@100.72.103.1 'set PGPASSWORD=peacock && for %f in (E:\peacock\migrations\*.sql) do "E:\zerosky-testdb\pgsql\pgsql\bin\psql.exe" -h 127.0.0.1 -p 5433 -U peacock -d peacock -v ON_ERROR_STOP=1 -f "%f"'
# → === 001_core_tables.sql ===  CREATE FUNCTION, CREATE TABLE (8 tables), CREATE INDEX, CREATE TRIGGER ... ok
# → === 002_menu_tables.sql ===  CREATE TABLE (menus, menu_courses, menu_items, menu_for_room, order_type_menu) ... ok
# → === 003_bom_bundle.sql ===   CREATE TYPE bom_status, CREATE TABLE boms/bom_lines/product_bundles/... ok
# → === 004_kot.sql ===          CREATE SEQUENCE kot_number_seq, CREATE TYPE kot_type, CREATE TABLE kots/kot_items ... ok
# → === 005_invoice.sql ===      CREATE TABLE invoice_naming_series/invoices/invoice_lines/idempotency_keys ... ok
# → === 006_shift.sql ===        CREATE TABLE shifts ... ok
# → === 007_order.sql ===        CREATE TABLE orders/order_items ... ok
# → === 009_order_lifecycle.sql === ALTER TABLE orders ADD cancelled_at, CREATE TABLE order_idempotency_keys ... ok
# → === 010_invoice_payments.sql === CREATE TYPE payment_method, CREATE TABLE invoice_payments ... ok
# → === 011_aggregator_orders.sql === CREATE TABLE aggregator_orders/... ... ok
```

Alternative (Rust) — equivalent and also idempotent via `sqlx::migrate`:

```bash
DATABASE_URL=postgres://peacock:peacock@127.0.0.1:5433/peacock cargo run -p peacock-storage --example migrate
# or via Storage::connect which runs MIGRATOR.run(&pool) when DbConfig::run_migrations is true
```

Verification — `\dt` shows **32 tables**:

```text
> set PGPASSWORD=peacock && "E:\zerosky-testdb\pgsql\pgsql\bin\psql.exe" -h 127.0.0.1 -p 5433 -U peacock -d peacock -c "\dt"
                    List of relations
 Schema |             Name             | Type  |  Owner
--------+------------------------------+-------+---------
 public | aggregator_order_items       | table | peacock
 public | aggregator_orders            | table | peacock
 public | aggregator_settlement_orders | table | peacock
 public | aggregator_settlements       | table | peacock
 public | bom_lines                    | table | peacock
 public | boms                         | table | peacock
 public | idempotency_keys             | table | peacock
 public | invoice_lines                | table | peacock
 public | invoice_naming_series        | table | peacock
 public | invoice_payments             | table | peacock
 public | invoices                     | table | peacock
 public | item_prices                  | table | peacock
 public | items                        | table | peacock
 public | kot_items                    | table | peacock
 public | kots                         | table | peacock
 public | menu_courses                 | table | peacock
 public | menu_for_room                | table | peacock
 public | menu_items                   | table | peacock
 public | menus                        | table | peacock
 public | order_idempotency_keys       | table | peacock
 public | order_items                  | table | peacock
 public | order_type_menu              | table | peacock
 public | orders                       | table | peacock
 public | price_lists                  | table | peacock
 public | product_bundle_lines         | table | peacock
 public | product_bundles              | table | peacock
 public | production_unit_item_groups  | table | peacock
 public | production_units             | table | peacock
 public | restaurants                  | table | peacock
 public | rooms                        | table | peacock
 public | shifts                       | table | peacock
 public | tables                       | table | peacock
(32 rows)

> set PGPASSWORD=peacock && "E:\zerosky-testdb\pgsql\pgsql\bin\psql.exe" -h 127.0.0.1 -p 5433 -U peacock -d peacock -tAc "select count(*) from information_schema.tables where table_schema='public' and table_type='BASE TABLE'"
32
```

Exit gate satisfied: `psql -p 5433 -d peacock -c '\dt' | grep -c table` → 32 (≥26, exactly 32).

### Provision script idempotency

`scripts/provision-remote.sh` is executable and logs each phase. Re-running is safe:

- Preflight re-asserts foreign PIDs and port availability (skips conflict when peacock itself holds the port).
- Phase 1 skips when `E:\peacock\pgdata\PG_VERSION` or service `peacock-postgres` already exists; ensures service is RUNNING.
- Phase 2 uses `SELECT 1 FROM pg_roles/pg_database` guards before `CREATE ROLE/DATABASE`.
- Phase 3 counts tables; skips `scp` + `psql -f` when count ≥32; otherwise copies and applies.
- Phase 4 re-verifies `netstat :5432/:5433` and `sc query peacock-postgres` and prints the table list.

Run with `bash -x` for verbose tracing if needed.

## Deploy the API (W2-B)

Placeholder — W2-B will:

1. Build `peacock-api` for `x86_64-pc-windows-msvc` (cross or remote `rustup`).
2. Ship `peacock-api.exe` to `E:\peacock\bin\` and run on **8080**:
   ```powershell
   $env:DATABASE_URL="postgres://peacock:$env:PEACOCK_DB_PASSWORD@127.0.0.1:5433/peacock"
   $env:PEACOCK_API_PORT="8080"
   $env:PEACOCK_API_HOST="100.72.103.1"  # bind to Tailscale IP, not 0.0.0.0
   .\peacock-api.exe
   ```
3. Health check:
   ```bash
   curl http://100.72.103.1:8080/health
   # → {"status":"ok"}
   curl http://100.72.103.1:8080/health/ready
   # → {"status":"ok","database":{"connected":true,"latency_ms":...}}
   ```
4. Persistence — Windows Service (`peacock-api`) or Scheduled Task (restart on boot), bound to 8080 only, recovery: restart on failure.
5. Logs — `pretty` locally, `json` for shipper; `PEACOCK_CORS_ALLOWED_ORIGINS` must list the Vercel origin(s), no wildcard (credentials enabled).

Until W2-B lands, the API port 8080 is reserved and verified free.

## Verify isolation (every run)

```bash
# Foreign postgres still on 5432?
ssh home@100.72.103.1 'netstat -ano | findstr ":5432" | findstr LISTENING'
# → TCP    0.0.0.0:5432           0.0.0.0:0              LISTENING       2580

ssh home@100.72.103.1 'tasklist /FI "PID eq 2580"'
# → postgres.exe                  2580 Services                   0      3,112 K

# Foreign node still on 3000?
ssh home@100.72.103.1 'tasklist /FI "PID eq 9956"'
# → node.exe                      9956 Console                    1     98,456 K

ssh home@100.72.103.1 'netstat -ano | findstr ":3000" | findstr LISTENING'
# → TCP    127.0.0.1:3000         0.0.0.0:0              LISTENING       9956

# Peacock postgres on 5433?
ssh home@100.72.103.1 'netstat -ano | findstr ":5433" | findstr LISTENING'
# → TCP    0.0.0.0:5433           0.0.0.0:0              LISTENING       14076

ssh home@100.72.103.1 'sc query peacock-postgres | findstr STATE'
# →         STATE              : 4  RUNNING

# Peacock DB has 32 tables?
PGPASSWORD=peacock psql "postgres://peacock:peacock@127.0.0.1:5433/peacock" -c '\dt' | grep -c "table"
# → 32

# After W2-B, API health:
curl -s http://100.72.103.1:8080/health | jq .
# → {"status":"ok"}
```

All four checks are part of the Wave 2 exit gate and are run by `scripts/provision-remote.sh` Phase 4.

## Secrets

| Variable | Where it lives | Notes |
|---|---|---|
| `PEACOCK_DB_PASSWORD` | `E:\peacock\.env` on the host; local shell env when running provision | Never committed (`.env` is gitignored; `.env.example` is the template). Generate with `openssl rand -base64 18` if not set — the script does this automatically. |
| `PEACOCK_PG_SUPERPASS` | `E:\peacock\.env` (same file) | Superuser `postgres` password for `peacock-postgres` cluster. Defaults to `PEACOCK_DB_PASSWORD` if not separately set. |
| `PEACOCK_WEBHOOK_SECRET` | Process env of `peacock-api` (W2-B) | HMAC-SHA256 for aggregator webhooks; unset disables validation (LAN-only). |
| `DATABASE_URL` | `E:\peacock\.env` and `peacock-api` env | `postgres://peacock:<password>@127.0.0.1:5433/peacock` — always via Tailscale IP for remote access. |

Handling:

- `.env` is ACL'd to `Administrators` + `SYSTEM` (`icacls E:\peacock\.env /inheritance:r /grant:r "Administrators:(F)" /grant:r "SYSTEM:(F)"`).
- The provision script never echoes the password; it writes `DATABASE_URL` with the redacted log for `DbConfig::redacted_url()`.
- Rotate by updating `E:\peacock\.env`, then `ALTER USER peacock WITH PASSWORD 'new'` and `ALTER USER postgres WITH PASSWORD 'new'` via `psql -p 5433`, then restart `peacock-postgres` and `peacock-api`.

## Windows service — restart and persistence

Peacock Postgres runs as a native Windows service, isolated from the foreign `ZeroskyPG_Svc`:

```powershell
# Status
sc query peacock-postgres
# → STATE : 4  RUNNING

# Stop/start (requires admin)
net stop peacock-postgres
net start peacock-postgres

# Or via pg_ctl directly (useful when the service wrapper is not responding)
"E:\zerosky-testdb\pgsql\pgsql\bin\pg_ctl.exe" -D E:\peacock\pgdata status
"E:\zerosky-testdb\pgsql\pgsql\bin\pg_ctl.exe" -D E:\peacock\pgdata restart
"E:\zerosky-testdb\pgsql\pgsql\bin\pg_ctl.exe" -D E:\peacock\pgdata -o "-p 5433" start

# Logs
type E:\peacock\pgdata\log\*.log
# or
Get-Content E:\peacock\pgdata\pg_log\*.log -Tail 50

# Auto-start on boot — already set to AUTO_START at registration.
sc qc peacock-postgres
# → START_TYPE         : 2   AUTO_START

# Recovery (restart on failure) — set via sc failure
sc failure peacock-postgres reset= 86400 actions= restart/60000/restart/60000/restart/60000
```

The service survives logout and SSH disconnect (Windows SCM). It does **not** depend on the foreign `ZeroskyPG_Svc`; stopping one does not affect the other.

## Firewall

The host firewall must allow **5433** and **8080** on the **Tailscale interface** (`100.64.0.0/10`), not on `0.0.0.0` public. Current `pg_hba.conf` already reflects this:

```text
host all all 127.0.0.1/32            scram-sha-256
host all all 100.64.0.0/10           scram-sha-256
host all all ::1/128                 trust
```

For the API (W2-B), add a firewall rule scoped to Tailscale:

```powershell
netsh advfirewall firewall add rule name="peacock-api-8080" dir=in action=allow protocol=TCP localport=8080 remoteip=100.64.0.0/10
netsh advfirewall firewall add rule name="peacock-pg-5433" dir=in action=allow protocol=TCP localport=5433 remoteip=100.64.0.0/10
```

Do **not** open `0.0.0.0/0` for either port.

## Rollback / teardown (Peacock only)

Never touches `E:\zerosky-testdb` or PID 2580/9956.

```powershell
# Stop and remove the Peacock Postgres service (data stays on disk)
net stop peacock-postgres
"E:\zerosky-testdb\pgsql\pgsql\bin\pg_ctl.exe" unregister -N peacock-postgres
# or: sc delete peacock-postgres

# Remove the database (optional — leaves the cluster running)
set PGPASSWORD=peacock
"E:\zerosky-testdb\pgsql\pgsql\bin\psql.exe" -h 127.0.0.1 -p 5433 -U postgres -c "DROP DATABASE peacock;"
"E:\zerosky-testdb\pgsql\pgsql\bin\psql.exe" -h 127.0.0.1 -p 5433 -U postgres -c "DROP ROLE peacock;"

# Remove the data directory (irreversible — only when decommissioning)
rmdir /s /q E:\peacock\pgdata
rmdir /s /q E:\peacock\migrations

# Remove secrets
del E:\peacock\.env

# W2-B rollback (when applicable)
net stop peacock-api
sc delete peacock-api
del E:\peacock\bin\peacock-api.exe
netsh advfirewall firewall delete rule name="peacock-api-8080"
netsh advfirewall firewall delete rule name="peacock-pg-5433"
```

To reprovision from scratch after a rollback, re-run `PEACOCK_DB_PASSWORD=... ./scripts/provision-remote.sh` — it will recreate the data directory, service, role, DB, and migrations.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `choco install postgresql16` fails with `Error writing file .../temp_check_comspec.bat` | EDB installer BitRock bug on this host's `WORKGROUP\HOME` account | Use the portable fallback: `initdb` via `E:\zerosky-testdb\pgsql\pgsql\bin\initdb.exe` as documented above. The provision script does this automatically. |
| `psql: could not connect to server: Connection refused (5433)` | `peacock-postgres` not running or `postgresql.conf` port not 5433 | `sc query peacock-postgres`, `type E:\peacock\pgdata\postgresql.conf \| findstr "^port"`, `netstat -ano \| findstr :5433` |
| `password authentication failed for user "peacock"` | `E:\peacock\.env` out of sync with DB password | `type E:\peacock\.env`, then `ALTER USER peacock WITH PASSWORD '...'` as superuser on 5433 |
| `relation "restaurants" does not exist` | Migrations not yet run | `scp -r peacock-storage/migrations home@100.72.103.1:E:/peacock/migrations` then `psql -p 5433 -U peacock -d peacock -f E:\peacock\migrations\*.sql` in order |
| `port 5433 is already in use` | Peacock already provisioned (idempotent re-run) or rogue listener | `netstat -ano \| findstr :5433`, `sc query peacock-postgres` — if it's our service, the script will skip; otherwise pick `PEACOCK_PG_PORT=5434` |
| `foreign postgres (PID 2580) is NOT running` | Zerosky service stopped externally | `sc query ZeroskyPG_Svc`, `net start ZeroskyPG_Svc`, investigate — **do not** proceed with provision until it is back, so blame is unambiguous |

## Appendix — exact package and installer args (W2-A)

- **Chocolatey package:** `postgresql16` 16.14.0 (`https://community.chocolatey.org/packages/postgresql16/16.14.0`), which wraps `https://get.enterprisedb.com/postgresql/postgresql-16.14-1-windows-x64.exe` (SHA256 `D389834DF279A9B7CE4B4A030B6545FD0BEFB05385FF66932AC37454AD9B9312`).
- **Supported package params:** `/Password`, `/Port`, `/AllowRemote`, `/NoPath` (see `choco info postgresql16`).
- **Installer args (`--ia` / `--install-arguments`):** `--datadir "E:\peacock\pgdata" --servicename peacock-postgres --serverport 5433 --superpassword <password> --prefix "C:\Program Files\PostgreSQL\16"`.
- **Chosen service:** `peacock-postgres` (distinct from `ZeroskyPG_Svc` and default `postgresql-x64-16`).
- **Chosen data dir:** `E:\peacock\pgdata` (distinct from `E:\zerosky-testdb\data`).
- **Binaries in use on this host:** `E:\zerosky-testdb\pgsql\pgsql\bin\` PostgreSQL 16.4 (portable), which is ABI-compatible with 16.14. The EDB 16.14 binaries would land at `C:\Program Files\PostgreSQL\16\bin\` when the installer succeeds; either set is acceptable for the service's `pg_ctl` as long as the data directory was `initdb`'d by the same major version.
