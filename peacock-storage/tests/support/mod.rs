//! Test harness: a throwaway database per test.
//!
//! Tests run in parallel by default, so they cannot share one database — a `DROP` in one
//! would pull the schema out from under another. Each [`TestDb`] creates
//! `peacock_test_<uuid>`, migrates it and drops it on `Drop`, which also proves the
//! migration set is repeatable rather than only working on a hand-prepared database.
//!
//! Requires a reachable Postgres. `TEST_DATABASE_URL` (or `DATABASE_URL`) points at any
//! database on the target server; it is used only to issue `CREATE DATABASE`, never
//! written to.

use std::time::Duration;

use peacock_storage::{DbConfig, Storage};
use sqlx::{Connection, Executor, PgConnection};

/// Env var for the admin connection, checked before `DATABASE_URL`.
pub const TEST_DATABASE_URL: &str = "TEST_DATABASE_URL";

/// Fallback when neither env var is set: a local server with the current OS user.
const DEFAULT_ADMIN_URL: &str = "postgres://localhost:5432/postgres";

pub struct TestDb {
    pub storage: Storage,
    admin_url: String,
    db_name: String,
}

impl TestDb {
    /// Create a fresh database and run every migration against it.
    pub async fn new() -> TestDb {
        Self::with_config(|c| c).await
    }

    /// Same, with a chance to tune the pool (used by the concurrency test).
    pub async fn with_config(tune: impl FnOnce(DbConfig) -> DbConfig) -> TestDb {
        let admin_url = admin_url();
        let db_name = format!("peacock_test_{}", uuid::Uuid::new_v4().simple());

        let mut admin = PgConnection::connect(&admin_url).await.unwrap_or_else(|e| {
            panic!(
                "cannot reach Postgres at {}: {e}\n\
                 Start a server and set {TEST_DATABASE_URL}, e.g.\n  \
                 export {TEST_DATABASE_URL}=postgres://localhost:5432/postgres",
                redact(&admin_url)
            )
        });

        // Identifier is a uuid we generated, but quote it anyway: CREATE DATABASE takes
        // no bind parameters, so quoting is the only thing standing between this and
        // injection if the name ever becomes caller-supplied.
        admin
            .execute(format!(r#"CREATE DATABASE "{db_name}""#).as_str())
            .await
            .unwrap_or_else(|e| panic!("CREATE DATABASE {db_name} failed: {e}"));
        admin.close().await.ok();

        let url = swap_database(&admin_url, &db_name);
        let config = tune(
            DbConfig::from_url(&url)
                .expect("test url should be valid")
                .with_acquire_timeout(Duration::from_secs(5)),
        );

        let storage = Storage::connect(config)
            .await
            .unwrap_or_else(|e| panic!("connect + migrate on {db_name} failed: {e}"));

        TestDb {
            storage,
            admin_url,
            db_name,
        }
    }

    pub fn pool(&self) -> &sqlx::PgPool {
        self.storage.pool()
    }

    /// `support` is compiled once per integration-test binary, so a helper only some of
    /// them use reads as dead code in the others. `schema.rs` asserts on this.
    #[allow(dead_code)]
    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    /// Insert the minimum graph a table row needs: a room and a restaurant.
    ///
    /// `#[allow(dead_code)]` for the same reason as `db_name` above: `support` is compiled
    /// once per integration-test binary, and the BOM/bundle tests seed neither.
    #[allow(dead_code)]
    pub async fn seed_restaurant_and_room(&self, restaurant: &str, room: &str, branch: &str) {
        sqlx::query("INSERT INTO rooms (name, branch, room_type) VALUES ($1, $2, 'AC')")
            .bind(room)
            .bind(branch)
            .execute(self.pool())
            .await
            .expect("seed room");

        sqlx::query(
            "INSERT INTO restaurants
                 (name, company, branch, pos_profile, invoice_series_prefix, default_room)
             VALUES ($1, 'Peacock Foods', $2, 'Peacock POS', 'PCK-', $3)",
        )
        .bind(restaurant)
        .bind(branch)
        .bind(room)
        .execute(self.pool())
        .await
        .expect("seed restaurant");
    }

    /// Drop the database. `Drop` does this too; exposed for a test that wants to close
    /// the pool and reclaim the database within the test body.
    #[allow(dead_code)]
    pub async fn cleanup(&self) {
        let Ok(mut admin) = PgConnection::connect(&self.admin_url).await else {
            return;
        };
        // Sessions still attached would block the DROP; FORCE (PG13+) evicts them.
        let _ = admin
            .execute(format!(r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#, self.db_name).as_str())
            .await;
        let _ = admin.close().await;
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // `Drop` is sync and the cleanup is async. The pool's own runtime is going away
        // with us, so the drop work gets its own short-lived one.
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
    std::env::var(TEST_DATABASE_URL)
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_ADMIN_URL.to_owned())
}

/// Replace the database component of a connection URL, preserving query parameters
/// such as `sslmode`.
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
