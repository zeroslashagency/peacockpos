# Phase 2 + Phase 3 Implementation Plan
# Peacock POS — Storage & API Layers

**Target:** Build production-ready PostgreSQL storage layer and HTTP API layer
**Team:** 10-15 parallel agents across 2 phases
**Timeline:** Phase 2 (4-6 weeks) + Phase 3 (6-8 weeks) = 10-14 weeks total
**Context:** Phase 1 (domain layer) is DONE — 6,487 lines, 156 tests passing

---

## Phase 2: PostgreSQL Storage Layer (4-6 weeks)

### Overview

Implement all 12 repository traits from `ports.rs` with PostgreSQL backends. Replace Frappe patterns with normalized schema. All 156 domain tests must keep passing with real storage.

### Schema Design Principles

**Frappe → PostgreSQL mapping:**
- `docstatus` (0/1/2) → `status` ENUM per entity
- Child tables → proper FKs with `parent_id` + `idx` for ordering
- `merged_with` CSV → `merged_with` JSONB array
- `naming_series` → PostgreSQL sequences
- `owner`, `modified_by` → user ID references
- Timestamps: `created_at`, `updated_at` with triggers

**Indexes:** Every query in Phase 1 tests gets an index.

---

### Lane Structure — 8 Parallel Lanes

#### Lane 2A: Core Tables Schema + Connection Pool
**Model:** Opus 5  
**Scope:**
- Database connection pool setup (`sqlx::PgPool`, env config)
- Migration framework (sqlx-cli or custom)
- Core tables DDL:
  - `restaurants` (branch → restaurant renaming)
  - `tables` (with `merged_with` JSONB)
  - `production_units` (kitchen stations)
  - `items` (menu items with BOM flag)
  - `item_prices` (multi-pricelist support)

**Files to create:**
- `peacock-storage/src/lib.rs` (pool setup, error mapping)
- `peacock-storage/migrations/001_core_tables.sql`
- `peacock-storage/src/config.rs` (DB config from env)

**Tests:**
- Connection pool health check
- Migration runs cleanly
- All 5 tables exist with correct schema

**Success:** `cargo test -p peacock-storage` passes, 5+ tests.

---

#### Lane 2B: Table & Merge Repository
**Model:** Kimi K3  
**Scope:**
- Implement `TableRepo` trait from `ports.rs`
- Handle `merged_with` JSONB storage/retrieval
- Room-scoped table queries
- Merge cluster BFS traversal (uses `merge.rs` logic)

**Files to create:**
- `peacock-storage/src/repos/table.rs`

**Dependencies:** Lane 2A (pool + schema)

**Tests:**
- CRUD operations on tables
- `merged_with` round-trip (Vec → JSONB → Vec)
- BFS cluster retrieval matches `merge.rs` test expectations
- Concurrent merge updates don't corrupt JSONB

**Success:** 15+ tests passing, including all `merge.rs` integration tests.

---

#### Lane 2C: Menu & Price Repository
**Model:** Fable 5  
**Scope:**
- Implement `MenuRepo` and `PriceRepo` traits
- Menu resolution (3 strategies: room-wise, order-type, default)
- Price lookup with pricelist precedence
- Course ordering

**Files to create:**
- `peacock-storage/src/repos/menu.rs`
- `peacock-storage/src/repos/price.rs`
- `peacock-storage/migrations/002_menu_tables.sql` (menu, menu_item, menu_course)

**Dependencies:** Lane 2A

