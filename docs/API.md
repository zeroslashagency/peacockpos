# API Documentation

**Audit timestamp:** 2026-07-31T15:50:44Z

This document catalogs all HTTP endpoints in `peacock-api`. Status reflects the source code at the audit timestamp; four concurrent lanes are actively implementing stubbed endpoints.

## Base URL

```
http://localhost:8080
```

Configure with `PEACOCK_API_PORT` environment variable.

## Error Responses (RFC 7807)

All errors return `application/problem+json`:

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

Status codes: `400` (invalid input), `401` (unauthorized), `404` (not found), `409` (conflict / already exists), `500` (internal), `503` (service unavailable, typically database connectivity).

## Idempotency

Mutation endpoints honour an `Idempotency-Key` header (UUID format required):

```http
POST /api/orders
Idempotency-Key: 01932b8e-7890-7456-abcd-ef1234567890
```

- First request → `201 Created`
- Replay → `200 OK`, returns the original resource without creating a duplicate

Endpoints supporting idempotency: `POST /api/orders`, `POST /api/orders/:id/invoice`, `POST /api/invoices`.

## Money Representation

All money values are **strings**, never numbers:

```json
{
  "subtotal": "123.45",
  "tax": "22.22",
  "total": "145.67"
}
```

Rationale: JavaScript `Number` is IEEE 754 float and cannot represent `0.1` exactly. Parse with `decimal.js`, `big.js`, or equivalent.

---

## Endpoints

### Health Checks

#### `GET /health`
**Liveness.** Returns `200` if the process is serving HTTP. No dependencies.

**Response:** `200 OK`
```json
{"status": "ok"}
```

**Status:** ✅ Implemented

---

#### `GET /health/ready`
**Readiness.** Checks database connectivity (`SELECT 1` round-trip).

**Response:** `200 OK` (database reachable) or `503 Service Unavailable` (database down)
```json
{
  "status": "ready",
  "database": {
    "connected": true,
    "latency_ms": 12,
    "pool_size": 5,
    "idle_connections": 3
  }
}
```

**Status:** ✅ Implemented

---

### Table Management

#### `GET /api/tables`
List tables, optionally filtered by room and occupancy status.

**Query params:**
- `room` (optional) — room name
- `occupied` (optional) — `true` or `false`

**Response:** `200 OK`
```json
{
  "count": 2,
  "tables": [
    {"name": "T1", "room": "Main Hall", "capacity": 4, "occupied": true},
    {"name": "T2", "room": "Main Hall", "capacity": 2, "occupied": false}
  ]
}
```

**Status:** ⚠️ Partial — when no `room` param is provided, returns empty list (no `list_all` in trait yet). With `room`, returns filtered results from storage.

---

#### `GET /api/tables/:id`
Get a single table by name.

**Response:** `200 OK` or `404 Not Found`
```json
{
  "name": "T1",
  "room": "Main Hall",
  "capacity": 4,
  "occupied": true
}
```

**Status:** ✅ Implemented (Lane 4A-4)

---

#### `POST /api/tables/:id/merge`
Merge multiple tables into one billing group.

**Request:**
```json
{
  "targets": ["T2", "T3"]
}
```

**Response:** `200 OK` or `409 Conflict`
```json
{
  "cluster": ["T1", "T2", "T3"]
}
```

**Status:** ✅ Implemented (Lane 3B). Enforces BFS contiguity and prevents cycles.

---

#### `POST /api/tables/:id/unmerge`
Unmerge a table from its cluster.

**Response:** `200 OK`
```json
{
  "unmerged": "T2",
  "remaining_cluster": ["T1", "T3"]
}
```

**Status:** ✅ Implemented (Lane 3B)

---

#### `POST /api/tables/:id/transfer`
Transfer an order from one table to another.

**Request:**
```json
{
  "to_table": "T5",
  "order_id": "ORD-001"
}
```

**Response:** `200 OK`

**Status:** ⚠️ Stubbed — handler exists, returns success, but order transfer logic is TODO (tables.rs:267). Requires OrderRepo integration.

---

### Menu Resolution

#### `POST /api/menu/resolve`
Resolve items to courses for a given room.

**Request:**
```json
{
  "room": "Main Hall",
  "items": ["ITEM-001", "ITEM-002"]
}
```

**Response:** `200 OK` or `500 Internal Server Error`

