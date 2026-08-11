//! Invoice + KOT endpoints against a real Postgres — Lane 4A-3. **Money lane.**
//!
//! The unit tests in `src/routes/invoices.rs` drive the in-memory backend. These drive
//! the *same handlers* with a pool attached, so what is proved here is the wiring: that
//! `POST /api/invoices` reaches [`PgInvoiceRepo`], that the number on the response is the
//! one the row-locked counter issued, and that the two backends publish the same shape.
//!
//! # The two gates this file exists for
//!
//! * **Gapless under load** — 100 concurrent `POST /api/invoices`, distinct keys, must
//!   yield exactly the numbers 1..=100 with no gap and no duplicate. That is CGST Rule
//!   46(b) and it is the reason the counter is a locked row rather than a sequence.
//! * **Idempotency** — one key submitted 10 times must yield one invoice, one number, and
//!   a 201 followed by nine 200s.
//!
//! # Skipping
//!
//! Every test here needs a server. With `TEST_DATABASE_URL`/`DATABASE_URL` unset and no
//! Postgres on `localhost:5432`, they skip with a printed note rather than fail: a bare
//! checkout must still be able to run `cargo test`. The storage crate's own suites do the
//! same. A CI job that must not skip should assert on the connection separately.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use peacock_api::config::Config;
use peacock_api::state::AppState;
use peacock_storage::{DbConfig, Storage};
use serde_json::{json, Value};
use sqlx::{Connection, Executor, PgConnection};
use tower::ServiceExt;
use uuid::Uuid;

const BRANCH: &str = "Peacock - Main";
const RESTAURANT: &str = "Peacock Grand";
const ROOM: &str = "Main Hall";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A throwaway database, dropped when this goes out of scope.
///
/// A copy of `peacock-storage/tests/support`, not a reuse: that module is a test-only
/// sibling of another crate and Cargo gives no path to it from here.
struct TestDb {
    storage: Storage,
    admin_url: String,
    db_name: String,
}

impl TestDb {
    /// `None` when no server is reachable, so the caller can skip rather than fail.
    async fn try_new() -> Option<TestDb> {
        let admin_url = admin_url();
        let db_name = format!("peacock_api_test_{}", Uuid::new_v4().simple());

        let mut admin = PgConnection::connect(&admin_url).await.ok()?;
        admin
            // The identifier is a uuid we generated, but quoted anyway: CREATE DATABASE
            // takes no bind parameters.
            .execute(format!(r#"CREATE DATABASE "{db_name}""#).as_str())
            .await
            .ok()?;
        admin.close().await.ok();

        let url = swap_database(&admin_url, &db_name);
        let storage = Storage::connect(
            DbConfig::from_url(&url)
                .expect("test url is valid")
                .with_acquire_timeout(Duration::from_secs(5)),
        )
        .await
        .expect("connect + migrate");

        Some(TestDb {
            storage,
            admin_url,
            db_name,
        })
    }

    fn pool(&self) -> &sqlx::PgPool {
        self.storage.pool()
    }

    /// The graph an invoice and a KOT need: a restaurant, a room, a table, items, and two
    /// stations.
    async fn seed(&self) {
        sqlx::query("INSERT INTO rooms (name, branch, room_type) VALUES ($1, $2, 'AC')")
            .bind(ROOM)
            .bind(BRANCH)
            .execute(self.pool())
            .await
            .expect("seed room");

        sqlx::query(
            "INSERT INTO restaurants
                 (name, company, branch, pos_profile, invoice_series_prefix, default_room)
             VALUES ($1, 'Peacock Foods', $2, 'Peacock POS', 'PCK-', $3)",
        )
        .bind(RESTAURANT)
        .bind(BRANCH)
        .bind(ROOM)
        .execute(self.pool())
        .await
        .expect("seed restaurant");

        for table in ["T-01", "T-02"] {
            sqlx::query(
                "INSERT INTO tables (name, no_of_seats, restaurant, restaurant_room, branch)
                 VALUES ($1, 4, $2, $3, $4)",
            )
            .bind(table)
            .bind(RESTAURANT)
            .bind(ROOM)
            .bind(BRANCH)
            .execute(self.pool())
            .await
            .expect("seed table");
        }

        for (code, name, group) in [
            ("CURRY", "Chicken Curry", "Main Course"),
            ("NAAN", "Butter Naan", "Breads"),
            ("CHAI", "Masala Chai", "Beverages"),
        ] {
            sqlx::query("INSERT INTO items (code, name, item_group) VALUES ($1, $2, $3)")
                .bind(code)
                .bind(name)
                .bind(group)
                .execute(self.pool())
                .await
                .expect("seed item");
        }

        for (unit, groups) in [
            ("Hot Kitchen", vec!["Main Course", "Breads"]),
            ("Bar", vec!["Beverages"]),
        ] {
            sqlx::query("INSERT INTO production_units (name, branch) VALUES ($1, $2)")
                .bind(unit)
                .bind(BRANCH)
                .execute(self.pool())
                .await
                .expect("seed production unit");

            for (position, group) in groups.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO production_unit_item_groups (production_unit, idx, item_group)
                     VALUES ($1, $2, $3)",
                )
                .bind(unit)
                .bind(position as i32 + 1)
                .bind(group)
                .execute(self.pool())
                .await
                .expect("seed item group");
            }
        }
    }