**Tests:**
- All `menu.rs` tests pass with real storage
- Price lookup respects pricelist precedence
- Missing price returns None (doesn't panic)

**Success:** 20+ tests passing.

---

#### Lane 2D: BOM & Bundle Repository
**Model:** Opus 5  
**Scope:**
- Implement `BomRepo` and `ProductBundleRepo` traits
- Handle 2-level BOM explosion
- Bundle → BOM → plain item precedence
- `quantity` normalisation (prevents v1's 10× bug)

**Files to create:**
- `peacock-storage/src/repos/bom.rs`
- `peacock-storage/src/repos/bundle.rs`
- `peacock-storage/migrations/003_bom_bundle.sql`

**Dependencies:** Lane 2A

**Tests:**
- All `cogs.rs` tests pass with real storage
- Parity harness BOM fixtures still green
- Missing BOM returns empty (doesn't fail)

**Success:** 15+ tests passing, parity harness remains 22/22 green.

---

#### Lane 2E: KOT Repository
**Model:** Sol  
**Scope:**
- Implement `KotRepo` trait
- Production unit routing storage
- KOT item child table with proper FKs
- Gapless KOT numbering (sequence-based)

**Files to create:**
- `peacock-storage/src/repos/kot.rs`
- `peacock-storage/migrations/004_kot.sql`

**Dependencies:** Lane 2A, 2B (needs production units)

**Tests:**
- All `kot.rs` tests pass with real storage
- N+1 query fix verified (36 queries → 3)
- KOT numbering is gapless under concurrent inserts

**Success:** 20+ tests passing.

---

#### Lane 2F: Invoice Repository
**Model:** Fable 5  
**Scope:**
- Implement `InvoiceRepo` trait
- Invoice + invoice_line (child table)
- Gapless invoice numbering (CGST Rule 46(b) compliant)
- Idempotency key support
- Status transitions (Draft → Paid → Consolidated)

**Files to create:**
- `peacock-storage/src/repos/invoice.rs`
- `peacock-storage/migrations/005_invoice.sql`

**Dependencies:** Lane 2A

**Tests:**
- All `invoicing.rs` tests pass with real storage
- All `tax.rs` tests pass
- Parity harness tax fixtures still green (13/13)
- Idempotency: same key → same invoice, no duplicate
- Gapless numbering under concurrent inserts (simulate with threads)

**Success:** 25+ tests passing, parity harness remains 22/22 green.

---

#### Lane 2G: Shift & Business Day Repository
**Model:** Kimi K3  
**Scope:**
- Implement `ShiftRepo` trait
- Business day calculation (midnight crossing fix)
- Shift open/close with Z-report data
- Cash threshold tracking (CGST Rule 56)

**Files to create:**
- `peacock-storage/src/repos/shift.rs`
- `peacock-storage/migrations/006_shift.sql`

**Dependencies:** Lane 2A

**Tests:**
- All `businessday.rs` tests pass with real storage
- Midnight-crossing bug cannot reproduce
- Shift Z-report cash delta matches invoice totals

**Success:** 15+ tests passing.

---

#### Lane 2H: Order Repository (Coordination)
**Model:** Opus 5  
**Scope:**
- Implement `OrderRepo` trait (the UI form binding, NOT the POS Invoice)
- Row-level locking for concurrent waiter protection
- Order → Invoice relationship tracking

**Files to create:**
- `peacock-storage/src/repos/order.rs`
- `peacock-storage/migrations/007_order.sql`

**Dependencies:** Lane 2F (invoice must exist first)

**Tests:**
- Two concurrent updates to same order → one blocks, no corruption
- `last_invoice` FK constraint enforced
- Order CRUD operations

**Success:** 10+ tests passing.

---

### Phase 2 Verification Gates

**After all 8 lanes complete:**

1. **Domain tests still pass:** All 156 tests in `peacock-core` pass with real Postgres storage
2. **Parity harness still green:** 22/22 fixtures match to the paisa
3. **No N+1 queries:** Instrument query count, verify KOT batch optimization works
4. **Concurrency tests pass:** Gapless numbering, row locks, idempotency under load
5. **Migration is repeatable:** `DROP DATABASE; run migrations; run tests` → green
6. **Clippy clean:** Zero warnings in `peacock-storage`

**Integration test:** Full order flow end-to-end using only repository traits.

---

## Phase 3: HTTP API Layer (6-8 weeks)

### Overview

Expose all 59 endpoints from plan v2 §4. Axum-based HTTP server with SSE for realtime KDS updates. Authentication deferred to Phase 3B (add after core endpoints work).

### API Design Principles

- **Separation:** Request/response DTOs separate from domain models
- **Validation:** At API boundary, before domain
- **Errors:** RFC 7807 Problem Details JSON
- **Idempotency:** `Idempotency-Key` header for mutations
- **Realtime:** SSE (NOT WebSocket) for KDS, order updates

---

### Lane Structure — 10 Parallel Lanes

#### Lane 3A: Axum Foundation + Middleware
**Model:** Opus 5  
**Scope:**
- Axum server setup with graceful shutdown
- Middleware stack:
  - Request ID (X-Request-ID header)
  - Structured logging (tracing)
  - Error handling (domain errors → HTTP status + Problem JSON)
  - CORS (Vercel frontend origin)
- Health check endpoint (`GET /health`)

**Files to create:**
- `peacock-api/src/main.rs`
- `peacock-api/src/middleware/request_id.rs`
- `peacock-api/src/middleware/logging.rs`
- `peacock-api/src/middleware/error.rs`
- `peacock-api/src/error.rs` (HTTP error mapping)

**Tests:**
- Server starts and responds to `/health`
- Request ID appears in logs
- Domain error maps correctly (e.g., `NotFound` → 404)

**Success:** 10+ tests passing.

---

#### Lane 3B: Table Management API
**Model:** Sol  
**Endpoints:**
- `GET /api/tables` (list, filter by room/status)
- `GET /api/tables/:id`
- `POST /api/tables/:id/merge` (merge tables)
- `POST /api/tables/:id/unmerge`
- `POST /api/tables/:id/transfer` (order transfer)

**Files to create:**
- `peacock-api/src/routes/tables.rs`
- `peacock-api/src/dto/table.rs`

**Dependencies:** Lane 3A, Phase 2 complete

**Tests:**
- All 5 endpoints respond correctly
- Merge/unmerge updates `merged_with` JSONB
- Transfer moves order between tables

**Success:** 15+ tests passing (HTTP integration tests).

---

#### Lane 3C: Menu & Item API
**Model:** Kimi K3  
**Endpoints:**
- `GET /api/menu` (resolve menu for room + order type)
- `GET /api/menu/:menu_id/items` (with courses)
- `GET /api/items/:item_code` (item details)
- `GET /api/items/:item_code/price` (price lookup with pricelist)

**Files to create:**
- `peacock-api/src/routes/menu.rs`
- `peacock-api/src/routes/items.rs`
- `peacock-api/src/dto/menu.rs`

**Dependencies:** Lane 3A, Phase 2 complete

**Tests:**
- Menu resolution returns correct strategy result
- Course ordering matches `menu.rs` logic
- Price lookup respects pricelist precedence

**Success:** 15+ tests passing.

---

#### Lane 3D: Order Creation & Modification API
**Model:** Fable 5  
**Endpoints:**
- `POST /api/orders` (create new order)
- `GET /api/orders/:id`
- `PATCH /api/orders/:id` (modify items)
- `POST /api/orders/:id/invoice` (convert to invoice)
- `DELETE /api/orders/:id` (cancel)

**Files to create:**
- `peacock-api/src/routes/orders.rs`
- `peacock-api/src/dto/order.rs`

**Dependencies:** Lane 3A, Phase 2 complete

**Tests:**
- Order CRUD operations
- Idempotency: same key → same order ID
- Invoice creation triggers KOT generation
- Row lock prevents concurrent modification

**Success:** 20+ tests passing.

---

#### Lane 3E: KOT Generation & Routing API
**Model:** Sol  
**Endpoints:**
- `POST /api/kot/generate` (for an order)
- `GET /api/kot/:id`
- `GET /api/production-units/:unit_id/pending-kots` (kitchen view)
- `POST /api/kot/:id/mark-prepared`

**Files to create:**
- `peacock-api/src/routes/kot.rs`
- `peacock-api/src/dto/kot.rs`

**Dependencies:** Lane 3A, Phase 2 complete

**Tests:**
- KOT generation routes items to correct stations
- Pending KOTs filtered by production unit
- Mark prepared updates status

**Success:** 15+ tests passing.

---

#### Lane 3F: Invoice & Payment API
**Model:** Fable 5  
**Endpoints:**
- `POST /api/invoices` (create from order)
- `GET /api/invoices/:id`
- `POST /api/invoices/:id/pay` (record payment)
- `GET /api/invoices` (list with filters: date range, status, table)
- `POST /api/invoices/:id/consolidate`

**Files to create:**
- `peacock-api/src/routes/invoices.rs`
- `peacock-api/src/dto/invoice.rs`

**Dependencies:** Lane 3A, Phase 2 complete

**Tests:**
- Invoice creation respects gapless numbering
- Payment records correctly
- Consolidation changes status
- Parity harness: invoice totals match domain layer

**Success:** 20+ tests passing, parity harness remains 22/22 green.

---

#### Lane 3G: Shift Management API
**Model:** Kimi K3  
**Endpoints:**
- `POST /api/shifts/open` (POS opening)
- `GET /api/shifts/current`
- `POST /api/shifts/close` (Z-report generation)
- `GET /api/shifts/:id/report`
- `GET /api/shifts` (history)

**Files to create:**
- `peacock-api/src/routes/shifts.rs`
- `peacock-api/src/dto/shift.rs`

**Dependencies:** Lane 3A, Phase 2 complete

**Tests:**
- Shift open/close enforces single open shift per terminal
- Z-report totals match invoice sums
- Business day calculation handles midnight crossing

**Success:** 15+ tests passing.

---

#### Lane 3H: Realtime SSE Event Stream
**Model:** Opus 5  
**Endpoints:**
- `GET /api/events/stream` (SSE endpoint)
- Event types: `order.created`, `order.updated`, `kot.generated`, `kot.prepared`, `invoice.paid`

**Architecture:**
- Postgres NOTIFY/LISTEN OR in-memory broadcast channel
- Event serialization (JSON)
- Reconnection handling (Last-Event-ID)

**Files to create:**
- `peacock-api/src/events/mod.rs`
- `peacock-api/src/events/sse.rs`
- `peacock-api/src/events/broadcaster.rs`

**Dependencies:** Lane 3A, Phase 2 complete

**Tests:**
- Client connects and receives events
- Event ordering is preserved
- Disconnected client can resume with Last-Event-ID

**Success:** 10+ tests passing.

---

#### Lane 3I: COGS & P&L Calculation API
**Model:** Fable 5  
**Endpoints:**
- `POST /api/cogs/calculate` (for invoice or date range)
- `GET /api/reports/daily-pl` (P&L for business day)
- `GET /api/reports/item-costing` (COGS breakdown by item)

**Files to create:**
- `peacock-api/src/routes/cogs.rs`
- `peacock-api/src/routes/reports.rs`
- `peacock-api/src/dto/reports.rs`

**Dependencies:** Lane 3A, Phase 2 complete

**Tests:**
- COGS calculation matches parity harness
- P&L sums match shift close totals
- Unset BOM items surface in response

**Success:** 15+ tests passing, parity harness remains 22/22 green.

---

#### Lane 3J: Aggregator Integration API (Swiggy/Zomato)
**Model:** Sol  
**Endpoints:**
- `POST /api/aggregators/orders` (webhook receiver)
- `GET /api/aggregators/orders/:id`
- `POST /api/aggregators/orders/:id/accept`
- `POST /api/aggregators/orders/:id/reject`
- `GET /api/aggregators/settlements` (payout reconciliation)

**Files to create:**
- `peacock-api/src/routes/aggregators.rs`
- `peacock-api/src/dto/aggregator.rs`

**Dependencies:** Lane 3A, Phase 2 complete

**Tests:**
- Webhook signature validation
- Aggregator order converts to internal order
- Settlement reconciliation matches payout

**Success:** 15+ tests passing.

---

### Phase 3 Verification Gates

**After all 10 lanes complete:**

1. **All 59 endpoints respond:** Smoke test every route
2. **End-to-end flows work:**
   - Order creation → KOT generation → preparation → invoice → payment → shift close
   - Table merge → order transfer → split invoice
   - Aggregator order → acceptance → invoice → COGS calculation
3. **SSE events fire correctly:** Order updates reach KDS in <1s
4. **Parity harness still green:** 22/22 fixtures, API layer doesn't break money arithmetic
5. **Idempotency works:** Replay same request 10 times → same result, no duplicates
6. **Error responses are RFC 7807 compliant**
7. **Clippy clean:** Zero warnings in `peacock-api`

**Load test (optional, defer to Phase 8):**
- 100 concurrent orders/sec
- SSE fan-out to 50 concurrent KDS clients

---

## Dependency Graph (Critical Path)

```
Phase 2:
  2A (Foundation) ━━━┳━━━━━━━━━━━━━━━━━━━━━━━━━━┓
                     ┣━━━━━━━━━━━━━━━━━━━━━━━━━━┫
                     ┃                          ┃
  ┌──────────────────┼──────────────────────────┼────────────┐
  ↓                  ↓                          ↓            ↓
  2B (Table)    2C (Menu)    2D (BOM)    2E (KOT)    2G (Shift)
  ↓                  ↓                          ↓            ↓
  │                  └──────────┬───────────────┘            │
  │                             ↓                            │
  │                        2F (Invoice) ←────────────────────┘
  │                             ↓
  └──────────────────────→ 2H (Order)

Phase 3:
  3A (Axum) ━━━┳━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
                ┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫
                ┃                                    ┃
  ┌─────────────┼────────────────────────────────────┼──────────┐
  ↓             ↓                                    ↓          ↓
  3B (Tables)  3C (Menu)  3D (Orders)  3E (KOT)  3F (Invoice)  3G (Shifts)
                                 ↓                      ↓
                            3H (SSE Events) ←───────────┘
                                 ↓
                            3I (COGS/P&L)
                                 ↓
                            3J (Aggregators)
```

**Critical path:** 2A → 2F → 2H → 3A → 3F → 3I  
**Estimated:** 18-24 weeks solo, 10-14 weeks with 10-agent parallelism

---

## Team Dispatch Strategy

### Model Selection Per Lane Type

| Lane Type | Recommended Model | Why |
|-----------|------------------|-----|
| Foundation, coordination | **Opus 5** | Handles state, architecture decisions |
| Money paths (invoice, tax, COGS) | **Fable 5** | Extended thinking, verified arithmetic |
| Complex domain logic (BOM, merge) | **Kimi K3** | Deep reasoning, edge case handling |
| CRUD endpoints, simple repos | **Sol** | Fast, reliable for straightforward work |

### Verification Strategy

**Each lane gets TWO agents:**
1. **Implementation agent** (assigned model from above)
2. **Verification agent** (DIFFERENT model, preferably Opus 5)

**Verification agent tasks:**
- Run tests, confirm all pass
- Run clippy, confirm zero warnings
- Check for SQL injection risks (use parameterized queries)
- Verify integration with other lanes (if dependencies exist)
- Confirm parity harness still green (for money lanes)

**Gate:** Implementation agent delivers → Verification agent approves → Lane marked done.

---

## Risk Assessment & Mitigation

### Risk 1: Schema Design Mistakes
**Impact:** Hard to fix after data exists  
**Mitigation:**
- Lane 2A (Opus 5) designs schema first, other lanes review
- Run migration + full test suite before considering schema frozen
- Keep ALTER TABLE migrations for Phase 4+ changes

### Risk 2: Connection Pool Exhaustion
**Impact:** API timeouts under load  
**Mitigation:**
- Configure pool size based on `(2 × num_cpus) + effective_spindle_count`
- Monitor pool usage in Phase 3
- Use PgBouncer if needed (defer to Phase 8)

### Risk 3: Transaction Isolation Issues
**Impact:** Gapless numbering breaks, idempotency fails  
**Mitigation:**
- Use `SERIALIZABLE` isolation for invoice/KOT numbering
- Test with concurrent load (simulate 10 parallel requests)
- Document retry behavior for serialization failures

### Risk 4: SSE Fan-Out Scalability
**Impact:** 100+ KDS clients, event delivery slow  
**Mitigation:**
- Design for 50 concurrent clients (Phase 3 target)
- Measure latency: event created → client receives
- If >1s, switch to Redis Pub/Sub or separate event service (Phase 8)

### Risk 5: Parity Harness Breaks
**Impact:** Money arithmetic regresses silently  
**Mitigation:**
- Run parity harness in every lane's CI
- Any lane that touches `tax.rs`, `cogs.rs`, `invoicing.rs` must keep harness green
- Treat parity failure as blocker (no other work proceeds)

### Risk 6: API/Domain Coupling
**Impact:** Cannot change domain without breaking API  
**Mitigation:**
- Keep request/response DTOs separate from domain models
- Use explicit mapping (e.g., `impl From<Order> for OrderResponse`)
- Domain changes require conscious DTO updates

---

## Success Criteria (Phase 2 + 3 Complete)

1. ✅ **All 156 domain tests pass** with real Postgres storage
2. ✅ **Parity harness remains 22/22 green** throughout
3. ✅ **All 59 API endpoints respond** with correct status codes
4. ✅ **End-to-end flows work:** Order → KOT → Invoice → Payment → Shift Close
5. ✅ **Idempotency verified:** Same key = same result, no duplicates
6. ✅ **SSE delivers events** to KDS clients in <1s
7. ✅ **Zero clippy warnings** across `peacock-core`, `peacock-storage`, `peacock-api`
8. ✅ **SQL injection impossible:** All queries use `sqlx::query!` macros or parameterized
9. ✅ **Gapless numbering under load:** 100 concurrent invoice creates → no gaps, no duplicates
10. ✅ **Error responses are RFC 7807 compliant**

---

## Next Steps After Phase 2+3

**Phase 4:** LAN Print Agent (2-3 weeks) — ESC/POS thermal printing, offline queue, Indic script bitmap rendering  
**Phase 5:** Authentication & Authorization (2-3 weeks) — JWT, role-based access, waiter/manager/admin  
**Phase 6:** Aggregator Settlement Reconciliation (2 weeks) — Payout matching, dispute tracking  
**Phase 7:** Multi-Currency Support (2 weeks) — USD/EUR settlement, exchange rates  
**Phase 8:** Cutover & Optimization (2-3 weeks) — 30-day invoice replay, load testing, production deployment

---

**Plan Status:** ✅ Ready for dispatch  
**Estimated Delivery:** 10-14 weeks with 10-agent parallelism  
**Budget:** ~$200-400 in compute (Postgres, agent runs)

