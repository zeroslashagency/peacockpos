//! Storage ports.
//!
//! Domain logic depends on these traits, never on SQL. That keeps every rule in
//! this crate testable with `cargo test` and no database — which matters here
//! because there is no Postgres on this machine (see GROUND-TRUTH.md).
//!
//! Deliberately synchronous: these are pure lookups, and keeping them sync means
//! the domain rules stay free of async plumbing. The SQL adapter can block or
//! prefetch as it sees fit.

use crate::error::Result;
use crate::ids::*;
use crate::model::*;
use crate::money::Money;
use std::collections::HashMap;

pub trait TableRepo {
    /// All tables in a room. One query — the merge BFS must not re-query per hop.
    fn list_by_room(&self, room: &RoomName) -> Result<Vec<Table>>;
    /// All tables across all rooms. Supports optional filtering by room and occupied state.
    fn list_all(&self, room: Option<&RoomName>, occupied: Option<bool>) -> Result<Vec<Table>>;
    fn get(&self, name: &TableName) -> Result<Table>;
}

pub trait OrderRepo {
    /// Number of *separate* live orders across a set of tables.
    /// `merge_tables_batch` refuses when this exceeds 1.
    fn count_separate_active(&self, tables: &[TableName]) -> Result<usize>;
}

pub trait ItemRepo {
    /// Batched item-group lookup. Replaces the per-item `frappe.db.get_value`
    /// N+1 at ury_kot_generate.py:154 and :214.
    fn item_groups(&self, codes: &[ItemCode]) -> Result<HashMap<ItemCode, ItemGroupName>>;
}

pub trait ProductionRepo {
    fn list_for_branch(&self, branch: &BranchName) -> Result<Vec<ProductionUnit>>;
}

pub trait KotRepo {
    /// True when a submitted KOT already exists for this invoice + production unit.
    /// Drives the `NewOrder` → `OrderModified` flip.
    fn exists_for(&self, invoice: &str, production: &ProductionUnitName) -> Result<bool>;
}

pub trait MenuRepo {
    /// Course per item, scoped to the room's active menu.
    fn courses_for_menu(
        &self,
        room: &RoomName,
        codes: &[ItemCode],
    ) -> Result<HashMap<ItemCode, MenuCourseName>>;
}

/// BOM line, as ERPNext stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct BomLine {
    pub item_code: ItemCode,
    pub qty: rust_decimal::Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bom {
    pub name: BomName,
    /// Batch size. COGS divides by this for per-unit cost
    /// (ury_daily_p_and_l.py:38). Dropping it scales COGS by the batch.
    pub quantity: rust_decimal::Decimal,
    pub items: Vec<BomLine>,
}

pub trait BomRepo {
    fn find_for_item(&self, item: &ItemCode) -> Result<Option<Bom>>;
}

/// One child line of an ERPNext `Product Bundle` (`pb.items`,
/// ury_daily_p_and_l.py:225-226).
#[derive(Debug, Clone, PartialEq)]
pub struct ProductBundleLine {
    pub item_code: ItemCode,
    pub qty: rust_decimal::Decimal,
}

/// ERPNext `Product Bundle` — a sold item that is really a set of other items.
///
/// Keyed by `new_item_code`, the code that appears on the POS Invoice line
/// (ury_daily_p_and_l.py:82, :222).
///
/// The bundle's Frappe docname (`pb_items[0].name`, :222-223) is deliberately not
/// modelled: it is only used upstream to re-fetch the document, and carrying it
/// would need an id newtype this crate does not have. Nothing in the COGS
/// arithmetic reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductBundle {
    pub new_item_code: ItemCode,
    pub items: Vec<ProductBundleLine>,
}

pub trait ProductBundleRepo {
    /// The bundle sold under this item code, if any.
    ///
    /// Upstream's `d.new_item_code IS NOT NULL` join (ury_daily_p_and_l.py:156, :170)
    /// is what puts an invoice line in the bundle bucket, so this lookup decides
    /// the cost basis before any BOM lookup happens. See `cogs::cogs_for_item_with_bundles`.
    ///
    /// Note there is **no `docstatus` or `is_active` filter** upstream (:222), unlike
    /// the BOM lookup which filters `is_active=1, is_default=1, docstatus=1` (:227).
    /// A draft Product Bundle therefore still captures the item.
    fn find_by_new_item_code(&self, item: &ItemCode) -> Result<Option<ProductBundle>>;
}

pub trait PriceRepo {
    /// `Item Price` on the given price list.
    ///
    /// COGS prices from the **buying** price list (ury_daily_p_and_l.py:30),
    /// NOT from stock valuation — a different cost basis that would diverge.
    fn item_price(&self, item: &ItemCode, price_list: &PriceListName) -> Result<Option<Money>>;
}

