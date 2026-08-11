# Phase 4 + 5 Implementation Plan
# Peacock POS — Integration, Authentication & Print Agent

**Target:** Complete the remaining critical-path work to reach production-ready state
**Team:** 6-8 parallel agents (Opus 5, Sol, Terra)
**Timeline:** 3-4 weeks with agent parallelism
**Context:** Phase 1-3 complete (35,069 lines, 542+ tests), needs integration + auth + printing

---

## Overview

Three remaining critical phases:
1. **Phase 4A:** Integration (wire Phase 2 repos into Phase 3 API)
2. **Phase 4B:** Authentication & Authorization
3. **Phase 4C:** LAN Print Agent (offline-capable)

---

## Phase 4A: Integration (Week 1)

### Objective
Wire all Phase 2 PostgreSQL repositories into Phase 3 API, replacing in-memory stores.

### Lane Structure — 4 Parallel Lanes

#### Lane 4A-1: Core Integration (Opus 5)
**Scope:**
- Update `peacock-api/src/state.rs` to hold all repos
- Wire `Storage` from `peacock-storage` into `AppState`
- Replace in-memory `OrderStore` with `PostgresOrderRepo`
- Database connection in main.rs (read `DATABASE_URL`)

**Files to modify:**
- `peacock-api/src/state.rs`
- `peacock-api/src/main.rs`
- `peacock-api/src/routes/orders.rs` (remove in-memory store)

**Success criteria:**
- Server starts with real Postgres connection
- All order endpoints hit real database
- 10+ integration tests pass

---

#### Lane 4A-2: Menu & Price Integration (Sol)
**Scope:**
- Wire `PgMenuRepo` and `PgPriceRepo` into handlers
- Update menu resolution endpoints
- Update price lookup endpoints

**Files to modify:**
- `peacock-api/src/routes/menu.rs`
- `peacock-api/src/routes/items.rs`

**Success criteria:**
- Menu resolution hits Postgres
- Price lookup returns real data
- 15+ tests pass

---

#### Lane 4A-3: Invoice & KOT Integration (Opus 5)
**Scope:**
- Wire `PgInvoiceRepo` and `PgKotRepo` into handlers
- Gapless numbering under real load
- Idempotency with real database

**Files to modify:**
- `peacock-api/src/routes/invoices.rs`
- `peacock-api/src/routes/kot.rs`

**Success criteria:**
- Invoice creation allocates real gapless numbers
- KOT generation routes to real stations
- Parity harness remains 22/22 green
- 20+ tests pass

---

#### Lane 4A-4: Shift & Table Integration (Sol)
**Scope:**
- Wire `PgShiftRepo` and `PgTableRepo` into handlers
- Shift open/close with real Z-reports
- Table merge with real JSONB

**Files to modify:**
- `peacock-api/src/routes/shifts.rs`
- `peacock-api/src/routes/tables.rs`

**Success criteria:**
- Shift operations work end-to-end
- Table merge updates Postgres JSONB
- 15+ tests pass

---

### Phase 4A Verification Gates

1. ✅ All 59 endpoints hit real Postgres
2. ✅ Parity harness 22/22 green (money still correct)
3. ✅ End-to-end flow: Order → KOT → Invoice → Payment → Shift Close
4. ✅ Gapless numbering under 100 concurrent requests
5. ✅ Idempotency proven with real database
6. ✅ All workspace tests pass (540+)

---

## Phase 4B: Authentication & Authorization (Week 2)

### Objective
Add JWT-based authentication with role-based access control (waiter, manager, admin).

### Lane Structure — 3 Parallel Lanes

#### Lane 4B-1: Auth Foundation (Opus 5)
**Scope:**
- JWT generation and validation
- Middleware: extract and validate JWT from `Authorization: Bearer <token>`
- User model and storage (username, password hash, role)
- Login endpoint: `POST /api/auth/login`
- Token refresh endpoint: `POST /api/auth/refresh`

**Files to create:**
- `peacock-storage/migrations/008_auth.sql` (users table)
- `peacock-storage/src/repos/auth.rs`
- `peacock-api/src/middleware/auth.rs`
- `peacock-api/src/routes/auth.rs`
- `peacock-api/src/dto/auth.rs`

