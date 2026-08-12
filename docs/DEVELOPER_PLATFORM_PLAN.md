# Peacock POS — Developer Platform + Auth + Dashboard Plan

> **Goal:** From `https://peacockpos.vercel.app` today (POS/KDS/Shifts only, no auth, no developer view) → **Developer-grade platform** where you (owner/dev) can add users, audit live service, manage system, and compete with Petpooja (PitPooja).
>
> **Date:** 2026-08-12 | **Stack:** Next 16.3 + Rust Axum 0.7 + Postgres 16 + Hetzner cpx12 2.28.30.22:8080 + Vercel + SSE

---

## 1. Where we are — Audit (live)

### 1.1 What we have (implements)

| Layer | Count | Implements | Source |
|---|---|---|---|
| **Backend Rust** | 40 endpoints | `health`, `tables` (list/merge/unmerge/transfer), `menu` (resolve/items), `items` (price), `orders` (create/patch/cancel/invoice), `kot` (generate/pending/mark-prepared), `invoices` (create/list/pay/consolidate), `shifts` (open/current/close/report/list), `cogs` (calculate), `reports` (daily-pl/item-costing), `aggregators` (webhook/accept/reject/settlements), `events` SSE | `peacock-api/src/routes/*.rs` `docs/API.md` |
| **Core** | 36 doctypes, 32 tables | `NUMERIC(18,6)` money as `String`, `half-away-from-zero`, gapless `UPDATE RETURNING` invoicing, BFS merge, `MAX_LEVEL=2` BOM, GST split | `peacock-core 6,487 lines 156 tests` `peacock-storage 10 migrations` |
| **Frontend** | 4 routes | `/` (home bento), `/pos` (floor 3 + menu 4 + cart), `/kds` (live status + carousel), `/shifts` (timeline + ZReport) | `peacock-web` `Next 16.3` `Tailwind` `Framer` `Phosphor` |
| **Infra** | 2 | Hetzner `cpx12 1vCPU 2GB $13.49` `2.28.30.22:8080` + Vercel `peacockpos.vercel.app` with `rewrites` proxy (no mixed-content) | `docker-compose.yml` `next.config.ts` |

### 1.2 What we don't have (gaps)

| Area | Status | Gap |
|---|---|---|
| **Auth/RBAC** | ❌ Nothing | No login, no JWT, no roles (waiter/cashier/manager/owner/dev), no `X-Restaurant` → `CallerContext` binding, no session, `40` endpoints open + `test-secret-key` HMAC |
| **Developer Settings** | ❌ Missing | No `/settings` at all — no user CRUD, no roles, no API keys, no webhooks UI, no branch/terminal config, no feature flags |
| **Dashboard** | ❌ Missing | No owner view of live: no active orders, no revenue today, no KDS backlog, no shift cash, no error logs, no system health |
| **User Management** | ❌ Missing | No add user, no invite, no password reset, no deactivate |
| **Observability** | Partial | `tracing` logs server-only, no UI; no audit log (`who did what on which table/order`); no SSE health in UI |
| **Petpooja parity** | ~35% | See §2 — missing inventory, CRM/loyalty, online ordering, multi-outlet, advanced reports, payment gateway, printer agent |

**Money, CORS, SSE, gapless — PASS** (`parity 22/22`, `core 156/156`). **Auth, isolation, observability — FAIL** (see `docs/history/W4_SECURITY.md` 2 CRITICAL).

---

## 2. Competitor — Petpooja (PitPooja) vs Peacock

> Petpooja = India's #1 restaurant POS (billing + KOT + inventory + aggregators + CRM). Peacock today is ~35% of its surface.

### 2.1 Feature matrix