    /// The production router, wired to this database.
    fn app(&self) -> axum::Router {
        peacock_api::app::build_with_state(
            AppState::builder(
                Config {
                    bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
                    ..Config::default()
                },
                self.storage.clone(),
            )
            .build(),
        )
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin_url = self.admin_url.clone();
        let db_name = self.db_name.clone();
        // Drop is sync and the cleanup is async, and this pool's runtime is going away
        // with us, so the drop work gets its own short-lived one.
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                return;
            };
            rt.block_on(async {
                if let Ok(mut admin) = PgConnection::connect(&admin_url).await {
                    let _ = admin
                        .execute(
                            format!(r#"DROP DATABASE IF EXISTS "{db_name}" WITH (FORCE)"#).as_str(),
                        )
                        .await;
                    let _ = admin.close().await;
                }
            });
        })
        .join();
    }
}

fn admin_url() -> String {
    std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "postgres://localhost:5432/postgres".to_owned())
}

fn swap_database(url: &str, db: &str) -> String {
    let (scheme, rest) = url.split_once("://").expect("url has a scheme");
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let query = tail.find('?').map(|q| &tail[q..]).unwrap_or("");
    format!("{scheme}://{authority}/{db}{query}")
}

/// Sets the database up, or prints why it is skipping and returns `None`.
macro_rules! db_or_skip {
    () => {
        match TestDb::try_new().await {
            Some(db) => {
                db.seed().await;
                db
            }
            None => {
                eprintln!(
                    "skipping: no Postgres reachable at {}. \
                     export TEST_DATABASE_URL to run this test.",
                    admin_url()
                );
                return;
            }
        }
    };
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.expect("router responds");
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("every response body must be JSON")
    };
    (status, json)
}

fn post(uri: &str, key: Option<Uuid>, body: Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key.to_string());
    }
    builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

/// The §5 worked example: 4 × ₹100, ₹40 discount, 5% GST → grand 378.00, rounded 378.
fn create_body() -> Value {
    json!({
        "order_id": "ORD-001",
        "table": "T-01",
        "customer_name": "Walk-in",
        "lines": [
            {"item_code": "CURRY", "item_name": "Chicken Curry", "quantity": "4", "rate": "100"}
        ],
        "discount": "40",
        "tax_rate": "0.05",
        "series": "POS",
        "posted_at": "2026-07-28T14:30:00Z"
    })
}

