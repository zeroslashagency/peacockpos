# W4-A Adversarial Review — Muse Spark 1.2

**Reviewer:** W4-A (Muse Spark 1.2 — adversarial)
**Date:** 2026-08-11
**Scope:** Every Wave 1–3 diff; ground truth in `docs/MASTER_PLAN.md`; money lanes `peacock-core/src/money.rs`, `tax.rs`, `cogs.rs`, `invoicing.rs`; storage `peacock-storage/src/repos/*`; API `peacock-api/src/routes/*`; web `peacock-web/src/lib/*`
**Mode:** Read-only audit — no code edits. Autonomous.
**Gate referenced:** MASTER_PLAN §4 Wave 1 exit gate (grep for stubs, `InvoiceBackend::Memory`, warnings, clippy, tests, parity 22/22)

---

## 0. Executive verdict

| Wave | Claim | Verdict | Comment |
|------|-------|---------|---------|
| **W1-A** Storage non-optional, delete `InvoiceBackend::Memory` | **PASS** | `AppState::storage()` returns `&Storage`, no `Option`, no fallback enum. `peacock-api/src/state.rs:115` `pub fn storage(&self) -> &Storage`. `grep -rn InvoiceBackend::Memory` → 0 hits outside archived `docs/history`. `peacock-api/src/routes/invoices.rs` no memory branch. |
| **W1-B** Restaurant context + menu wiring | **PASS** | `middleware/context.rs:RestaurantContext` validates `X-Restaurant`, `menu.rs:113`->`resolve_menu` wired to `PgMenuResolutionRepo`, `items.rs:67` wired to `PgItemDetailsRepo`. Dead `strategy` binding removed. |
| **W1-C** COGS + reports → BOM/bundle/invoice repos | **PASS** | `cogs.rs:172` handler queries `revenue_lines_between` + BOM/bundle snapshots bounded. `reports.rs:307` `daily_pl` uses `PosInvoiceStatus::REVENUE` + half-open `[start,end)`. Parity-critical arithmetic stays in `peacock-core`. |
| **W1-D** Tables + aggregators | **PASS (with debt)** | `tables.rs:48` `list_all`, `transfer_order:252` real via `order_repo.transfer_table`. `aggregators.rs:88` webhook persists, `accept_order:189` creates real order/invoice/KOT. Debt: merge guard `FakeOrderRepo` in `tables.rs:127` (see §1.4). |
| **W1-E** Docs structure | **PASS** | `docs/{ARCHITECTURE,API,DEPLOYMENT,GROUND-TRUTH}.md` present; `docs/history/` archived. |
| **W2** Deploy (5433/8080 untouched 5432/3000) | **NOT VERIFIED HERE** | No SSH probe in this lane; `docs/DEPLOYMENT.md` exists. |
| **W3** Web + live verification | **PASS (minor)** | `peacock-web/src/lib/money.ts` string end-to-end via `decimal.js`; `lib/api.ts` typed client; `ShiftPanel.tsx` correct. One LOW money-adjacent `Number()` for display grouping only (§2). |

**Overall money safety:** No `f64` for money on any wire path; no `format!("{}", money)` bypass; parity harness untouched (22 fixtures). **One LOW JS `Number` for grouping fallback, safe with guard.**

**Test suite health:** **FAIL** — 2 test modules are stale and cannot fail for the right reason (§4). They assert `500` where handlers now return `2xx` after W1-A made `Storage` mandatory. In CI with a real DB they flip from green to red for the *wrong* reason, hiding future regressions.

**Panic safety:** **PASS with 1 MEDIUM** — no `unwrap`/`expect`/`panic!` on any handler's request path. One `expect` remains in the blocking bridge that can panic a request thread if mis-configured (§1.4).

**SQL injection:** **PASS** — all user input via `bind()`; `format!` interpolations are constants (`SELECT_ORDER`) or identifiers from trusted code, never raw query fragments.

**Stubs:** **PASS** — `grep -rn "not yet implemented|todo!|unimplemented!" peacock-api/src` → 0 hits outside `#[cfg(test)]`. The 6 stubs listed in MASTER_PLAN §1 are gone.

---

## 1. Task 1 — `unwrap` / `expect` / `panic!` on request path

**Rule:** Handlers must propagate via `?` + `ApiError`. `unwrap`/`expect` allowed only in `#[cfg(test)]`, build scripts, or startup that runs once before serving.

### 1.1 Method

- `grep -rn "unwrap()|expect(|panic!" peacock-api/src/routes` → 189 hits. Triaged by location: outside `#[cfg(test)]` vs inside; handler function vs helper.
- Manually read each handler: `aggregators.rs`, `cogs.rs`, `health.rs`, `invoices.rs`, `items.rs`, `kot.rs`, `menu.rs`, `orders.rs`, `reports.rs`, `shifts.rs`, `tables.rs`.
- Cross-checked `peacock-storage/src/repos/*` and `peacock-core/src/*` for non-test panics.

### 1.2 Per-file findings — routes (request path only)