| Petpooja Feature | Peacock Has? | Gap | Priority |
|---|---|---|---|
| **Billing POS** (floor/table, quick cart, split/merge, discounts, round-off) | ✅ 90% | Missing split bill, NC/KOT cancel, table transfer → order/invoice (we have `transfer` TODO) | **P1** |
| **KDS** (kitchen stations, fire/ready, bump) | ✅ 80% | Missing station-wise routing rules UI, KDS bump → invoice auto, printer agent (LAN thermal) | **P1** |
| **Inventory** (stock ledger, purchase, consumption via BOM, low-stock alert) | ⚠️ 30% | We have `BOM` + `COGS` calc, but no purchase, no stock ledger UI, no `Stock Ledger` post on invoice | **P0** for COGS truth |
| **Menu/Recipe** (BOM, product bundles) | ✅ 70% | We have `boms`/`product_bundles` + `is_bom` trigger, but no recipe cost UI | **P1** |
| **Aggregators** (Swiggy/Zomato webhook → order → KOT) | ⚠️ 60% | We have `POST /api/aggregators/orders` HMAC + `accept` → real `order/KOT` (fixed `999999`), but no auto-print, no settlement reconciliation UI | **P1** |
| **CRM / Loyalty** | ❌ 0% | No customer DB, no phone lookup, no points, no wallet | **P2** |
| **Online ordering** (own QR/URL) | ❌ 0% | No customer-facing ordering page | **P2** |
| **Payments** (cash/card/UPI split, gateway) | ✅ 60% | We have `pay` `Cash/Card/Upi/Wallet/Credit` + `outstanding`, but no gateway (Razorpay), no split UI | **P1** |
| **Reports** (day P&L, item costing, Z-report) | ✅ 70% | We have `daily-pl` `item-costing` `ZReport` (single `REVENUE`), but no 30-day, no outlet-wise, no export CSV | **P1** |
| **Multi-outlet / Branch** | ❌ 0% | Single `Peacock Restaurant` `Main Branch` only; no outlet switcher, no `X-Restaurant` scoping per user | **P0** for dev |
| **User/Roles** | ❌ 0% | Petpooja: Owner/Manager/Captain/Cashier + permissions; Peacock: `no auth` | **P0** |
| **Settings** (printer, tax, terminal, receipt) | ❌ 0% | No UI — all env `PEACOCK_*` | **P0** |
| **Owner Dashboard** (live sales, active KOTs, low stock) | ❌ 0% | No UI | **P0** |
| **Audit log** | ❌ 0% | No UI | **P0** |

**Can we compete today?** **No** — we cover *service* (POS→KOT→Invoice) but Petpooja covers *operations* (inventory→staff→loyalty→online). We need **Developer + Auth + Dashboard** to get to 60% and be sellable to one outlet.

---

## 3. Developer Platform — What you asked

> “I’m the developer, I want to add new user. I want to check what is currently going on and everything about the dashboard for me.”

### 3.1 IA — `/settings` under Developer

```
/settings
  /settings/profile          — you (owner/dev) profile
  /settings/users            — P0 — add user, roles, deactivate, reset
  /settings/roles            — P0 — RBAC matrix (waiter/cashier/manager/owner)
  /settings/branches         — P0 — branches + terminals (Main Branch → T1/T2/T3)
  /settings/menu             — P1 — menus, courses, items, prices, BOM (today via DB only)
  /settings/printers         — P1 — LAN print agent, KOT stations → printers
  /settings/api-keys         — P0 — issue/revoke API keys (replaces X-Restaurant spoof)
  /settings/webhooks         — P0 — aggregator HMAC secrets, retry log
  /settings/audit            — P0 — who did what (order/KOT/invoice/shift) with X-Request-ID
  /settings/system           — P0 — health (Hetzner 2.28.30.22, DB 33 tables, pool, SSE clients), logs tail
  /settings/billing          — P2 — Hetzner $13.49, Vercel hobby, traffic 0/20TB
```

**Nav:** `Peacock` `POS` `KDS` `Shifts` **`Developer`** `▾` `Settings` + `Dashboard` (owner only). `Developer` visible only to `role=owner/dev`.

### 3.2 Auth — must build first