// ===========================================================================
// 1. Creation reaches Postgres
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_allocates_a_real_number_and_persists_the_row() {
    let db = db_or_skip!();
    let app = db.app();

    let (status, json) = send(&app, post("/api/invoices", Some(Uuid::new_v4()), create_body())).await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {json}");

    // Rule 46(b): series-fycode-counter, ≤16 chars. 2026-07-28 → FY 2026-27 → "2627".
    assert_eq!(json["invoice_id"], "POS-2627-000001");
    assert!(json["invoice_id"].as_str().unwrap().len() <= 16);
    assert_eq!(json["fiscal_year"], "2026-27");

    // Every money figure is the one `compute_totals` produced, as a string.
    assert_eq!(json["net_total"], "400");
    assert_eq!(json["discount"], "40");
    assert_eq!(json["taxable_value"], "360");
    assert_eq!(json["tax"]["cgst"], "9.00");
    assert_eq!(json["tax"]["sgst"], "9.00");
    assert_eq!(json["tax"]["igst"], "0");
    assert_eq!(json["tax"]["total_tax"], "18.00");
    assert_eq!(json["grand_total"], "378.00");
    assert_eq!(json["rounded_total"], "378");
    assert_eq!(json["status"], "Draft");
    assert_eq!(json["order_id"], "ORD-001");
    assert_eq!(json["table"], "T-01");

    // The row is really there, with the figures the response claimed.
    let (name, grand, rounded): (String, rust_decimal::Decimal, rust_decimal::Decimal) =
        sqlx::query_as("SELECT name, grand_total, rounded_total FROM invoices")
            .fetch_one(db.pool())
            .await
            .expect("exactly one invoice row");
    assert_eq!(name, "POS-2627-000001");
    assert_eq!(grand, rust_decimal_macros::dec!(378.00));
    assert_eq!(rounded, rust_decimal_macros::dec!(378));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_get_reads_back_exactly_what_the_create_returned() {
    let db = db_or_skip!();
    let app = db.app();

    let (_, created) = send(&app, post("/api/invoices", Some(Uuid::new_v4()), create_body())).await;
    let id = created["invoice_id"].as_str().unwrap();

    let (status, fetched) = send(&app, get(&format!("/api/invoices/{id}"))).await;
    assert_eq!(status, StatusCode::OK);

    // The create echoes the owning key; a GET has none. Everything else must match to the
    // paisa — a read that restated a money figure would be a second source of truth.
    let mut expected = created.clone();
    expected.as_object_mut().unwrap().remove("idempotency_key");
    let mut actual = fetched.clone();
    actual.as_object_mut().unwrap().remove("idempotency_key");
    assert_eq!(actual, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_invoice_is_404_not_409() {
    // PgInvoiceRepo reports a missing invoice as a domain `Conflict` (peacock_core::Error
    // has no generic NotFound). Left alone that surfaces as 409; the handler maps it back.
    let db = db_or_skip!();
    let (status, json) = send(&db.app(), get("/api/invoices/POS-2627-999999")).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["status"], 404);
    assert_eq!(json["instance"], "/api/invoices/POS-2627-999999");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_input_does_not_burn_a_number() {
    // The gapless guarantee at the HTTP layer: validation runs before any allocation.
    let db = db_or_skip!();
    let app = db.app();

    let mut bad = create_body();
    bad["lines"] = json!([]);
    let (status, _) = send(&app, post("/api/invoices", Some(Uuid::new_v4()), bad)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, good) = send(&app, post("/api/invoices", Some(Uuid::new_v4()), create_body())).await;
    assert_eq!(
        good["invoice_id"], "POS-2627-000001",
        "a rejected request must not consume a number"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_still_requires_an_idempotency_key() {
    let db = db_or_skip!();
    let (status, json) = send(&db.app(), post("/api/invoices", None, create_body())).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["detail"].as_str().unwrap().contains("Idempotency-Key"));
}

// ===========================================================================
// 2. GATE: gapless numbering under 100 concurrent creates
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn one_hundred_concurrent_creates_are_gapless_and_unique() {
    // The Rule 46(b) gate. `nextval()` would pass the uniqueness half of this and fail the
    // gapless half the moment one insert rolled back, which is why the counter is a locked
    // row (005_invoice.sql).
    let db = db_or_skip!();
    let app = db.app();

    let mut handles = Vec::with_capacity(100);
    for n in 0..100 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let mut body = create_body();
            body["order_id"] = json!(format!("ORD-{n:03}"));
            let (status, json) = send(&app, post("/api/invoices", Some(Uuid::new_v4()), body)).await;
            (status, json["invoice_id"].as_str().unwrap_or("").to_owned())
        }));
    }

    let mut ids = Vec::with_capacity(100);
    for handle in handles {
        let (status, id) = handle.await.expect("no task may panic");
        assert_eq!(status, StatusCode::CREATED, "every distinct key must create");
        ids.push(id);
    }

    // No duplicates: 100 requests, 100 distinct names.
    let unique: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), 100, "a number was issued twice");

    // No gaps: exactly 1..=100.
    let mut numbers: Vec<u32> = ids
        .iter()
        .map(|id| {
            id.rsplit('-')
                .next()
                .expect("name ends in the counter")
                .parse()
                .expect("the counter is numeric")
        })
        .collect();
    numbers.sort_unstable();
    assert_eq!(
        numbers,
        (1..=100).collect::<Vec<u32>>(),
        "the series must be an unbroken run"
    );

    // And the database agrees, which is the claim that actually matters.
    let repo = peacock_storage::repos::PgInvoiceRepo::new(db.storage.clone());
    assert!(
        repo.find_series_gaps("POS", "2627").await.unwrap().is_empty(),
        "the schema's own gapless audit must find nothing"
    );
    assert_eq!(
        repo.issued_numbers("POS", "2627").await.unwrap(),
        (1..=100).collect::<Vec<u64>>()
    );

    // The counter advanced exactly 100 times, so the next number is 101 — no burn.
    assert_eq!(repo.peek_series("POS", "2627").await.unwrap(), Some(101));

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 100);
}

