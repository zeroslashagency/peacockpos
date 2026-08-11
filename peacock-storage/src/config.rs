//! Database configuration, read from the environment.
//!
//! Only `DATABASE_URL` is required. Everything else has a default that is safe for a
//! single-terminal POS and tunable for a multi-terminal branch.

use std::env;
use std::time::Duration;

use crate::error::{StorageError, StorageResult};

/// The env var holding the connection string.
pub const DATABASE_URL: &str = "DATABASE_URL";

/// Pool sizing and timeout knobs.
///
/// The default `max_connections` follows the sizing rule in PHASE_2_3_PLAN.md
/// (Risk 2): `(2 × num_cpus) + effective_spindle_count`, with the spindle count taken
/// as 1 for SSD-backed storage.
#[derive(Clone)]
pub struct DbConfig {
    /// Postgres connection string. Never logged — it carries the password.
    url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    /// How long `acquire()` waits before giving up. A POS request that cannot get a
    /// connection in this window should fail loudly rather than hang the till.
    pub acquire_timeout: Duration,
    /// Idle connections are recycled so a Postgres restart does not leave the pool
    /// holding dead sockets.
    pub idle_timeout: Option<Duration>,
    pub max_lifetime: Option<Duration>,
    /// Statement timeout applied to every session via `SET statement_timeout`.
    /// Guards against a runaway report query pinning a connection.
    pub statement_timeout: Option<Duration>,
    /// Run pending migrations when the pool is built.
    pub run_migrations: bool,
}

impl DbConfig {
    /// Read config from the environment.
    ///
    /// Fails when `DATABASE_URL` is missing or blank. Optional overrides:
    /// `PEACOCK_DB_MAX_CONNECTIONS`, `PEACOCK_DB_MIN_CONNECTIONS`,
    /// `PEACOCK_DB_ACQUIRE_TIMEOUT_SECS`, `PEACOCK_DB_IDLE_TIMEOUT_SECS`,
    /// `PEACOCK_DB_MAX_LIFETIME_SECS`, `PEACOCK_DB_STATEMENT_TIMEOUT_SECS`,
    /// `PEACOCK_DB_RUN_MIGRATIONS`.
    ///
    /// A timeout var set to `0` disables that timeout. `acquire_timeout` is the one
    /// exception: 0 there would mean "fail instantly", so it is rejected.
    pub fn from_env() -> StorageResult<Self> {
        let url = env::var(DATABASE_URL)
            .ok()
            .filter(|u| !u.trim().is_empty())
            .ok_or_else(|| StorageError::MissingConfig(DATABASE_URL))?;

        Ok(DbConfig {
            url,
            max_connections: env_u32("PEACOCK_DB_MAX_CONNECTIONS")?
                .unwrap_or_else(default_max_connections),
            min_connections: env_u32("PEACOCK_DB_MIN_CONNECTIONS")?.unwrap_or(1),
            acquire_timeout: env_duration("PEACOCK_DB_ACQUIRE_TIMEOUT_SECS")?
                .flatten()
                .unwrap_or(Duration::from_secs(10)),
            idle_timeout: env_duration("PEACOCK_DB_IDLE_TIMEOUT_SECS")?
                .unwrap_or(Some(Duration::from_secs(600))),
            max_lifetime: env_duration("PEACOCK_DB_MAX_LIFETIME_SECS")?
                .unwrap_or(Some(Duration::from_secs(1800))),
            statement_timeout: env_duration("PEACOCK_DB_STATEMENT_TIMEOUT_SECS")?
                .unwrap_or(Some(Duration::from_secs(30))),
            run_migrations: env_bool("PEACOCK_DB_RUN_MIGRATIONS")?.unwrap_or(true),
        })
    }

    /// Build a config directly from a URL, taking defaults for everything else.
    /// Used by tests and by callers that get the URL from somewhere other than env.
    pub fn from_url(url: impl Into<String>) -> StorageResult<Self> {
        let url = url.into();
        if url.trim().is_empty() {
            return Err(StorageError::MissingConfig(DATABASE_URL));
        }
        Ok(DbConfig {
            url,
            max_connections: default_max_connections(),
            min_connections: 1,
            acquire_timeout: Duration::from_secs(10),
            idle_timeout: Some(Duration::from_secs(600)),
            max_lifetime: Some(Duration::from_secs(1800)),
            statement_timeout: Some(Duration::from_secs(30)),
            run_migrations: true,
        })
    }

    /// The connection string. Kept behind a method so it is never picked up by
    /// `#[derive(Debug)]` output or a struct-wide serialisation.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Host/port/database with the credentials stripped, for logs and errors.
    pub fn redacted_url(&self) -> String {
        redact_url(&self.url)
    }

    pub fn with_max_connections(mut self, n: u32) -> Self {
        self.max_connections = n;
        self
    }

    pub fn with_min_connections(mut self, n: u32) -> Self {
        self.min_connections = n;
        self
    }

    pub fn with_acquire_timeout(mut self, d: Duration) -> Self {
        self.acquire_timeout = d;
        self
    }

    pub fn with_statement_timeout(mut self, d: Option<Duration>) -> Self {
        self.statement_timeout = d;
        self
    }

    pub fn with_run_migrations(mut self, yes: bool) -> Self {
        self.run_migrations = yes;
        self
    }

