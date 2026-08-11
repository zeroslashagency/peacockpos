//! COGS and P&L reporting DTOs (Lane 3I).
//!
//! ## Money on the wire
//!
//! Every monetary field is [`Money`], which serialises as a JSON **string**
//! (`peacock_core::money`). COGS is never rounded — `peacock_core::cogs` keeps full
//! `Decimal` precision and so does this layer. Rounding money here would invent a
//! second cost basis and break the parity harness.
//!
//! ## The three unset lists stay separate
//!
//! [`UnsetItems`] mirrors `CogsResult`'s three lists rather than merging them. The
//! label is the actionable part: `bom_items` sends the operator to a BOM ingredient's
//! buying price, `bundle_items` to a Product Bundle component, `item_prices` to the
//! sold item itself (ury_daily_p_and_l.py:262-264).

use chrono::{DateTime, NaiveDate, Utc};
use peacock_core::cogs::CogsResult;
use peacock_core::money::Money;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Default business-day cutoff hour (03:00 IST), matching `URY Report Settings.hours`
/// as used at ury_daily_p_and_l.py:98-99.
pub const DEFAULT_CUTOFF_HOUR: u32 = 3;

fn default_cutoff_hour() -> u32 {
    DEFAULT_CUTOFF_HOUR
}

// ---------------------------------------------------------------------------
// POST /api/cogs/calculate
// ---------------------------------------------------------------------------

/// Request body for `POST /api/cogs/calculate`.
///
/// Exactly one scope must be supplied: either `invoice`, or the `from_date`/`to_date`
/// pair. See [`CogsCalculateRequest::scope`].
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct CogsCalculateRequest {
    /// A single POS Invoice name.
    #[serde(default)]
    pub invoice: Option<String>,
    /// First business day of the range (inclusive).
    #[serde(default)]
    pub from_date: Option<NaiveDate>,
    /// Last business day of the range (inclusive — its whole business day is covered).
    #[serde(default)]
    pub to_date: Option<NaiveDate>,
    /// Hour (0–23) in IST when the business day rolls over.
    #[serde(default = "default_cutoff_hour")]
    pub cutoff_hour: u32,
}

/// The resolved scope of a COGS calculation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CogsScope {
    Invoice(String),
    DateRange { from: NaiveDate, to: NaiveDate },
}

impl CogsCalculateRequest {
    /// Resolves the request into exactly one scope.
    ///
    /// # Errors
    /// A human-readable message when the caller supplied both scopes, neither, half a
    /// date range, an inverted range, or an out-of-range `cutoff_hour`. Callers map
    /// this to a 400.
    pub fn scope(&self) -> Result<CogsScope, String> {
        if self.cutoff_hour > 23 {
            return Err(format!(
                "cutoff_hour must be 0-23, got {}",
                self.cutoff_hour
            ));
        }

        let has_invoice = self.invoice.as_deref().is_some_and(|s| !s.trim().is_empty());
        let has_range = self.from_date.is_some() || self.to_date.is_some();

        match (has_invoice, has_range) {
            (true, true) => Err(
                "supply either 'invoice' or 'from_date'/'to_date', not both".to_string(),
            ),
            (false, false) => Err(
                "one of 'invoice' or 'from_date'/'to_date' is required".to_string(),
            ),
            (true, false) => Ok(CogsScope::Invoice(
                self.invoice.as_deref().unwrap_or_default().trim().to_string(),
            )),
            (false, true) => match (self.from_date, self.to_date) {
                (Some(from), Some(to)) if from > to => Err(format!(
                    "from_date {from} must not be after to_date {to}"
                )),
                (Some(from), Some(to)) => Ok(CogsScope::DateRange { from, to }),
                _ => Err("both 'from_date' and 'to_date' are required for a date range".to_string()),
            },
        }
    }
}

/// Which of upstream's three cost bases priced an item.
///
/// A partition, not a fallback chain: an item that is both a Product Bundle and has an
/// active default BOM is `Bundle`, and its own BOM is never consulted
/// (ury_daily_p_and_l.py:170).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostBasis {
    Bundle,
    Bom,
    Plain,
}

/// The three unset-price lists, kept separate by label.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnsetItems {
    /// Sold items priced directly from `Item Price` that have none (label `ITEMS`).
    pub item_prices: Vec<String>,
    /// Product Bundle child lines with no `Item Price` (label `BUNDLE SUB ITEMS`).
    pub bundle_items: Vec<String>,
    /// BOM ingredients with no `Item Price` (label `BOM SUB ITEMS`).
    pub bom_items: Vec<String>,
}