// ===========================================================================
// 3. GATE: idempotency
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_replays_of_one_key_yield_one_invoice_and_one_number() {
    let db = db_or_skip!();
    let app = db.app();
    let key = Uuid::new_v4();

    let (status, first) = send(&app, post("/api/invoices", Some(key), create_body())).await;
    assert_eq!(status, StatusCode::CREATED);
    let invoice_id = first["invoice_id"].as_str().unwrap().to_owned();
    assert_eq!(invoice_id, "POS-2627-000001");

    for attempt in 1..=10 {
        let (status, replay) = send(&app, post("/api/invoices", Some(key), create_body())).await;
        // 200, not 201: the invoice already existed.
        assert_eq!(status, StatusCode::OK, "replay {attempt} must not create");
        assert_eq!(
            replay["invoice_id"], invoice_id,
            "replay {attempt} returned a different invoice"
        );
        assert_eq!(replay, first, "replay {attempt} body diverged");
    }

    // One row, and the counter moved exactly once: a fresh key gets 000002.
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1, "10 replays must not add rows");

    let repo = peacock_storage::repos::PgInvoiceRepo::new(db.storage.clone());
    assert_eq!(repo.peek_series("POS", "2627").await.unwrap(), Some(2));

    let (status, fresh) = send(&app, post("/api/invoices", Some(Uuid::new_v4()), create_body())).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(fresh["invoice_id"], "POS-2627-000002");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn ten_concurrent_replays_of_one_key_still_yield_one_invoice() {
    // The race the repository retries for: all ten miss the key lookup, one wins the
    // unique insert, and the losers' whole transactions — counter increment included —
    // roll back. Net effect must be one number, one invoice, no gap.
    let db = db_or_skip!();
    let app = db.app();
    let key = Uuid::new_v4();

    let mut handles = Vec::with_capacity(10);
    for _ in 0..10 {
        let app = app.clone();
        handles.push(tokio::spawn(async move {
            let (status, json) = send(&app, post("/api/invoices", Some(key), create_body())).await;
            (status, json["invoice_id"].as_str().unwrap_or("").to_owned())
        }));
    }

    let mut created = 0;
    let mut ids = BTreeSet::new();
    for handle in handles {
        let (status, id) = handle.await.expect("no task may panic");
        assert!(
            status == StatusCode::CREATED || status == StatusCode::OK,
            "a concurrent replay must not error, got {status}"
        );
        if status == StatusCode::CREATED {
            created += 1;
        }
        ids.insert(id);
    }

    assert_eq!(created, 1, "exactly one request may create the invoice");
    assert_eq!(ids.len(), 1, "every response must name the same invoice");

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1);

    let repo = peacock_storage::repos::PgInvoiceRepo::new(db.storage.clone());
    assert!(repo.find_series_gaps("POS", "2627").await.unwrap().is_empty());
    assert_eq!(
        repo.peek_series("POS", "2627").await.unwrap(),
        Some(2),
        "nine rolled-back allocations must burn nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replay_ignores_a_changed_body() {
    // The key owns the invoice. A retry with drifted content must not mutate the original,
    // or the stored total would stop matching the number that was issued for it.
    let db = db_or_skip!();
    let app = db.app();
    let key = Uuid::new_v4();

    let (_, first) = send(&app, post("/api/invoices", Some(key), create_body())).await;

    let mut tampered = create_body();
    tampered["lines"] =
        json!([{"item_code": "CURRY", "item_name": "Chicken Curry", "quantity": "99", "rate": "999"}]);
    let (status, replay) = send(&app, post("/api/invoices", Some(key), tampered)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["invoice_id"], first["invoice_id"]);
    assert_eq!(replay["grand_total"], first["grand_total"]);
    assert_eq!(replay["grand_total"], "378.00");
}

// ===========================================================================
// 4. Payments through the endpoint
// ===========================================================================