| File | Verdict | Lines | Severity | Detail & Fix |
|------|---------|-------|----------|--------------|
| `peacock-api/src/routes/aggregators.rs` | **PASS** | — | — | 0 unwrap/expect in handlers. `validate_webhook_signature` returns `ApiError::{invalid_input,unauthorized}`. `receive_webhook:78` `serde_json::from_str` → `map_err(ApiError::invalid_input)`. `accept_order:192` uses `?` + `ApiError::from`. Test helpers `HmacSha256::new_from_slice(...).unwrap()` at `:521,602,638` etc — **test-only**, gated by `#[cfg(test)]`. |
| `peacock-api/src/routes/cogs.rs` | **PASS** | — | — | Handler `calculate_cogs:170` and helpers `aggregate_cogs:100`, `cost_basis_for:81`, `response_for:287` use `?` + `ApiError`. No unwrap outside tests. All `unwrap()` at `:429,438,478,512,542,574,612,640,682,716,741,764,778,813,952` are `#[cfg(test)]` (oneshot, JSON parse, `NaiveDate::from_ymd_opt().unwrap()` on literal dates — acceptable in test fixtures). |
| `peacock-api/src/routes/health.rs` | **PASS** | — | — | `health_check`, `readiness_check` no unwrap. One `unwrap()` in `impl From<u16>` fallback in error path `ProblemDetails::into_response_with_status:253` is `StatusCode::from_u16(...).unwrap_or(INTERNAL_SERVER_ERROR)` — **not** `unwrap()`, it falls back. Safe. |
| `peacock-api/src/routes/invoices.rs` | **PASS** | — | — | All handlers `?` + `storage_error`. No handler unwrap. Test `unwrap()`s at `:617,637,675,721,763,787,798,862,939,968,1017,1036,1047,1093,1122,1156,1180,1236,1270,1358,1554,1593` — test-only, mostly `as_str().unwrap()` on JSON that the handler produced. No panic path. |
| `peacock-api/src/routes/items.rs` | **PASS** | — | — | `get_item_details:69`, `get_item_price:109` use `require_storage` (returns `&Storage`, no panic) + `?`. No unwrap. Tests at `:205,267,286,305,322,357,385,420` etc — all test. |
| `peacock-api/src/routes/kot.rs` | **PASS** | — | — | `generate_kot:95`, `get_kot:193`, `pending_kots:214`, `mark_prepared:239` all `?` + `storage_error`. `kot_repo(state)` is infallible (`AppState::kot_repo()` returns value, no `Option`). No unwrap. Tests `oneshot(...).await.unwrap()` at `:284,308,415,452` — test-only. |
| `peacock-api/src/routes/menu.rs` | **PASS** | — | — | `resolve_menu:113`, `get_menu_items:190` use `?`. `classify_strategy:159` `?`. No unwrap outside tests. Tests `unwrap()` at `:272,284,412,595,713,749` — test-only (`Request::builder().body().unwrap()` on static strings, `collect().await.unwrap()` on in-memory body). |
| `peacock-api/src/routes/orders.rs` | **PASS with note** | `peacock-api/src/routes/aggregators.rs:227-233` analog, `orders.rs:312`-ish | **LOW** | Handlers `create_order:89`, `get_order:161`, `patch_order:178`, `create_invoice:252`, `cancel_order:366` — all `?`. One handler-adjacent fragility: `orders.rs:229-233` and `aggregators.rs:229` `ai.quantity.round().to_string().parse::<i32>().unwrap_or(1).max(1)` — **not** `unwrap()`, it's `unwrap_or`, so no panic. However silent `1` on overflow is a correctness risk, not a panic. Flagged in §2.3 but not a Task-1 fail. |
| `peacock-api/src/routes/reports.rs` | **PASS** | — | — | `daily_pl:308`, `item_costing:365` use `?`. Helpers `summarise_revenue:88`, `compute_daily_pl:134` no unwrap. Tests `unwrap()` at `:539,563,733,768,940,1011,1035,1097,1125` etc — test-only (`Utc.with_ymd_and_hms(...).unwrap()` on literal dates, `app.oneshot().unwrap()`). |
| `peacock-api/src/routes/shifts.rs` | **PASS (handler)** / **FAIL (tests)** | Handler `:32-163` PASS; tests `:181-426` FAIL | **MEDIUM (tests)** | Handlers 0 unwrap. Tests expect `is_server_error()` for every endpoint (see `:199,220,273,290,309,345,358,371,384`). After W1-A handlers are real (Postgres) and now return `200/409/400`, not `500`. These tests *cannot* catch a future stub regression — they are inverted. Fix: rewrite tests against `TestDb` (like `invoices.rs`/`tables.rs`) and assert `201/409` etc. |
| `peacock-api/src/routes/tables.rs` | **PASS** | `:43-267` PASS | — | Handlers `list_tables:39`, `get_table:61`, `merge_tables:99`, `unmerge_table:170`, `transfer_order:219` — all `?`. `merge_tables:127` `FakeOrderRepo` is **not** a panic but a stub that disables active-order guard (debt, §1.4). |
| `peacock-api/src/middleware/context.rs` | **PASS** | — | — | Extractor `FromRequestParts for RestaurantContext:147` returns `ApiError`, no unwrap. `sanitize` pure. Test `sanitize` only. |
| `peacock-api/src/middleware/error.rs` | **PASS** | — | — | No unwrap.Design: `ProblemDetails::into_response_with_status:253` uses `unwrap_or_else` fallback, not panic. |
| `peacock-api/src/middleware/request_id.rs` | **PASS** | — | — | `propagate` no unwrap. Test `builder.body().unwrap()` at `:92` — test-only. |
| `peacock-api/src/app.rs` | **PASS** | — | — | `build` no unwrap. Tests `oneshot().await.unwrap()` at `:86,101,122,134,147,175,197,221,246,282,296,300` — test-only, in-memory routing. |

### 1.3 Other crates — request-adjacent panics

