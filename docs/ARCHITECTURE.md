# Architecture

## Crate Boundaries

Peacock POS is structured as a workspace with four crates:

```
peacock-core         Pure domain logic, zero I/O
    ↑
peacock-storage      PostgreSQL adapters (sqlx)
    ↑
peacock-api          HTTP server (Axum), RFC 7807 errors, SSE
```

`peacock-parity` is independent — it validates Rust invoicing against a Python oracle.

### Dependency Direction

- `peacock-api` depends on `peacock-storage` and `peacock-core`
- `peacock-storage` depends on `peacock-core`
- `peacock-core` has **zero dependencies** on storage or HTTP. It defines the ports (traits); other crates provide the adapters.

This makes domain logic testable without infrastructure: `cargo test -p peacock-core` requires no database, no HTTP server, no external processes.

## Ports and Adapters

The domain defines what it needs in `peacock-core/src/ports.rs`:

```rust
pub trait TableRepo {
    fn list_by_room(&self, room: &RoomName) -> Result<Vec<Table>>;
    fn get(&self, name: &TableName) -> Result<Table>;
}

pub trait OrderRepo {
    fn count_separate_active(&self, tables: &[TableName]) -> Result<usize>;
}

pub trait ItemRepo {
    fn item_groups(&self, codes: &[ItemCode]) -> Result<HashMap<ItemCode, ItemGroupName>>;
}

// ... 6 more traits
```

These traits are:

- **Synchronous** — domain rules are pure lookups, not async workflows. The storage layer can block or prefetch as it sees fit.
- **Minimal** — they expose only what the domain rules need. Example: `TableRepo::list_by_room` returns all tables in a room so the merge BFS can run in one query, not `N` round-trips per hop.

Storage implementations live in `peacock-storage/src/repos/`:

- `PostgresTableRepo` → `TableRepo`
- `PostgresOrderRepo` → `OrderRepo`
- `PgMenuRepo` → `MenuRepo`
- `PgInvoiceRepo` → `SeriesAllocator` + `IdempotencyStore`
- ... and so on.

The HTTP layer (`peacock-api`) depends on these traits **through the storage crate's concrete types**, not through the traits directly. This is pragmatic: the API is the only consumer, and early abstraction would be speculative.

## The Money Rule

Money is handled uniformly across all layers:

| Layer | Representation |
|---|---|
| Domain (`peacock-core`) | `Money` newtype wrapping `rust_decimal::Decimal`, always in **paisa** (1/100 INR) |
| Database (`peacock-storage`) | `NUMERIC(18,6)` — six decimals to avoid rounding in intermediate calculations; final invoice totals are integers (paisa) |
| Wire (JSON, `peacock-api`) | **Strings** like `"123.45"`, never `Number`. Client must parse with a decimal library. |

**Why strings?** JavaScript `Number` is IEEE 754 binary64, which cannot represent `0.1` exactly. Sending `{"total": 123.45}` results in `123.44999999999999` on the client. Strings are unambiguous.

All tax and total calculations flow through `peacock_core::tax::compute_totals`, which is tested against a Python reference in the parity harness (22 test cases, zero tolerance). This ensures the Rust and Python implementations produce bitwise-identical results for every invoice line.

## Database Schema

**9 migrations**, **28 tables**. Key groups:

### Core entities (`001_core_tables.sql`)
- `restaurants`, `rooms`, `tables` — physical layout
- `production_units` — kitchen stations (Tandoor, Pantry, etc.)
- `production_unit_item_groups` — routing rules (e.g., "Breads" → Tandoor)
- `items`, `price_lists`, `item_prices` — catalog and pricing

### Menus (`002_menu_tables.sql`)
- `menus`, `menu_courses`, `menu_items` — course sequencing
- `menu_for_room`, `order_type_menu` — menu assignment by room and order type (Dine-In, Delivery, etc.)

### Bill of Materials and Bundles (`003_bom_bundle.sql`)
- `boms`, `bom_lines` — recipes (raw material → product item)
- `product_bundles`, `product_bundle_lines` — combo deals

### Kitchen Orders (`004_kot.sql`)
- `kots`, `kot_items` — kitchen order tickets, grouped by production unit

### Invoices (`005_invoice.sql`)
- `invoice_naming_series` — gapless numbering counter (row-locked)
- `invoices`, `invoice_lines` — billing, tax computation
- `idempotency_keys` — deduplication for invoice creation