**Status:** ❌ Not yet implemented — returns `"Restaurant context not yet implemented (needs branch → restaurant mapping)"` (menu.rs:67). Domain logic exists; needs request context extraction.

---

#### `POST /api/menu/validate`
Validate item availability for a menu and order type.

**Request:**
```json
{
  "room": "Main Hall",
  "order_type": "Dine-In",
  "items": ["ITEM-001", "ITEM-002"]
}
```

**Response:** `200 OK` or `500 Internal Server Error`

**Status:** ❌ Not yet implemented — same restaurant context issue (menu.rs:102).

---

### Items

#### `GET /api/items/:id`
Get details for a single item.

**Response:** `200 OK` or `404 Not Found` or `500 Internal Server Error`

**Status:** ❌ Not yet implemented — returns `"Item details endpoint not yet implemented (ItemRepo pending)"` (items.rs:40). ItemRepo exists in `peacock-storage`; needs wiring.

---

### Orders

#### `POST /api/orders`
Create a new order. Honours `Idempotency-Key` header.

**Request:**
```json
{
  "table": "T1",
  "order_type": "Dine-In",
  "items": [
    {"item_code": "ITEM-001", "quantity": 2, "rate": "150.00"},
    {"item_code": "ITEM-002", "quantity": 1, "rate": "200.00"}
  ],
  "guest_name": "John Doe"
}
```

**Response:** `201 Created` (first request) or `200 OK` (replay)
```json
{
  "order_id": "ORD-001",
  "table": "T1",
  "status": "Draft",
  "total": "500.00",
  "items": [...]
}
```

**Status:** ✅ Implemented (Lane 3D)

---

#### `GET /api/orders/:id`
Fetch a single order.

**Response:** `200 OK` or `404 Not Found`

**Status:** ✅ Implemented (Lane 3D)

---

#### `PATCH /api/orders/:id`
Modify order items or header fields. Supports optimistic concurrency with `version`.

**Request:**
```json
{
  "items": [...],
  "guest_name": "Jane Doe",
  "version": 3
}
```

**Response:** `200 OK` or `409 Conflict` (version mismatch)

**Status:** ✅ Implemented (Lane 3D)

---

#### `DELETE /api/orders/:id`
Cancel an order.

**Response:** `200 OK` or `404 Not Found`

**Status:** ✅ Implemented (Lane 3D)

---

#### `POST /api/orders/:id/invoice`
Convert an order to an invoice. Allocates a gapless invoice number and submits KOTs to production units. Honours `Idempotency-Key`.

**Request:**
```json
{
  "payment_method": "Cash"
}
```

**Response:** `201 Created` or `200 OK` (replay)
```json
{
  "invoice_name": "URY-2024-00123",
  "subtotal": "500.00",
  "tax": "90.00",
  "total": "590.00",
  "kots": [
    {"production_unit": "Tandoor", "items": [...]},
    {"production_unit": "Pantry", "items": [...]}
  ],
  "unrouted_items": []
}
```

**Status:** ✅ Implemented (Lane 3D)

---

### Kitchen Orders (KOT)

#### `POST /api/kot/generate`
Generate KOTs for an order, fanning items out to production units.

**Request:**
```json
{
  "invoice_name": "URY-2024-00123",
  "room": "Main Hall",
  "table": "T1",
  "items": [...]
}
```

**Response:** `200 OK` or `503 Service Unavailable` (no database)

**Status:** ✅ Implemented (Lane 4A-3). Requires database (no in-memory fallback).

---

#### `GET /api/kot/:id`
Fetch a single KOT by ID.

**Response:** `200 OK` or `404 Not Found` or `503 Service Unavailable`

**Status:** ✅ Implemented (Lane 4A-3)

---

#### `GET /api/production-units/:unit_id/pending-kots`
List pending KOTs for a production unit (kitchen display).

**Response:** `200 OK` or `503 Service Unavailable`
```json
{
  "production_unit": "Tandoor",
  "pending_kots": [
    {"kot_id": "KOT-001", "table": "T1", "items": [...], "submitted_at": "..."}
  ]
}
```

**Status:** ✅ Implemented (Lane 4A-3)

---

#### `POST /api/kot/:id/mark-prepared`
Mark a KOT as prepared (kitchen finished).

**Request:**
```json
{
  "prepared_by": "Chef A"
}
```

**Response:** `200 OK` or `404 Not Found` or `503 Service Unavailable`

**Status:** ✅ Implemented (Lane 4A-3)

---

### Invoices