    /// Reject combinations the pool would either refuse or silently mangle.
    pub(crate) fn validate(&self) -> StorageResult<()> {
        if self.max_connections == 0 {
            return Err(StorageError::InvalidConfig {
                key: "PEACOCK_DB_MAX_CONNECTIONS",
                reason: "must be at least 1".into(),
            });
        }
        if self.min_connections > self.max_connections {
            return Err(StorageError::InvalidConfig {
                key: "PEACOCK_DB_MIN_CONNECTIONS",
                reason: format!(
                    "min_connections ({}) exceeds max_connections ({})",
                    self.min_connections, self.max_connections
                ),
            });
        }
        if self.acquire_timeout.is_zero() {
            return Err(StorageError::InvalidConfig {
                key: "PEACOCK_DB_ACQUIRE_TIMEOUT_SECS",
                reason: "must be greater than zero".into(),
            });
        }
        Ok(())
    }
}

/// Hand-written so the password cannot reach a log line through a `{:?}` of the config.
/// A derived `Debug` prints `url` verbatim, which is exactly the leak this avoids.
impl std::fmt::Debug for DbConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbConfig")
            .field("url", &self.redacted_url())
            .field("max_connections", &self.max_connections)
            .field("min_connections", &self.min_connections)
            .field("acquire_timeout", &self.acquire_timeout)
            .field("idle_timeout", &self.idle_timeout)
            .field("max_lifetime", &self.max_lifetime)
            .field("statement_timeout", &self.statement_timeout)
            .field("run_migrations", &self.run_migrations)
            .finish()
    }
}

/// `(2 × num_cpus) + 1`, clamped to at least 5 so a single-core box still has headroom
/// for the API, the KDS stream and a background report at once.
fn default_max_connections() -> u32 {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(2);
    ((2 * cpus) + 1).max(5)
}

/// `None` when unset. `Some(None)` when explicitly `0`, meaning "no timeout".
fn env_duration(key: &'static str) -> StorageResult<Option<Option<Duration>>> {
    match env_u64(key)? {
        None => Ok(None),
        Some(0) => Ok(Some(None)),
        Some(secs) => Ok(Some(Some(Duration::from_secs(secs)))),
    }
}

fn env_u32(key: &'static str) -> StorageResult<Option<u32>> {
    read_env(key)?
        .map(|raw| {
            raw.parse::<u32>().map_err(|_| StorageError::InvalidConfig {
                key,
                reason: format!("expected a non-negative integer, got {raw:?}"),
            })
        })
        .transpose()
}

fn env_u64(key: &'static str) -> StorageResult<Option<u64>> {
    read_env(key)?
        .map(|raw| {
            raw.parse::<u64>().map_err(|_| StorageError::InvalidConfig {
                key,
                reason: format!("expected a non-negative integer of seconds, got {raw:?}"),
            })
        })
        .transpose()
}

fn env_bool(key: &'static str) -> StorageResult<Option<bool>> {
    let Some(raw) = read_env(key)? else {
        return Ok(None);
    };
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        other => Err(StorageError::InvalidConfig {
            key,
            reason: format!("expected a boolean (true/false/1/0), got {other:?}"),
        }),
    }
}

fn read_env(key: &str) -> StorageResult<Option<String>> {
    Ok(env::var(key)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty()))
}

/// Strip userinfo from a connection string so it is safe to log.
///
/// Deliberately string-level rather than URL-parser based: a malformed URL must still
/// come out redacted, and a parser that rejects it would leave the caller formatting
/// the raw string instead.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "<redacted>".to_owned();
    };
    // Userinfo is everything before the LAST '@' preceding the first '/' of the path:
    // a password may itself contain '@'.
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    match authority.rfind('@') {
        Some(at) => format!("{scheme}://***@{}{}", &authority[at + 1..], path),
        None => format!("{scheme}://{authority}{path}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_credentials_from_url() {
        assert_eq!(
            redact_url("postgres://peacock:s3cret@db.internal:5432/peacock"),
            "postgres://***@db.internal:5432/peacock"
        );
        // A password containing '@' must not leak its tail.
        assert_eq!(
            redact_url("postgres://peacock:p@ss@db.internal:5432/peacock"),
            "postgres://***@db.internal:5432/peacock"
        );
        // No credentials to strip.
        assert_eq!(
            redact_url("postgres://localhost/peacock_test"),
            "postgres://localhost/peacock_test"
        );
        // Unparseable input still redacts rather than echoing the string.
        assert_eq!(redact_url("not-a-url"), "<redacted>");
    }

    #[test]
    fn debug_output_never_contains_the_password() {
        let cfg = DbConfig::from_url("postgres://peacock:s3cret@localhost/peacock").unwrap();
        let dumped = format!("{cfg:?}");
        assert!(
            !dumped.contains("s3cret"),
            "password leaked into Debug output: {dumped}"
        );
    }

    #[test]
    fn blank_url_is_rejected() {
        assert!(matches!(
            DbConfig::from_url("   "),
            Err(StorageError::MissingConfig(DATABASE_URL))
        ));
    }

    #[test]
    fn validate_rejects_impossible_pool_sizes() {
        let base = DbConfig::from_url("postgres://localhost/x").unwrap();

        assert!(base.clone().with_max_connections(0).validate().is_err());
        assert!(base
            .clone()
            .with_max_connections(2)
            .with_min_connections(5)
            .validate()
            .is_err());
        assert!(base
            .clone()
            .with_acquire_timeout(Duration::ZERO)
            .validate()
            .is_err());
        assert!(base.validate().is_ok());
    }

    #[test]
    fn default_pool_size_follows_the_cpu_rule() {
        let n = default_max_connections();
        assert!(n >= 5, "pool floor not applied: {n}");
        let cpus = std::thread::available_parallelism()
            .map(|c| c.get() as u32)
            .unwrap_or(2);
        assert_eq!(n, ((2 * cpus) + 1).max(5));
    }
}