impl UnsetItems {
    pub fn from_cogs(result: &CogsResult) -> Self {
        let names = |v: &[peacock_core::ids::ItemCode]| -> Vec<String> {
            v.iter().map(|c| c.as_str().to_owned()).collect()
        };
        Self {
            item_prices: names(&result.unset_item_prices),
            bundle_items: names(&result.unset_bundle_items),
            bom_items: names(&result.unset_bom_items),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.item_prices.is_empty() && self.bundle_items.is_empty() && self.bom_items.is_empty()
    }

    /// Total number of flagged items across all three lists.
    pub fn count(&self) -> usize {
        self.item_prices.len() + self.bundle_items.len() + self.bom_items.len()
    }
}

/// COGS for one sold item, aggregated across every line that sold it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemCogsBreakdown {
    pub item_code: String,
    pub item_name: String,
    /// Total quantity sold in scope.
    #[serde(with = "rust_decimal::serde::str")]
    pub qty: Decimal,
    pub cogs: Money,
    pub cost_basis: CostBasis,
    /// Items with no configured buying price that this row's cost is missing.
    #[serde(skip_serializing_if = "UnsetItems::is_empty", default)]
    pub unset: UnsetItems,
}

/// Response body for `POST /api/cogs/calculate`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CogsCalculateResponse {
    /// `"invoice"` or `"date_range"`.
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_date: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_date: Option<NaiveDate>,
    /// Number of revenue-counting invoices included (always 1 for invoice scope).
    pub invoice_count: usize,
    /// Total COGS, unrounded.
    pub cogs: Money,
    /// Per-item breakdown, sorted by `item_code`.
    pub items: Vec<ItemCogsBreakdown>,
    pub unset: UnsetItems,
    /// True when any of the three unset lists is non-empty — the COGS figure
    /// understates cost by the missing prices.
    pub has_unset_items: bool,
}

// ---------------------------------------------------------------------------
// GET /api/reports/daily-pl
// ---------------------------------------------------------------------------

/// Query for `GET /api/reports/daily-pl` and `GET /api/reports/item-costing`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct BusinessDayQuery {
    /// Business day to report on. Defaults to today's business day in IST.
    #[serde(default)]
    pub date: Option<NaiveDate>,
    #[serde(default = "default_cutoff_hour")]
    pub cutoff_hour: u32,
}

impl Default for BusinessDayQuery {
    fn default() -> Self {
        Self {
            date: None,
            cutoff_hour: DEFAULT_CUTOFF_HOUR,
        }
    }
}

/// Response body for `GET /api/reports/daily-pl`.
///
/// `revenue` sums `rounded_total` over invoices whose status is in
/// `PosInvoiceStatus::REVENUE` — the single source of truth shared with shift close, so
/// the two reports cannot disagree (GROUND-TRUTH.md bugs 3 and 4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DailyPlResponse {
    pub business_day: NaiveDate,
    /// Inclusive start of the business day.
    pub start: DateTime<Utc>,
    /// **Exclusive** end of the business day.
    pub end: DateTime<Utc>,
    pub cutoff_hour: u32,
    /// Invoices counted as revenue.
    pub invoice_count: usize,
    /// Invoices in range that did NOT count as revenue (Draft, Return).
    pub excluded_invoice_count: usize,
    pub revenue: Money,
    pub cogs: Money,
    /// `revenue - cogs`.
    pub gross_profit: Money,
    /// `gross_profit / revenue * 100`, rounded to 2dp. `null` when revenue is zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gross_margin_pct: Option<String>,
    /// Sum of `rounded_total - grand_total` over counted invoices. Ledgered separately.
    pub round_off_total: Money,
    pub unset: UnsetItems,
    pub has_unset_items: bool,
}

// ---------------------------------------------------------------------------
// GET /api/reports/item-costing
// ---------------------------------------------------------------------------

/// One row of the item-costing report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemCostingRow {
    pub item_code: String,
    pub item_name: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub qty_sold: Decimal,
    /// Line revenue: `sum(rate * qty)`. Pre-tax and pre-invoice-discount, so it does
    /// not sum to the P&L's `revenue` (which is `rounded_total`).
    pub revenue: Money,
    pub cogs: Money,
    /// `revenue - cogs` on the line basis above.
    pub gross_profit: Money,
    pub cost_basis: CostBasis,
    #[serde(skip_serializing_if = "UnsetItems::is_empty", default)]
    pub unset: UnsetItems,
}