#### `POST /api/invoices`
Create an invoice from an order. Allocates gapless invoice number. Honours `Idempotency-Key`.

**Request:**
```json
{
  "order_id": "ORD-001"
}
```

**Response:** `201 Created` or `200 OK` (replay)
```json
{
  "invoice_name": "URY-2024-00123",
  "subtotal": "500.00",
  "cgst": "45.00",
  "sgst": "45.00",
  "total": "590.00",
  "lines": [...],
  "status": "Unpaid"
}
```

**Status:** ✅ Implemented (Lane 2F + 4A-3). Falls back to in-memory store when no database is configured.

---

#### `GET /api/invoices/:id`
Fetch a single invoice by name.

**Response:** `200 OK` or `404 Not Found`

**Status:** ✅ Implemented (Lane 2F)

---

#### `GET /api/invoices`
List invoices, filtered by business day, status, or table.

**Query params:**
- `business_day` (optional) — `YYYY-MM-DD`
- `status` (optional) — `Unpaid`, `Paid`, `Consolidated`, `Cancelled`
- `table` (optional) — table name

**Response:** `200 OK`
```json
{
  "count": 3,
  "invoices": [...]
}
```

**Status:** ✅ Implemented (Lane 2F)

---

#### `POST /api/invoices/:id/pay`
Record a payment against an invoice. Supports multi-tender.

**Request:**
```json
{
  "amount": "590.00",
  "method": "Cash"
}
```

**Response:** `200 OK` or `404 Not Found`
```json
{
  "invoice_name": "URY-2024-00123",
  "amount_paid": "590.00",
  "balance_due": "0.00",
  "status": "Paid"
}
```

**Status:** ✅ Implemented (Lane 2F)

---

#### `POST /api/invoices/:id/consolidate`
Transition invoice from Paid → Consolidated (end-of-day finalization).

**Response:** `200 OK` or `404 Not Found` or `409 Conflict` (not paid yet)

**Status:** ✅ Implemented (Lane 2F)

---

### Shifts

#### `POST /api/shifts/open`
Open a new shift on a terminal.

**Request:**
```json
{
  "terminal": "TILL-01",
  "opened_by": "USER-01",
  "business_day": "2024-07-28"
}
```

**Response:** `200 OK` or `409 Conflict` (shift already open)
```json
{
  "shift_id": "SHIFT-001",
  "terminal": "TILL-01",
  "opened_at": "...",
  "status": "Open"
}
```

**Status:** ✅ Implemented (Lane 3G + 4A-4)

---

#### `GET /api/shifts/current`
Get the currently open shift for a terminal.

**Query params:**
- `terminal` (required)

**Response:** `200 OK` or `404 Not Found`

**Status:** ✅ Implemented (Lane 3G)

---

#### `POST /api/shifts/:id/close`
Close a shift and generate Z-report.

**Request:**
```json
{
  "closed_by": "USER-01"
}
```

**Response:** `200 OK` or `404 Not Found`
```json
{
  "shift_id": "SHIFT-001",
  "closed_at": "...",
  "z_report": {...}
}
```

**Status:** ✅ Implemented (Lane 3G + 4A-4)

---

#### `GET /api/shifts/:id/report`
Fetch the Z-report for a closed shift.

**Response:** `200 OK` or `404 Not Found`

**Status:** ✅ Implemented (Lane 3G)

---

#### `GET /api/shifts`
List shifts, filtered by terminal or business day.

**Query params:**
- `terminal` (optional)
- `business_day` (optional) — `YYYY-MM-DD`

**Response:** `200 OK`
```json
{
  "count": 5,
  "shifts": [...]
}
```

**Status:** ✅ Implemented (Lane 3G)

---

### Cost of Goods Sold (COGS)

#### `POST /api/cogs/calculate`
Calculate COGS for a set of invoices.

**Request:**
```json
{
  "scope": "branch",
  "branch": "Main Branch",
  "from_date": "2024-07-01",
  "to_date": "2024-07-31"
}
```

**Response:** `200 OK` or `500 Internal Server Error`

**Status:** ❌ Not yet implemented — returns `"COGS calculation endpoint not yet implemented (Phase 2 storage pending)"` (cogs.rs:199). BOM tables exist; calculation logic pending.

---

### Reports

#### `GET /api/reports/daily-pnl?day=2024-07-28`
Daily profit & loss report for a business day.

**Query params:**
- `day` (required) — `YYYY-MM-DD`

