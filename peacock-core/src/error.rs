//! Typed errors.
//!
//! Upstream uses `frappe.throw` with translated strings, so callers cannot branch
//! on failure kind. These variants are what the HTTP layer maps to status codes.

use crate::ids::*;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    // ---- table merge (ury_order.py merge_tables_batch guards) ----
    #[error("tables {seed} and {target} are in different rooms; merge is room-scoped")]
    CrossRoomMerge { seed: TableName, target: TableName },

    #[error("table {0} is already merged into another cluster")]
    AlreadyMerged(TableName),

    #[error("table {0} is occupied")]
    TableOccupied(TableName),

    #[error("merge would combine {count} separate active orders; at most one is allowed")]
    MultipleActiveOrders { count: usize },

    #[error("table {0} not found")]
    TableNotFound(TableName),

    // ---- COGS / BOM ----
    #[error("BOM {0} has quantity zero; per-unit normalisation would divide by zero")]
    BomZeroQuantity(BomName),

    // ---- menu ----
    #[error("no active menu found for the requested strategy")]
    NoActiveMenu,

    // ---- concurrency ----
    #[error("stale write: caller saw version {expected}, current is {actual}")]
    Conflict { expected: String, actual: String },

    // ---- invoice numbering (CGST Rule 46(b)) ----
    #[error("naming series {0} is not configured for fiscal year {1}")]
    SeriesNotConfigured(String, String),

    /// Distinct from `SeriesNotConfigured`: the counter exists, but the name it would
    /// produce is illegal. Conflating the two hides a configuration bug behind a
    /// "series missing" message.
    #[error(
        "invoice name {name:?} is {len} characters; CGST Rule 46(b) caps invoice names at {limit}",
        len = name.chars().count()
    )]
    InvoiceNameTooLong { name: String, limit: usize },

    // ---- data quality: upstream stores some numerics as Data (text) ----
    #[error("field {field} on {entity} holds non-numeric value {raw:?}")]
    NonNumericData {
        entity: String,
        field: String,
        raw: String,
    },

    // ---- shift management ----
    #[error("shift not found: {0}")]
    ShiftNotFound(ShiftName),

    #[error("a shift is already open on terminal {0}")]
    ShiftAlreadyOpen(TerminalName),

    #[error("no open shift found on terminal {0}")]
    NoOpenShift(TerminalName),
}

pub type Result<T> = std::result::Result<T, Error>;