/// Creates an invoice and returns `(id, rounded_total)`.
async fn create_invoice(app: &axum::Router) -> (String, String) {
    let (status, json) = send(app, post("/api/invoices", Some(Uuid::new_v4()), create_body())).await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {json}");
    (
        json["invoice_id"].as_str().unwrap().to_owned(),
        json["rounded_total"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_payment_settles_the_invoice_through_the_endpoint() {
    let db = db_or_skip!();
    let app = db.app();
    let (id, due) = create_invoice(&app).await;

    let (status, json) = send(
        &app,
        post(
            &format!("/api/invoices/{id}/pay"),
            None,
            json!({"method": "Upi", "amount": due, "reference": "txn-4242",
                   "paid_at": "2026-07-28T15:00:00Z"}),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "payment failed: {json}");
    assert_eq!(json["status"], "Paid");
    assert_eq!(json["paid_amount"], "378");
    assert_eq!(json["outstanding_amount"], "0");
    assert_eq!(json["payments"][0]["method"], "Upi");
    assert_eq!(json["payments"][0]["amount"], "378");
    assert_eq!(json["payments"][0]["reference"], "txn-4242");
    assert_eq!(json["payments"][0]["paid_at"], "2026-07-28T15:00:00Z");

    // The tender is a row, not just a column, so the Z-report can split the drawer.
    let (method, amount): (String, rust_decimal::Decimal) =
        sqlx::query_as("SELECT method::TEXT, amount FROM invoice_payments WHERE invoice = $1")
            .bind(&id)
            .fetch_one(db.pool())
            .await
            .expect("one payment row");
    assert_eq!(method, "Upi");
    assert_eq!(amount, rust_decimal_macros::dec!(378));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn payment_does_not_alter_any_invoice_total() {
    let db = db_or_skip!();
    let app = db.app();
    let (id, due) = create_invoice(&app).await;
    let (_, before) = send(&app, get(&format!("/api/invoices/{id}"))).await;

    let (_, after) = send(
        &app,
        post(
            &format!("/api/invoices/{id}/pay"),
            None,
            json!({"method": "Cash", "amount": due}),
        ),
    )
    .await;

    for field in [
        "net_total",
        "discount",
        "taxable_value",
        "grand_total",
        "rounded_total",
        "round_off",
    ] {
        assert_eq!(after[field], before[field], "{field} moved during payment");
    }
    assert_eq!(after["tax"], before["tax"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn split_tender_accumulates_then_settles_through_the_endpoint() {
    let db = db_or_skip!();
    let app = db.app();
    let (id, _) = create_invoice(&app).await;

    let (_, part) = send(
        &app,
        post(
            &format!("/api/invoices/{id}/pay"),
            None,
            json!({"method": "Card", "amount": "300"}),
        ),
    )
    .await;
    assert_eq!(part["status"], "Draft", "a short payment must not settle");
    assert_eq!(part["paid_amount"], "300");
    assert_eq!(part["outstanding_amount"], "78");

    let (_, rest) = send(
        &app,
        post(
            &format!("/api/invoices/{id}/pay"),
            None,
            json!({"method": "Cash", "amount": "78"}),
        ),
    )
    .await;
    assert_eq!(rest["status"], "Paid");
    assert_eq!(rest["paid_amount"], "378");
    assert_eq!(rest["outstanding_amount"], "0");
    assert_eq!(rest["payments"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overpayment_is_refused_through_the_endpoint() {
    let db = db_or_skip!();
    let app = db.app();
    let (id, _) = create_invoice(&app).await;

    let (status, _) = send(
        &app,
        post(
            &format!("/api/invoices/{id}/pay"),
            None,
            json!({"method": "Cash", "amount": "500"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Nothing was recorded.
    let (_, invoice) = send(&app, get(&format!("/api/invoices/{id}"))).await;
    assert_eq!(invoice["paid_amount"], "0");
    assert_eq!(invoice["status"], "Draft");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zero_and_negative_payments_are_refused_through_the_endpoint() {
    let db = db_or_skip!();
    let app = db.app();
    let (id, _) = create_invoice(&app).await;

    for amount in ["0", "-10"] {
        let (status, _) = send(
            &app,
            post(
                &format!("/api/invoices/{id}/pay"),
                None,
                json!({"method": "Cash", "amount": amount}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "amount {amount}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paying_an_unknown_invoice_is_404() {
    let db = db_or_skip!();
    let (status, _) = send(
        &db.app(),
        post(
            "/api/invoices/POS-2627-999999/pay",
            None,
            json!({"method": "Cash", "amount": "10"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ===========================================================================
// 5. Consolidation
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consolidate_moves_paid_to_consolidated_without_touching_money() {
    let db = db_or_skip!();
    let app = db.app();
    let (id, due) = create_invoice(&app).await;
    send(
        &app,
        post(
            &format!("/api/invoices/{id}/pay"),
            None,
            json!({"method": "Cash", "amount": due}),
        ),
    )
    .await;

    let (_, before) = send(&app, get(&format!("/api/invoices/{id}"))).await;
    let (status, after) = send(
        &app,
        post(&format!("/api/invoices/{id}/consolidate"), None, json!({})),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(after["status"], "Consolidated");
    assert_eq!(after["rounded_total"], before["rounded_total"]);
    assert_eq!(after["grand_total"], before["grand_total"]);
    assert_eq!(after["tax"], before["tax"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consolidating_a_draft_invoice_is_a_conflict() {
    let db = db_or_skip!();
    let app = db.app();
    let (id, _) = create_invoice(&app).await;

    let (status, _) = send(
        &app,
        post(&format!("/api/invoices/{id}/consolidate"), None, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consolidating_twice_is_idempotent() {
    let db = db_or_skip!();
    let app = db.app();
    let (id, due) = create_invoice(&app).await;
    send(
        &app,
        post(
            &format!("/api/invoices/{id}/pay"),
            None,
            json!({"method": "Cash", "amount": due}),
        ),
    )
    .await;

    let uri = format!("/api/invoices/{id}/consolidate");
    let (first_status, first) = send(&app, post(&uri, None, json!({}))).await;
    let (second_status, second) = send(&app, post(&uri, None, json!({}))).await;

    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(first, second, "a retried end-of-day job must be a no-op");
}

// ===========================================================================
// 6. Listing
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_list_endpoint_filters_and_sums_revenue_from_postgres() {
    let db = db_or_skip!();
    let app = db.app();

    // T-01 paid, T-02 left Draft, both on business day 2026-07-28.
    let (paid_id, due) = create_invoice(&app).await;
    send(
        &app,
        post(
            &format!("/api/invoices/{paid_id}/pay"),
            None,
            json!({"method": "Cash", "amount": due}),
        ),
    )
    .await;

    let mut second = create_body();
    second["table"] = json!("T-02");
    second["posted_at"] = json!("2026-07-28T15:30:00Z");
    send(&app, post("/api/invoices", Some(Uuid::new_v4()), second)).await;

    let (status, all) = send(&app, get("/api/invoices")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all["count"], 2);
    // Revenue is `rounded_total` over PosInvoiceStatus::REVENUE — the single definition
    // shift close and the P&L share (bugs 3 and 4). The Draft invoice is excluded.
    assert_eq!(all["total_revenue"], "378");

    // Newest first.
    let posted: Vec<&str> = all["invoices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["posted_at"].as_str().unwrap())
        .collect();
    let mut sorted = posted.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(posted, sorted);

    // Status filter.
    let (_, paid_only) = send(&app, get("/api/invoices?status=Paid")).await;
    assert_eq!(paid_only["count"], 1);
    assert_eq!(paid_only["invoices"][0]["invoice_id"], paid_id);

    // Table filter.
    let (_, t2) = send(&app, get("/api/invoices?table=T-02")).await;
    assert_eq!(t2["count"], 1);
    assert_eq!(t2["invoices"][0]["table"], "T-02");
    assert_eq!(t2["total_revenue"], "0", "a Draft invoice is not revenue yet");

    // Business-day range, inclusive on both ends.
    let (_, one_day) = send(&app, get("/api/invoices?from=2026-07-28&to=2026-07-28")).await;
    assert_eq!(one_day["count"], 2);

    let (_, other_day) = send(&app, get("/api/invoices?from=2026-07-29&to=2026-07-29")).await;
    assert_eq!(other_day["count"], 0);

    // Junk status is still rejected before it reaches SQL.
    let (status, _) = send(&app, get("/api/invoices?status=settled")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ===========================================================================
// 7. KOT endpoints
// ===========================================================================

fn kot_body() -> Value {
    json!({
        "invoice": "POS-2627-000001",
        "branch": BRANCH,
        "naming_series": "KOT-",
        "date": "2026-07-28",
        "room": ROOM,
        "restaurant_table": "T-01",
        "items": [
            {"item_code": "CURRY", "item_name": "Chicken Curry", "qty": "2"},
            {"item_code": "NAAN", "item_name": "Butter Naan", "qty": "4"},
            {"item_code": "CHAI", "item_name": "Masala Chai", "qty": "2"}
        ]
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generate_routes_to_the_real_stations_and_persists_the_tickets() {
    let db = db_or_skip!();
    let app = db.app();

    let (status, json) = send(&app, post("/api/kot/generate", None, kot_body())).await;
    assert_eq!(status, StatusCode::OK, "generate failed: {json}");

    let kots = json["kots"].as_array().expect("kots array");
    assert_eq!(kots.len(), 2, "two stations have work");

    let kitchen = kots
        .iter()
        .find(|k| k["production"] == "Hot Kitchen")
        .expect("Hot Kitchen ticket");
    let bar = kots
        .iter()
        .find(|k| k["production"] == "Bar")
        .expect("Bar ticket");

    // FIX BUG 1: each station's ticket carries only its own items.
    let kitchen_items: Vec<&str> = kitchen["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item"].as_str().unwrap())
        .collect();
    assert_eq!(kitchen_items, vec!["CURRY", "NAAN"]);

    let bar_items: Vec<&str> = bar["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item"].as_str().unwrap())
        .collect();
    assert_eq!(bar_items, vec!["CHAI"]);

    // Both got a real name from the sequence, and both were persisted.
    for kot in kots {
        assert!(
            kot["id"].as_str().unwrap().starts_with("KOT-"),
            "the sequence must assign a name: {kot}"
        );
        assert_eq!(kot["kot_type"], "New Order");
    }

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM kots")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 2);

    let items: i64 = sqlx::query_scalar("SELECT count(*) FROM kot_items")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(items, 3, "three lines across the two tickets");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_generate_flips_only_the_stations_that_already_printed() {
    let db = db_or_skip!();
    let app = db.app();

    // First pass: both stations print.
    send(&app, post("/api/kot/generate", None, kot_body())).await;

    // Second pass, drinks only: the Bar has printed, so its ticket is a modification.
    let mut drinks = kot_body();
    drinks["items"] = json!([{"item_code": "CHAI", "item_name": "Masala Chai", "qty": "1"}]);
    let (status, json) = send(&app, post("/api/kot/generate", None, drinks)).await;

    assert_eq!(status, StatusCode::OK);
    let kots = json["kots"].as_array().unwrap();
    assert_eq!(kots.len(), 1, "only the Bar has work");
    assert_eq!(kots[0]["production"], "Bar");
    assert_eq!(kots[0]["kot_type"], "Order Modified");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_that_routes_nowhere_is_reported_and_the_rest_still_goes() {
    let db = db_or_skip!();
    let app = db.app();

    sqlx::query("INSERT INTO items (code, name, item_group) VALUES ('PEN', 'Souvenir Pen', 'Retail')")
        .execute(db.pool())
        .await
        .unwrap();

    let mut body = kot_body();
    body["items"] = json!([
        {"item_code": "CURRY", "item_name": "Chicken Curry", "qty": "1"},
        {"item_code": "PEN", "item_name": "Souvenir Pen", "qty": "1"}
    ]);

    let (status, json) = send(&app, post("/api/kot/generate", None, body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["unrouted_items"], json!(["PEN"]));

    // One mis-configured item must not stop the table being fed.
    let kots = json["kots"].as_array().unwrap();
    assert_eq!(kots.len(), 1);
    assert_eq!(kots[0]["production"], "Hot Kitchen");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generate_validates_its_payload() {
    let db = db_or_skip!();
    let app = db.app();

    let cases: Vec<(&str, Value)> = vec![
        ("items", json!({"items": []})),
        ("branch", json!({"branch": "   "})),
        ("naming_series", json!({"naming_series": ""})),
        ("invoice", json!({"invoice": ""})),
        (
            "qty",
            json!({"items": [{"item_code": "CURRY", "item_name": "C", "qty": "0"}]}),
        ),
        (
            "item_code",
            json!({"items": [{"item_code": "", "item_name": "C", "qty": "1"}]}),
        ),
    ];

    for (field, patch) in cases {
        let mut body = kot_body();
        for (k, v) in patch.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (status, json) = send(&app, post("/api/kot/generate", None, body)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{field} must be validated, got {json}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_generated_kot_is_fetchable_by_id() {
    let db = db_or_skip!();
    let app = db.app();

    let (_, generated) = send(&app, post("/api/kot/generate", None, kot_body())).await;
    let id = generated["kots"][0]["id"].as_str().unwrap().to_owned();

    let (status, fetched) = send(&app, get(&format!("/api/kot/{id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["id"], id);
    assert_eq!(fetched["invoice"], "POS-2627-000001");
    assert!(!fetched["items"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_kot_is_404() {
    let db = db_or_skip!();
    let (status, _) = send(&db.app(), get("/api/kot/KOT-99999")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_kitchen_display_shows_pending_work_and_loses_it_once_prepared() {
    let db = db_or_skip!();
    let app = db.app();

    send(&app, post("/api/kot/generate", None, kot_body())).await;

    let (status, pending) = send(
        &app,
        get("/api/production-units/Hot%20Kitchen/pending-kots"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pending["production_unit"], "Hot Kitchen");
    assert_eq!(pending["kots"].as_array().unwrap().len(), 1);

    let id = pending["kots"][0]["id"].as_str().unwrap().to_owned();

    let (status, prepared) = send(
        &app,
        post(
            &format!("/api/kot/{id}/mark-prepared"),
            None,
            json!({"prepared_at": "14:30:00"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mark-prepared failed: {prepared}");
    assert_eq!(prepared["order_status"], "Prepared");
    assert_eq!(prepared["start_time_prep"], "14:30:00");

    // A finished ticket leaves the display, or the queue only ever grows.
    let (_, after) = send(
        &app,
        get("/api/production-units/Hot%20Kitchen/pending-kots"),
    )
    .await;
    assert!(after["kots"].as_array().unwrap().is_empty());

    // The Bar's ticket is untouched: marking one station's work done must not clear
    // another's.
    let (_, bar) = send(&app, get("/api/production-units/Bar/pending-kots")).await;
    assert_eq!(bar["kots"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mark_prepared_is_idempotent_through_the_endpoint() {
    let db = db_or_skip!();
    let app = db.app();

    let (_, generated) = send(&app, post("/api/kot/generate", None, kot_body())).await;
    let id = generated["kots"][0]["id"].as_str().unwrap().to_owned();
    let uri = format!("/api/kot/{id}/mark-prepared");

    let (first_status, first) = send(&app, post(&uri, None, json!({"prepared_at": "14:30:00"}))).await;
    let (second_status, second) = send(&app, post(&uri, None, json!({"prepared_at": "18:00:00"}))).await;

    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(
        first["start_time_prep"], second["start_time_prep"],
        "a double-tapped display must not move the service-time figure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn marking_an_unknown_kot_prepared_is_404() {
    let db = db_or_skip!();
    let (status, _) = send(
        &db.app(),
        post(
            "/api/kot/KOT-99999/mark-prepared",
            None,
            json!({"prepared_at": null}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_station_reports_an_empty_queue_not_an_error() {
    let db = db_or_skip!();
    let (status, json) = send(&db.app(), get("/api/production-units/Bar/pending-kots")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["production_unit"], "Bar");
    assert!(json["kots"].as_array().unwrap().is_empty());
}

// ===========================================================================
// 8. The full money path
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn order_to_kot_to_invoice_to_payment_to_consolidation() {
    // The end-to-end flow Phase 4A gate 3 asks for, with every figure crossing a real
    // database on the way.
    let db = db_or_skip!();
    let app = db.app();

    // 1. Invoice, with a gapless number.
    let (id, due) = create_invoice(&app).await;
    assert_eq!(id, "POS-2627-000001");
    assert_eq!(due, "378");

    // 2. Tickets to the kitchen, routed by real item groups.
    let mut kot = kot_body();
    kot["invoice"] = json!(id.clone());
    let (_, generated) = send(&app, post("/api/kot/generate", None, kot)).await;
    assert_eq!(generated["kots"].as_array().unwrap().len(), 2);

    // 3. The kitchen finishes its ticket.
    let kitchen_id = generated["kots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["production"] == "Hot Kitchen")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _) = send(
        &app,
        post(
            &format!("/api/kot/{kitchen_id}/mark-prepared"),
            None,
            json!({"prepared_at": "14:45:00"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 4. Split tender settles the bill.
    send(
        &app,
        post(
            &format!("/api/invoices/{id}/pay"),
            None,
            json!({"method": "Card", "amount": "300", "reference": "txn-1"}),
        ),
    )
    .await;
    let (_, settled) = send(
        &app,
        post(
            &format!("/api/invoices/{id}/pay"),
            None,
            json!({"method": "Cash", "amount": "78"}),
        ),
    )
    .await;
    assert_eq!(settled["status"], "Paid");
    assert_eq!(settled["outstanding_amount"], "0");

    // 5. End of day.
    let (_, consolidated) = send(
        &app,
        post(&format!("/api/invoices/{id}/consolidate"), None, json!({})),
    )
    .await;
    assert_eq!(consolidated["status"], "Consolidated");

    // The money never moved through any of it.
    assert_eq!(consolidated["grand_total"], "378.00");
    assert_eq!(consolidated["rounded_total"], "378");
    assert_eq!(consolidated["tax"]["total_tax"], "18.00");
    assert_eq!(consolidated["tax"]["cgst"], "9.00");
    assert_eq!(consolidated["tax"]["sgst"], "9.00");

    // And the series is still defensible.
    let repo = peacock_storage::repos::PgInvoiceRepo::new(db.storage.clone());
    assert!(repo.find_series_gaps("POS", "2627").await.unwrap().is_empty());
}

// ===========================================================================
// 9. There is no pool-less mode
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invoice_number_is_only_ever_issued_by_the_database() {
    // This replaces `without_storage_invoices_fall_back_to_memory_and_kot_reports_
    // unavailable`, which asserted the behaviour Lane W1-A exists to delete: that with no
    // `DATABASE_URL` the invoice endpoints answered 201 with `POS-2627-000001` out of a
    // `HashMap`. That number is a tax document identifier. Serving it from process memory
    // meant it was reissued after every restart, and no test that only read the response
    // body could tell the difference.
    //
    // The old test cannot be ported, because the state it constructed
    // (`app::build(Config::default())`, no storage) is no longer expressible — that is the
    // deliverable. What is checked instead is the property that replaced it: every number
    // on the wire came from the counter row, so the database's own gap check agrees with
    // what the API returned.
    let db = db_or_skip!();
    let app = db.app();

    let mut issued = Vec::new();
    for _ in 0..3 {
        let (status, json) =
            send(&app, post("/api/invoices", Some(Uuid::new_v4()), create_body())).await;
        assert_eq!(status, StatusCode::CREATED);
        issued.push(json["invoice_id"].as_str().unwrap().to_owned());
    }

    let repo = peacock_storage::repos::PgInvoiceRepo::new(db.storage.clone());
    let numbers = repo.issued_numbers("POS", "2627").await.unwrap();
    assert_eq!(numbers, vec![1, 2, 3], "the wire numbers must be the row's");
    assert!(repo.find_series_gaps("POS", "2627").await.unwrap().is_empty());

    // And each one is a committed row, not just a response.
    for name in &issued {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM invoices WHERE name = $1)")
                .bind(name)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(exists, "{name} was returned to a client but never stored");
    }
}