### Shifts (`006_shift.sql`)
- `shifts` — open/close timestamps, revenue totals per business day

### Orders (`007_order.sql`, `009_order_lifecycle.sql`)
- `orders`, `order_items` — POS order state
- `order_idempotency_keys` — deduplication for order creation/invoicing

### Payments (`010_invoice_payments.sql`)
- `invoice_payments` — multi-tender payment records (Cash, Card, UPI, etc.)

## HTTP Layer

The API crate (`peacock-api`) is built with [Axum](https://github.com/tokio-rs/axum). Key design choices:

### Error Handling (RFC 7807)

All errors are returned as [RFC 7807 Problem Details](https://datatracker.ietf.org/doc/html/rfc7807):

```json
{
  "type": "https://peacock.example.com/problems/conflict",
  "title": "Conflict",
  "status": 409,
  "detail": "Table T1 is already occupied",
  "instance": "/api/tables/T1/merge",
  "request_id": "01932b8e-7890-7456-abcd-ef1234567890"
}
```

Content-Type is `application/problem+json`, not `application/json`. Clients should branch on this.

The error types are defined in `peacock-api/src/error.rs` as a closed enum (`ProblemKind`), so handlers cannot invent ad-hoc status/type pairings.

### Idempotency

Mutation endpoints honour an `Idempotency-Key` header:

```http
POST /api/invoices
Idempotency-Key: 01932b8e-7890-7456-abcd-ef1234567890
Content-Type: application/json

{"order_id": "..."}
```

- First request with a given key → `201 Created`, allocates an invoice number
- Repeat request → `200 OK`, returns the same invoice, **does not burn a second number**

The key is stored atomically with the allocated resource (invoice, order) in Postgres, so the gapless numbering guarantee holds even under retries.

### Server-Sent Events (SSE)

Kitchen display systems subscribe to real-time KOT updates via:

```
GET /api/events/stream
Accept: text/event-stream
```

The endpoint streams `text/event-stream` with events like:

```
event: kot.submitted
data: {"kot_id":"KOT-2024-001","production_unit":"Tandoor","items":[...]}

event: kot.modified
data: {"kot_id":"KOT-2024-001","action":"item_added","items":[...]}
```

Implementation: `peacock-api/src/events/sse.rs`.

## What Is Not Here (Yet)

- **Authentication / Authorization** — all endpoints are currently unauthenticated. This is a known gap. Multi-tenant authentication (per-restaurant isolation) is required before production deployment.
- **Frontend** — the `peacock-web` crate (Next.js POS + KDS UI) is planned but not started. This repository is backend-only.
- **COGS / Cost Accounting** — the BOM tables and COGS calculation logic are present, but the `/api/reports/cogs` endpoint returns "not yet implemented" (as of 2026-07-31).
- **Menu strategy computation** — `menu.rs:53` computes a pricing strategy and never uses it (the build emits an unused-variable warning). This is a TODO for a future lane.

See `docs/API.md` for the current status of each endpoint (implemented, stubbed, or unwired).

## Testing Strategy

### Domain Tests (`peacock-core`)
Pure unit tests, no infrastructure. Fast. Example:

```bash
cargo test -p peacock-core
```

Tests cover:
- Table merge/unmerge BFS (prevents cycles, enforces contiguity)
- Business day boundary logic (3am cutoff in IST, handles DST transitions)
- Invoice number allocation (gapless, idempotent)
- Tax computation (verified against Python oracle in parity harness)

### Storage Tests (`peacock-storage`)
Require a live Postgres connection (`DATABASE_URL`). Tests use transactions and rollback, so they do not pollute the database.

### Integration Tests (`peacock-api`)
HTTP layer tests use Axum's `oneshot` helper to exercise the full router without binding to a port. They can run with or without a database — when `DATABASE_URL` is absent, the invoice endpoints fall back to an in-memory store (same domain logic, different adapter).

### Parity Harness (`peacock-parity`)
22 test cases that run both Rust and Python invoicing logic side-by-side and diff every money figure. Zero tolerance: a single paisa discrepancy fails the build. This is the source of truth for "the port is correct."

## Further Reading

- [MASTER_PLAN.md](MASTER_PLAN.md) — wave execution strategy, lane boundaries, verified ground truth
- [API.md](API.md) — endpoint catalog, request/response shapes, implementation status
- [GROUND-TRUTH.md](GROUND-TRUTH.md) — verified system constraints (ports in use, memory, pre-existing services)
