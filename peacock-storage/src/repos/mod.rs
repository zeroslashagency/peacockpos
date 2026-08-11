//! Repository implementations for the domain's port traits.

pub mod aggregator;
pub mod blocking;
pub mod bom;
pub mod bundle;
pub mod invoice;
pub mod item;
pub mod kot;
pub mod menu;
pub mod order;
pub mod price;
pub mod restaurant;
pub mod routing;
pub mod shift;
pub mod table;

pub use aggregator::{
    AggregatorOrderItem, AggregatorOrderStatus, NewAggregatorOrder, PgAggregatorRepo,
    StoredAggregatorOrder, StoredAggregatorOrderItem, StoredSettlement,
};
pub use bom::{BomSnapshot, PgBomRepo};
pub use bundle::{BundleSnapshot, PgProductBundleRepo};
pub use invoice::{
    CreateOutcome, CreatedInvoice, NewInvoice, NewInvoiceLine, NewPayment, PaymentMethod,
    PgInvoiceRepo, StoredInvoice, StoredInvoiceLine, StoredPayment,
};
pub use item::{ItemDetails, PgItemDetailsRepo};
pub use kot::PgKotRepo;
pub use menu::{PgMenuRepo, PgMenuResolutionRepo};
pub use order::{
    CreateOutcome as OrderCreateOutcome, CreatedOrder, OrderId, OrderLifecycle, PgOrderRepo,
    StoredOrder,
};
pub use price::PgPriceRepo;
pub use restaurant::{PgRestaurantRepo, RestaurantSummary};
pub use routing::{PgItemRepo, PgProductionRepo, RoutingSnapshot};
pub use shift::PostgresShiftRepo;
pub use table::{batch_update_merged_with, update_merged_with, PostgresTableRepo};

use crate::error::StorageError;
use peacock_core::error::Error as DomainError;

/// Collapse a [`StorageError`] into the domain vocabulary a port trait can return.
///
/// The port traits return `peacock_core::error::Result`, which has no variant for "the
/// database was unreachable" — on purpose: the domain is I/O-free and has no word for it.
/// So the sync trait wrappers need one funnel, and this is it.
///
/// * A `Domain` error passes through unchanged. This is the important case: `NoActiveMenu`
///   and `TableNotFound` must reach the caller intact, because the HTTP layer maps them to
///   404 rather than 500 (`peacock-api/src/error.rs`).
/// * Everything else becomes `NonNumericData`, carrying the original message in `raw`.
///   That variant maps to a 500, which is the right status for an infrastructure fault,
///   and the message survives for the logs.
///
/// The name is a poor fit for a connection failure, and that is a wart in
/// `peacock_core::Error` rather than here: it has no generic infrastructure variant. The
/// alternative — inventing a domain variant per SQL failure — would put storage concerns
/// in the domain crate, which is the coupling the ports exist to prevent. Callers that
/// need the real error use the `*_async` methods, which return `StorageError` unchanged.
pub fn to_domain_error(err: StorageError) -> DomainError {
    match err {
        StorageError::Domain(domain) => domain,
        other => DomainError::NonNumericData {
            entity: "storage".to_owned(),
            field: "query".to_owned(),
            raw: other.to_string(),
        },
    }
}
