# Peacock POS — Master Execution Plan

**Created:** 2026-07-31
**Models permitted:** `local-claude-opus-5`, `local-gpt-5-6-sol`, `local-gpt-5-6-terra` only.
**Deployment target:** `home@100.72.103.1` (Windows 10 Pro x64, 16 GB RAM, Chocolatey, admin, WSL feature present but no distro).

---

## 0. Verified ground truth (measured, not assumed)

| Fact | Evidence |
|---|---|
| Remote OS | `Microsoft Windows [Version 10.0.19044.1288]`, Windows 10 Pro x64 |
| Remote RAM | 16,208 MB |
| Remote admin | `net session` succeeds → `IS_ADMIN` |
| Package manager | `C:\ProgramData\chocolatey\bin\choco.exe`; no winget, no scoop |
| **Port 5432 OCCUPIED** | PID 2580 `postgres.exe` → `E:\zerosky-testdb\pgsql\pgsky\bin\postgres.exe` |
| **Port 3000 OCCUPIED** | PID 9956 `node.exe` |
| Docker | not installed |
| Rust on remote | not installed |
| WSL distro | none installed |
| Local repo | 4 Rust crates, no frontend, no `package.json` |
| Local build | `cargo build --workspace` → exit 0, 3 warnings |
| Parity harness | 22/22 green |

**Hard constraint:** PID 2580 (`zerosky-testdb` Postgres) and PID 9956 (node) are pre-existing user work. They must not be stopped, reconfigured, or have their data directories touched. Peacock gets its own Postgres on **5433** and its own API port **8080**.

---

## 1. Correction to earlier reporting

A previous summary in this session claimed "Phase 4A complete — no stubs remaining." That was inaccurate; it relayed subagent self-reports without independent verification. Verified state:

- `peacock-api/src/state.rs:124` — `pub fn storage(&self) -> Option<&Storage>` — storage is optional.
- `peacock-api/src/routes/invoices.rs:404` — falls back to `InvoiceBackend::Memory` when storage is absent.
- 6 handlers return `not yet implemented`: `cogs.rs:199`, `items.rs:40`, `menu.rs:67`, `menu.rs:102`, `reports.rs:332`, `reports.rs:371`.
- `tables.rs:53` list is a stub; `tables.rs:267` order transfer is a TODO.
- `aggregators.rs` has 6 TODOs; 0 storage calls.
- `menu.rs:53` computes `strategy` and never uses it (build warning proves the handler is dead).

**Rule for this run:** no lane is marked complete on its own report. Every lane is verified by the orchestrator running the build, the tests, and the live HTTP probe.

---

## 2. Target architecture

```
Vercel (later)            SSH box 100.72.103.1 (Windows)
┌──────────────┐          ┌────────────────────────────────────────┐
│ peacock-web  │  HTTPS   │  peacock-api.exe        :8080          │
│ Next.js 15   │ ───────► │    │                                    │
│ POS/KDS UI   │   SSE    │    └── sqlx pool ──► Postgres :5433    │
└──────────────┘          │                       (peacock DB,      │
                          │                        own datadir)     │
                          │  UNTOUCHED:                             │
                          │    postgres.exe :5432 (zerosky-testdb)  │
                          │    node.exe     :3000                   │
                          └────────────────────────────────────────┘
```

---

## 3. Target file structure

```
peacock-pos/
├── Cargo.toml                  workspace
├── README.md                   entry point: what this is, how to run
├── .env.example
├── docs/
│   ├── MASTER_PLAN.md          this file
│   ├── ARCHITECTURE.md         system design, crate boundaries
│   ├── DEPLOYMENT.md           SSH box runbook
│   ├── API.md                  all endpoints, request/response
│   ├── GROUND-TRUTH.md         verified upstream facts
│   └── history/                superseded plans + lane reports (archive)
├── peacock-core/               domain logic, no I/O
├── peacock-storage/            Postgres adapters + migrations
├── peacock-api/                HTTP + SSE
├── peacock-parity/             money oracle diff harness
├── peacock-web/                Next.js POS + KDS UI
├── scripts/
│   ├── deploy-remote.sh        provision + deploy to SSH box
│   ├── verify-api.sh           live endpoint probe
│   └── parity_reference.py     Python money oracle
└── target/                     (gitignored)
```

Currently 16 markdown files sit at the repo root, 10 of which are per-lane delivery reports. Those move to `docs/history/`.

---

## 4. Waves

Lanes inside a wave run in parallel. A wave does not start until the previous wave is verified by the orchestrator.

### Wave 1 — Finish backend integration (blocking everything)

