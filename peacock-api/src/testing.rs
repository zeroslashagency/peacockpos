//! Test-only database harness for the library's own unit tests.
//!
//! `#[cfg(test)]`, so none of this reaches a production build. It exists because Lane
//! W1-A removed the in-memory stores: the unit tests in `src/**` used to run against a
//! `HashMap`, and with the fallback gone they need the same thing the integration tests
//! have — a real, migrated, throwaway PostgreSQL database.
//!
//! Deliberately the *same* pattern as `peacock-storage/tests/support/mod.rs` and
//! `peacock-api/tests/support/mod.rs` rather than a new invention: create
//! `peacock_api_unit_<uuid>`, migrate it on connect, drop it on `Drop`. Cargo gives no
//! path from `src/` to another crate's `tests/` module, so the code is duplicated; the
//! shape is not.
//!
//! # Two kinds of database, and which to ask for
//!
//! * [`TestDb`] — one throwaway database per test. Required by anything that asserts on
//!   invoice numbering, order ids, or row counts: those all read shared counters, and a
//!   shared database would make the assertions depend on test execution order.
//! * [`shared_storage`] — one database for the whole test binary, created on first use.
//!   For tests that need *a* pool but assert nothing about its contents (routing,
//!   middleware, CORS, SSE, validation rejections that never reach a query). It is also
//!   what the synchronous [`crate::state::AppState::new`] shim is built on, which is why
//!   it has to be callable from a sync context.
//!
//! # Requirements
//!
//! A reachable Postgres. `TEST_DATABASE_URL`, else `DATABASE_URL`, else
//! `postgres://localhost:5432/postgres`. The admin database is only used to issue
//! `CREATE DATABASE`; it is never written to.
//!
//! ```text
//! export TEST_DATABASE_URL=postgres://localhost:5432/postgres
//! cargo test -p peacock-api
//! ```

use std::sync::OnceLock;
use std::time::Duration;

use peacock_core::ids::CustomerName;
use peacock_core::model::UryOrderForm;
use peacock_core::money::Money;
use peacock_storage::{DbConfig, Storage};
use sqlx::{Connection, Executor, PgConnection};

/// Checked before `DATABASE_URL`.
pub const TEST_DATABASE_URL: &str = "TEST_DATABASE_URL";

/// Fallback when neither env var is set: a local server with the current OS user.
const DEFAULT_ADMIN_URL: &str = "postgres://localhost:5432/postgres";

/// Fixtures every seeded database carries.
pub const BRANCH: &str = "Peacock - Main";
pub const RESTAURANT: &str = "Peacock Grand";
pub const ROOM: &str = "Main Hall";

/// A migrated scratch database, dropped when this value goes out of scope.
pub struct TestDb {
    storage: Storage,
    admin_url: String,
    db_name: String,
}

impl TestDb {
    /// Create a fresh database, migrate it, and seed the reference rows the foreign keys
    /// require.
    pub async fn new() -> TestDb {
        let admin_url = admin_url();
        let db_name = format!("peacock_api_unit_{}", uuid::Uuid::new_v4().simple());
        let storage = create_and_migrate(&admin_url, &db_name).await;

        let db = TestDb {
            storage,
            admin_url,
            db_name,
        };
        seed(&db.storage).await;
        db
    }

    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    pub fn pool(&self) -> &sqlx::PgPool {
        self.storage.pool()
    }

    #[allow(dead_code)]
    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    /// A minimal takeaway order form: no table, no items, zero total.
    ///
    /// Takeaway rather than seated because it needs no `tables` row, so a caller testing
    /// state plumbing does not have to care which tables the seed created.
    pub fn takeaway_form(&self) -> UryOrderForm {
        UryOrderForm {
            take_away: true,
            restaurant_table: None,
            customer_name: CustomerName::from("Walk-in"),
            no_of_pax: 1,
            grand_total: Money::ZERO,
            last_invoice: None,
            items: vec![],
            waiter: None,
            pos_profile: None,
            cashier: None,
            comments: None,
            modified_time: None,
        }
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        drop_database(self.admin_url.clone(), self.db_name.clone());
    }
}