| File | Verdict | Lines | Severity | Detail & Fix |
|------|---------|-------|----------|--------------|
| `peacock-storage/src/repos/blocking.rs` | **FAIL** | `55,133` | **MEDIUM** | `55: .expect("building the storage sync-bridge runtime")` and `133: .expect("current-thread runtime")` are **on the handler's thread** when `products/shifts/reports` call `block_on`/`block_in_place`. On a `current_thread` runtime this panics the worker thread instead of returning `500`. Upstream comment says it panics with actionable message by design. Adversarial view: a mis-configured `#[tokio::main(flavor="current_thread")]` in a future binary or a test harness turns a DB call into `SIGABRT` rather than `ApiError::internal`. Fix: replace `expect` with `ApiError::internal` propagation or `Result` return; at minimum gate with `Handle::try_current().map_err(...)` and map to `StorageError::Internal`. |
| `peacock-storage/src/repos/invoice.rs` | **PASS** | — | — | No handler unwrap. `TxSeriesAllocator::allocate:1690` `block_on_current` may panic if no runtime, but callers always hold one (W1-A multi_thread). Test `unwrap()` at `:1785,1834,1884` — test-only config parsing. |
| `peacock-storage/src/repos/order.rs` | **PASS** | — | — | No handler unwrap. `format!("{SELECT_ORDER} WHERE ...")` interpolates constant `SELECT_ORDER` (see §3). No panic. |
| `peacock-core/src/*` | **PASS** | — | — | `unwrap()` at `invoicing.rs:372,399` etc are in `#[cfg(test)]` FakeAllocator or doc examples (`NaiveDate::from_ymd_opt(...).unwrap()` on literal). No panics on domain path; domain fns return `Result`. One `expect("valid date")` at `kot.rs:787` inside a unit test helper — test-only. |
| `peacock-web/src/lib/money.ts` | **PASS** | — | — | No `!` non-null assertion on money path that can throw in render; `parseMoney` throws typed `Error` and callers catch (`ShiftPanel.tsx:115`). No panic. |

### 1.4 Debt carried as non-panic stub

**`peacock-api/src/routes/tables.rs:127-132` — `FakeOrderRepo` inside `merge_tables`**

```rust
struct FakeOrderRepo;
impl OrderRepo for FakeOrderRepo {
    fn count_separate_active(&self, _: &[TableName]) -> Result<usize> { Ok(0) }
}
```

- **Severity:** MEDIUM (correctness, not panic).
- **Finding:** Merge guard `Error::MultipleActiveOrders` is dead in this handler. Two tables each with a Draft unprinted invoice can be merged, violating `ury_order.py:236` semantics. The port exists, the repo implements it (`PgOrderRepo::count_separate_active_async:836`), but the handler does not call it.
- **Fix:** Replace with `let order_repo = storage.order_repo(); let count = order_repo.count_separate_active_async(&members).await?;` (async) or bridge via `block_on`. Wire through `AppState::order_repo`/`table_repo` same as `transfer_order` does at `:251`.

---

## 2. Task 2 — Money paths

**Rules:** Money is `Decimal`/`Money` only, never `f64`; crosses wire as `string`; web never `Number()` money; no `format!("{}", money)` bypassing `Money` type.

### 2.1 Rust — grep `f64` for money

Grep `f64|: f64|as f64` across workspace → 55 hits, triaged:

| Location | Verdict | Fix |
|----------|---------|-----|
| `peacock-core/src/model.rs:113-117` `layout_x/y/width/height: f64` | **PASS** | Comment `// Float in the JSON — geometry, so f64 is correct here (not money)` — correct. Pixels, not paisa. |
| `peacock-core/src/money.rs:247` doc `// in f64 it is not` | **PASS** | Prose, not code. |
| `peacock-core/src/cogs.rs` `quantity: Decimal` etc | **PASS** | No f64. |
| `peacock-storage/src/repos/table.rs:90-92,176,260` `let layout_x: f64 = row.try_get(...)` | **PASS** | Geometry columns `DOUBLE PRECISION` per `RUST_MIGRATION_PLAN_V2.md:203`. Not money. |
| `peacock-storage/src/repos/bom.rs:82` `quantity: Decimal`, comment `not f64` | **PASS** | Deliberately Decimal. |
| `peacock-storage/src/repos/invoice.rs:45` `// No f64 appears anywhere` | **PASS** | Enforced. |
| `peacock-api/src/dto/table.rs:24` `layout_x: f64` | **PASS** | Echo of model geometry. |
| `peacock-api/src/middleware/logging.rs:89` `elapsed.as_secs_f64()*1000.0` | **PASS** | Latency metric, not money. |
| `peacock-api/src/dto/order.rs:75-80` `visit_f64` | **PASS with note** | `decimal_flexible::visit_f64` converts `f64 -> String -> Decimal` via `to_string()` to avoid binary noise. Accepts JSON numbers but never **stores** as f64; immediately widens to `Decimal`. Documented at `:35-37` “no value ever passes through an `f64`”. Acceptable — the `f64` is an input serde branch, not a money variable. No `Money(f64)` exists. |
| `peacock-storage/tests/schema.rs:322` `bind(1.5_f64)` | **PASS** | Test seeding geometry (`layout_x=1.5`), not rate. |
| All other `f64` hits in `Cargo.lock`, docs | **PASS** | Not code. |

**Money type bypass:** Grep `format!(".*money` / `format!("{}" ,.*money` / `Money(` construction — no bypass. Every money column is `NUMERIC(18,6)` ↔ `Decimal` ↔ `Money(Decimal)` with `#[serde(with="rust_decimal::serde::str")]` (`money.rs:54`). `dto/invoice.rs:437` test `a_value_that_f64_cannot_hold_round_trips_exactly` pins string wire. `menu.rs:242` `rate: MoneyString` string, `invoices.rs:8` docs forbid `Decimal`-as-number. No `format!("{}", money)` constructing a name or amount outside `invoicing::allocate_invoice_number` (which formats counter, not money).

### 2.2 Web — `Number(` in `lib/money.ts` (required grep)

`grep -rn "Number(" peacock-web/src` → 5 hits:

| File:Lines | Verdict | Severity | Detail |
|------------|---------|----------|--------|
| `peacock-web/src/lib/money.ts:103` `const intNum = Number(absInt)` | **PASS with LOW** | **LOW** | `absInt` is the **integer part** of a paisa-rounded string (`paisa.split(".")[0]`). Used only to decide whether `Intl.NumberFormat` can group it (`Number.isSafeInteger(intNum)`) else fallback `groupIndian`. No arithmetic on money; grouping only. Guarded: `isSafeInteger` check + `try/catch` + manual fallback. Safe — but deserves a comment that it never loses paisa because fractional part is handled separately. No fix needed; optional: add `// grouping only, not arithmetic; fallback is manual` (already has). |
| `peacock-web/src/lib/money.ts` other lines | **PASS** | — | `toFixed`, `toDecimalPlaces`, `Decimal` arithmetic — no `Number` on money. |
| `peacock-web/src/components/ShiftPanel.tsx:248` `Number(e.target.value)` | **PASS** | — | `cutoffHour` 0-23, not money. |
| `peacock-web/src/app/pos/page.tsx:236` `Number(e.target.value)` | **PASS** | — | `pax` count, not money. |
| `peacock-web/src/app/kds/page.tsx:28-30,68` `Number(parts[0])`, `Number(it.quantity)` | **PASS** | — | Time parsing + quantity count display, not money. |

**Web money helpers audit:**

- `money.ts:1-9` header “never JS Number” honoured: every fn takes `MoneyString`, returns `MoneyString`, internal `Decimal` (decimal.js) with `ROUND_HALF_UP` matching Rust `MidpointAwayFromZero`. `parseMoney` never via `Number`. `formatMoney:87` splits paisa string and groups integer via `Intl` or manual — correct.
- `lib/api.ts`: `MoneyString = string` (47), all rate/total/paid fields `MoneyString` (242,273,315,352,397,618,735,826,915). No `number` for money. `ordersApi:326-330` comment “Money as string (Decimal) — accepts number|string on input, string on output” — input flexibility via `decimal_flexible` server-side, not wire `Number`.
- No `Number(rate)` / `parseFloat(total)` in `pos/page.tsx`, `kds/page.tsx`, `ShiftPanel.tsx` — they use `formatMoney`, `sumMoney`, `mulMoney`.

**Rust → Web contract:**

- `money.rs:49` `#[serde(with="rust_decimal::serde::str")]` → wire string. Test `money_serialises_as_string_not_number:143` asserts `"1234.56"`.
- `tax.rs:188` `taxable_value * rate` via `Money*Decimal`, `to_paisa()` — never f64.
- `cogs.rs:211` `Money::new(batch_cost.inner()/bom.quantity)` — Decimal division, no f64.
- `reports.rs:121` `gross_margin_pct` computes `(gross_profit.inner()/revenue.inner())*100` as `Decimal`, rounds 2 dp, `to_string()` — string on wire, not `f64` `gross_margin_pct: f64`.

### 2.3 One silent-coercion note (not a fail)

`peacock-api/src/routes/aggregators.rs:229-232`

```rust
let qty = ai.quantity.round().to_string().parse::<i32>().unwrap_or(1).max(1);
```

`ai.quantity` is `Decimal` (fractional “2.5 plates”). Rounding to `i32` truncates and `unwrap_or(1)` hides overflow. `OrderItem.qty` is `i32` (Int upstream). No panic, but a large quantity (e.g., `1e12`) silently becomes `1`, understating KOT quantity. **Severity LOW** — aggregators are external, quantities small in practice. Fix: `qty.try_into()?` with `ApiError::invalid_input` on overflow, or keep `Decimal` qty end-to-end (schema `orders.qty Decimal` already).

---

## 3. Task 3 — SQL string interpolation vs `bind()`

**Rule:** No `format!("SELECT ... {}", user_input)`; all external values via `sqlx::query(...).bind()`.

### 3.1 Method

- Grep `format!.*SELECT|format!.*INSERT|format!.*UPDATE|format!.*DELETE` → 3 hits, all `SELECT_ORDER` constant (below).
- Grep `format!` in `peacock-storage/src/repos` → audited each interpolation target.

### 3.2 Findings

| Pattern | File:Lines | Verdict | Detail |
|---------|------------|---------|--------|
| `format!("{SELECT_ORDER} WHERE id = $1")` | `peacock-storage/src/repos/order.rs:343,358,1217` | **PASS** | `SELECT_ORDER` is `const &str` defined at `:959` containing only column list + `FROM orders`. Interpolated fragment is **compile-time constant**, not user input. Actual filter value bound via `.bind(id.get())`. `cargo clippy` `format_in_format_args` not relevant. No injection. |
| `format!("DELETE FROM {}", tbl)` | `peacock-api/src/routes/tables.rs:297` (test `clean_and_seed`) | **PASS with note** | `tbl` iterates over hard-coded `&["order_items","orders","invoice_lines",...]` — literal slice, not user input. Test-only. Production code never cleans via this pattern. Consider `sqlx::query("DELETE FROM order_items")` per table to silence the pattern, but not a vuln. |
| `format!("POS-2627-{:06}", ...)` etc | `peacock-core/src/invoicing.rs:141` `format!("{series}-{fiscal_year}-{next:06}")` | **PASS** | Invoice name formatting from trusted counters, not SQL. Not a `query`. |
| All other `sqlx::query` / `query_as` / `query_scalar` | `invoice.rs`, `order.rs`, `kot.rs`, `menu.rs`, `table.rs`, `routing.rs`, `aggregator.rs`, `shift.rs`, `price.rs`, `bom.rs` | **PASS** | Every external value (table name, invoice name, item_code, qty, rate, series) via `.bind()`. Examples: `invoice.rs:418` `.bind(series).bind(fiscal_year).bind(u64_to_i64(start)?)`, `order.rs:863` `.bind(from.as_str()).bind(to.as_str())`, `aggregator.rs` all `bind`. No `format!("...{}", var)` building a WHERE. |
| `sqlx::query("SELECT 1 FROM ...")` checks | `tests/schema.rs`, `peacock-api/tests/integration_orders.rs:58` | **PASS** | Existence probes, no interpolation. |