| Piece | Design |
|---|---|
| **Login** | `POST /api/auth/login` `{email,password}` → `HttpOnly Secure SameSite=Lax` cookie `peacock_session` (JWT `sub, role, restaurant, branch`) + `X-CSRF` header; `POST /api/auth/logout` clears |
| **Me** | `GET /api/auth/me` → `CallerContext` from cookie (not `X-Restaurant`) |
| **RBAC** | `owner > manager > cashier > waiter` — middleware `require_role!(manager)`; `waiter` can `create order/KOT` but not `close shift`; `cashier` can `pay/consolidate`; `manager` can `daily-pl`; `owner` can `users/api-keys` |
| **User Store** | `users` table `id, email, password_hash (argon2), role, restaurant, branch, active, created_by`, seed `owner@peacock.local / dev` |
| **Frontend** | `/login` `email + password` `rounded-[2.5rem]` `Geist` + `useAuth` hook + `ShellNav` shows `you · role` + `Logout` |

Without this, `X-Restaurant` is spoofable and W4 `CRITICAL` 3b/6 remain.

### 3.3 Owner Dashboard — “what is going on”

```
/dashboard (owner/dev only)
  KPI row:  Today revenue (REVENUE) · COGS · Gross · Active orders (open) · KDS backlog (pending) · Shift cash (if open)
  Live:  SSE `order_created, kot_generated, invoice_paid` → `AnimatePresence` list + `breathing` dots
  System:  Hetzner `cpx12 1vCPU 2GB` `2.28.30.22:8080` `POSTGRES 16` `pool 5` `SSE 12 clients` + recent `x-request-id` errors
  Shifts:  Current shift `opened_at` `T-...` `Z today` sparkline 7d
```

**Data:** `GET /api/dashboard/summary` (single `REVENUE` + `COGS` + `shifts/current` + `orders?status=open` + `kots pending`) — one round-trip, half-open `[start,end)` `Asia/Kolkata`.

---

## 4. All flows — are they really working?

### 4.1 End-to-end (MCP via `apiBase`)

```
1. health             GET /health → 200 {"status":"ok"}
2. login              POST /api/auth/login → 200 Set-Cookie (new)
3. tables             GET /api/tables → 200 3 tables (T1/T2/T3) — after seed
4. menu               GET /api/menu -H X-Restaurant → 200 4 items (after hard-coded rewrites)
5. order              POST /api/orders + Idempotency-Key → 201 {id, grand_total String}
6. kot                POST /api/kot/generate → 200 {kots:1}
7. invoice            POST /api/orders/:id/invoice → 201 {invoice_name POS-...}
8. pay                POST /api/invoices/:id/pay {Cash, amount String} → 200 {paid_amount, outstanding 0}
9. shift              POST /api/shifts/open → 201 → POST /api/shifts/:id/close → 200 ZReport cash_threshold_warning
10. SSE               GET /api/events/stream → 200 text/event-stream kot.generated
```

**Current MCP check (Hetzner direct):** `health` `tables` `menu` `invoices` `shifts` ✅ `200`. **Vercel via rewrites:** `https://peacockpos.vercel.app/api/menu` ✅ `200` after `b8dc50f` (before `=http` bug → `404` HTML + `308`).

**Prod skills to keep flow green:** `peacock-parity 22/22` (money `half-away`), `clippy -D warnings 0`, `cargo test --workspace` (with `DATABASE_URL`), `npm run build` 7 routes.

### 4.2 Verify via MCP (how)

```bash
# Hetzner
curl -H "X-Restaurant: Peacock Restaurant" http://2.28.30.22:8080/api/tables
curl -H "X-Restaurant: Peacock Restaurant" http://2.28.30.22:8080/api/menu
# Vercel (same-origin rewrites, no http from browser)
curl -H "X-Restaurant: Peacock Restaurant" https://peacockpos.vercel.app/api/menu
# SSE
curl -N https://peacockpos.vercel.app/api/events/stream  # or Hetzner :8080
```

