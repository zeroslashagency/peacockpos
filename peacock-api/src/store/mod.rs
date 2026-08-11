//! The HTTP layer's order store: one implementation, over PostgreSQL.
//!
//! ## Why this layer exists
//!
//! `peacock_core::ports` is deliberately synchronous and covers only what the domain
//! rules read. An HTTP handler needs more: it creates and mutates orders, it replays
//! idempotent writes, and it has to hold a row lock across a read-modify-write. Those
//! are async, stateful operations, so they live here rather than being forced into the
//! domain's port set.
//!
//! ## Wiring
//!
//! [`postgres_order::PostgresOrderStore`] is what the routes depend on, concretely.
//! [`order`] holds the shared vocabulary — [`order::StoreError`], [`order::OrderRecord`],
//! [`order::InvoiceRecord`], [`order::Mutation`] — that the routes and the repository
//! translate between.
//!
//! There is no in-memory implementation and no `OrderStore` trait. Lane W1-A removed
//! both: the fake was selected automatically whenever `DATABASE_URL` was unset, which
//! turned a missing database into silent data loss rather than a failure to start, and a
//! trait with one implementor was only paying for `async_trait`'s boxed futures. See
//! [`order`] for the full argument.

pub mod order;
pub mod postgres_order;

pub use order::{InvoiceRecord, Mutation, OrderRecord, StoreError, StoreResult};
pub use postgres_order::{InvoiceDefaults, PostgresOrderStore};