**Conclusion:** No SQL injection. The only `format!` touching SQL is the `SELECT_ORDER` constant idiom — idiomatic `sqlx` reuse, not string concatenation of input. Recommend keeping a `// SELECT_ORDER is a constant, not user input` comment (already implied by name, but explicit helps future auditors).

---

## 4. Task 4 — Tests that cannot fail

**Rule:** No `assert!(true)`, no test without `assert`, no `200` assert where DB is required but not seeded.

### 4.1 Method

- Grep `assert!(true)|assert_eq!(1, 1)` → 0 hits.
- Grep `fn .*_test|#[tokio::test]` then inspect body for at least one `assert*` or `assert_eq!`/`assert_ne!`/`assert!(...is_err())`/`expect` on error. Flagged tests with `let _ =` or no assertion.
- Cross-reference tests that assert `StatusCode::OK`/`200` but run without `TestDb`/`MenuFixture`.

### 4.2 Per-file test audit

| File | Verdict | Lines | Severity | Finding & Fix |
|------|---------|-------|----------|---------------|
| `peacock-api/src/routes/cogs.rs` | **PASS** | `:450-986` | — | Every `#[test]` asserts numeric equality or error detail: `assert_eq!(aggregate.total, Money::new(dec!(35.00)))`, `assert_ne!(…, dec!(350))`, `assert!(aggregate.has_unset_items())`. No vacuous. Async `calculate_rejects_*` assert `BAD_REQUEST` + `detail.contains(...)` — real. `calculate_accepts_a_valid_scope_and_reaches_the_storage_gap:933` asserts `409` for missing invoice and `200` for empty range — both prove handler hit Postgres (uses `Config::default()` with `shared_storage`). Correctly seeds via `shared_storage` migrated DB. |
| `peacock-api/src/routes/invoices.rs` | **PASS** | `:580-1746` | — | `TestDb::new().await` per test (isolated DB). `create_returns_201_with_the_domain_totals:693` asserts 8 money strings + status. `a_created_invoice_is_a_committed_row_not_just_a_201:715` reads back via `sqlx::query_as` from DB — can’t pass without commit. `ten_replays_of_one_key_yield_one_invoice:931` asserts `CREATED`→`OK`, counter `000002`, `count()==2` via list + `SELECT count(*)`. Not vacuous. `invariant_remainder_checks` at `:1554,1576` assert `cgst+sgst==total_tax` via Decimal parse. No `assert!(true)`. |
| `peacock-api/src/routes/menu.rs` | **PASS** | `:238-777` | — | Every test `assert_eq!(status, ...)` + body field: `assert_eq!(body["rate"], "250.000000")`, `assert!(item["rate"].is_string())`, `assert_eq!(codes, vec!["BIRYANI",...])`. `MenuFixture::try_new().await else { return; }` is skip-on-no-DB, not vacuous — when DB present it seeds via `seed_room/restaurant/item/course/menu` etc. |
| `peacock-api/src/routes/items.rs` | **PASS** | `:170-580` | — | Similar: every test asserts status + body. `item_details_never_carries_a_price:224` loops 5 absent fields + asserts raw not containing `"250"`. `price_returns...` asserts `"99.000000"`. DB via `MenuFixture`. |
| `peacock-api/src/routes/tables.rs` | **PASS** | `:271-651` | — | `TestDb` + `clean_and_seed` per test. `list_tables_returns_all_when_no_filters:377` asserts `count==5` after seed. `merge_tables_creates_real_cluster:551` asserts `count==2` + `cluster.contains` + **persistence check** `repo.get(&TableName::from("T-03")).unwrap()` still `contains("T-05")`. Not vacuous. |
| `peacock-api/src/routes/orders.rs` | **PASS** | `:569-1100` | — | `Fixture::new().await` gives `TestDb` + migrated DB. `create_returns_201...:714` asserts `grand_total=="540.00"`. `concurrent_replays_of_one_key_return_one_order:915` is `flavor="multi_thread", worker_threads=4` with `tokio::spawn` — actually concurrent. `a_replay_does_not_add_a_second_order:900` asserts `order_count()==1` via `SELECT count(*)`. No stub. |
| `peacock-api/src/routes/reports.rs` | **PASS** | `:420-1229` | — | Pure tests: `revenue_counts_paid_and_consolidated_only:558` asserts `revenue==350`, `invoice_count==2`. `business_day_end_is_exclusive:650` asserts `!day.contains(day.end)` etc. Integration probes at `:1100` assert `400` for missing date etc. The 4 endpoint probes at `:1189` assert `405/404`. Not vacuous. |
| **`peacock-api/src/routes/shifts.rs`** | **FAIL** | `:169-426` | **HIGH** | **Stale stub tests masquerading as coverage.** Every test uses `app::build(Config::default()).oneshot` with **no** `TestDb`. After W1-A `AppState::new` now returns a real `shared_storage` DB, so handlers are real `PostgresShiftRepo`, not stubs. Yet tests still assert `is_server_error()` for *successful* paths: `open_shift_requires_terminal_and_user:181` `assert!(is_server_error())` expects 500 for a valid open; with a real DB it should be `201` or `409`. `get_current_shift_with_terminal:261` expects 500; should be `404` or `200`. `close_shift_accepts_default_cutoff:274` expects 500. 9 of 12 tests assert 500 as *correct* — they will **fail** when the DB is up (correct implementation looks like a failure) and **pass** when the DB is down (outage looks like success). Classic “cannot fail for the right reason”. Fix: rewrite like `invoices.rs`: `let f = Fixture::new().await;` `TestDb`, seed terminal, assert `201`, `404`, `409` per `PostgresShiftRepo` contract. Keep one negative test (`close_shift_rejects_invalid_cutoff_hour:311` correctly asserts `400`) — that one is PASS. |
| **`peacock-api/src/routes/kot.rs`** | **FAIL (conditional)** | `:270-520` | **MEDIUM** | Mixed. Validation tests `generate_kot_requires_items:288` etc correctly assert `400` (no DB needed) — **PASS**. But `every_endpoint_reports_storage_unavailable_without_a_pool:382` asserts `500` with message `"database error"` for *valid* generate+get+pending+mark-prepared. Comment at `:359` says “`Config::default()` carries no pool, so these pin the no-storage behaviour”. **False since W1-A**: `build(Config::default())` now *does* carry `shared_storage` (see `app.rs:41`), so a valid generate no longer returns the opaque 500 — it either succeeds (200) or returns a domain 409/400. This test now **fails when the fix is present** and **passes only when the DB is unreachable**. It masks regressions: a future regression that returns `KOT-` stub would still pass this test if the DB happens to be down. Fix: split into (a) validation-only tests (keep) and (b) storage tests using `TestDb` with real routing fixtures (like `tests/invoice_kot_postgres.rs`) asserting `200` + persisted KOT rows. |
| `peacock-api/src/app.rs` | **PASS** | `:70-310` | — | Every test asserts status + body/headers: `health_returns_200:89`, `every_response_carries_a_request_id:100`, `wrong_method_returns_405:169`. Not vacuous. Uses `shared_storage`, not asserting DB writes. |
| `peacock-api/src/middleware/error.rs` | **PASS** | `:144-260` | — | `handler_error_keeps_its_detail:199` asserts `detail=="Table 'T-001' does not exist"` + `instance` + `request_id`. `internal_details_are_not_leaked:210` asserts opaque detail. Not vacuous. |
| `peacock-api/src/state.rs` | **PASS** | `:212-356` | — | `clones_share_one_connection_pool:232` asserts `ptr_eq`. `a_repository_accessor_answers_instead_of_panicking:338` asserts `peek_series("NOPE").await.unwrap().is_none()` on real DB. Real. |
| `peacock-core/src/money.rs` | **PASS** | `:135-250` | — | 7 tests, each pins rounding: `rounding_strategy_pin_paisa_boundary_positive:180` asserts `Money(9.005).to_paisa()==9.01` (half-up) vs bankers. `no_float_drift_over_many_additions:244` sum of `0.1*10==1.0`. Could fail if rounding constant flipped — intended. |
| `peacock-core/src/tax.rs` | **PASS** | `:222-510` | — | 12 tests, each asserts totals: `worked_example_from_plan:237` asserts `net_total 400`, `isz_zero`. `cgst_and_sgst_split_odd_paisa:280` asserts `cgst 9.01, sgst 9.00`. Not vacuous. |
| `peacock-core/src/cogs.rs` | **PASS** | `:370-1080` | — | 18 tests, each asserts `cost` equality to hand-computed Decimal, not `assert!(result.is_ok())`. `one_level_bom_with_quantity_not_one:494` asserts `35.00` not `350.00` — the 10× bug guard. `three_level_bom_third_level_priced_as_leaf:574` asserts `30` with `assert!(unset.is_empty())`. |
| `peacock-core/src/invoicing.rs` | **PASS** | `:230-800` | — | 18 tests, each asserts gapless, idempotency, 16-char cap. `over_long_series_does_not_burn_a_number:511` asserts `peek()==Some(7)` after rejection — proves no burn. Not vacuous. |
| `peacock-storage/tests/*` `peacock-storage/src/repos/*` `#[test]` | **PASS** | — | — | Tests that touch DB use `TestDb`/`database_url` + explicit `DELETE FROM`/`INSERT`. E.g., `shift.rs:439` `biased` vs `aggregator.rs:602` asserts `find_order` returns same `total`. No assertion-free tests. One `let _ = repo.list_settlements(...).unwrap()` at `aggregator.rs:770` discards `len` but still asserts no panic + `is_empty` semantics via prior seed — borderline but not vacuous; the settlement test at `:750` asserts `unwrap()` success as the claim. Suggest `assert!(settlements.is_empty() || ...)` instead of `let _ =`. |

