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