/// Response body for `GET /api/reports/item-costing`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemCostingResponse {
    pub business_day: NaiveDate,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub cutoff_hour: u32,
    pub invoice_count: usize,
    /// Rows sorted by `item_code`.
    pub items: Vec<ItemCostingRow>,
    /// Sum of the rows' `cogs`.
    pub total_cogs: Money,
    /// Sum of the rows' line revenue. Not the P&L revenue figure; see
    /// [`ItemCostingRow::revenue`].
    pub total_line_revenue: Money,
    pub unset: UnsetItems,
    pub has_unset_items: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use peacock_core::ids::ItemCode;
    use rust_decimal_macros::dec;

    #[test]
    fn request_resolves_invoice_scope() {
        let req = CogsCalculateRequest {
            invoice: Some("ACC-PSINV-2026-00042".into()),
            ..Default::default()
        };
        assert_eq!(
            req.scope().unwrap(),
            CogsScope::Invoice("ACC-PSINV-2026-00042".into())
        );
    }

    #[test]
    fn request_resolves_date_range_scope() {
        let req = CogsCalculateRequest {
            from_date: Some(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()),
            to_date: Some(NaiveDate::from_ymd_opt(2026, 7, 31).unwrap()),
            cutoff_hour: 3,
            ..Default::default()
        };
        assert_eq!(
            req.scope().unwrap(),
            CogsScope::DateRange {
                from: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                to: NaiveDate::from_ymd_opt(2026, 7, 31).unwrap(),
            }
        );
    }

    #[test]
    fn request_rejects_both_scopes_and_neither() {
        let both = CogsCalculateRequest {
            invoice: Some("INV-1".into()),
            from_date: Some(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()),
            to_date: Some(NaiveDate::from_ymd_opt(2026, 7, 2).unwrap()),
            ..Default::default()
        };
        assert!(both.scope().unwrap_err().contains("not both"));

        let neither = CogsCalculateRequest::default();
        assert!(neither.scope().unwrap_err().contains("required"));
    }

    #[test]
    fn request_rejects_half_and_inverted_ranges() {
        let half = CogsCalculateRequest {
            from_date: Some(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()),
            ..Default::default()
        };
        assert!(half.scope().unwrap_err().contains("both"));

        let inverted = CogsCalculateRequest {
            from_date: Some(NaiveDate::from_ymd_opt(2026, 7, 31).unwrap()),
            to_date: Some(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()),
            ..Default::default()
        };
        assert!(inverted.scope().unwrap_err().contains("must not be after"));
    }

    #[test]
    fn request_rejects_out_of_range_cutoff_hour() {
        let req = CogsCalculateRequest {
            invoice: Some("INV-1".into()),
            cutoff_hour: 24,
            ..Default::default()
        };
        assert!(req.scope().unwrap_err().contains("cutoff_hour"));
    }

    #[test]
    fn request_defaults_cutoff_hour_to_three() {
        let req: CogsCalculateRequest =
            serde_json::from_str(r#"{"invoice":"ACC-PSINV-2026-00042"}"#).unwrap();
        assert_eq!(req.cutoff_hour, DEFAULT_CUTOFF_HOUR);
    }

    #[test]
    fn blank_invoice_is_not_a_scope() {
        let req = CogsCalculateRequest {
            invoice: Some("   ".into()),
            ..Default::default()
        };
        assert!(req.scope().unwrap_err().contains("required"));
    }

    #[test]
    fn unset_items_keeps_the_three_lists_separate() {
        let result = CogsResult {
            cost: Money::new(dec!(10)),
            unset_item_prices: vec![ItemCode::from("NO-PRICE")],
            unset_bundle_items: vec![ItemCode::from("PICKLE")],
            unset_bom_items: vec![ItemCode::from("CHEESE")],
        };
        let unset = UnsetItems::from_cogs(&result);

        assert_eq!(unset.item_prices, vec!["NO-PRICE"]);
        assert_eq!(unset.bundle_items, vec!["PICKLE"]);
        assert_eq!(unset.bom_items, vec!["CHEESE"]);
        assert!(!unset.is_empty());
        assert_eq!(unset.count(), 3);
    }

    #[test]
    fn money_fields_serialise_as_strings() {
        let response = CogsCalculateResponse {
            scope: "invoice".into(),
            invoice: Some("ACC-PSINV-2026-00042".into()),
            from_date: None,
            to_date: None,
            invoice_count: 1,
            cogs: Money::new(dec!(35.00)),
            items: vec![ItemCogsBreakdown {
                item_code: "MASALA-CHAI".into(),
                item_name: "Masala Chai".into(),
                qty: dec!(5),
                cogs: Money::new(dec!(35.00)),
                cost_basis: CostBasis::Bom,
                unset: UnsetItems::default(),
            }],
            unset: UnsetItems::default(),
            has_unset_items: false,
        };

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["cogs"], "35.00");
        assert_eq!(json["items"][0]["qty"], "5");
        assert_eq!(json["items"][0]["cost_basis"], "bom");
        // Empty per-row unset lists are omitted, keeping large reports readable.
        assert!(json["items"][0].get("unset").is_none());
        assert!(json.get("from_date").is_none());
    }

    #[test]
    fn business_day_query_defaults() {
        let query: BusinessDayQuery = serde_json::from_str("{}").unwrap();
        assert_eq!(query.cutoff_hour, DEFAULT_CUTOFF_HOUR);
        assert_eq!(query.date, None);
        assert_eq!(query, BusinessDayQuery::default());
    }
}
