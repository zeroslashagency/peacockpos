# W4-B Security Review — Muse Spark 1.2 (API has NO AUTHENTICATION)

**Reviewer:** `muse-spark-1.2-contributor` (report-only, no code changes)  
**Date:** 2026-08-11  
**Scope:** `peacock-api/src/main.rs`, `app.rs`, `config.rs`, `middleware/cors.rs`, `middleware/logging.rs`, `middleware/request_id.rs`, `middleware/context.rs`, `middleware/error.rs`, `state.rs`, `peacock-storage/src/config.rs`, `peacock-storage/src/error.rs`, `peacock-web/src/lib/api.ts`, plus route enumeration  
**Deployment target:** `home@100.72.103.1` (Tailscale) — see [`docs/DEPLOYMENT.md`](../DEPLOYMENT.md)

> **Context:** Lane W2-B deploys `peacock-api.exe` to `E:\peacock\bin\` on the Windows host and expects it bound **only** to the Tailscale IP `100.72.103.1:8080`, with Postgres on `5433`. The codebase at this review has **no authentication layer mounted** — `main.rs:109` logs `peacock-api listening (no authentication layer mounted yet)` and every handler is reachable by any caller that can reach the port. This report enumerates that exposure, rates each finding, and proposes the next auth phase.

---

## Severity legend

| Severity | Meaning |
|---|---|
| **CRITICAL** | Immediate compromise: unauthenticated write/money path, credential fallback, public bind |
| **HIGH** | Data exfiltration or spoofable trust boundary, no auth required |
| **MEDIUM** | Information disclosure, scope for enumeration, or defense-in-depth gap |
| **LOW / INFO** | Hardening, operational correctness, or pass |
| **PASS** | No issue found — documented as verified |

---

## 1 — Bind address: does `TcpListener::bind` use `0.0.0.0:8080` publicly?

**Finding: HIGH — default is `0.0.0.0:3000` and is bind-public unless overridden.**

### Evidence

```startLine:41:endLine:65:peacock-api/src/config.rs
pub struct Config {
    pub bind_addr: SocketAddr, // 0.0.0.0:3000
}
impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([0, 0, 0, 0], 3000)),
        }
    }
}
```

```startLine:32:endLine:43:peacock-api/src/config.rs
pub mod env_keys {
    pub const HOST: &str = "PEACOCK_API_HOST";
    pub const PORT: &str = "PEACOCK_API_PORT";
}
```

```startLine:88:endLine:95:peacock-api/src/main.rs
let bind_addr = config.bind_addr;
let listener = match TcpListener::bind(bind_addr).await {
```

```startLine:55:endLine:62:peacock-api/.env.example
# PEACOCK_API_HOST=0.0.0.0
# PEACOCK_API_PORT=3000
```

Expected deployment (`docs/DEPLOYMENT.md:358-359`):

```powershell
$env:PEACOCK_API_HOST="100.72.103.1"  # bind to Tailscale IP, not 0.0.0.0
$env:PEACOCK_API_PORT="8080"
.\peacock-api.exe
```

### Assessment

- The binary **does not hard-code** `0.0.0.0:8080`. It binds whatever `Config::from_env()` resolves to.
- **Default is `0.0.0.0:3000`**, i.e. all interfaces, port 3000. On a bare checkout without env, both the interface and the port are wrong for the stated deployment (should be `100.72.103.1:8080` or `127.0.0.1:8080` behind Tailscale). There is no startup warning when the default is used; the log at `main.rs:105` prints `addr = %bind_addr` and `database = %db_url` but does not flag `0.0.0.0`.
- If the operator follows the documented deploy step and sets `PEACOCK_API_HOST=100.72.103.1` + `PEACOCK_API_PORT=8080`, the bind is correct and isolated to the Tailscale interface. The provision doc's firewall rules then further restrict to `100.64.0.0/10`.
- **Risk:** Any deployment that omits `PEACOCK_API_HOST` (e.g. systemd without `EnvironmentFile`, double-clicking the exe) silently falls back to a public bind. Combined with no authentication (finding §6), the entire money and PII surface is Internet-reachable.
- The env parsing itself is sound: malformed `HOST`/`PORT` are hard errors (`config.rs:91-102`), not silent defaults.

### Remediation

- Set `PEACOCK_API_HOST=100.72.103.1` and `PEACOCK_API_PORT=8080` on the host. Assert `netstat -ano | findstr :8080` shows only `100.72.103.1:8080`, not `0.0.0.0:8080`.
- Harden default in a follow-up: either default `bind_addr` to `127.0.0.1:8080` (fail-closed) or make bare `0.0.0.0` with no explicit env a startup error/warn. At minimum log a warning when `bind_addr.ip().is_unspecified()`.
- Add `--validate-bind` in provision/deploy scripts: after `peacock-api` starts, assert `Get-NetTCPConnection -LocalPort 8080` is `100.72.103.1`.

---

## 2 — CORS origin list: is `*` allowed or specific?

**Finding: PASS (design) + LOW (missing reject-* guard).**

### Evidence

```startLine:1:endLine:10:peacock-api/src/middleware/cors.rs
//! Credentials are allowed, which rules out Access-Control-Allow-Origin: * — the
//! Fetch spec rejects that combination. Origins are therefore an explicit allow-list
//! from configuration.
```

```startLine:43:endLine:65:peacock-api/src/middleware/cors.rs
pub fn layer(allowed_origins: &[String]) -> CorsLayer {
    let origins: Vec<HeaderValue> = allowed_origins
        .iter()
        .filter_map(|origin| match HeaderValue::from_str(origin) {
            Ok(value) => Some(value),
            Err(_) => { tracing::warn!(origin = %origin, "ignoring unparsable CORS origin"); None }
        })
        .collect();
    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods(allowed_methods())
        .allow_headers(allowed_headers())
        .allow_credentials(true)
        .expose_headers(vec![HeaderName::from_static("x-request-id")])
        .max_age(Duration::from_secs(600))
}
```

```startLine:32:endLine:43:peacock-api/src/config.rs
pub cors_allowed_origins: Vec<String>, // exact origins, credentials enabled
```

Defaults (`config.rs:60-63`):

```rust
cors_allowed_origins: vec![
    "http://localhost:5173".to_string(),
    "http://localhost:3000".to_string(),
],
```

Env override (`config.rs:104-121`): `PEACOCK_CORS_ALLOWED_ORIGINS` is a comma-separated list, empty value is an **error** (`is set but empty`), otherwise split/trim. Tests in `app.rs:192-234` assert allowed origin gets `Access-Control-Allow-Origin` + `Allow-Credentials: true`, unlisted origin gets **no** header.

### Assessment

- **Correct by design:** `allow_credentials(true)` + explicit origin list is the only Fetch-legal shape. `*` is not programmatically required anywhere. The frontend (`peacock-web/src/lib/api.ts:76-77`, `docs/DEPLOYMENT.md:365`) expects the Vercel origin(s) to be listed explicitly.
- **No wildcard today:** defaults and tests do not use `*`. The layer echoes only origins that parse as `HeaderValue`s.
- **Gap:** If an operator sets `PEACOCK_CORS_ALLOWED_ORIGINS=*`, the code will accept it — `HeaderValue::from_str("*")` is valid, and `tower-http` with `allow_credentials(true)` will then serve `Access-Control-Allow-Origin: *` plus `Allow-Credentials: true`, which modern browsers **reject** (so it fails closed), but the configuration is still misleading and violates the spec comment. The code also accepts `*` mixed with other origins.
- `allowed_headers()` correctly includes `authorization`, `idempotency-key`, `x-request-id`, `x-restaurant` — the headers the future auth layer will need, so preflight will not break after auth lands.
- Preflight advertises only `GET, POST, PATCH, DELETE, OPTIONS` (no `PUT`) — intentional.

### Remediation

- In `Config::from_source`, after splitting `PEACOCK_CORS_ALLOWED_ORIGINS`, reject `*` explicitly: `if origins.iter().any(|o| o == "*") { return Err("wildcard CORS origin is forbidden with credentials"); }`
- In `middleware::cors::layer`, skip/log-warn any `*` entry as invalid when credentials are on. Keep the existing unparsable-origin warn.

---

## 3 — Secret handling: `DATABASE_URL` in logs? `redacted_url` used? `Debug` leaks password?

**Three sub-findings.**

### 3a — `DATABASE_URL` handling: PASS

| File | Handling |
|---|---|
| `peacock-storage/src/config.rs:20-95` | `DbConfig.url` is **private**; accessor is `url()` (never derived in Debug). |
| `peacock-storage/src/config.rs:100-103` | `redacted_url()` strips userinfo via last-`@` before `/`, string-level so malformed URLs still redact. |
| `peacock-storage/src/config.rs:155-170` | `Debug` is hand-written, prints only `redacted_url()`, never `url`. Tests at `277-285` assert `!dumped.contains("s3cret")`. |
| `peacock-storage/src/lib.rs:320-326` | `Storage` Debug likewise prints `redacted_url()`. |
| `peacock-storage/src/lib.rs:66-68,104-106,119-122` | `StorageError::Connect { redacted_url, source }` carries redacted form only. |
| `peacock-api/src/main.rs:90-109` | Startup logs `database = %db_url` where `db_url = storage.config().redacted_url()`; bind failure closes the pool first. |
| `peacock-api/src/routes/health.rs:59` | Comment: `Never carries the connection string` — readiness `error` field is `err.to_string()` from the redacted error. |
| `peacock-storage/src/config.rs:244` | `redact_url` handles `p@ss`-style passwords (`rfind('@')`). |

No code path was found logging the raw `DATABASE_URL` or `DbConfig.url`. `.env` is gitignored; `.env.example` contains only placeholders.

### 3b — Aggregator webhook secret fallback: **CRITICAL**

```startLine:70:endLine:85:peacock-api/src/routes/aggregators.rs
let secret = state
    .config()
    .webhook_secret
    .as_deref()
    .unwrap_or("test-secret-key");
validate_webhook_signature(&headers, body.as_bytes(), secret)?;
```

Also:

```startLine:70:endLine:74:peacock-api/.env.example
# HMAC-SHA256 secret for aggregator webhook signatures. Unset means webhook signature
# validation is disabled, which is only safe on a trusted LAN.
# PEACOCK_WEBHOOK_SECRET=CHANGE_ME
```

- When `PEACOCK_WEBHOOK_SECRET` is unset, `Config.webhook_secret` is `None` (correct), but `receive_webhook` **falls back to `"test-secret-key"`** — a hard-coded, publicly visible string in the repo. Any caller knowing that constant can forge `X-Webhook-Signature: sha256=<hmac("test-secret-key", body)>` and inject arbitrary aggregator orders (Swiggy/Zomato) that become real orders/invoices/KOTs via `accept_order`.
- Tests use `test-secret-key` intentionally (`aggregators.rs:520-630`), but the production handler must not.
- Severity is **CRITICAL** because it is an unauthenticated **write** path (creates orders) guarded only by a predictable secret.

**Remediation:** Remove the fallback. When `webhook_secret` is `None`, reject the webhook with `401` / `500` (`ApiError::unauthorized("webhook signing not configured")` or `ApiError::internal`), or gate the route behind a feature flag. Provisioning must set `PEACOCK_WEBHOOK_SECRET` to a high-entropy value (e.g. `openssl rand -hex 32`) and the deploy script should assert it is set when the public aggregator endpoint is exposed.

### 3c — `Config::webhook_secret` leaks via `#[derive(Debug)]`: **MEDIUM**

```startLine:40:endLine:53:peacock-api/src/config.rs
#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub cors_allowed_origins: Vec<String>,
    pub log_format: LogFormat,
    pub problem_base_uri: String,
    pub webhook_secret: Option<String>, // <-- Debug prints the raw secret
    pub buying_price_list: peacock_core::ids::PriceListName,
}
```

```startLine:165:endLine:175:peacock-api/src/state.rs
impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &self.inner.config) // <-- delegates to Config's derived Debug
            .field("storage", &self.inner.storage)
            .finish()
    }
}
```

- `DbConfig` deliberately hand-writes `Debug` to redact `url`, but `Config` derives `Debug` and `AppState::Debug` prints `config` verbatim, so any `tracing::debug!(?state)` / `?config` / panic payload will contain the raw webhook secret.
- Not currently observed in hot paths (`main.rs` only logs `bind_addr` + `redacted_url` + `cors_origins`), but the type-level guarantee is missing.

**Remediation:** Hand-write `Debug` for `Config` (like `DbConfig`) and print `webhook_secret` as `Some("<redacted>")` / `None`, or wrap the secret in a `SecretString` that redacts on `Debug`. Do the same for any error path that formats `Config`.

### 3d — Other credential surfaces: INFO

- `peacock-web/src/lib/api.ts` does not handle secrets; it is a fetch wrapper.
- `StorageError::Connect` and `health.rs` correctly use `redacted_url`; no raw `sqlx::Error` containing the URL is forwarded to the client (client sees opaque `500` via `middleware::error::normalize`).

---

## 4 — SQL injection: all queries use `bind` vs string concat

**Finding: PASS — no injection via string concatenation found. One safe exception documented.**

### Method

Searched the workspace for `query.*format!`, `sqlx::query.*format`, `SELECT.*format`, and `.bind(`. 169 `.bind(` call sites use parametrised bind parameters. Only occurrences of `format!` near SQL were:

| Location | Usage | Safe? |
|---|---|---|
| `peacock-storage/src/lib.rs:95-98` | `format!("SET statement_timeout = {ms}")` | **Yes** — `ms` is a `u32` derived from `Duration::as_millis()` of the configured statement timeout, not caller input. `SET` does not accept bind parameters; comment explicitly notes this. |
| `peacock-storage/src/repos/order.rs:343,358,372,383,1217` | `format!("{SELECT_ORDER} WHERE id = $1")` | **Yes** — `SELECT_ORDER` is a `const &str` (`SELECT id, version ... FROM orders`), concatenation is with a constant, not user input. Values are bound via `.bind(id.get())`. |
| `peacock-storage/src/repos/table.rs:334,378` & `peacock-storage/src/repos/order.rs:670` in test scaffolding | `format!("DELETE FROM {}", tbl)` where `tbl` is from a literal slice | Safe — enumerated literals, not request data. |
| All other repos (`menu.rs:125-369`, `routing.rs:98-266`, `bom.rs:113-162`, `table.rs:68-506`, `shift.rs:54-391`, `invoice.rs:725` etc.) | Every external value (`item_code`, `restaurant`, `branch`, `room`, `terminal`, `shift_id`, etc.) flows through `.bind()` | **PASS** |

- `X-Restaurant` header is bound via `PgRestaurantRepo::find_async(&name)` → `.bind(restaurant.as_str())` (`restaurant.rs:99`), not interpolated.
- `menu.rs` notes SQL injection is not the concern for `X-Restaurant` — the value is a bind parameter; log injection and size are the mitigations there.

No `sqlx::query!("SELECT ... $1")` compile-time macros were found interpolating format args from request data.

---

## 5 — Log redaction: `request_id` ≤128 printable ASCII, query string not logged, etc.

**Finding: PASS — redaction and sanitization are well-implemented with documented rationale.**

### 5a — `X-Request-ID` sanitization

```startLine:17:endLine:53:peacock-api/src/middleware/request_id.rs
const MAX_INBOUND_LEN: usize = 128;
fn sanitize_inbound(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let acceptable = !trimmed.is_empty()
        && trimmed.len() <= MAX_INBOUND_LEN
        && trimmed.bytes().all(|b| b.is_ascii_graphic() || b == b'-' || b == b'_');
    acceptable.then(|| trimmed.to_string())
}
```

- Bounded at **128** bytes (header-sized, prevents memory exhaustion).
- Requires non-empty after trim, printable ASCII graphic (`0x21-0x7E`) plus `-`/`_`. Rejects spaces, tabs, newlines, control bytes — explicitly prevents log-injection via newlines.
- On failure, replaced with a fresh `Uuid::new_v4()` (`request_id.rs:58-62`), never forwards hostile input.
- Id is stored in request extensions and echoed in response header (`X-Request-ID`), and also in RFC 7807 `request_id` field via `middleware::error::normalize`.

Tests (`request_id.rs:110-131`) pin hostile cases: `"   "`, `"has space"`, `"line\tbreak"`, over-long.

### 5b — Request logging omits query strings

```startLine:1:endLine:10:peacock-api/src/middleware/logging.rs
//! Query strings are not logged. They routinely carry customer identifiers and the
//! path plus request id is enough to correlate with the access log.
```

```startLine:66:endLine:95:peacock-api/src/middleware/logging.rs
pub async fn log_requests(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string(); // <-- path only, no query
    // ...
}
```

- Only `method`, `path` (via `uri().path()`), `status`, `duration_ms`, `request_id` are emitted. `uri().query()` is never recorded.
- Level is derived from status (`5xx → error`, `4xx → warn`, else `info`).

### 5c — `X-Restaurant` sanitization

```startLine:78:endLine:145:peacock-api/src/middleware/context.rs
const MAX_NAME_LEN: usize = 140;
fn sanitize(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let acceptable = !trimmed.is_empty()
        && trimmed.len() <= MAX_NAME_LEN
        && !trimmed.chars().any(|c| c.is_control());
    acceptable.then_some(trimmed)
}
```

- Non-empty, ≤140 chars (matches docname length, prevents unbounded bind param), rejects control characters (no log injection, no header splitting). SQL is still safe via bind even without this; this mitigates log abuse.

### 5d — Error middleware redacts 5xx detail

```startLine:22:endLine:85:peacock-api/src/middleware/error.rs
const OPAQUE_INTERNAL_DETAIL: &str = "The server encountered an internal error.";
// ...
if status.is_server_error() {
    tracing::error!(request_id = ..., detail = %detail, "returning error response");
}
let client_detail = if kind.status().is_server_error() {
    OPAQUE_INTERNAL_DETAIL.to_string()
} else { detail };
```

- 5xx internal messages (naming series, table names, storage errors) are logged server-side with `request_id` but **not** returned to the client (client sees `Opaque Internal Detail`). 4xx details are returned verbatim (they are client-actionable).
- Tests in `middleware/error.rs:211-227` assert `!json["detail"].contains("ACC-PSINV-")`.

### 5e — Problem JSON carries `request_id` + `instance` (path only)

All error paths via `ApiError` stash the typed error and `error::handle_errors` renders `ProblemDetails` with `instance = request.uri().path()` and `request_id`, so support can correlate without query leakage.

**No finding** — this is solid.

---

## 6 — Enumerate exposure: all endpoints that are unauthenticated

**Finding: CRITICAL — 39 route entries (40 handler+method combinations) are unauthenticated. Any Tailscale peer (or public caller if bind is misconfigured) can read and mutate every resource.**

### 6a — Count reconciliation

`peacock-api/src/app.rs` composes:

```startLine:52:endLine:68:peacock-api/src/app.rs
pub fn build_with_state(state: AppState) -> Router {
    let cors = middleware::cors::layer(&state.config().cors_allowed_origins);
    routes::routes()
        .fallback(middleware::error::not_found)
        .layer(cors)
        .layer(axum::middleware::from_fn_with_state(state.clone(), middleware::error::handle_errors))
        .layer(axum::middleware::from_fn(middleware::logging::log_requests))
        .layer(axum::middleware::from_fn(middleware::request_id::propagate))
        .with_state(state)
}
```

No authentication / authorization / API-key / session / JWT layer is present. `peacock-api/src/routes/mod.rs` merges 12 routers plus 2 health probes. `peacock-web/src/lib/api.ts:9` confirms: `// - no auth (Wave 3 auth-less)`.

Route table produced by grepping `pub fn routes() -> Router<AppState>` across all modules (paths verified in source):

| # | Method | Path | File | Auth | Money/PII/Operational impact |
|---|---|---|---|---|---|
| 1 | `GET` | `/health` | `routes/health.rs` | none | liveness; low risk but confirms host is up |
| 2 | `GET` | `/health/ready` | `routes/health.rs` | none | leaks pool stats + `connected:false` error (redacted, but confirms DB availability) |
| 3 | `GET` | `/api/tables` | `routes/tables.rs` | none | lists all tables + merged clusters — floor plan / occupancy |
| 4 | `GET` | `/api/tables/:id` | `routes/tables.rs` | none | single table + branch/room/occupancy |
| 5 | `POST` | `/api/tables/:id/merge` | `routes/tables.rs` | none | **mutating**: merges arbitrary tables (FakeOrderRepo count=0 in prod — bypasses merge guard) |
| 6 | `POST` | `/api/tables/:id/unmerge` | `routes/tables.rs` | none | **mutating**: unmerges |
| 7 | `POST` | `/api/tables/:id/transfer` | `routes/tables.rs` | none | **mutating**: moves live order + draft invoices between tables |
| 8 | `GET` | `/api/menu` | `routes/menu.rs` | **spoofable** `X-Restaurant` | leaks menu + rates (selling prices) for any restaurant; enumerates menus |
| 9 | `GET` | `/api/menu/:menu_id/items` | `routes/menu.rs` | **spoofable** `X-Restaurant` | leaks menu items + rates; cross-branch read blocked by `menu_belongs_to_scope` but branch existence is enumerable via 404 vs 200 |
| 10 | `GET` | `/api/items/:item_code` | `routes/items.rs` | none | item master (name, group, UOM, disabled) — shared catalog |
| 11 | `GET` | `/api/items/:item_code/price` | `routes/items.rs` | none | **pricing leakage**: buying or selling price for any item on any price list |
| 12 | `POST` | `/api/kot/generate` | `routes/kot.rs` | none | **mutating**: fans order to tickets, creates KOTs with sequential numbers, publishes SSE |
| 13 | `GET` | `/api/kot/:id` | `routes/kot.rs` | none | reads any KOT (invoice, table, comments) — kitchen intel |
| 14 | `GET` | `/api/production-units/:unit_id/pending-kots` | `routes/kot.rs` | none | kitchen queue — live work state |
| 15 | `POST` | `/api/kot/:id/mark-prepared` | `routes/kot.rs` | none | **mutating**: marks ticket prepared, falsifies service-time metrics |
| 16 | `POST` | `/api/invoices` | `routes/invoices.rs` | none (idempotency-key replayable) | **CRITICAL money path**: allocates **gapless** invoice number (Rule 46(b) compliance), inserts invoice; replay with same `Idempotency-Key` returns original |
| 17 | `GET` | `/api/invoices` | `routes/invoices.rs` | none | lists invoices filtered by business_day/status/table — **revenue disclosure**, paginatable |
| 18 | `GET` | `/api/invoices/:id` | `routes/invoices.rs` | none | single invoice with lines, payments, totals — **full financials** |
| 19 | `POST` | `/api/invoices/:id/pay` | `routes/invoices.rs` | none | **CRITICAL money path**: records payment (method/amount/reference), multi-tender, moves status → `Paid` |
| 20 | `POST` | `/api/invoices/:id/consolidate` | `routes/invoices.rs` | none | **mutating**: `Paid → Consolidated` (day-close finalization) |
| 21 | `POST` | `/api/aggregators/orders` | `routes/aggregators.rs` | **HMAC only; fallback `test-secret-key` when unset** | **mutating**: creates aggregator order, then `accept` can mint order+invoice+KOT |
| 22 | `GET` | `/api/aggregators/orders/:id` | `routes/aggregators.rs` | none | reads aggregator order incl. customer phone, items, totals |
| 23 | `POST` | `/api/aggregators/orders/:id/accept` | `routes/aggregators.rs` | none | **CRITICAL**: creates internal `UryOrder`, **allocates gapless invoice**, generates KOT — attacker-controlled items/prices flow into `OrderItem.rate` → `compute_totals` |
| 24 | `POST` | `/api/aggregators/orders/:id/reject` | `routes/aggregators.rs` | none | state mutation (Pending → Rejected) |
| 25 | `GET` | `/api/aggregators/settlements` | `routes/aggregators.rs` | none | settlement totals (gross/commission/net) — **payout financials**, date/platform filterable |
| 26 | `POST` | `/api/shifts/open` | `routes/shifts.rs` | none | **mutating**: opens shift on any terminal, as any user (`opened_by` is caller-supplied) |
| 27 | `GET` | `/api/shifts/current` | `routes/shifts.rs` | none | current shift per terminal — operational |
| 28 | `POST` | `/api/shifts/:id/close` | `routes/shifts.rs` | none | **mutating**: closes shift, generates Z-report (cash/card totals), compares `grand_total` vs `rounded_total` |
| 29 | `GET` | `/api/shifts/:id/report` | `routes/shifts.rs` | none | Z-report with revenue breakdown — **cash in drawer** |
| 30 | `GET` | `/api/shifts` | `routes/shifts.rs` | none | lists all shifts (terminal/limit/offset) — historical ops |
| 31 | `POST` | `/api/cogs/calculate` | `routes/cogs.rs` | none | **cost intelligence**: BOM explosion per invoice or date range → per-item COGS, unset price lists; enumerates cost structure |
| 32 | `GET` | `/api/reports/daily-pl` | `routes/reports.rs` | none | **CRITICAL**: daily P&L → revenue (`rounded_total`), COGS, gross profit, margin, round-off — **the till's books** for any `?date=` |
| 33 | `GET` | `/api/reports/item-costing` | `routes/reports.rs` | none | per-item revenue/COGS/margin for a day — **margin leakage** |
| 34 | `POST` | `/api/orders` | `routes/orders.rs` | none (UUID idempotency-key) | **mutating**: creates order (table or takeaway), caller-controlled `rate`s accepted; idempotent with `Idempotency-Key: <uuid>` |
| 35 | `GET` | `/api/orders/:id` | `routes/orders.rs` | none | single order incl. cart, customer name, waiter, comments |
| 36 | `PATCH` | `/api/orders/:id` | `routes/orders.rs` | none (optional `version` OCC) | **mutating**: rewrites cart/header; `version` gives OCC but is optional |
| 37 | `DELETE` | `/api/orders/:id` | `routes/orders.rs` | none | **mutating**: soft-cancels order (idempotent) |
| 38 | `POST` | `/api/orders/:id/invoice` | `routes/orders.rs` | none (UUID idempotency-key) | **CRITICAL money path**: gapless number allocation + KOT routing; `201` first time, `200` replay |
| 39 | `GET` | `/api/events/stream` | `events/sse.rs` | none | **SSE real-time**: live `order.created/updated`, `kot.generated/prepared`, `invoice.paid` events; filterable via `?events=`, resumable via `Last-Event-ID`; 15s keep-alive |

> The 39 route entries above expose **40 method+path combinations** when counting the two verbs on `/api/invoices` separately (`POST` and `GET` share one `.route("/api/invoices", post(...).get(...))` call). Some trackers count them as two endpoints; the table lists both. The `fallback` `not_found` 404 handler applies to every other path.

### 6b — Cross-cutting observations

- **Every mutating path is authenticated as “anyone”.** Invoice numbering (gapless, Rule 46(b)), payments, shift open/close, table merges/transfers, order create/patch/cancel, KOT generate/mark-prepared, aggregator accept, COGS/P&L are all caller-anonymous. The `X-Restaurant` header documented in `middleware/context.rs:40-48` is explicitly called **spoofable** by the code comments and deferred to Wave 4.
- **Read paths leak business intelligence:** item prices (buying rates), menu rates, invoice totals, daily P&L, COGS with unset-price lists, aggregator settlements, and shift Z-reports are all public reads. Enumerable via pagination (`limit/offset`) and date iteration.
- **Aggregator path chains unauthenticated writes into money:** `POST /api/aggregators/orders` (with the weak HMAC fallback) → `POST …/accept` mints a real order, invoice, and KOT. Items with attacker-chosen `item_code`/`rate` flow into `compute_totals` and `cogs`.
- **Table merge currently uses `FakeOrderRepo { count_separate_active: 0 }`** (`routes/tables.rs:125-132`), so the “refuse when two tables each have an active draft invoice” guard is inert — merges that upstream would 409 will succeed here.
- **SSE (`/api/events/stream`)** is unauthenticated and filterable; an observer on the Tailscale network (or public if bind is wrong) sees every order/KOT/invoice-paid event in real time. No auth, no per-restaurant scoping on the stream (the filter is global).

### 6c — What is *not* exposed (to avoid confusion)

- No `/api/auth`, `/api/users`, or session endpoints exist — auth is wholly absent, not merely weak.
- `docs/API.md` historically listed stub endpoints; the source at this review implements the table above (all lanes except auth).

### 6d — Suggested next auth phase (per task)

Scope this as **Lane 5A — Auth foundation** (ahead of `PHASE_4_5_PLAN.md`'s Phase 5 auth milestone):

1. **Threat model first:** Decide trust boundary — TLS-terminating reverse proxy vs Tailscale-only, Vercel → `100.72.103.1:8080` requires TLS; document whether `100.64.0.0/10` is the whole boundary or an additional service auth is required.

2. **Choose mechanism:** API-key + `Authorization: Bearer <jwt>` (or session cookie) for the POS terminals; keep `X-Webhook-Signature` for aggregators but enforce a required secret (no fallback).Prefer `argon2`/`bcrypt` per existing migration plan references (`docs/history/RUST_MIGRATION_PLAN.md:49`).

3. **Middleware — ideas that fit the current stack:**
   - New `peacock-api/src/middleware/auth.rs` extracting `X-Session`/`Authorization` before `context.rs`; attach a `CallerContext { user, role, restaurant, branch }` to extensions.
   - Fail-closed: missing/invalid token → `401` problem+json (already defined as `ProblemKind::Unauthorized` in `error.rs:30-43`).
   - Validate `X-Restaurant` **against the session’s restaurant**, not the header. The current `RestaurantContext::from_request_parts` (`context.rs:145-202`) should prefer `CallerContext.restaurant` when present and treat `X-Restaurant` as untrusted input to be ignored or cross-checked.
   - Order: `request_id → logging → error → auth → cors → handler` (auth inside error so `401`s get `request_id` + problem+json; auth before CORS so preflight still answers with correct `Allow-Credentials` + origin echoing, but authenticated endpoints return `401` before handler).

4. **RBAC for financials:** At minimum separate `waiter / cashier / manager` roles. `POST …/pay`, `POST …/consolidate`, `POST …/close`, `GET …/daily-pl`, `GET …/settlements`, `POST …/cogs/calculate` should require `manager` or `cashier` as appropriate, not any authenticated POS.

5. **Per-restaurant scoping on reads:** `GET /api/invoices`, `GET /api/orders/:id`, SSE stream, aggregator reads should filter by the caller’s restaurant/branch, not an attacker-controlled header/query.

6. **Harden operationally in the same lane:**
   - Bind to `100.72.103.1:8080` (provision/deploy asserts `Get-NetTCPConnection -LocalPort 8080` is Tailscale-only) and add a scoped Windows Firewall rule (`remoteip=100.64.0.0/10`) per `docs/DEPLOYMENT.md:456-473`.
   - Enforce CORS origin list must include Vercel origin(s) and must never be `*` (fail startup if `*`).
   - Remove HMAC fallback, require `PEACOCK_WEBHOOK_SECRET`, rotate once.
   - Rate-limit `POST /api/aggregators/orders` and `POST /api/orders` (idempotency-key helps but does not prevent volume).

7. **Test plan:** Unit tests for `auth` extractor (missing header → 401, bad JWT → 401, wrong restaurant → 403), integration tests for every endpoint in §6a (200/401/403), and a “no session leaks data” regression that asserts every `POST/GET` money read without a token is `401` (not `200`).

Until that lane lands, **do not expose `100.72.103.1:8080` beyond Tailscale** and keep the firewall rule scoped to `100.64.0.0/10`. The current build is safe on `127.0.0.1` or `100.72.103.1` inside Tailscale, unsafe on `0.0.0.0`.

---

## Appendix — files read for this review

| File | Lines (reported) | Relevance |
|---|---|---|
| `peacock-api/src/main.rs` | 1-153 | startup order (DB before socket), bind, secret redaction, no-auth note |
| `peacock-api/src/app.rs` | 1-310 | middleware order (no auth layer) |
| `peacock-api/src/config.rs` | 1-209 | `bind_addr` default `0.0.0.0:3000`, CORS origins, webhook_secret |
| `peacock-api/src/middleware/cors.rs` | 1-100 | `allow_origin(explicit) + allow_credentials(true)` |
| `peacock-api/src/middleware/logging.rs` | 1-145 | path-only logging, level mapping |
| `peacock-api/src/middleware/request_id.rs` | 1-134 | 128-char printable-ASCII cap |
| `peacock-api/src/middleware/context.rs` | 1-250 | `X-Restaurant` sanitization, spoofable note |
| `peacock-api/src/middleware/error.rs` | 1-263 | 5xx redaction, problem+json |
| `peacock-api/src/error.rs` | 1-414 | Opaque 5xx detail, `ProblemKind::Unauthorized` |
| `peacock-api/src/state.rs` | 1-360 | `AppState::Debug` leaks `Config::Debug` |
| `peacock-storage/src/config.rs` | 1-325 | `redacted_url`, hand-written `Debug` |
| `peacock-storage/src/error.rs` | 1-202 | redacted connect error, SQLSTATE mapping |
| `peacock-storage/src/lib.rs` | 1-381 | pool, `SET statement_timeout` safe case |
| `peacock-storage/src/repos/order.rs` | 343-383 | `format!("{SELECT_ORDER} WHERE ...")` constant case |
| `peacock-api/src/routes/*.rs` | various | route table (12 routers) |
| `peacock-web/src/lib/api.ts` | 1-1010 | `API_BASE_URL` default `http://100.72.103.1:8080`, “Wave 3 auth-less” |

---

## Summary of recommendations (by priority)

| Priority | Finding | Action |
|---|---|---|
| **P0** | §3b HMAC fallback `test-secret-key` | Remove fallback; require `PEACOCK_WEBHOOK_SECRET`; reject webhooks when unset |
| **P0** | §6 unauthenticated money + P&L writes/reads | Implement auth middleware (Lane 5A) before any public exposure; keep Tailscale-scoped firewall |
| **P1** | §1 `0.0.0.0:3000` default bind | Deploy with `PEACOCK_API_HOST=100.72.103.1` + `PORT=8080`; assert via `netstat`/`Get-NetTCPConnection`; harden default to `127.0.0.1` or fail-closed in code |
| **P1** | §3c `Config::webhook_secret` leaked in Debug | Hand-write `Config::Debug` to redact secret |
| **P1** | §2 `*` not rejected | Reject `*` in `PEACOCK_CORS_ALLOWED_ORIGINS` at startup |
| **P2** | §6 table merge `FakeOrderRepo` | Wire real `OrderRepo::count_separate_active` into `POST …/merge` |
| **P2** | §6 SSE + enumerability | Scope SSE + list endpoints by caller restaurant after auth |

*End of W4-B report.*