### 4.3 Tests without DB but assert 200

Audit: no test asserts `200` without seeding when a DB read is required. `shifts.rs` and `kot.rs` do the opposite (assert `500` without DB) — already flagged as FAIL. `cogs.rs:933` `calculate_accepts_a_valid_scope_and_reaches_the_storage_gap` correctly expects `409`/`200` *with* `shared_storage` (has DB), and the test that seeded `range_resp` asserts `json["cogs"]=="0"` only after confirming `status==200` — valid 0-COGS empty range, not a missing seed.

---

## 5. Wave 1 exit gate — stub hunt (adversarial)

**Gate: `grep -rn "not yet implemented|todo!|unimplemented!" peacock-api/src` → 0 hits outside tests**

Result: **0 hits.** Verified via `grep` above. The 6 stubs listed in MASTER_PLAN §1:

- `cogs.rs:199` — gone, handler `calculate_cogs` wired.
- `items.rs:40` — gone.
- `menu.rs:67,102` — gone, `menu.rs:53` dead `strategy` binding removed (now `query.strategy()` at `:90` + `classify_strategy`).
- `reports.rs:332,371` — gone, `reports.rs:308` `daily_pl` + `365` `item_costing` wired.
- `tables.rs:53` list — now `list_all` at `:47`.
- `tables.rs:267` transfer — now `transfer_table` at `:252`.
- `aggregators.rs` 6 TODOs — now 5 routes wired, 0 TODO.

**`InvoiceBackend::Memory` gate:** `grep -rn InvoiceBackend::Memory` → 0 hits. `state.rs` holds `Storage` not `Option<Storage>`.

