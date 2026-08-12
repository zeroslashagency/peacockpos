//! PostgreSQL storage layer for Peacock POS.
//!
//! This crate owns the connection pool, the migration set and the schema. The
//! repository implementations of the [`peacock_core::ports`] traits land here too, one
//! module per aggregate, added by the later Phase 2 lanes.
//!
//! # Why the pool is here and not in the API crate
//!
//! `peacock_core` is deliberately I/O-free and its port traits are synchronous, so the
//! async boundary has to live somewhere. It lives here: repositories hold a [`PgPool`],
//! do their own `block_on` or prefetch as they see fit, and the domain never learns
//! that a database exists.
//!
//! # Usage
//!
//! ```no_run
//! # async fn run() -> Result<(), peacock_storage::StorageError> {
//! use peacock_storage::{DbConfig, Storage};
//!
//! // DATABASE_URL from the environment, migrations applied on startup.
//! let storage = Storage::connect(DbConfig::from_env()?).await?;
//! storage.health_check().await?;
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod error;
pub mod repos;

pub use config::{DbConfig, DATABASE_URL};
pub use error::{StorageError, StorageResult};

use std::time::{Duration, Instant};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, Executor, PgPool, Postgres, Transaction};

/// The migration set, compiled into the binary.
///
/// Embedding it means a deployed binary can never drift from the schema it was built
/// against, and there is no `migrations/` directory to ship alongside it.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// A live connection pool plus the config it was built from.
#[derive(Clone)]
pub struct Storage {
    pool: PgPool,
    config: DbConfig,
}

impl Storage {
    /// Build the pool, verify it, and run pending migrations when
    /// [`DbConfig::run_migrations`] is set.
    ///
    /// Connections are established lazily by `sqlx`, so this eagerly acquires one to
    /// turn a bad URL or an unreachable server into an error here rather than inside
    /// the first request.
    pub async fn connect(config: DbConfig) -> StorageResult<Self> {
        config.validate()?;

        let mut connect_options: PgConnectOptions =
            config
                .url()
                .parse()
                .map_err(|e: sqlx::Error| StorageError::Connect {
                    redacted_url: config.redacted_url(),
                    source: e,
                })?;
        // sqlx logs every statement at INFO by default, which on a POS means the whole
        // day's traffic in the log. Drop it to DEBUG and keep only slow queries at WARN.
        connect_options = connect_options
            .log_statements(tracing::log::LevelFilter::Debug)
            .log_slow_statements(tracing::log::LevelFilter::Warn, Duration::from_secs(1));

        let statement_timeout_ms = config
            .statement_timeout
            .map(|d| d.as_millis().min(u32::MAX as u128) as u32);

        let mut opts = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .acquire_timeout(config.acquire_timeout)
            .idle_timeout(config.idle_timeout)
            .max_lifetime(config.max_lifetime)
            // A connection handed back from the pool is verified before reuse, so a
            // server restart surfaces as a fresh connection rather than a broken one.
            .test_before_acquire(true);

        if let Some(ms) = statement_timeout_ms {
            opts = opts.after_connect(move |conn, _meta| {
                Box::pin(async move {
                    // Not a bind parameter: SET does not accept one. The value is a u32
                    // formatted by us, never caller input, so there is nothing to inject.
                    conn.execute(format!("SET statement_timeout = {ms}").as_str())
                        .await?;
                    Ok(())
                })
            });
        }

        let pool =
            opts.connect_with(connect_options)
                .await
                .map_err(|source| StorageError::Connect {
                    redacted_url: config.redacted_url(),
                    source,
                })?;

        let storage = Storage { pool, config };

        // Fail fast: prove the credentials work before reporting success.
        storage.health_check().await?;

        if storage.config.run_migrations {
            storage.migrate().await?;
        }

        tracing::info!(
            target: "peacock_storage",
            url = %storage.config.redacted_url(),
            max_connections = storage.config.max_connections,
            "database pool ready"
        );

        Ok(storage)
    }