/// POS shift (POS Opening Entry).
#[derive(Debug, Clone, PartialEq)]
pub struct Shift {
    pub name: ShiftName,
    pub terminal: TerminalName,
    pub opened_at: chrono::DateTime<chrono::Utc>,
    pub closed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub opened_by: UserName,
    pub business_day: chrono::NaiveDate,
}

/// Z-report for a closed shift.
#[derive(Debug, Clone, PartialEq)]
pub struct ZReport {
    pub shift_name: ShiftName,
    pub terminal: TerminalName,
    pub business_day: chrono::NaiveDate,
    pub opened_at: chrono::DateTime<chrono::Utc>,
    pub closed_at: chrono::DateTime<chrono::Utc>,
    pub invoice_count: i64,
    pub cash_total: Money,
    pub card_total: Money,
    pub total_revenue: Money,
    pub cash_threshold_warning: bool, // true if cash >= ₹10,000 (CGST Rule 56)
}

pub trait ShiftRepo {
    /// Create a new shift. Returns error if one is already open on this terminal.
    fn open_shift(
        &self,
        terminal: &TerminalName,
        opened_by: &UserName,
        business_day: chrono::NaiveDate,
    ) -> Result<Shift>;

    /// Get the currently open shift for a terminal, if any.
    fn get_current_shift(&self, terminal: &TerminalName) -> Result<Option<Shift>>;

    /// Close a shift and generate Z-report.
    /// Calculates totals from invoices in the shift's business day range.
    fn close_shift(
        &self,
        shift_name: &ShiftName,
        cutoff_hour: u32,
        tz: chrono_tz::Tz,
    ) -> Result<ZReport>;

    /// Get Z-report for a closed shift.
    fn get_report(&self, shift_name: &ShiftName) -> Result<ZReport>;

    /// Get shift by name.
    fn get(&self, shift_name: &ShiftName) -> Result<Shift>;

    /// List shifts with pagination and filters.
    fn list_shifts(
        &self,
        terminal: Option<&TerminalName>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Shift>>;
}

/// Aggregator order repository (Lane W1-F).
///
/// Handles third-party delivery platform (Swiggy/Zomato) webhook orders:
/// - Idempotent insert (replay-safe)
/// - Accept (creates internal order + invoice)
/// - Reject (terminal status)
/// - Settlement reconciliation queries
pub trait AggregatorRepo {
    /// Insert a new aggregator order with items.
    /// Idempotent on aggregator_order_id: replaying returns the existing order.
    /// Returns the internal order ID.
    fn insert_order(&self, order: &AggregatorOrderInput) -> Result<String>;

    /// Find an aggregator order by internal ID.
    fn find_order(&self, id: &str) -> Result<Option<AggregatorOrderData>>;

    /// Accept an order, linking it to internal order + invoice.
    /// Returns error if not in Pending status.
    fn accept_order(
        &self,
        id: &str,
        internal_order_id: i64,
        internal_invoice_id: &InvoiceName,
    ) -> Result<()>;

    /// Reject an order with a reason.
    /// Returns error if not in Pending status.
    fn reject_order(&self, id: &str, reason: &str) -> Result<()>;

    /// List settlements for reconciliation, filtered by date range and optional platform.
    fn list_settlements(
        &self,
        start_date: chrono::NaiveDate,
        end_date: chrono::NaiveDate,
        platform: Option<&str>,
    ) -> Result<Vec<SettlementData>>;
}

/// Input for inserting an aggregator order.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregatorOrderInput {
    pub aggregator_order_id: String,
    pub platform: String,
    pub customer_name: String,
    pub customer_phone: Option<String>,
    pub total: Money,
    pub ordered_at: chrono::DateTime<chrono::Utc>,
    pub instructions: Option<String>,
    pub items: Vec<AggregatorOrderItemInput>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AggregatorOrderItemInput {
    pub item_code: String,
    pub item_name: String,
    pub quantity: rust_decimal::Decimal,
    pub rate: Money,
    pub special_instructions: Option<String>,
}

/// Aggregator order data returned by the repo.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregatorOrderData {
    pub id: String,
    pub aggregator_order_id: String,
    pub platform: String,
    pub customer_name: String,
    pub customer_phone: Option<String>,
    pub total: Money,
    pub ordered_at: chrono::DateTime<chrono::Utc>,
    pub status: String,
    pub internal_order_id: Option<i64>,
    pub internal_invoice_id: Option<InvoiceName>,
    pub instructions: Option<String>,
    pub items: Vec<AggregatorOrderItemData>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AggregatorOrderItemData {
    pub item_code: String,
    pub item_name: String,
    pub quantity: rust_decimal::Decimal,
    pub rate: Money,
}

/// Settlement data for reconciliation.
#[derive(Debug, Clone, PartialEq)]
pub struct SettlementData {
    pub id: String,
    pub platform: String,
    pub settlement_date: chrono::NaiveDate,
    pub total_orders: i32,
    pub gross_amount: Money,
    pub commission: Money,
    pub net_amount: Money,
}