| Lane | Model | Scope | Files |
|---|---|---|---|
| **W1-A** | Opus 5 | Make `Storage` non-optional. Delete `InvoiceBackend::Memory` and the in-memory `InvoiceStore`/`OrderStore` fallbacks. One construction path: `AppState::new(Storage)`. Test helpers get a real test DB, not a memory store. | `state.rs`, `app.rs`, `main.rs`, `routes/invoices.rs`, `store/` |
| **W1-B** | Opus 5 | Restaurant/branch request context (the blocker behind both `menu.rs` stubs), then wire `menu.rs` + `items.rs` to `PgMenuRepo`/`PgPriceRepo`/item lookup. Remove the dead `strategy` binding. | `routes/menu.rs`, `routes/items.rs`, `middleware/context.rs` |
| **W1-C** | Sol | Wire `cogs.rs` + `reports.rs` to `PgBomRepo`/`PgProductBundleRepo`/`PgInvoiceRepo`. Daily P&L revenue must come from `PosInvoiceStatus::REVENUE`. Parity harness must stay 22/22. | `routes/cogs.rs`, `routes/reports.rs` |
| **W1-D** | Sol | `tables.rs`: real list (add `list_all` to the port), real order transfer via order repo. `aggregators.rs`: persist webhook orders, real accept/reject, real settlement query. | `routes/tables.rs`, `routes/aggregators.rs`, `ports.rs` |
| **W1-E** | Terra | File structure: create `docs/`, move 10 lane reports to `docs/history/`, write `README.md`, `ARCHITECTURE.md`, `API.md`. No code changes. | repo root, `docs/` |

**Wave 1 exit gate (orchestrator-run):**
1. `grep -rn "not yet implemented\|todo!\|unimplemented!" peacock-api/src` → 0 hits outside tests.
2. `grep -rn "InvoiceBackend::Memory" peacock-api/src` → 0 hits.
3. `cargo build --workspace` → 0 warnings.
4. `cargo clippy --workspace --all-targets -- -D warnings` → exit 0.
5. `cargo test --workspace` → all pass.
6. `cargo run -p peacock-parity` → 22/22, exit 0.

### Wave 2 — Deploy to SSH box

| Lane | Model | Scope |
|---|---|---|
| **W2-A** | Opus 5 | Provision remote: Chocolatey-install PostgreSQL **on 5433** with its own data dir, create `peacock` DB + least-privilege role, run all 7 migrations, verify PID 2580 and PID 9956 still alive and untouched. Produce `scripts/deploy-remote.sh` + `docs/DEPLOYMENT.md`. |
| **W2-B** | Sol | Build `peacock-api` for `x86_64-pc-windows-msvc` (cross or remote-native via rustup on the box), ship the binary, run it on **8080** bound to the Tailscale IP, confirm `/health` reports DB connectivity. Windows service or scheduled task for restart-on-boot. |

**Wave 2 exit gate:**
1. `ssh ... 'netstat -ano | findstr ":5432"'` → PID 2580 still LISTENING.
2. `ssh ... 'tasklist /FI "PID eq 9956"'` → node still alive.
3. `curl http://100.72.103.1:8080/health` → 200, DB connected.
4. `psql -p 5433 -d peacock -c '\dt'` → 26 tables.

### Wave 3 — UI + live API verification (parallel)

| Lane | Model | Scope |
|---|---|---|
| **W3-A** | Opus 5 | `peacock-web` foundation: Next.js 15 + TS + Tailwind, typed API client generated from `docs/API.md`, money as string end-to-end (never JS `Number`), auth-less for now, SSE hook for KDS. |
| **W3-B** | Sol | POS screens: table floor plan (merge state visible), order cart, menu browse by course, send-to-kitchen, payment + invoice. |
| **W3-C** | Terra | KDS screen (live SSE ticket board per station, mark-prepared) + shift screens (open, close with Z-report, cash threshold warning). |
| **W3-D** | Opus 5 | Live API verification: probe all 59 endpoints against the deployed box, full lifecycle order→KOT→invoice→payment→shift-close, 100-concurrent gapless-numbering check, idempotency replay, SSE latency. Emit `docs/VERIFICATION_REPORT.md` with evidence per endpoint. |

**Wave 3 exit gate:**
1. All 59 endpoints return non-stub responses against live Postgres.
2. Full lifecycle passes end-to-end.
3. 100 concurrent invoices → no gaps, no duplicates.
4. Same idempotency key ×10 → one invoice.
5. `peacock-web` builds, `npm run build` exit 0, UI drives the live API.

### Wave 4 — Adversarial review

| Lane | Model | Scope |
|---|---|---|
| **W4-A** | Opus 5 | Read every Wave 1–3 diff. Hunt for: stubs reported as done, tests that cannot fail, money paths bypassing `Money`, SQL string interpolation, unwrap/panic on request paths. |
| **W4-B** | Terra | Security pass: the API has **no authentication** — enumerate exposure, confirm nothing binds 0.0.0.0 publicly, check secret handling, CORS origin list, SQL injection, log redaction. |

---

## 5. Known gaps deliberately deferred

| Gap | Why deferred |
|---|---|
| Authentication / RBAC | Wave 4-B documents the exposure. Real auth is the next phase; building UI against a moving auth contract wastes work. |
| LAN print agent | Needs physical thermal printer + EDC terminal to verify. Cannot be proven on this hardware. |
| 30-day invoice replay | Needs production invoice data. Parity harness covers arithmetic; replay covers integration. |
| Multi-currency | Schema hook only. No aggregator settles non-INR yet. |

---

## 6. Orchestrator discipline

1. No lane is trusted. Every gate is re-run by the orchestrator.
2. One file per lane where possible; overlaps are sequenced, not parallelised.
3. Any lane touching `tax.rs`, `cogs.rs`, `invoicing.rs`, or `money.rs` must leave parity 22/22.
4. Remote destructive actions (stopping services, dropping databases, deleting data dirs) are forbidden without explicit approval.
5. No commits, no pushes, no GitHub. All work stays local and on the SSH box.