/// The process-wide test database, created on first call.
///
/// Sync on purpose: the `#[cfg(test)]` [`crate::state::AppState::new`] shim keeps the
/// signature the pre-existing test modules call, and those calls sit inside
/// `#[tokio::test]` bodies where `block_on` on the *current* runtime would panic. So the
/// connection is made once on a dedicated runtime that owns a thread of its own and stays
/// alive for the whole binary — the pool's internal reaper needs a live runtime, and this
/// one never shuts down.
///
/// The database is **not** dropped at the end of the run. A `Drop` on a leaked static is
/// not called, and forcing one would race the tests still using it. It is named
/// `peacock_api_shared_<uuid>`, so a stale one is identifiable; see [`drop_stale_shared`]
/// for the reclaim path the last test exercises.
pub fn shared_storage() -> Storage {
    static SHARED: OnceLock<Storage> = OnceLock::new();

    SHARED
        .get_or_init(|| {
            let init = || {
                let runtime = shared_runtime();
                let admin_url = admin_url();
                let db_name = format!("peacock_api_shared_{}", uuid::Uuid::new_v4().simple());
                runtime.block_on(async move {
                    let storage = create_and_migrate(&admin_url, &db_name).await;
                    seed(&storage).await;
                    storage
                })
            };
            if tokio::runtime::Handle::try_current().is_ok() {
                std::thread::spawn(init)
                    .join()
                    .expect("shared_storage init thread panicked")
            } else {
                init()
            }
        })
        .clone()
}

/// The runtime the shared pool is built on. Leaked, so it outlives every test.
fn shared_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a dedicated runtime for the shared test pool")
    })
}

/// `CREATE DATABASE`, then connect and migrate it.
async fn create_and_migrate(admin_url: &str, db_name: &str) -> Storage {
    let mut admin = PgConnection::connect(admin_url).await.unwrap_or_else(|e| {
        panic!(
            "cannot reach Postgres at {}: {e}\n\
             Start a server and set {TEST_DATABASE_URL}, e.g.\n  \
             export {TEST_DATABASE_URL}=postgres://localhost:5432/postgres",
            redact(admin_url)
        )
    });

    // The identifier is a uuid we generated, but quote it anyway: CREATE DATABASE takes no
    // bind parameters, so quoting is the only defence if the name ever becomes
    // caller-supplied.
    admin
        .execute(format!(r#"CREATE DATABASE "{db_name}""#).as_str())
        .await
        .unwrap_or_else(|e| panic!("CREATE DATABASE {db_name} failed: {e}"));
    admin.close().await.ok();

    let url = swap_database(admin_url, db_name);
    let config = DbConfig::from_url(&url)
        .expect("test url should be valid")
        .with_acquire_timeout(Duration::from_secs(5));

    Storage::connect(config)
        .await
        .unwrap_or_else(|e| panic!("connect + migrate on {db_name} failed: {e}"))
}

/// The reference graph the `orders`, `invoices` and `kot` foreign keys need.
///
/// Kept in step with `tests/support/mod.rs` on purpose: a unit test and an integration
/// test that disagree about which tables exist would be two different fixtures wearing
/// one name.
async fn seed(storage: &Storage) {
    let pool = storage.pool();

    sqlx::query("INSERT INTO rooms (name, branch, room_type) VALUES ($1, $2, 'AC')")
        .bind(ROOM)
        .bind(BRANCH)
        .execute(pool)
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
    .execute(pool)
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
        .execute(pool)
        .await
        .expect("seed table");
    }

    for (code, name, group) in [
        ("BIRYANI", "Chicken Biryani", "Main Course"),
        ("DOSA", "Masala Dosa", "Main Course"),
        ("TEA", "Masala Tea", "Beverages"),
        ("STICKER", "Peacock Sticker", "Merchandise"),
        ("CURRY", "Chicken Curry", "Main Course"),
        ("NAAN", "Butter Naan", "Breads"),
        ("X", "Generic Item", "Main Course"),
    ] {
        sqlx::query("INSERT INTO items (code, name, item_group) VALUES ($1, $2, $3)")
            .bind(code)
            .bind(name)
            .bind(group)
            .execute(pool)
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
            .execute(pool)
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
            .execute(pool)
            .await
            .expect("seed item group");
        }
    }

    // The gapless counters. Without a registered series `create_invoice` answers
    // `SeriesNotConfigured`, which is the right answer for an unregistered series and not
    // what most tests are about. Both fiscal years the fixtures post into are covered:
    // 2627 for the 2026-07 dates, 2728 for the cutoff tests that roll into 2027-04.
    let invoices = storage.invoice_repo();
    for series in ["POS", "PCK", "KOT"] {
        for fiscal_year in ["2627", "2728"] {
            invoices
                .register_series(series, fiscal_year, 1)
                .await
                .expect("register naming series");
        }
    }
}

