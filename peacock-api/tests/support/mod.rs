//! Test harness for the API integration tests: a throwaway database plus a booted app.
//!
//! Mirrors `peacock-storage/tests/support/mod.rs` — a fresh `peacock_api_test_<uuid>` per
//! test, migrated on connect and dropped on `Drop`. Tests run in parallel, so a shared
//! database would let one test's teardown pull the schema out from under another, and a
//! shared *series counter* would make every gapless-numbering assertion depend on test
//! ordering.
//!
//! Requires a reachable Postgres. `TEST_DATABASE_URL` (or `DATABASE_URL`) points at any
//! database on the target server; it is used only to issue `CREATE DATABASE`.
//!
//! ```text
//! export TEST_DATABASE_URL=postgres://localhost:5432/postgres
//! cargo test -p peacock-api
//! ```

#![allow(dead_code)] // `support` is compiled once per test binary; not every one uses all of it.

use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use peacock_api::config::Config;
use peacock_storage::{DbConfig, Storage};
use serde_json::Value;
use sqlx::{Connection, Executor, PgConnection};
use tower::ServiceExt;

pub const TEST_DATABASE_URL: &str = "TEST_DATABASE_URL";
const DEFAULT_ADMIN_URL: &str = "postgres://localhost:5432/postgres";

/// The default naming series and branch the fixtures register.
pub const SERIES: &str = "PCK";
pub const BRANCH: &str = "Peacock - Main";
pub const RESTAURANT: &str = "Peacock Grand";
pub const ROOM: &str = "Main Hall";
/// Fiscal year code for [`BUSINESS_DATE`]: 2026-07-31 falls in FY 2026-27.
pub const FISCAL_YEAR: &str = "2627";
pub const BUSINESS_DATE: &str = "2026-07-31";

/// A migrated scratch database and the app built on top of it.
pub struct TestApp {
    pub storage: Storage,
    pub app: Router,
    admin_url: String,
    db_name: String,
}

impl TestApp {
    /// Create a database, migrate it, seed the reference data the FKs need, and build the
    /// production router over it.
    pub async fn new() -> TestApp {
        let admin_url = admin_url();
        let db_name = format!("peacock_api_test_{}", uuid::Uuid::new_v4().simple());

        let mut admin = PgConnection::connect(&admin_url).await.unwrap_or_else(|e| {
            panic!(
                "cannot reach Postgres at {}: {e}\n\
                 Start a server and set {TEST_DATABASE_URL}, e.g.\n  \
                 export {TEST_DATABASE_URL}=postgres://localhost:5432/postgres",
                redact(&admin_url)
            )
        });

        // The identifier is a uuid we generated, but quote it anyway: CREATE DATABASE takes
        // no bind parameters, so quoting is the only defence if the name ever becomes
        // caller-supplied.
        admin
            .execute(format!(r#"CREATE DATABASE "{db_name}""#).as_str())
            .await
            .unwrap_or_else(|e| panic!("CREATE DATABASE {db_name} failed: {e}"));
        admin.close().await.ok();

        let url = swap_database(&admin_url, &db_name);
        let config = DbConfig::from_url(&url)
            .expect("test url should be valid")
            .with_acquire_timeout(Duration::from_secs(5));

        let storage = Storage::connect(config)
            .await
            .unwrap_or_else(|e| panic!("connect + migrate on {db_name} failed: {e}"));

        let app = peacock_api::build_with_storage(test_config(), storage.clone());

        let harness = TestApp {
            storage,
            app,
            admin_url,
            db_name,
        };
        harness.seed().await;
        harness
    }

    pub fn pool(&self) -> &sqlx::PgPool {
        self.storage.pool()
    }

    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    /// The minimum graph the `orders` and `invoices` FKs require, plus the naming series.
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

        for table in ["T-01", "T-02", "T-03", "T-04", "T-05"] {
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
            ("BIRYANI", "Chicken Biryani", "Main Course"),
            ("DOSA", "Masala Dosa", "Main Course"),
            ("TEA", "Masala Tea", "Beverages"),
            ("STICKER", "Peacock Sticker", "Merchandise"),
        ] {
            sqlx::query("INSERT INTO items (code, name, item_group) VALUES ($1, $2, $3)")
                .bind(code)
                .bind(name)
                .bind(group)
                .execute(self.pool())
                .await
                .expect("seed item");
        }

        // The gapless counter. `create_invoice` returns `SeriesNotConfigured` without it,
        // which is the correct answer for an unregistered series and not what these tests
        // are about.
        self.storage
            .invoice_repo()
            .register_series(SERIES, FISCAL_YEAR, 1)
            .await
            .expect("register naming series");
    }

    // -- HTTP helpers ------------------------------------------------------

    /// Drive one request through the full middleware stack.
    pub async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, json)
    }

    pub async fn post(&self, uri: &str, body: &Value) -> (StatusCode, Value) {
        self.send(json_request("POST", uri, body, None)).await
    }

    pub async fn post_with_key(
        &self,
        uri: &str,
        body: &Value,
        key: &str,
    ) -> (StatusCode, Value) {
        self.send(json_request("POST", uri, body, Some(key))).await
    }

    pub async fn patch(&self, uri: &str, body: &Value) -> (StatusCode, Value) {
        self.send(json_request("PATCH", uri, body, None)).await
    }

    pub async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.send(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
    }

    pub async fn delete(&self, uri: &str) -> (StatusCode, Value) {
        self.send(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    // -- Row-level assertions ---------------------------------------------
    //
    // The point of this lane: an endpoint answering 201 proves nothing if the row is not
    // there. Every write test checks the database directly as well as the response.

    pub async fn order_count(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM orders")
            .fetch_one(self.pool())
            .await
            .expect("count orders")
    }

    pub async fn order_item_count(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM order_items")
            .fetch_one(self.pool())
            .await
            .expect("count order_items")
    }

    pub async fn invoice_count(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM invoices")
            .fetch_one(self.pool())
            .await
            .expect("count invoices")
    }

    pub async fn invoice_names(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT name FROM invoices ORDER BY series_number")
            .fetch_all(self.pool())
            .await
            .expect("list invoices")
    }

    /// Every issued number for the default series, ascending. Gaplessness reads this.
    pub async fn issued_numbers(&self) -> Vec<u64> {
        self.storage
            .invoice_repo()
            .issued_numbers(SERIES, FISCAL_YEAR)
            .await
            .expect("issued numbers")
    }

    pub async fn series_gaps(&self) -> Vec<u64> {
        self.storage
            .invoice_repo()
            .find_series_gaps(SERIES, FISCAL_YEAR)
            .await
            .expect("series gaps")
    }

    /// The raw row behind a wire id, so a test can assert on columns the API does not
    /// expose (`cancelled_at`, `version`, `last_invoice`).
    pub async fn order_row(&self, wire_id: &str) -> OrderRow {
        let id: i64 = wire_id
            .strip_prefix("ORD-")
            .and_then(|d| d.parse().ok())
            .unwrap_or_else(|| panic!("{wire_id} is not a wire order id"));

        sqlx::query_as::<_, OrderRow>(
            "SELECT id, version, customer_name, no_of_pax, grand_total, last_invoice,
                    restaurant_table, take_away, cancelled_at, cancel_reason
             FROM orders WHERE id = $1",
        )
        .bind(id)
        .fetch_one(self.pool())
        .await
        .unwrap_or_else(|e| panic!("order row {wire_id}: {e}"))
    }

    pub async fn cleanup(&self) {
        let Ok(mut admin) = PgConnection::connect(&self.admin_url).await else {
            return;
        };
        let _ = admin
            .execute(
                format!(r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#, self.db_name).as_str(),
            )
            .await;
        let _ = admin.close().await;
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        // `Drop` is sync and the cleanup is async, and the pool's runtime is going away
        // with us, so the teardown gets its own short-lived one.
        let admin_url = self.admin_url.clone();
        let db_name = self.db_name.clone();
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async {
                if let Ok(mut admin) = PgConnection::connect(&admin_url).await {
                    let _ = admin
                        .execute(
                            format!(r#"DROP DATABASE IF EXISTS "{db_name}" WITH (FORCE)"#)
                                .as_str(),
                        )
                        .await;
                    let _ = admin.close().await;
                }
            });
        })
        .join();
    }
}

