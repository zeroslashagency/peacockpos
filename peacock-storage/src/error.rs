//! Storage errors, and the mapping from SQLSTATE to `peacock_core::error::Error`.
//!
//! The domain crate's `Error` is the vocabulary the HTTP layer branches on, so a
//! repository must not leak raw `sqlx::Error` upward for failures the domain already
//! has a word for. Two SQLSTATEs matter most:
//!
//! * `40001` serialization_failure — invoice and KOT numbering run at SERIALIZABLE
//!   (PHASE_2_3_PLAN.md Risk 3). A loser transaction is a retryable conflict, not a
//!   server fault, so it maps to `Error::Conflict`.
//! * `23505` unique_violation on an idempotency key — the caller replayed a request.
//!   Also `Error::Conflict`; the repository decides whether to return the existing row.
//!
//! Everything with no domain equivalent stays a `StorageError` and surfaces as a 500.

use peacock_core::error::Error as DomainError;
use sqlx::error::DatabaseError;

pub type StorageResult<T> = std::result::Result<T, StorageError>;

/// SQLSTATE codes this crate branches on.
pub mod sqlstate {
    pub const UNIQUE_VIOLATION: &str = "23505";
    pub const FOREIGN_KEY_VIOLATION: &str = "23503";
    pub const NOT_NULL_VIOLATION: &str = "23502";
    pub const CHECK_VIOLATION: &str = "23514";
    pub const SERIALIZATION_FAILURE: &str = "40001";
    pub const DEADLOCK_DETECTED: &str = "40P01";
    pub const LOCK_NOT_AVAILABLE: &str = "55P03";
    pub const QUERY_CANCELED: &str = "57014";
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("required configuration {0} is not set")]
    MissingConfig(&'static str),

    #[error("configuration {key} is invalid: {reason}")]
    InvalidConfig { key: &'static str, reason: String },

    #[error("could not connect to the database at {redacted_url}: {source}")]
    Connect {
        redacted_url: String,
        #[source]
        source: sqlx::Error,
    },

    #[error("database health check failed: {0}")]
    HealthCheck(#[source] sqlx::Error),

    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    /// A constraint the schema enforces was violated. Distinct from `Domain` because
    /// this one means the caller sent data the schema forbids, not that a business
    /// rule fired.
    #[error("constraint {constraint} violated on {table}: {message}")]
    Constraint {
        table: String,
        constraint: String,
        message: String,
    },

    /// The write lost a race and can be retried as-is.
    #[error("transaction conflict ({sqlstate}), safe to retry: {message}")]
    Retryable { sqlstate: String, message: String },

    /// A domain rule expressed in SQL (or recognised from a SQLSTATE) fired.
    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error("unexpected database error: {0}")]
    Sqlx(#[source] sqlx::Error),
}

impl StorageError {
    /// True when re-running the same transaction unchanged is a sensible response.
    pub fn is_retryable(&self) -> bool {
        matches!(self, StorageError::Retryable { .. })
    }

    /// The SQLSTATE behind this error, when there is one.
    ///
    /// Owned rather than borrowed: `sqlx` hands back a `Cow` built from the wire
    /// message, so there is no `&str` in `self` to lend out for the `Sqlx` arm.
    pub fn sqlstate(&self) -> Option<String> {
        match self {
            StorageError::Retryable { sqlstate, .. } => Some(sqlstate.clone()),
            StorageError::Sqlx(e) | StorageError::Connect { source: e, .. } => e
                .as_database_error()
                .and_then(|d| d.code())
                .map(|c| c.into_owned()),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for StorageError {
    fn from(err: sqlx::Error) -> Self {
        classify(err)
    }
}

/// Turn a `sqlx::Error` into the most specific `StorageError` available.
fn classify(err: sqlx::Error) -> StorageError {
    let Some(db) = err.as_database_error() else {
        return StorageError::Sqlx(err);
    };
    let Some(code) = db.code() else {
        return StorageError::Sqlx(err);
    };

    match code.as_ref() {
        sqlstate::SERIALIZATION_FAILURE | sqlstate::DEADLOCK_DETECTED => StorageError::Retryable {
            sqlstate: code.into_owned(),
            message: db.message().to_owned(),
        },
        sqlstate::UNIQUE_VIOLATION
        | sqlstate::FOREIGN_KEY_VIOLATION
        | sqlstate::CHECK_VIOLATION
        | sqlstate::NOT_NULL_VIOLATION => StorageError::Constraint {
            table: db.table().unwrap_or("<unknown>").to_owned(),
            constraint: db.constraint().unwrap_or("<unknown>").to_owned(),
            message: db.message().to_owned(),
        },
        _ => StorageError::Sqlx(err),
    }
}

/// Map a "row expected but absent" outcome onto the domain's own not-found variant.
///
/// `peacock_core::Error` has no generic `NotFound`: it has one variant per entity
/// (`TableNotFound`) and returns `Option` elsewhere, on purpose. Repositories therefore
/// pass in the constructor for their entity rather than this module guessing.
pub fn on_missing<T>(
    result: Result<Option<T>, sqlx::Error>,
    not_found: impl FnOnce() -> DomainError,
) -> StorageResult<T> {
    match result {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(StorageError::Domain(not_found())),
        Err(e) => Err(classify(e)),
    }
}

/// True when the failure is a unique violation on the named constraint — the signal
/// that an idempotency key was replayed.
pub fn is_unique_violation(err: &StorageError, constraint: &str) -> bool {
    matches!(
        err,
        StorageError::Constraint { constraint: c, .. } if c == constraint
    )
}

/// The `sqlx` trait object equivalent of [`classify`], for call sites holding a
/// `&dyn DatabaseError` rather than the owned error.
pub fn is_retryable_sqlstate(db: &dyn DatabaseError) -> bool {
    matches!(
        db.code().as_deref(),
        Some(sqlstate::SERIALIZATION_FAILURE) | Some(sqlstate::DEADLOCK_DETECTED)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_not_found_maps_to_the_domain_variant() {
        let missing: Result<Option<i32>, sqlx::Error> = Ok(None);
        let err = on_missing(missing, || {
            DomainError::TableNotFound(peacock_core::ids::TableName::from("T-01"))
        })
        .unwrap_err();

        match err {
            StorageError::Domain(DomainError::TableNotFound(t)) => assert_eq!(t.as_str(), "T-01"),
            other => panic!("expected domain TableNotFound, got {other:?}"),
        }
    }

    #[test]
    fn present_row_passes_through() {
        let found: Result<Option<i32>, sqlx::Error> = Ok(Some(7));
        assert_eq!(on_missing(found, || DomainError::NoActiveMenu).unwrap(), 7);
    }

    #[test]
    fn non_database_errors_are_not_reclassified() {
        let err = classify(sqlx::Error::RowNotFound);
        assert!(matches!(err, StorageError::Sqlx(_)));
        assert!(!err.is_retryable());
    }

    #[test]
    fn domain_errors_convert_without_losing_their_message() {
        let domain = DomainError::MultipleActiveOrders { count: 3 };
        let expected = domain.to_string();
        let wrapped: StorageError = domain.into();
        assert_eq!(wrapped.to_string(), expected);
    }
}
