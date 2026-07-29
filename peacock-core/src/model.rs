//! Domain entities, transcribed from the real doctype JSON.
//!
//! Field names and types match `_upstream/ury-ury/ury/ury/doctype/*/*.json`.
//! See GROUND-TRUTH.md. 12 root doctypes, 24 child tables.

use crate::error::{Error, Result};
use crate::ids::*;
use crate::money::Money;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Frappe docstatus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocStatus {
    Draft = 0,
    Submitted = 1,
    Cancelled = 2,
}

/// ERPNext POS Invoice status.
///
/// Modelled as an enum specifically because bug 4 (GROUND-TRUTH.md) exists from
/// two call sites disagreeing on which values count: shift close used only
/// `Paid`, the P&L used `("Consolidated", "Paid")`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PosInvoiceStatus {
    Draft,
    Paid,
    Consolidated,
    Return,
}

impl PosInvoiceStatus {
    /// The single authoritative definition of "counts as revenue".
    /// Both shift close and the P&L must use this.
    pub const REVENUE: [PosInvoiceStatus; 2] =
        [PosInvoiceStatus::Paid, PosInvoiceStatus::Consolidated];

    pub fn counts_as_revenue(&self) -> bool {
        Self::REVENUE.contains(self)
    }
}

// ---------------------------------------------------------------------------
// URY Table (root) — ury_table.json
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableShape {
    Rectangle,
    Square,
    Circle,
}

/// `merged_with` is a `Data` field holding CSV, and it lives on **URY Table** —
/// not on the order. v1 of the plan read it off the order and would have merged
/// the wrong things.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedWith(Vec<TableName>);

impl MergedWith {
    /// Mirrors `_parse_merged_with` in ury_order.py. Production data contains
    /// empty strings, stray whitespace and trailing commas — all tolerated.
    pub fn parse(raw: Option<&str>) -> Self {
        MergedWith(
            raw.unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(TableName::from)
                .collect(),
        )
    }

    pub fn iter(&self) -> impl Iterator<Item = &TableName> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, t: &TableName) -> bool {
        self.0.contains(t)
    }

    pub fn to_csv(&self) -> String {
        self.0
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub name: TableName,
    pub no_of_seats: i32,
    pub minimum_seating: i32,
    pub restaurant: RestaurantName,
    pub restaurant_room: RoomName,
    pub branch: BranchName,
    pub is_take_away: bool,
    pub occupied: bool,
    pub latest_invoice_time: Option<NaiveTime>,
    pub table_shape: Option<TableShape>,
    // Float in the JSON — geometry, so f64 is correct here (not money).
    pub layout_x: f64,
    pub layout_y: f64,
    pub layout_width: f64,
    pub layout_height: f64,
    pub merged_with: MergedWith,
}

// ---------------------------------------------------------------------------
// URY Order (root) — ury_order.json
// ---------------------------------------------------------------------------

/// `URY Order` is a Frappe **UI form**, not the order of record.
///
/// The JSON is mostly screen furniture (`table_tab`, `menu_tab`, `cart_items` HTML,
/// `favorite_items` HTML). It has **no status field** and no tax or payment fields.
/// The real record is ERPNext's POS Invoice, reachable via `last_invoice`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UryOrderForm {
    pub take_away: bool,
    pub restaurant_table: Option<TableName>,
    pub customer_name: CustomerName,
    pub no_of_pax: i32,
    pub grand_total: Money,
    pub last_invoice: Option<InvoiceName>,
    pub items: Vec<OrderItem>,
    pub waiter: Option<UserName>,
    pub pos_profile: Option<PosProfileName>,
    pub cashier: Option<UserName>,
    pub comments: Option<String>,
    pub modified_time: Option<DateTime<Utc>>,
}

/// `ury_order_item` (child table).
///
/// Note `qty` is `Int` upstream, so fractional quantities (0.5 kg) are not
/// representable. Changing that is a schema change, not a port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderItem {
    pub item: ItemCode,
    pub item_name: String,
    pub qty: i32,
    pub rate: Money,
    pub comments: Option<String>,
}