    /// Wrap a pool that was built elsewhere. No migrations, no health check.
    pub fn from_pool(pool: PgPool, config: DbConfig) -> Self {
        Storage { pool, config }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn config(&self) -> &DbConfig {
        &self.config
    }

    // -----------------------------------------------------------------------
    // Repository accessors
    // -----------------------------------------------------------------------
    //
    // Each returns a fresh handle rather than a cached one. A repository is a `Storage`
    // (or a `PgPool`) plus nothing: cloning one clones an `Arc` to the pool, so building
    // it per call costs an atomic increment and saves the API from threading nine fields
    // through its state. The pool — the thing that must be shared — is shared either way.

    /// Order forms and their carts (Lane 2H).
    pub fn order_repo(&self) -> repos::PgOrderRepo {
        repos::PgOrderRepo::new(self.clone())
    }

    /// POS Invoices: gapless numbering, idempotency, status transitions (Lane 2F).
    pub fn invoice_repo(&self) -> repos::PgInvoiceRepo {
        repos::PgInvoiceRepo::new(self.clone())
    }

    /// Kitchen order tickets (Lane 2E).
    pub fn kot_repo(&self) -> repos::PgKotRepo {
        repos::PgKotRepo::new(self.clone())
    }

    /// Shift open/close and Z-reports (Lane 2G).
    pub fn shift_repo(&self) -> repos::PostgresShiftRepo {
        repos::PostgresShiftRepo::new(self.clone())
    }

    /// Tables and the merge cluster (Lane 2B).
    pub fn table_repo(&self) -> repos::PostgresTableRepo {
        repos::PostgresTableRepo::new(self.pool.clone())
    }

    /// Menus and their items (Lane 2C).
    pub fn menu_repo(&self) -> repos::PgMenuRepo {
        repos::PgMenuRepo::new(self.pool.clone())
    }

    /// Price lists (Lane 2C).
    pub fn price_repo(&self) -> repos::PgPriceRepo {
        repos::PgPriceRepo::new(self.pool.clone())
    }

    /// Bills of material, for COGS (Lane 2D).
    pub fn bom_repo(&self) -> repos::PgBomRepo {
        repos::PgBomRepo::new(self.pool.clone())
    }

    /// Product bundles, for COGS (Lane 2D).
    pub fn bundle_repo(&self) -> repos::PgProductBundleRepo {
        repos::PgProductBundleRepo::new(self.pool.clone())
    }

    /// Aggregator orders (Lane W1-F).
    pub fn aggregator_repo(&self) -> repos::PgAggregatorRepo {
        repos::PgAggregatorRepo::new(self.pool.clone())
    }

    /// Apply every migration that has not run yet. Idempotent: sqlx records applied
    /// versions in `_sqlx_migrations` and skips them, and a checksum mismatch on an
    /// already-applied file is an error rather than a silent re-run.
    pub async fn migrate(&self) -> StorageResult<()> {
        let started = Instant::now();
        MIGRATOR.run(&self.pool).await?;
        tracing::info!(
            target: "peacock_storage",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "migrations applied"
        );
        Ok(())
    }

    /// Round-trip a trivial query.
    ///
    /// `SELECT 1` rather than a `ping`: it proves the credentials, the database and the
    /// session all work, which is what a load balancer's readiness probe needs to know.
    pub async fn health_check(&self) -> StorageResult<Health> {
        let started = Instant::now();
        let one: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .map_err(StorageError::HealthCheck)?;
        debug_assert_eq!(one, 1);

        Ok(Health {
            latency: started.elapsed(),
            pool_size: self.pool.size(),
            idle_connections: self.pool.num_idle(),
        })
    }

    /// Begin a transaction at the default isolation level (READ COMMITTED).
    pub async fn begin(&self) -> StorageResult<Transaction<'static, Postgres>> {
        self.pool.begin().await.map_err(StorageError::from)
    }

    /// Begin a SERIALIZABLE transaction.
    ///
    /// Required for gapless invoice and KOT numbering (PHASE_2_3_PLAN.md Risk 3). The
    /// caller must be ready to see [`StorageError::Retryable`] on commit and re-run the
    /// whole closure — see [`Storage::with_serializable_retry`].
    pub async fn begin_serializable(&self) -> StorageResult<Transaction<'static, Postgres>> {
        let mut tx = self.begin().await?;
        tx.execute("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
            .await?;
        Ok(tx)
    }