**Dependencies:**
- `jsonwebtoken` crate
- `argon2` or `bcrypt` for password hashing

**Success criteria:**
- Login returns valid JWT
- Token validation middleware works
- 15+ tests pass

---

#### Lane 4B-2: Role-Based Access Control (Sol)
**Scope:**
- Define roles: Waiter, Manager, Admin
- Permissions matrix:
  - Waiter: create/update orders, generate KOT
  - Manager: close shifts, view reports
  - Admin: all operations
- Middleware: check role against endpoint requirements

**Files to create:**
- `peacock-api/src/middleware/rbac.rs`
- `peacock-core/src/auth.rs` (role definitions)

**Files to modify:**
- All route handlers (add role requirements)

**Success criteria:**
- Waiter cannot close shift (403)
- Manager can close shift (200)
- Admin can do everything (200)
- 20+ tests pass

---

#### Lane 4B-3: Session Management (Terra)
**Scope:**
- Token expiry and refresh
- Logout (token blacklist or short expiry)
- Multi-device support (optional)

**Files to modify:**
- `peacock-api/src/routes/auth.rs`
- `peacock-storage/src/repos/auth.rs`

**Success criteria:**
- Expired token returns 401
- Refresh generates new token
- Logout invalidates token
- 10+ tests pass

---

### Phase 4B Verification Gates

1. ✅ All endpoints require authentication
2. ✅ Role-based access enforced (waiter cannot close shift)
3. ✅ JWT validation works
4. ✅ Password hashing secure (argon2 or bcrypt)
5. ✅ Token expiry and refresh work
6. ✅ 45+ auth tests pass

---

## Phase 4C: LAN Print Agent (Week 3-4)

### Objective
Build offline-capable LAN print agent for thermal printers and EDC terminals.

### Lane Structure — 3 Parallel Lanes

#### Lane 4C-1: ESC/POS Thermal Printing (Opus 5)
**Scope:**
- ESC/POS command generation
- Print KOT to thermal printer (58mm and 80mm)
- Print invoice receipt
- Support for Indic scripts (Devanagari/Tamil as bitmap)

**Files to create:**
- `peacock-print/` (new crate)
- `peacock-print/src/escpos.rs` (command builder)
- `peacock-print/src/printer.rs` (network socket to printer)
- `peacock-print/src/templates/kot.rs`
- `peacock-print/src/templates/invoice.rs`

**Dependencies:**
- `escpos` crate (or build from scratch)
- Raw socket to port 9100 (standard thermal printer port)

**Success criteria:**
- KOT prints to thermal printer
- Invoice receipt prints correctly
- Column math correct for 58mm and 80mm
- 10+ tests pass (mock printer)

---

#### Lane 4C-2: Offline Print Queue (Sol)
**Scope:**
- Local SQLite queue for print jobs
- Retry failed prints (network down, paper out)
- Print job status tracking (pending, printed, failed)

**Files to create:**
- `peacock-print/src/queue.rs`
- `peacock-print/migrations/001_print_queue.sql` (SQLite)

**Success criteria:**
- Print jobs queued when printer offline
- Jobs retry when printer comes back
- Failed jobs flagged for manual intervention
- 10+ tests pass

---

#### Lane 4C-3: Print Agent HTTP API (Terra)
**Scope:**
- HTTP server on LAN (bind to local IP)
- Endpoints:
  - `POST /print/kot` — print KOT
  - `POST /print/invoice` — print invoice
  - `GET /print/status` — printer status
  - `GET /print/queue` — pending jobs
- Discovery: mDNS or static IP config

**Files to create:**
- `peacock-print/src/main.rs` (Axum server)
- `peacock-print/src/routes/print.rs`

**Success criteria:**
- Print agent runs on LAN
- Peacock API can call print agent
- Printer status reported correctly
- 10+ tests pass

---

### Phase 4C Verification Gates