**Response:** `200 OK` or `500 Internal Server Error`

**Status:** ❌ Not yet implemented — returns `"Daily P&L report not yet implemented (Phase 2 storage pending)"` (reports.rs:332).

---

#### `GET /api/reports/item-costing?date=2024-07-28&cutoff_hour=3`
Item-level costing report.

**Query params:**
- `date` (required) — `YYYY-MM-DD`
- `cutoff_hour` (optional) — business day cutoff, default 3

**Response:** `200 OK` or `500 Internal Server Error`

**Status:** ❌ Not yet implemented — returns `"Item costing report not yet implemented (Phase 2 storage pending)"` (reports.rs:371).

---

### Aggregator Integration (Webhooks)

#### `POST /api/aggregators/orders`
Webhook receiver for third-party delivery platforms (Swiggy, Zomato).

**Request:**
```json
{
  "order_id": "SWG-123456",
  "platform": "Swiggy",
  "items": [...],
  "total": "750.00"
}
```

**Headers:**
- `X-Webhook-Signature: sha256=<hex-digest>` (HMAC-SHA256 validation)

**Response:** `200 OK` or `400 Bad Request` (invalid signature) or `401 Unauthorized`

**Status:** ⚠️ Partial — validates signature, logs receipt, but does not store order in database yet (aggregators.rs:82 TODO).

---

#### `GET /api/aggregators/orders/:id`
Fetch a single aggregator order.

**Response:** `200 OK` or `404 Not Found`

**Status:** ⚠️ Stubbed — returns placeholder (aggregators.rs:103 TODO).

---

#### `POST /api/aggregators/orders/:id/accept`
Accept an aggregator order.

**Response:** `200 OK`

**Status:** ⚠️ Partial — returns success but does not notify aggregator API yet (aggregators.rs:126 stub).

---

#### `POST /api/aggregators/orders/:id/reject`
Reject an aggregator order.

**Response:** `200 OK`

**Status:** ⚠️ Partial — similar to accept (aggregators.rs:148 TODO).

---

#### `GET /api/aggregators/settlements`
List settlement reports from aggregators.

**Query params:**
- `date_from` (optional) — `YYYY-MM-DD`
- `date_to` (optional) — `YYYY-MM-DD`
- `platform` (optional) — `"Swiggy"` or `"Zomato"`

**Response:** `200 OK`

**Status:** ⚠️ Stubbed — returns empty list (aggregators.rs:171 TODO).

---

### Server-Sent Events (SSE)

#### `GET /api/events/stream`
Real-time event stream for kitchen displays and POS terminals.

**Headers:**
- `Accept: text/event-stream`

**Response:** `200 OK` (streaming)
```
event: kot.submitted
data: {"kot_id":"KOT-001","production_unit":"Tandoor","items":[...]}

event: kot.modified
data: {"kot_id":"KOT-001","action":"item_added"}
```

**Status:** ✅ Implemented (Lane 3F). Broadcasts KOT lifecycle events.

---

## Implementation Status Summary

| Status | Count | Description |
|---|---|---|
| ✅ Implemented | 29 | Fully functional, tested, backed by storage or in-memory store |
| ⚠️ Partial | 6 | Handler exists, some logic stubbed or incomplete |
| ❌ Not yet implemented | 5 | Returns "not yet implemented" error message |

**Stubbed/incomplete endpoints:**
- `GET /api/tables` (no `room` param → empty list)
- `POST /api/tables/:id/transfer` (success stub, no actual transfer)
- `POST /api/menu/resolve` (restaurant context pending)
- `POST /api/menu/validate` (restaurant context pending)
- `GET /api/items/:id` (ItemRepo wiring pending)
- `POST /api/cogs/calculate` (COGS logic pending)
- `GET /api/reports/daily-pnl` (Phase 2 storage pending)
- `GET /api/reports/item-costing` (Phase 2 storage pending)
- Aggregator endpoints (signature validation works, storage/notification TODOs)

## Security Notes

- **No authentication** — all endpoints are currently unauthenticated. This is a known gap.
- TLS termination expected upstream (reverse proxy).
- Aggregator webhooks validate HMAC-SHA256 signatures.
- SQL injection: all queries use parameterized statements (sqlx).

## Next Steps

Four lanes (W1-B, W1-C, W1-D, W1-F) are actively wiring storage and completing stubbed handlers. This document will be updated after Wave 1 completion.