**Build gates:** Not executed in this read-only lane (require `cargo build --workspace`, `cargo clippy -D warnings`, `cargo test --workspace`, `parity 22/22`), but code inspection shows no `todo!` to break them. The only clippy risks are in `blocking.rs` panic and `FakeOrderRepo` dead guard — not warnings.

**Stubs reported as done but incomplete:**

- **`tables.rs:127 FakeOrderRepo`** — handler reports “merge done” while active-order guard is stubbed to `Ok(0)`. **Severity MEDIUM**, fix in §1.4.
- **`shifts.rs` / `kot.rs` tests reporting “verified” while testing the old 503 behavior** — not a handler stub but a verification stub. **Severity HIGH/MEDIUM**, fix in §4.

---

## 6. Cross-cutting money invariant check (not in gate but adjacent)

- **Parity:** `peacock-parity/fixtures/*.json` 22 fixtures, `peacock-core` docs claim `scripts/parity_reference.py` `ROUNDING = HALF_UP` matches Rust `ROUNDING:191`. No drift observed in this audit (no run).
- **Gapless:** `invoice.rs:1210` `allocate_number` is single `UPDATE ... RETURNING` held to commit; `create_invoice_idempotent:468` lookup before counter. `order.rs:697` same. `invoicing.rs:130` probe before allocate. Correct.
- **Revenue single definition:** `reports.rs:88` `summarise_revenue` filters `day.contains && counts_as_revenue()`. `shift.rs` close uses same `PosInvoiceStatus::REVENUE` via `shift_repo.close_shift`. `invoices.rs:565` `list_filtered` revenue sum via same. No second list.
- **Business day half-open:** `reports.rs:88` `day.contains` is `[start,end)`. `invoice.rs:649` `WHERE posted_at >= $1 AND posted_at < $2` — consistent. `shifts.rs` uses `BusinessDay` same.

---

## 7. Fix suggestions — priority order

### P0 — Must fix before calling W1 “verified”

1. **`peacock-api/src/routes/shifts.rs:169-426` rewrite tests** — Create `Fixture` with `TestDb` (pattern `invoices.rs:603` or `tables.rs:281`). Seed `terminals`, `shifts` via `PostgresShiftRepo::open_shift`. Assert:
   - `open_shift` → `201` + `shift.name` starts `SHIFT-`
   - `open_shift` dup terminal → `409`
   - `get_current_shift` → `200` when open, `404` when none
   - `close_shift` → `200` + `invoice_count` + `cash_total` string
   - `close_shift` with `cutoff_hour=25` → `400`
   Keep only `open_shift_rejects_invalid_date_format` as 400 path. Delete the 8 `assert!(is_server_error())` blocks.

2. **`peacock-api/src/routes/tables.rs:127` wire real order guard** — Replace `FakeOrderRepo` with:
   ```rust
   let distinct: Vec<TableName> = cluster.sorted_members().to_vec(); // or targets+anchor
   let count = storage.order_repo().count_separate_active_async(&distinct).await.map_err(ApiError::from)?;
   if count > 1 { return Err(ApiError::conflict("multiple active orders")) }
   ```
   Or reuse `storage.table_repo().count_separate_active` if port is sync via `block_on` (but prefer async). Add a test: seed two Draft unprinted invoices on `T-01`, `T-02`, attempt merge `T-01 <- T-02` → `409`.

### P1 — Should fix in W1 follow-up

3. **`peacock-storage/src/repos/blocking.rs:55,133` eliminate `expect`** — Change `fn block_on` to return `Result<T, StorageError>` and propagate `Handle::try_current().map_err(|_| StorageError::Internal("not in a tokio runtime".into()))?`. Callers map to `ApiError::internal`. Prevents thread panic on mis-configured runtime. Test with `#[tokio::test(flavor="current_thread")]` should get `500` not panic.

4. **`peacock-api/src/routes/kot.rs:382-440` split test** —
   - Keep `generate_kot_requires_*` (400) as is.
   - Move `every_endpoint_reports_storage_unavailable...` to `peacock-api/tests/invoice_kot_postgres.rs` or to a `#[ignore]` integration test that *disables* pool (e.g., `AppState::with_storage(Config, Storage::new(DatabaseUrl("postgres://invalid")))`), assert `500` opaque. The unit test with `shared_storage` should instead assert happy path: generate with seeded `rooms/production_units/item_groups` → `200` with `kots.len()==2`.

5. **`peacock-storage/tests/aggregator.rs:770` `let _ = settlements.len()`** — Replace with `assert!(settlements.len() <= expected_seed_count)` or at least `assert!(settlements.is_empty() || settlements.len() >= 0)` is vacuous; make it `assert_eq!(settlements, vec![])` when seeded empty. Not blocking but sloppy.

### P2 — Polish / LOW

6. **`peacock-web/src/lib/money.ts:103` document `Number` grouping** — Add inline `// grouping only; paisa kept as string` (already partially). No behavior change.

7. **`peacock-api/src/routes/aggregators.rs:229` quantity overflow** — Change to:
   ```rust
   let qty: i32 = ai.quantity.round().to_i32().ok_or_else(|| ApiError::invalid_input(format!("quantity {} overflows i32", ai.quantity)))?.max(1);
   ```
   using `Decimal::to_i32()` (or `TryInto<i32>`). Fail the webhook with `400` rather than silently `1`.

8. **`peacock-api/src/routes/tables.rs:297` test `format!("DELETE FROM {}", tbl)`** — Replace loop with explicit `sqlx::query("DELETE FROM orders").execute` etc to remove `format!` from audit grep (already safe, but reduces noise).

---

## 8. File-by-file verdict summary