1. ✅ KOT prints to thermal printer (58mm and 80mm)
2. ✅ Invoice receipt prints correctly
3. ✅ Offline queue works (printer down → queued → retried)
4. ✅ Print job audit log for GST inspectors
5. ✅ Indic script bitmaps render (Devanagari/Tamil)
6. ✅ 30+ print tests pass

---

## Dependency Graph

```
Phase 4A (Integration) → Week 1
  ├─ 4A-1 (Core) ────────┐
  ├─ 4A-2 (Menu/Price)   ├─ All block Phase 4B
  ├─ 4A-3 (Invoice/KOT)  │
  └─ 4A-4 (Shift/Table)──┘
                         ↓
Phase 4B (Auth) → Week 2
  ├─ 4B-1 (JWT)─────────┐
  ├─ 4B-2 (RBAC)        ├─ All block Phase 4C
  └─ 4B-3 (Session)─────┘
                         ↓
Phase 4C (Print) → Week 3-4
  ├─ 4C-1 (ESC/POS)
  ├─ 4C-2 (Queue)
  └─ 4C-3 (HTTP API)
```

**Critical path:** 4A → 4B → 4C (sequential phases, parallel lanes within each)

---

## Team Dispatch Strategy

### Model Selection

| Lane Type | Model | Why |
|-----------|-------|-----|
| Integration, coordination | **Opus 5** | Handles state, complex wiring |
| CRUD, straightforward work | **Sol** | Fast, reliable |
| Session management, HTTP | **Terra** | Good for API/network work |

### Verification Strategy

Each lane gets:
1. **Implementation agent** (assigned model)
2. **Verification agent** (different model, preferably Opus 5)

**Gate:** Implementation → Verification approves → Lane marked done.

---

## Success Criteria (Phase 4A+4B+4C Complete)

1. ✅ All 59 endpoints hit real Postgres (no stubs)
2. ✅ All endpoints require authentication
3. ✅ Role-based access enforced
4. ✅ Parity harness 22/22 green throughout
5. ✅ End-to-end flow: Order → KOT (printed) → Invoice (printed) → Payment → Shift Close
6. ✅ Gapless numbering under load
7. ✅ Offline print queue works
8. ✅ All workspace tests pass (600+)
9. ✅ Clippy clean
10. ✅ Ready for production pilot

---

## Risk Assessment

### Risk 1: Integration Breaks Existing Tests
**Impact:** Phase 1-3 tests fail after integration  
**Mitigation:** Run full test suite after each integration lane

### Risk 2: Auth Breaks Existing API Tests
**Impact:** All endpoint tests fail (need auth tokens)  
**Mitigation:** Update test fixtures with valid JWTs

### Risk 3: Printer Hardware Unavailable
**Impact:** Cannot test real printing  
**Mitigation:** Mock printer in tests, document real printer setup

### Risk 4: Offline Queue Complexity
**Impact:** SQLite + retry logic + network detection  
**Mitigation:** Start simple (queue + manual retry), iterate

---

## Timeline Estimate

| Phase | Duration | Parallel Lanes | Model Mix |
|-------|----------|----------------|-----------|
| **4A: Integration** | 1 week | 4 | 2× Opus 5, 2× Sol |
| **4B: Auth** | 1 week | 3 | 1× Opus 5, 1× Sol, 1× Terra |
| **4C: Print** | 2 weeks | 3 | 1× Opus 5, 1× Sol, 1× Terra |

**Total:** 3-4 weeks with agent parallelism (vs 8-12 weeks solo)

---

## After Phase 4A+4B+4C

**You will have:**
- Complete, integrated, authenticated POS system
- All 59 endpoints working against real Postgres
- Thermal printing (KOT + Invoice)
- Offline-capable print queue
- Role-based access control
- 600+ tests passing
- Parity harness green

**Remaining for production:**
- Phase 5: 30-day invoice replay (validation)
- Phase 6: Load testing & optimization
- Phase 7: Production deployment
- Phase 8: Monitoring & alerting

**Estimated time to first production order:** 4-5 weeks from now.

---

**Status:** ✅ Plan Ready  
**Next Step:** Dispatch Phase 4A (Integration) — 4 lanes in parallel  
**Budget:** ~$300-500 in compute