/// An `orders` row, for assertions the HTTP response cannot make.
#[derive(Debug, sqlx::FromRow)]
pub struct OrderRow {
    pub id: i64,
    pub version: i64,
    pub customer_name: String,
    pub no_of_pax: i32,
    pub grand_total: rust_decimal::Decimal,
    pub last_invoice: Option<String>,
    pub restaurant_table: Option<String>,
    pub take_away: bool,
    pub cancelled_at: Option<chrono::DateTime<chrono::Utc>>,
    pub cancel_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

pub fn test_config() -> Config {
    Config {
        cors_allowed_origins: vec!["https://pos.vercel.app".to_string()],
        ..Config::default()
    }
}

/// An order on T-01: 2 biryani at 250 and 2 tea at 20 → 540.
pub fn order_body(table: &str) -> Value {
    serde_json::json!({
        "restaurant_table": table,
        "customer_name": "Walk-in",
        "no_of_pax": 2,
        "items": [
            {"item": "BIRYANI", "item_name": "Chicken Biryani", "qty": 2, "rate": 250},
            {"item": "TEA", "item_name": "Masala Tea", "qty": 2, "rate": 20}
        ]
    })
}

pub fn invoice_body() -> Value {
    serde_json::json!({
        "series": SERIES,
        "date": BUSINESS_DATE,
        "branch": BRANCH,
        "room": ROOM
    })
}

fn json_request(method: &str, uri: &str, body: &Value, key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    builder
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn admin_url() -> String {
    std::env::var(TEST_DATABASE_URL)
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_ADMIN_URL.to_owned())
}

/// Replace the database component of a URL, preserving query parameters such as `sslmode`.
fn swap_database(url: &str, db: &str) -> String {
    let (scheme, rest) = url.split_once("://").expect("url should have a scheme");
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let query = tail.find('?').map(|q| &tail[q..]).unwrap_or("");
    format!("{scheme}://{authority}/{db}{query}")
}

fn redact(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => match rest.rfind('@') {
            Some(at) => format!("{scheme}://***@{}", &rest[at + 1..]),
            None => url.to_owned(),
        },
        None => "<redacted>".to_owned(),
    }
}