// ---------------------------------------------------------------------------
// URY KOT (root) — ury_kot.json
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KotType {
    NewOrder,
    OrderModified,
    Cancelled,
    PartiallyCancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Kot {
    pub name: Option<KotName>,
    pub naming_series: String,
    /// `Data` upstream, not a Link — stores the raw invoice name.
    pub invoice: String,
    pub restaurant_table: Option<TableName>,
    pub customer_name: Option<CustomerName>,
    /// `Small Text` — back-link to the KOT being cancelled.
    pub original_kot: Option<String>,
    pub date: NaiveDate,
    pub time: Option<NaiveTime>,
    pub kot_type: KotType,
    pub order_status: Option<String>,
    pub production: Option<ProductionUnitName>,
    pub start_time_prep: Option<NaiveTime>,
    pub kot_items: Vec<KotItem>,
    pub pos_profile: Option<PosProfileName>,
    pub branch: Option<BranchName>,
    pub verified: bool,
    pub verified_by: Option<UserName>,
    pub table_takeaway: bool,
    pub is_aggregator: bool,
    pub aggregator_id: Option<String>,
    pub comments: Option<String>,
    pub order_no: Option<String>,
}

/// `ury_kot_items` (child table).
///
/// Upstream stores `quantity` and `cancelled_qty` as `Data` (text), not numeric.
/// Migration must validate and report unparseable rows rather than coercing to zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KotItem {
    pub item: ItemCode,
    pub item_name: String,
    pub quantity: Decimal,
    pub cancelled_qty: Decimal,
    pub comments: Option<String>,
    pub course: Option<MenuCourseName>,
    pub serve_priority: i32,
    pub indicate_course: bool,
}

/// Parse a `Data`-typed numeric field, surfacing bad data instead of hiding it.
pub fn parse_data_numeric(entity: &str, field: &str, raw: Option<&str>) -> Result<Decimal> {
    let s = raw.unwrap_or("").trim();
    if s.is_empty() {
        return Ok(Decimal::ZERO);
    }
    Decimal::from_str(s).map_err(|_| Error::NonNumericData {
        entity: entity.to_owned(),
        field: field.to_owned(),
        raw: s.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Production unit (root) + its item groups (child)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductionUnit {
    pub name: ProductionUnitName,
    pub branch: BranchName,
    /// From the `ury_production_item_groups` child table.
    pub item_groups: Vec<ItemGroupName>,
}

// ---------------------------------------------------------------------------
// A line as it arrives from the POS client
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderLine {
    pub item_code: ItemCode,
    pub item_name: String,
    pub qty: Decimal,
    pub rate: Money,
    pub comments: Option<String>,
    pub serve_priority: i32,
    pub indicate_course: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn merged_with_tolerates_real_world_csv() {
        let m = MergedWith::parse(Some("T-01, T-02 ,,T-03,"));
        let got: Vec<_> = m.iter().map(|t| t.as_str()).collect();
        assert_eq!(got, vec!["T-01", "T-02", "T-03"]);
    }

    #[test]
    fn merged_with_handles_null_and_empty() {
        assert!(MergedWith::parse(None).is_empty());
        assert!(MergedWith::parse(Some("")).is_empty());
        assert!(MergedWith::parse(Some("  ,  ")).is_empty());
    }

    #[test]
    fn merged_with_round_trips_through_csv() {
        let m = MergedWith::parse(Some("T-01,T-02"));
        assert_eq!(MergedWith::parse(Some(&m.to_csv())), m);
    }

    #[test]
    fn revenue_definition_is_single_and_includes_consolidated() {
        // Regression for bug 4: shift close previously omitted Consolidated.
        assert!(PosInvoiceStatus::Paid.counts_as_revenue());
        assert!(PosInvoiceStatus::Consolidated.counts_as_revenue());
        assert!(!PosInvoiceStatus::Draft.counts_as_revenue());
        assert!(!PosInvoiceStatus::Return.counts_as_revenue());
    }

    #[test]
    fn data_typed_numerics_parse_or_report() {
        assert_eq!(
            parse_data_numeric("URY KOT Items", "quantity", Some("2.5")).unwrap(),
            dec!(2.5)
        );
        assert_eq!(
            parse_data_numeric("URY KOT Items", "quantity", Some("")).unwrap(),
            Decimal::ZERO
        );
        // Bad data surfaces as an error instead of silently becoming zero.
        assert!(parse_data_numeric("URY KOT Items", "quantity", Some("two")).is_err());
    }
}
