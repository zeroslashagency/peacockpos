# Peacock POS

A restaurant point-of-sale backend in Rust. Clean rewrite from Python/Frappe, not a fork.

## What This Is

Peacock POS is the backend for URY Restaurant Group's table service, kitchen operations, and billing. It provides:

- **Table management** — room assignments, table merging/unmerging, occupancy tracking
- **Order lifecycle** — from POS order creation through kitchen order tickets (KOT) to invoicing
- **Menu resolution** — course sequencing, item availability by menu and order type
- **Invoicing and payments** — gapless invoice numbering, multi-tender payments, tax computation
- **Shift operations** — open/close, revenue tracking per business day
- **Aggregator integration** — webhook endpoints for Swiggy/Zomato third-party delivery
- **Server-Sent Events** — real-time KOT notifications to kitchen display systems
- **Reporting** — daily P&L, item costing (in progress)

**Current state (2026-07-31):** Core domain logic complete and verified against a Python oracle. Storage layer integrated. Some report endpoints are still stubbed (see `docs/API.md`).

## Architecture

Four Rust crates in a workspace:

- **`peacock-core`** — Pure domain logic, no I/O. Business rules, money arithmetic, validation. Storage is behind traits (`peacock-core/src/ports.rs`), so the entire crate tests with `cargo test` and zero infrastructure.
- **`peacock-storage`** — PostgreSQL adapters implementing the `peacock-core` ports. Includes 9 migrations defining 28 tables. Money is stored as `NUMERIC(18,6)` (paisa precision).
- **`peacock-api`** — HTTP server (Axum). REST endpoints + Server-Sent Events. RFC 7807 error responses. **There is currently no authentication.**
- **`peacock-parity`** — Test harness that runs the Rust invoicing logic and a Python reference implementation side-by-side, diffs every money figure, and fails on any discrepancy. 22/22 test cases pass.

Dependency direction: `peacock-api` → `peacock-storage` → `peacock-core`. The core crate has no knowledge of Postgres or HTTP.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for crate boundaries and the ports-and-adapters design.

## Prerequisites

- **Rust:** 1.80 or later
- **PostgreSQL:** 16+ (earlier versions may work but are untested)
- **Python 3.11+** (only for the parity harness reference oracle)

## Setup

### 1. Database

Create a Postgres database:

```bash
createdb peacock
```

Set the connection string:

```bash
export DATABASE_URL="postgres://localhost:5432/peacock"
```

Or copy `.env.example` to `.env`, edit it, and source it:

```bash
cp .env.example .env
# Edit .env with your DATABASE_URL
set -a && . ./.env && set +a
```

### 2. Migrations

Migrations run automatically on startup by default. To run them manually:

```bash
cd peacock-storage
cargo run --bin peacock-storage -- migrate
```

This creates 28 tables across 9 migration files in `peacock-storage/migrations/`.

## Build

```bash
cargo build --workspace          # debug build, all crates
cargo build --workspace --release
```

## Running the API

```bash
cargo run -p peacock-api
```

The server listens on `http://localhost:8080` by default. Configure the port with:

```bash
export PEACOCK_API_PORT=8080
cargo run -p peacock-api
```

Health check:

```bash
curl http://localhost:8080/health
```

## Deployment (remote host)

Provision the Windows host at `100.72.103.1` (see [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md)):

```bash
PEACOCK_DB_PASSWORD=… ./scripts/provision-remote.sh   # Postgres on 5433, API on 8080
```

The host also runs foreign services on `5432` (zerosky-testdb) and `3000` (node) — the provision script never touches them.

Live frontend (Vercel) will consume `GET /api/events/stream` via Server-Sent Events for the kitchen display; no WebSocket server is required.

## Testing

Run all tests (unit + integration, across all crates):

```bash
cargo test --workspace
```

Run tests for a specific crate:

```bash
cargo test -p peacock-core
cargo test -p peacock-storage  # requires DATABASE_URL
cargo test -p peacock-api
```

### Parity Harness

The parity harness validates Rust money calculations against a Python oracle. It requires Python 3.11+ with dependencies:

```bash
cd peacock-parity
pip install -r requirements.txt
cargo run
```

Expected output: `22/22 test cases PASSED`.

## API Endpoints

See [`docs/API.md`](docs/API.md) for the full endpoint catalog, request/response shapes, and implementation status.

Quick overview:

- `GET /health`, `GET /health/ready` — health checks
- `GET /api/tables`, `POST /api/tables/:id/merge` — table management
- `POST /api/orders`, `GET /api/orders/:id` — order lifecycle
- `POST /api/kot/submit` — kitchen order ticket generation
- `POST /api/invoices`, `POST /api/invoices/:id/pay` — invoicing and payments
- `POST /api/shifts/open`, `POST /api/shifts/close` — shift operations
- `GET /api/events/stream` — Server-Sent Events for real-time KOT updates
- `GET /api/reports/daily-pnl`, `GET /api/reports/cogs` — reports (some in progress)

Mutation endpoints honour an `Idempotency-Key` header to prevent duplicate submissions.

## Money Handling

All money is represented as **paisa** (1/100 INR) internally and in the database:

- **In Rust:** `peacock_core::money::Money` backed by `rust_decimal`
- **In Postgres:** `NUMERIC(18,6)` columns
- **On the wire (JSON):** strings like `"123.45"`, never floating-point numbers

Never use JavaScript `Number` for money values from this API. Parse strings into a decimal library (e.g., `decimal.js`, `big.js`).

## Security Notes

- **There is currently no authentication.** All endpoints are unauthenticated.
- TLS termination is expected to happen upstream (reverse proxy, load balancer).
- The aggregator webhook endpoints validate HMAC-SHA256 signatures.
- SQL injection: all queries use parameterized statements via `sqlx`.

## Documentation

- [`docs/MASTER_PLAN.md`](docs/MASTER_PLAN.md) — execution strategy, wave plan, verified ground truth
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — crate design, dependency graph, ports-and-adapters
- [`docs/API.md`](docs/API.md) — HTTP endpoints, status, request/response examples
- [`docs/GROUND-TRUTH.md`](docs/GROUND-TRUTH.md) — verified upstream system facts
- [`docs/history/`](docs/history/) — superseded plans and lane reports (archive only)

## License

AGPL-3.0