**With product dev skills:** use `debugger` for `500` → check `X-Request-ID` in `peacock-api` logs `docker compose logs api` + `tracing` + `RUST_LOG=info`; `supabase` skill not needed (we use `sqlx`).

---

## 5. Optimize-loop — `/workflows /goal /user:optimize-loop`

> **Goal:** `https://peacockpos.vercel.app` + `2.28.30.22:8080` → **sellable to one outlet** without Petpooja gaps that block service.

**Metric to minimize:** `time-to-serve` (POS open → KOT fire) + `W4 high findings` (auth, money)  
**Correctness gate:** `parity 22/22` + `cargo test` + `npm run build` + `MCP flows 10/10` + `no #000 / no Inter / no 3-col`  
**Workflow:** `Rhai` `agent_budget 128` `parallel 4` `isolated worktree` per lane

### 5.1 Loop slices (each one `if correctness gate fails → revert`)

| Slice | Budget | Lane | Verifies |
|---|---|---|---|
| **S0 Auth slice** | 1 | `migrations 012_users.sql` + `auth.rs` `login/me/logout` + `middleware auth` + `/login` page | `curl /api/auth/login` `200` + `GET /api/tables` without cookie `401` |
| **S1 Developer slice** | 2 | `/settings/users` CRUD + `/settings/roles` matrix + `seed owner` | `POST /api/users` as owner `201` vs waiter `403` |
| **S2 Dashboard slice** | 1 | `GET /api/dashboard/summary` + `/dashboard` KPI + live SSE list | `curl /api/dashboard/summary` `200` `revenue String` |
| **S3 Hardening slice** | 1 | Strip `X-Restaurant` spoof → `CallerContext` from session, fix HMAC `test-secret-key` (require `PEACOCK_WEBHOOK_SECRET`), CORS `*` reject, `0.0.0.0` bind guard | `W4_SECURITY 0 CRITICAL` |

**Run:** `npx @agent-native/core plan local serve --dir plans/developer-platform` → `workflow` `optimize-loop` with `agent_budget 32`.

---

## 6. Roadmap — what to build next

| Wave | Scope | Out |
|---|---|---|
| **W5 Auth** (1w) | `012_users.sql` + `argon2` + `POST /api/auth/*` + `GET /api/auth/me` + `middleware` + `/login` + `ShellNav` `you` | `peacockpos.vercel.app/login` `401` fail-closed |
| **W6 Developer** (1.5w) | `/settings/users` + `/settings/roles` + `/settings/api-keys` + `/settings/system` + `/settings/audit` | Owner can add `cashier@` and see `system 2.28.30.22` `pool 5` |
| **W7 Dashboard** (1w) | `/dashboard` KPI + live + system | Owner sees `today ₹12,400` `4 KOT pending` live |
| **W8 Petpooja gaps** (2w) | `split bill`, `KDS station UI`, `purchase/inventory`, `reports 30d` | 60% parity |

**File hygiene:** `peacock-web/src/app/(settings)/settings/*` `peacock-web/src/app/(dashboard)/dashboard/page.tsx` `peacock-api/src/routes/auth.rs` `peacock-storage/migrations/012_users.sql`.

---

## 7. Pre-flight (taste-v1 + product)

- [ ] `Geist` + `mono` money, `bg-[#f9fafb]` `rounded-[2.5rem]` `border-slate-200/50` `max-w-[1400px]` `min-h-[100dvh]`
- [ ] `grid` not `flex-math`, no `h-screen`, no `#000`, no `Inter`, no `3-col`
- [ ] Empty/loading/error + `spring 100/20` + `AnimatePresence` isolated
- [ ] `cargo test` + `parity 22/22` + `npm run build` + `MCP` `10/10` before promote
- [ ] No `X-Restaurant` spoof, no `test-secret-key`, no `0.0.0.0` public

**Next:** `S0` Auth slice — do you want me to start `S0` via `optimize-loop` workflow (Rhai `agent` + `parallel` + `judge`) with `opus 5` + `spark`?