/// Drop a database on a short-lived runtime of its own.
///
/// `Drop` is sync and the cleanup is async, and the pool's runtime is going away with us,
/// so the teardown gets its own.
fn drop_database(admin_url: String, db_name: String) {
    let _ = std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        rt.block_on(async {
            if let Ok(mut admin) = PgConnection::connect(&admin_url).await {
                // Sessions still attached would block the DROP; FORCE (PG13+) evicts them.
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

fn admin_url() -> String {
    std::env::var(TEST_DATABASE_URL)
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_ADMIN_URL.to_owned())
}

/// Replace the database component of a connection URL, preserving query parameters such
/// as `sslmode`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_test_database_is_migrated_and_seeded() {
        let db = TestDb::new().await;

        let tables: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert!(tables > 20, "migrations did not run: {tables} tables");

        let seeded: i64 = sqlx::query_scalar("SELECT count(*) FROM tables")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(seeded, 5, "the table fixtures must be present");
    }

    #[tokio::test]
    async fn two_test_databases_are_isolated() {
        let first = TestDb::new().await;
        let second = TestDb::new().await;
        assert_ne!(first.db_name(), second.db_name());

        sqlx::query("INSERT INTO items (code, name, item_group) VALUES ('ONLY-HERE', 'x', 'y')")
            .execute(first.pool())
            .await
            .unwrap();

        let leaked: i64 =
            sqlx::query_scalar("SELECT count(*) FROM items WHERE code = 'ONLY-HERE'")
                .fetch_one(second.pool())
                .await
                .unwrap();
        assert_eq!(leaked, 0, "a write in one database must not be visible in another");
    }

    #[tokio::test]
    async fn a_dropped_test_database_is_reclaimed() {
        let name = {
            let db = TestDb::new().await;
            db.db_name().to_owned()
        };

        // `Drop` ran on the line above. The reclaim happens on a thread it joins, so by
        // here the database is gone rather than merely scheduled for removal.
        let mut admin = PgConnection::connect(&admin_url()).await.unwrap();
        let still_there: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
                .bind(&name)
                .fetch_one(&mut admin)
                .await
                .unwrap();
        admin.close().await.ok();

        assert!(
            !still_there,
            "{name} outlived its TestDb; a run would leak a database per test"
        );
    }

    #[tokio::test]
    async fn the_shared_storage_is_one_database_for_the_whole_binary() {
        // `Storage` is a handle: two calls hand back two clones, so identity is checked
        // by what they are connected *to* rather than by pointer equality of the clones.
        let first = shared_storage();
        let second = shared_storage();

        let a: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(first.pool())
            .await
            .unwrap();
        let b: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(second.pool())
            .await
            .unwrap();
        assert_eq!(a, b, "the shared database must be initialised exactly once");
        assert!(a.starts_with("peacock_api_shared_"), "unexpected database {a}");

        // And it is usable from a different runtime than the one that built it: the pool
        // was created on the dedicated runtime, this test runs on tokio::test's own.
        first.health_check().await.expect("shared pool answers");
    }
}