    /// Run `f` inside a SERIALIZABLE transaction, retrying on serialization failure.
    ///
    /// `f` is handed the transaction and must not commit it; this method commits on
    /// success and rolls back before each retry. Retries are immediate and capped at
    /// `max_attempts` — a POS write that cannot land in a handful of tries should
    /// surface to the operator rather than spin.
    pub async fn with_serializable_retry<T, F>(
        &self,
        max_attempts: u32,
        mut f: F,
    ) -> StorageResult<T>
    where
        F: for<'t> FnMut(
            &'t mut Transaction<'static, Postgres>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = StorageResult<T>> + Send + 't>,
        >,
    {
        let attempts = max_attempts.max(1);
        let mut last: Option<StorageError> = None;

        for attempt in 1..=attempts {
            let mut tx = self.begin_serializable().await?;
            match f(&mut tx).await {
                Ok(value) => match tx.commit().await {
                    Ok(()) => return Ok(value),
                    Err(e) => {
                        let err = StorageError::from(e);
                        if !err.is_retryable() || attempt == attempts {
                            return Err(err);
                        }
                        tracing::warn!(
                            target: "peacock_storage",
                            attempt,
                            "serialization failure on commit, retrying"
                        );
                        last = Some(err);
                    }
                },
                Err(err) => {
                    let _ = tx.rollback().await;
                    if !err.is_retryable() || attempt == attempts {
                        return Err(err);
                    }
                    tracing::warn!(
                        target: "peacock_storage",
                        attempt,
                        "serialization failure in transaction body, retrying"
                    );
                    last = Some(err);
                }
            }
        }

        Err(last.unwrap_or(StorageError::Retryable {
            sqlstate: error::sqlstate::SERIALIZATION_FAILURE.to_owned(),
            message: "exhausted serializable retries".to_owned(),
        }))
    }

    /// Close the pool, waiting for in-flight queries to finish.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// `Debug` is hand-written so the connection string cannot reach a log line through it.
impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage")
            .field("url", &self.config.redacted_url())
            .field("max_connections", &self.config.max_connections)
            .field("pool_size", &self.pool.size())
            .field("idle", &self.pool.num_idle())
            .finish()
    }
}

/// What a health check observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    pub latency: Duration,
    pub pool_size: u32,
    pub idle_connections: usize,
}

/// Convenience: read `DATABASE_URL`, connect, migrate.
pub async fn connect_from_env() -> StorageResult<Storage> {
    Storage::connect(DbConfig::from_env()?).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrator_contains_the_core_tables_migration() {
        let versions: Vec<_> = MIGRATOR
            .iter()
            .map(|m| (m.version, m.description.to_string()))
            .collect();
        assert!(
            versions.iter().any(|(v, d)| *v == 1 && d == "core tables"),
            "001_core_tables.sql missing from the embedded migrator: {versions:?}"
        );
    }

    #[test]
    fn migrator_contains_the_users_migration() {
        // 012_users.sql must be embedded — without it every auth test gets "relation users does not exist".
        let versions: Vec<_> = MIGRATOR
            .iter()
            .map(|m| (m.version, m.description.to_string()))
            .collect();
        assert!(
            versions.iter().any(|(v, d)| *v == 12 && d == "users"),
            "012_users.sql missing from the embedded migrator: {versions:?}"
        );
    }

    #[test]
    fn migrations_are_uniquely_versioned_and_ordered() {
        let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        let mut sorted = versions.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            versions, sorted,
            "migration versions must be unique and ascending"
        );
    }

    #[test]
    fn debug_output_never_contains_the_password() {
        // Storage cannot be built without a server, so this covers the same guarantee
        // through the config the Debug impl delegates to.
        let cfg = DbConfig::from_url("postgres://peacock:s3cret@localhost/peacock").unwrap();
        assert!(!cfg.redacted_url().contains("s3cret"));
    }
}