| File | Unwrap | Money `f64`/`Number` | SQL inject | Tests | Overall |
|------|--------|----------------------|------------|-------|---------|
| `peacock-core/src/money.rs` | PASS | PASS | N/A | PASS | **PASS** |
| `peacock-core/src/tax.rs` | PASS | PASS | N/A | PASS | **PASS** |
| `peacock-core/src/cogs.rs` | PASS | PASS | N/A | PASS | **PASS** |
| `peacock-core/src/invoicing.rs` | PASS | PASS | N/A | PASS | **PASS** |
| `peacock-storage/src/repos/invoice.rs` | PASS | PASS | PASS | PASS | **PASS** |
| `peacock-storage/src/repos/order.rs` | PASS | PASS | PASS | PASS | **PASS** |
| `peacock-storage/src/repos/shift.rs` | PASS (tests have expect) | N/A | PASS | PASS | **PASS** |
| `peacock-storage/src/repos/table.rs` | PASS | PASS (f64 geometry) | PASS | PASS | **PASS** |
| `peacock-storage/src/repos/aggregator.rs` | PASS | PASS | PASS | PASS (minor) | **PASS** |
| `peacock-storage/src/repos/blocking.rs` | **FAIL MEDIUM** `expect` can panic thread | N/A | N/A | — | **FAIL** |
| `peacock-api/src/routes/health.rs` | PASS | PASS | N/A | PASS | **PASS** |
| `peacock-api/src/routes/menu.rs` | PASS | PASS | PASS | PASS | **PASS** |
| `peacock-api/src/routes/items.rs` | PASS | PASS | PASS | PASS | **PASS** |
| `peacock-api/src/routes/tables.rs` | PASS (handler) **FAIL debt** `FakeOrderRepo` | PASS | PASS | PASS (handler) | **FAIL debt** |
| `peacock-api/src/routes/orders.rs` | PASS | PASS | PASS | PASS | **PASS** |
| `peacock-api/src/routes/invoices.rs` | PASS | PASS | PASS | PASS | **PASS** |
| `peacock-api/src/routes/cogs.rs` | PASS | PASS | PASS | PASS | **PASS** |
| `peacock-api/src/routes/reports.rs` | PASS | PASS | PASS | PASS | **PASS** |
| `peacock-api/src/routes/kot.rs` | PASS | PASS | PASS | **FAIL MEDIUM** stale 500 test | **FAIL** |
| `peacock-api/src/routes/shifts.rs` | PASS (handler) | PASS | PASS | **FAIL HIGH** inverted 500 tests | **FAIL** |
| `peacock-api/src/routes/aggregators.rs` | PASS | PASS (LOW qty overflow note) | PASS | PASS | **PASS with debt** |
| `peacock-api/src/middleware/context.rs` | PASS | N/A | PASS (`bind` only) | PASS | **PASS** |
| `peacock-web/src/lib/money.ts` | PASS | **PASS LOW** `Number` grouping only | N/A | N/A | **PASS** |
| `peacock-web/src/lib/api.ts` | PASS | PASS | N/A | N/A | **PASS** |

**Counts:** PASS 19, PASS-with-debt 2, FAIL 3 (1 MEDIUM thread-panic, 1 HIGH stale shifts tests, 1 MEDIUM stale kot test + 1 MEDIUM merge guard debt counted already).

---

## 9. Evidence — grep excerpts

**Stubs:**

```
$ grep -rn "not yet implemented|todo!|unimplemented!" peacock-api/src
(no output — 0 hits)
$ grep -rn "InvoiceBackend::Memory" peacock-api/src
(no output)
```

**Unwrap on routes (all test-only):**

```
peacock-api/src/routes/cogs.rs:429  app::build(Config::default()).oneshot(request).await.unwrap()          # test
peacock-api/src/routes/cogs.rs:438  Body::from(serde_json::to_vec(&body).unwrap())                          # test
peacock-api/src/routes/shifts.rs    0 unwrap in handler range :32-163; 8 unwraps in :169-426 all test      # handler PASS
peacock-storage/src/repos/blocking.rs:55  .expect("building the storage sync-bridge runtime")               # FAIL — request thread
```

**SQL:**

```
peacock-storage/src/repos/order.rs:343  format!("{SELECT_ORDER} WHERE id = $1")                              # constant SELECT_ORDER
peacock-storage/src/repos/order.rs:358  format!("{SELECT_ORDER} WHERE restaurant_table = $1")                # constant
All other queries: sqlx::query("... $1").bind(user_value)                                                    # PASS
```

**Money:**

```
peacock-web/src/lib/money.ts:103  const intNum = Number(absInt);  // grouping only, guard isSafeInteger fallback groupIndian
peacock-core/src/model.rs:113     pub layout_x: f64, // geometry
peacock-api/src/dto/order.rs:75  fn visit_f64(... f64) -> Result<Decimal>  // serde branch, immediate Decimal via to_string()
```

---

## 10. What was **not** checked (out of scope)

- Live SSH probes (Wave 2 exit gate `netstat`/`curl`/`psql \dt`) — no remote access in this lane.
- `npm run build` / `peacock-web` E2E — not executed (read-only).
- `cargo test --workspace` / `cargo clippy -D warnings` / `cargo build --workspace` / `parity 22/22` — not executed (would require toolchain + DB + 100-concurrent harness). Code-inspection gate only.

---

## 11. Report output

This file is the adversarial report. Per task it should be written to `docs/VERIFICATION_REPORT.md` **or** `docs/history/W4_ADVERSARIAL.md`. This run wrote:

- **Primary:** `/Users/xoxo/Documents/resreah/billing/peacock-pos/docs/history/W4_ADVERSARIAL.md` (this file)
- **Mirror (for lane compatibility):** `/Users/xoxo/Documents/resreah/billing/peacock-pos/docs/VERIFICATION_REPORT.md` (identical copy, created below)

Both are plain Markdown, no code edits.

---

*— End W4-A — Muse Spark 1.2, adversarial, 2026-08-11*
