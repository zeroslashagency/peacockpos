//! The order store's vocabulary: errors, records, and the mutation closure.
//!
//! # What used to be here, and why it is gone (Lane W1-A)
//!
//! This module held an `OrderStore` trait and an `InMemoryOrderStore` that implemented
//! it, alongside the Postgres implementation in [`crate::store::postgres_order`]. Both
//! are deleted.
//!
//! The in-memory store went because it was a *plausible* fake. It enforced real
//! idempotency and real per-order locking, so it passed every test the handlers had —
//! and that is precisely the problem. Within one process it looked correct; across two
//! processes, or across a restart, orders and their invoice numbers simply vanished, and
//! `AppState` would silently select it whenever `DATABASE_URL` was unset. A POS that
//! answers `201 Created` out of a `HashMap` reports success for takings it is about to
//! lose.
//!
//! The trait went with it for a simpler reason: with one implementor it bought nothing
//! and cost an `#[async_trait]` boxed future on every call, plus an `Arc<dyn OrderStore>`
//! in the state to describe a choice that no longer exists. [`AppState::orders`] now
//! returns the concrete [`crate::store::postgres_order::PostgresOrderStore`], and the
//! handlers are unchanged because they only ever called inherent-looking methods on it.
//!
//! What remains is the vocabulary the HTTP layer and the repository translate between:
//! [`StoreError`] (which `routes::orders` maps to status codes), [`OrderRecord`],
//! [`InvoiceRecord`] and [`Mutation`]. Keeping these here rather than in
//! `postgres_order` keeps `routes::orders` from importing the repository's module just to
//! name the error it handles.
//!
//! [`AppState::orders`]: crate::state::AppState::orders

use chrono::{DateTime, NaiveDate, Utc};

use peacock_core::ids::InvoiceName;
use peacock_core::model::UryOrderForm;
use peacock_core::money::Money;

use crate::dto::order::OrderStatus;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// What a store operation can refuse.
///
/// Mapped to HTTP by `crate::routes::orders`; kept separate from `ApiError` so the store
/// has no opinion about status codes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("order {0} not found")]
    NotFound(String),

    #[error("order {id} is {status} and cannot be modified")]
    NotModifiable { id: String, status: &'static str },

    #[error("order {id} already has invoice {invoice}")]
    AlreadyInvoiced { id: String, invoice: String },

    #[error("stale write on order {id}: caller saw version {expected}, current is {actual}")]
    VersionConflict {
        id: String,
        expected: u64,
        actual: u64,
    },

    /// The name the series would produce breaks CGST Rule 46(b)'s 16-character cap.
    #[error("invoice name {name:?} exceeds the {limit}-character limit of CGST Rule 46(b)")]
    InvoiceNameTooLong { name: String, limit: usize },

    #[error("{0}")]
    Invalid(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

/// The mutation handed to `PostgresOrderStore::modify`, run while the row lock is held.
///
/// `for<'a>` matters: without it the borrow of the form would be tied to the boxed
/// closure's own lifetime and the store could not touch the record afterwards to
/// recompute the total.
pub type Mutation = Box<dyn for<'a> FnOnce(&'a mut UryOrderForm) -> StoreResult<()> + Send>;

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// A stored order: the domain form plus the identity and lifecycle the API needs.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderRecord {
    pub id: String,
    pub status: OrderStatus,
    /// Incremented on every accepted write. The optimistic-concurrency token.
    pub version: u64,
    pub form: UryOrderForm,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
}

/// A created invoice, with the round-off split the P&L needs.
#[derive(Debug, Clone, PartialEq)]
pub struct InvoiceRecord {
    pub name: InvoiceName,
    pub order_id: String,
    pub grand_total: Money,
    pub rounded_total: Money,
    pub round_off: Money,
    pub fiscal_year: String,
    pub status: String,
    pub date: NaiveDate,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_not_modifiable_error_names_the_status_that_blocked_the_write() {
        // `routes::orders` maps these onto status codes by variant, and the message is
        // what the caller reads, so both halves are part of the contract.
        let err = StoreError::NotModifiable {
            id: "ORD-7".to_owned(),
            status: OrderStatus::Invoiced.as_str(),
        };
        assert_eq!(err.to_string(), "order ORD-7 is invoiced and cannot be modified");
    }

    #[test]
    fn a_version_conflict_reports_both_versions() {
        // A 409 that does not say what the current version is forces the client to guess.
        let err = StoreError::VersionConflict {
            id: "ORD-7".to_owned(),
            expected: 3,
            actual: 5,
        };
        assert_eq!(
            err.to_string(),
            "stale write on order ORD-7: caller saw version 3, current is 5"
        );
    }

    #[test]
    fn the_too_long_error_carries_the_rule_46b_limit() {
        let err = StoreError::InvoiceNameTooLong {
            name: "TOOLONG-2627-000001".to_owned(),
            limit: peacock_core::invoicing::MAX_INVOICE_NAME_LEN,
        };
        assert!(err.to_string().contains("16-character limit"));
    }
}
