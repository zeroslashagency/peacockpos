//! P&L and item-costing reports (Lane 3I).
//!
//! - `GET /api/reports/daily-pl` — revenue, COGS and gross profit for a business day
//! - `GET /api/reports/item-costing` — COGS broken down by item
//!
//! ## Revenue has exactly one definition
//!
//! [`compute_daily_pl`] filters invoices through
//! [`PosInvoiceStatus::counts_as_revenue`] and sums `rounded_total`. That is the same
//! call [`peacock_core::businessday::shift_revenue`] makes, which is what stops this
//! report and shift close from disagreeing — upstream had two definitions of revenue
//! and they diverged (GROUND-TRUTH.md bugs 3 and 4):
//!
//! - bug 3: `sub_pos_closing.py:45` summed `grand_total`, `ury_daily_p_and_l.py:297`
//!   summed `rounded_total`. We use `rounded_total`: what the customer pays, and what
//!   the printed invoice shows. The round-off delta is ledgered separately and is
//!   reported here as `round_off_total`.
//! - bug 4: shift close filtered `status = "Paid"` only, the P&L used
//!   `IN ("Consolidated","Paid")`. `PosInvoiceStatus::REVENUE` is now the only list.
//!
//! [`reconcile_with_shift`] makes that agreement assertable rather than assumed.
//!
//! ## Business-day boundaries
//!
//! Ranges are half-open `[start, end)` via [`BusinessDay`]. An invoice posted exactly at
//! `end` belongs to the next business day. Upstream filtered a DATE column `BETWEEN` two
//! datetimes, which MariaDB casts to dates, so every midnight-crossing dinner shift
//! counted its invoices in both days (bug 2).
//!
//! ## Gross profit
//!
//! `gross_profit = revenue - cogs`, both unrounded. COGS comes from the same
//! [`crate::routes::cogs::aggregate_cogs`] the calculate endpoint uses, so the two
//! endpoints cannot report different costs for the same day.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use peacock_core::businessday::{BusinessDay, InvoiceSummary};
use peacock_core::ids::{ItemCode, PriceListName};
use peacock_core::model::OrderLine;
use peacock_core::money::Money;
use peacock_core::ports::{BomRepo, PriceRepo, ProductBundleRepo};
use rust_decimal::Decimal;

use crate::dto::reports::{
    BusinessDayQuery, DailyPlResponse, ItemCostingResponse, ItemCostingRow,
};
use crate::error::{ApiError, ApiResult};
use crate::routes::cogs::{aggregate_cogs, CogsAggregate};
use crate::state::AppState;

/// URY runs in IST. The 30-minute offset is not negotiable — `BusinessDay` depends on it
/// for the cutoff comparison.
pub const REPORT_TZ: Tz = chrono_tz::Asia::Kolkata;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/reports/daily-pl", get(daily_pl))
        .route("/api/reports/item-costing", get(item_costing))
}

/// Revenue and invoice counts for a business day.
#[derive(Debug, Clone, PartialEq)]
pub struct RevenueSummary {
    /// Sum of `rounded_total` over revenue-counting invoices in range.
    pub revenue: Money,
    /// Sum of `rounded_total - grand_total` over those same invoices.
    pub round_off_total: Money,
    /// Invoices counted.
    pub invoice_count: usize,
    /// Invoices in range that did not count (Draft, Return).
    pub excluded_invoice_count: usize,
}

/// Sums revenue over the invoices falling inside a business day.
///
/// Two filters, in this order:
/// 1. `day.contains(posted_at)` — the half-open range, so no invoice lands in two days.
/// 2. `status.counts_as_revenue()` — `PosInvoiceStatus::REVENUE`, the shared list.
///
/// Invoices outside the range are not counted anywhere, including in
/// `excluded_invoice_count`: that count reports non-revenue invoices *within* the day,
/// which is the number an operator would reconcile against.
pub fn summarise_revenue(invoices: &[InvoiceSummary], day: &BusinessDay) -> RevenueSummary {
    let in_range = invoices.iter().filter(|inv| day.contains(inv.posted_at));

    let mut revenue = Money::ZERO;
    let mut round_off_total = Money::ZERO;
    let mut invoice_count = 0usize;
    let mut excluded_invoice_count = 0usize;

    for invoice in in_range {
        if invoice.status.counts_as_revenue() {
            revenue = revenue + invoice.rounded_total;
            round_off_total = round_off_total + invoice.round_off;
            invoice_count += 1;
        } else {
            excluded_invoice_count += 1;
        }
    }

    RevenueSummary {
        revenue,
        round_off_total,
        invoice_count,
        excluded_invoice_count,
    }
}

/// Gross margin as a percentage string with 2 decimals, or `None` when revenue is zero.
///
/// Zero revenue with non-zero COGS is a real state (everything voided, stock still
/// consumed) and has no meaningful margin, so the field is omitted rather than reported
/// as 0% or -inf.
fn gross_margin_pct(revenue: Money, gross_profit: Money) -> Option<String> {
    if revenue.is_zero() {
        return None;
    }
    let pct = (gross_profit.inner() / revenue.inner()) * Decimal::from(100);
    Some(
        pct.round_dp_with_strategy(2, peacock_core::money::ROUNDING)
            .to_string(),
    )
}

/// Builds the P&L for a business day from invoice summaries and their lines.
///
/// `lines` are the order lines of the revenue-counting invoices only; passing lines from
/// a Draft invoice would charge COGS for revenue that was never recognised.
#[allow(clippy::too_many_arguments)]
pub fn compute_daily_pl(
    day: &BusinessDay,
    cutoff_hour: u32,
    invoices: &[InvoiceSummary],
    lines: &[OrderLine],
    buying_price_list: &PriceListName,
    bundles: &dyn ProductBundleRepo,
    boms: &dyn BomRepo,
    prices: &dyn PriceRepo,
) -> Result<DailyPlResponse, peacock_core::Error> {
    let revenue = summarise_revenue(invoices, day);
    let cogs = aggregate_cogs(lines, buying_price_list, bundles, boms, prices)?;
    Ok(assemble_daily_pl(day, cutoff_hour, revenue, cogs))
}

/// Assembles the response from an already-computed revenue summary and COGS aggregate.
pub fn assemble_daily_pl(
    day: &BusinessDay,
    cutoff_hour: u32,
    revenue: RevenueSummary,
    cogs: CogsAggregate,
) -> DailyPlResponse {
    let gross_profit = revenue.revenue - cogs.total;

    DailyPlResponse {
        business_day: day.label,
        start: day.start,
        end: day.end,
        cutoff_hour,
        invoice_count: revenue.invoice_count,
        excluded_invoice_count: revenue.excluded_invoice_count,
        revenue: revenue.revenue,
        cogs: cogs.total,
        gross_profit,
        gross_margin_pct: gross_margin_pct(revenue.revenue, gross_profit),
        round_off_total: revenue.round_off_total,
        has_unset_items: cogs.has_unset_items(),
        unset: cogs.unset,
    }
}

/// Asserts that this report's revenue equals a shift close's total for the same day.
///
/// Both figures must come from `rounded_total` over `PosInvoiceStatus::REVENUE`. A
/// mismatch means one of the two call sites drifted, which is exactly the class of bug
/// this crate exists to prevent, so it is an error rather than a warning.
pub fn reconcile_with_shift(
    report: &DailyPlResponse,
    shift_total: Money,
) -> Result<(), peacock_core::Error> {
    peacock_core::businessday::reconcile(shift_total, report.revenue)
}

/// Builds the item-costing rows from a COGS aggregate and the line revenue per item.
///
/// `line_revenue` is `sum(rate × qty)` per item code. It is pre-tax and pre-invoice
/// discount, so it deliberately does not reconcile to the P&L's `revenue` — that figure
/// is `rounded_total`, which includes tax and rounding. Mixing the two bases is how a
/// margin report starts lying.
pub fn build_item_costing(
    day: &BusinessDay,
    cutoff_hour: u32,
    invoice_count: usize,
    aggregate: CogsAggregate,
    line_revenue: &std::collections::BTreeMap<String, Money>,
) -> ItemCostingResponse {
    let has_unset_items = aggregate.has_unset_items();
    let mut total_cogs = Money::ZERO;
    let mut total_line_revenue = Money::ZERO;

    let items: Vec<ItemCostingRow> = aggregate
        .items
        .into_iter()
        .map(|row| {
            let revenue = line_revenue
                .get(&row.item_code)
                .copied()
                .unwrap_or(Money::ZERO);
            total_cogs = total_cogs + row.cogs;
            total_line_revenue = total_line_revenue + revenue;

            ItemCostingRow {
                item_code: row.item_code,
                item_name: row.item_name,
                qty_sold: row.qty,
                revenue,
                cogs: row.cogs,
                gross_profit: revenue - row.cogs,
                cost_basis: row.cost_basis,
                unset: row.unset,
            }
        })
        .collect();

    ItemCostingResponse {
        business_day: day.label,
        start: day.start,
        end: day.end,
        cutoff_hour,
        invoice_count,
        items,
        total_cogs,
        total_line_revenue,
        unset: aggregate.unset,
        has_unset_items,
    }
}

/// Sums `rate × qty` per item code across lines.
pub fn line_revenue_by_item(lines: &[OrderLine]) -> std::collections::BTreeMap<String, Money> {
    let mut map: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    for line in lines {
        let entry = map
            .entry(line.item_code.as_str().to_owned())
            .or_insert(Money::ZERO);
        *entry = *entry + line.rate * line.qty;
    }
    map
}

/// Resolves the business day a report should cover.
///
/// With no `date`, `now` is bucketed by the cutoff — so a request at 01:00 IST reports
/// the day that is still being served, not the calendar date.
pub fn resolve_business_day(
    query: &BusinessDayQuery,
    now: DateTime<Utc>,
) -> Result<BusinessDay, String> {
    if query.cutoff_hour > 23 {
        return Err(format!(
            "cutoff_hour must be 0-23, got {}",
            query.cutoff_hour
        ));
    }

    match query.date {
        None => Ok(BusinessDay::for_instant(now, query.cutoff_hour, REPORT_TZ)),
        Some(date) => {
            // Anchor inside the requested day: `cutoff_hour` local time on that date is
            // by construction its first instant, so `for_instant` labels it `date`.
            // With cutoff 0 that is 00:00, which is also correct.
            let anchor = date
                .and_hms_opt(query.cutoff_hour, 0, 0)
                .ok_or_else(|| format!("invalid date {date}"))?;
            let instant = anchor
                .and_local_timezone(REPORT_TZ)
                .earliest()
                .ok_or_else(|| {
                    format!("{date} {}:00 does not exist in IST", query.cutoff_hour)
                })?
                .with_timezone(&Utc);
            Ok(BusinessDay::for_instant(
                instant,
                query.cutoff_hour,
                REPORT_TZ,
            ))
        }
    }
}

/// `GET /api/reports/daily-pl?date=2026-07-28&cutoff_hour=3`
///
/// P&L for one business day: revenue from invoices, COGS from BOM explosion, and the
/// gross profit between them.
///
/// Revenue counts `rounded_total` for invoices whose status is in
/// `PosInvoiceStatus::REVENUE`, the same filter shift close uses, so the two reports
/// agree by construction. `round_off_total` is reported separately because the delta
/// posts to its own ledger account.
///
/// `has_unset_items` true means COGS understates cost: the items in `unset` have no
/// buying price configured and contributed zero.
async fn daily_pl(
    State(state): State<AppState>,
    Query(query): Query<BusinessDayQuery>,
) -> ApiResult<Json<DailyPlResponse>> {
    let day = resolve_business_day(&query, Utc::now()).map_err(ApiError::invalid_input)?;

    let storage = state.storage();
    let invoice_repo = storage.invoice_repo();

    let invoices = invoice_repo.summaries_between(day.start, day.end).await?;
    let lines = invoice_repo.revenue_lines_between(day.start, day.end).await?;

    // Snapshot prefetch for COGS: bounded queries, no per-line blocking for BOM/bundle.
    // Bundle children are added to the BOM seed because a bundle line's BOM is walked at level 1.
    let distinct: Vec<ItemCode> = {
        let mut seen = std::collections::HashSet::new();
        lines
            .iter()
            .filter_map(|l| {
                if seen.insert(l.item_code.clone()) {
                    Some(l.item_code.clone())
                } else {
                    None
                }
            })
            .collect()
    };
    let bundle_snapshot = storage.bundle_repo().snapshot(&distinct).await?;
    let mut bom_seed = distinct.clone();
    bom_seed.extend(bundle_snapshot.child_items());
    {
        let mut seen = std::collections::HashSet::new();
        bom_seed.retain(|c| seen.insert(c.clone()));
    }
    let bom_snapshot = storage.bom_repo().snapshot_for_items(&bom_seed).await?;
    let price_repo = storage.price_repo();

    let report = compute_daily_pl(
        &day,
        query.cutoff_hour,
        &invoices,
        &lines,
        &state.config().buying_price_list,
        &bundle_snapshot,
        &bom_snapshot,
        &price_repo,
    )?;

    Ok(Json(report))
}

/// `GET /api/reports/item-costing?date=2026-07-28&cutoff_hour=3`
///
/// COGS per item for a business day, with each item's cost basis (bundle, BOM or plain)
/// and any missing buying prices attributed to that row.
///
/// `revenue` on a row is line revenue (`rate × qty`), pre-tax and pre-discount. It does
/// not sum to the P&L's `revenue`; see [`build_item_costing`].
async fn item_costing(
    State(state): State<AppState>,
    Query(query): Query<BusinessDayQuery>,
) -> ApiResult<Json<ItemCostingResponse>> {
    let day = resolve_business_day(&query, Utc::now()).map_err(ApiError::invalid_input)?;

    let storage = state.storage();
    let invoice_repo = storage.invoice_repo();

    let invoices = invoice_repo.summaries_between(day.start, day.end).await?;
    let lines = invoice_repo.revenue_lines_between(day.start, day.end).await?;

    let distinct: Vec<ItemCode> = {
        let mut seen = std::collections::HashSet::new();
        lines
            .iter()
            .filter_map(|l| {
                if seen.insert(l.item_code.clone()) {
                    Some(l.item_code.clone())
                } else {
                    None
                }
            })
            .collect()
    };
    let bundle_snapshot = storage.bundle_repo().snapshot(&distinct).await?;
    let mut bom_seed = distinct.clone();
    bom_seed.extend(bundle_snapshot.child_items());
    {
        let mut seen = std::collections::HashSet::new();
        bom_seed.retain(|c| seen.insert(c.clone()));
    }
    let bom_snapshot = storage.bom_repo().snapshot_for_items(&bom_seed).await?;
    let price_repo = storage.price_repo();

    let aggregate = aggregate_cogs(
        &lines,
        &state.config().buying_price_list,
        &bundle_snapshot,
        &bom_snapshot,
        &price_repo,
    )?;

    let revenue = summarise_revenue(&invoices, &day);

    Ok(Json(build_item_costing(
        &day,
        query.cutoff_hour,
        revenue.invoice_count,
        aggregate,
        &line_revenue_by_item(&lines),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app;
    use crate::config::Config;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::{NaiveDate, TimeZone};
    use http_body_util::BodyExt;
    use crate::dto::reports::UnsetItems;
    use peacock_core::ids::{BomName, ItemCode};
    use peacock_core::model::PosInvoiceStatus;
    use peacock_core::ports::{Bom, BomLine, ProductBundle};
    use rust_decimal_macros::dec;
    use std::collections::HashMap;
    use tower::ServiceExt;

    // ---- Fakes -------------------------------------------------------------

    #[derive(Default)]
    struct FakeBomRepo {
        boms: HashMap<ItemCode, Bom>,
    }

    impl FakeBomRepo {
        fn insert(&mut self, item: &str, quantity: Decimal, lines: &[(&str, Decimal)]) {
            self.boms.insert(
                ItemCode::from(item),
                Bom {
                    name: BomName::new(format!("BOM-{item}")),
                    quantity,
                    items: lines
                        .iter()
                        .map(|(code, qty)| BomLine {
                            item_code: ItemCode::from(*code),
                            qty: *qty,
                        })
                        .collect(),
                },
            );
        }
    }

    impl BomRepo for FakeBomRepo {
        fn find_for_item(&self, item: &ItemCode) -> peacock_core::Result<Option<Bom>> {
            Ok(self.boms.get(item).cloned())
        }
    }

    #[derive(Default)]
    struct FakePriceRepo {
        prices: HashMap<ItemCode, Money>,
    }

    impl FakePriceRepo {
        fn insert(&mut self, item: &str, price: Decimal) {
            self.prices.insert(ItemCode::from(item), Money::new(price));
        }
    }

    impl PriceRepo for FakePriceRepo {
        fn item_price(
            &self,
            item: &ItemCode,
            _price_list: &PriceListName,
        ) -> peacock_core::Result<Option<Money>> {
            Ok(self.prices.get(item).copied())
        }
    }

    #[derive(Default)]
    struct NoBundles;

    impl ProductBundleRepo for NoBundles {
        fn find_by_new_item_code(
            &self,
            _item: &ItemCode,
        ) -> peacock_core::Result<Option<ProductBundle>> {
            Ok(None)
        }
    }

    fn buying() -> PriceListName {
        PriceListName::from("Buying")
    }

    fn line(item: &str, name: &str, qty: Decimal, rate: Decimal) -> OrderLine {
        OrderLine {
            item_code: ItemCode::from(item),
            item_name: name.to_owned(),
            qty,
            rate: Money::new(rate),
            comments: None,
            serve_priority: 0,
            indicate_course: false,
        }
    }

    /// An invoice whose `rounded_total` and `round_off` are derived, so the ledger
    /// invariant `rounded_total - grand_total == round_off` always holds in fixtures.
    fn invoice(
        name: &str,
        posted_at: DateTime<Utc>,
        status: PosInvoiceStatus,
        grand_total: Decimal,
    ) -> InvoiceSummary {
        let round_off = peacock_core::money::RoundOff::apply(Money::new(grand_total));
        InvoiceSummary {
            name: name.to_owned(),
            posted_at,
            status,
            grand_total: round_off.grand_total,
            rounded_total: round_off.rounded_total,
            round_off: round_off.round_off,
        }
    }

    /// Business day 2026-07-28 with cutoff 3, i.e. [27th 21:30 UTC, 28th 21:30 UTC).
    fn day_28() -> BusinessDay {
        BusinessDay::for_instant(
            Utc.with_ymd_and_hms(2026, 7, 28, 6, 0, 0).unwrap(),
            3,
            REPORT_TZ,
        )
    }

    async fn send(request: Request<Body>) -> axum::response::Response {
        app::build(Config::default()).oneshot(request).await.unwrap()
    }

    async fn detail(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        json["detail"].as_str().unwrap_or_default().to_owned()
    }

    // ---- Revenue: the single source of truth --------------------------------

    #[test]
    fn revenue_counts_paid_and_consolidated_only() {
        // Bug 4 regression. Shift close once counted `Paid` alone; the P&L counted
        // `("Consolidated", "Paid")`. `PosInvoiceStatus::REVENUE` is now the only list,
        // so Consolidated is in and Draft/Return are out.
        let day = day_28();
        let at = Utc.with_ymd_and_hms(2026, 7, 28, 6, 0, 0).unwrap();

        let invoices = vec![
            invoice("INV-1", at, PosInvoiceStatus::Paid, dec!(100.00)),
            invoice("INV-2", at, PosInvoiceStatus::Consolidated, dec!(250.00)),
            invoice("INV-3", at, PosInvoiceStatus::Draft, dec!(999.00)),
            invoice("INV-4", at, PosInvoiceStatus::Return, dec!(500.00)),
        ];

        let summary = summarise_revenue(&invoices, &day);

        assert_eq!(summary.revenue, Money::new(dec!(350.00)));
        assert_eq!(summary.invoice_count, 2);
        assert_eq!(summary.excluded_invoice_count, 2);
    }

    #[test]
    fn revenue_uses_rounded_total_not_grand_total() {
        // Bug 3 regression. 377.60 → 378 and 199.70 → 200, so revenue is ₹578 while the
        // grand totals sum to ₹577.30. The ₹0.70 gap is the whole point: it is what the
        // customer actually paid, and it is ledgered as `round_off_total` rather than
        // being lost.
        let day = day_28();
        let at = Utc.with_ymd_and_hms(2026, 7, 28, 6, 0, 0).unwrap();

        let invoices = vec![
            invoice("INV-1", at, PosInvoiceStatus::Paid, dec!(377.60)),
            invoice("INV-2", at, PosInvoiceStatus::Paid, dec!(199.70)),
        ];

        let summary = summarise_revenue(&invoices, &day);
        let grand_sum: Money = invoices.iter().map(|i| i.grand_total).sum();

        assert_eq!(summary.revenue, Money::new(dec!(578)));
        assert_eq!(grand_sum, Money::new(dec!(577.30)));
        assert_ne!(
            summary.revenue, grand_sum,
            "summing grand_total is the bug this guards"
        );
        // The two bases differ by exactly the round-off, so nothing is unaccounted for.
        assert_eq!(summary.revenue - grand_sum, summary.round_off_total);
    }

    #[test]
    fn round_off_deltas_can_cancel_without_hiding_the_basis() {
        // 377.60 → 378 (+0.40) and 378.40 → 378 (-0.40). Here the deltas cancel, so both
        // bases happen to agree at ₹756. A test built only on this case would pass even
        // with `grand_total`, which is why the case above exists.
        let day = day_28();
        let at = Utc.with_ymd_and_hms(2026, 7, 28, 6, 0, 0).unwrap();

        let summary = summarise_revenue(
            &[
                invoice("INV-1", at, PosInvoiceStatus::Paid, dec!(377.60)),
                invoice("INV-2", at, PosInvoiceStatus::Paid, dec!(378.40)),
            ],
            &day,
        );

        assert_eq!(summary.revenue, Money::new(dec!(756)));
        assert_eq!(summary.round_off_total, Money::ZERO);
    }

    #[test]
    fn round_off_total_accumulates_across_mixed_signs() {
        // Deltas +0.40, -0.30, +0.50 net to +0.60. Only revenue-counting invoices
        // contribute, so the Draft's delta is not in the total.
        let day = day_28();
        let at = Utc.with_ymd_and_hms(2026, 7, 28, 6, 0, 0).unwrap();

        let summary = summarise_revenue(
            &[
                invoice("INV-1", at, PosInvoiceStatus::Paid, dec!(377.60)),
                invoice("INV-2", at, PosInvoiceStatus::Consolidated, dec!(199.30)),
                invoice("INV-3", at, PosInvoiceStatus::Paid, dec!(428.50)),
                invoice("INV-4", at, PosInvoiceStatus::Draft, dec!(999.90)),
            ],
            &day,
        );

        // 378 + 199 + 429 = 1006
        assert_eq!(summary.revenue, Money::new(dec!(1006)));
        assert_eq!(summary.round_off_total, Money::new(dec!(0.60)));
        assert_eq!(summary.invoice_count, 3);
        assert_eq!(summary.excluded_invoice_count, 1);
    }

    #[test]
    fn business_day_end_is_exclusive_no_double_counting() {
        // Bug 2 regression. The 28th's business day with cutoff 3 is
        // [27th 21:30 UTC, 28th 21:30 UTC) = [28th 03:00 IST, 29th 03:00 IST).
        //
        // A 00:30 IST invoice on the 29th (19:00 UTC on the 28th) is past midnight but
        // still the 28th's trade, and it is counted exactly once. The invoice at exactly
        // `end` belongs to the next day, and the one a second before `start` to the
        // previous — an inclusive-end range would have claimed both.
        let day = day_28();
        let past_midnight = Utc.with_ymd_and_hms(2026, 7, 28, 19, 0, 0).unwrap();

        let summary = summarise_revenue(
            &[
                invoice("IN", past_midnight, PosInvoiceStatus::Paid, dec!(100.00)),
                invoice("AT-END", day.end, PosInvoiceStatus::Paid, dec!(999.00)),
                invoice(
                    "BEFORE-START",
                    day.start - chrono::Duration::seconds(1),
                    PosInvoiceStatus::Paid,
                    dec!(888.00),
                ),
            ],
            &day,
        );

        assert!(day.contains(past_midnight));
        assert!(!day.contains(day.end), "end must be exclusive");
        assert!(day.contains(day.start), "start must be inclusive");
        assert_eq!(summary.revenue, Money::new(dec!(100.00)));
        assert_eq!(summary.invoice_count, 1);
        // Out-of-range invoices are not "excluded", they are simply not this day's.
        assert_eq!(summary.excluded_invoice_count, 0);
    }

    #[test]
    fn the_same_invoice_lands_in_exactly_one_business_day() {
        // The other half of bug 2: bucket one instant against both adjacent days and
        // confirm exactly one claims it. 01:30 IST on the 28th is the 27th's trade.
        let at = Utc.with_ymd_and_hms(2026, 7, 27, 20, 0, 0).unwrap();
        let invoices = vec![invoice("INV-1", at, PosInvoiceStatus::Paid, dec!(100.00))];

        let day_27 = BusinessDay::for_instant(at, 3, REPORT_TZ);
        let day_28 = day_28();

        assert_eq!(day_27.label, NaiveDate::from_ymd_opt(2026, 7, 27).unwrap());
        assert_eq!(summarise_revenue(&invoices, &day_27).invoice_count, 1);
        assert_eq!(summarise_revenue(&invoices, &day_28).invoice_count, 0);
    }

    // ---- P&L ---------------------------------------------------------------

    #[test]
    fn gross_profit_is_revenue_minus_cogs() {
        // Revenue: 2 paid invoices, ₹360 + ₹240 = ₹600.
        // COGS: 5 cups of MASALA-CHAI at ₹7/cup (batch of 10 costing ₹70) = ₹35.
        // Gross profit ₹565, margin 94.17%.
        let day = day_28();
        let at = Utc.with_ymd_and_hms(2026, 7, 28, 6, 0, 0).unwrap();

        let mut boms = FakeBomRepo::default();
        boms.insert(
            "MASALA-CHAI",
            dec!(10),
            &[("TEA-LEAVES", dec!(10)), ("MILK", dec!(100))],
        );
        let mut prices = FakePriceRepo::default();
        prices.insert("TEA-LEAVES", dec!(2.00));
        prices.insert("MILK", dec!(0.50));

        let report = compute_daily_pl(
            &day,
            3,
            &[
                invoice("INV-1", at, PosInvoiceStatus::Paid, dec!(360)),
                invoice("INV-2", at, PosInvoiceStatus::Paid, dec!(240)),
            ],
            &[line("MASALA-CHAI", "Masala Chai", dec!(5), dec!(120))],
            &buying(),
            &NoBundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(report.revenue, Money::new(dec!(600)));
        assert_eq!(report.cogs, Money::new(dec!(35.00)));
        assert_eq!(report.gross_profit, Money::new(dec!(565.00)));
        assert_eq!(report.gross_margin_pct.as_deref(), Some("94.17"));
        assert_eq!(report.business_day, NaiveDate::from_ymd_opt(2026, 7, 28).unwrap());
        assert_eq!(report.invoice_count, 2);
        assert!(!report.has_unset_items);
    }

    #[test]
    fn pl_revenue_reconciles_with_shift_close() {
        // The regression guard for bugs 3 and 4: given the same invoices, shift close and
        // the P&L must produce byte-identical revenue. `shift_revenue` is the shift-close
        // path; `summarise_revenue` is this report's.
        let day = day_28();
        let at = Utc.with_ymd_and_hms(2026, 7, 28, 6, 0, 0).unwrap();

        let invoices = vec![
            invoice("INV-1", at, PosInvoiceStatus::Paid, dec!(377.60)),
            invoice("INV-2", at, PosInvoiceStatus::Consolidated, dec!(199.70)),
            invoice("INV-3", at, PosInvoiceStatus::Draft, dec!(999.00)),
        ];

        let report = compute_daily_pl(
            &day,
            3,
            &invoices,
            &[],
            &buying(),
            &NoBundles,
            &FakeBomRepo::default(),
            &FakePriceRepo::default(),
        )
        .unwrap();

        let shift_total = peacock_core::businessday::shift_revenue(&invoices);

        assert_eq!(report.revenue, shift_total);
        reconcile_with_shift(&report, shift_total).expect("shift and P&L must agree");
    }

    #[test]
    fn reconcile_rejects_a_drifted_shift_total() {
        let day = day_28();
        let at = Utc.with_ymd_and_hms(2026, 7, 28, 6, 0, 0).unwrap();

        let report = compute_daily_pl(
            &day,
            3,
            &[invoice("INV-1", at, PosInvoiceStatus::Paid, dec!(377.60))],
            &[],
            &buying(),
            &NoBundles,
            &FakeBomRepo::default(),
            &FakePriceRepo::default(),
        )
        .unwrap();

        // 377.60 is the grand total — what the buggy shift close would have summed.
        let err = reconcile_with_shift(&report, Money::new(dec!(377.60))).unwrap_err();
        let api: ApiError = err.into();
        assert_eq!(api.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn pl_surfaces_unset_bom_items() {
        // SANDWICH BOM: 2× BREAD @ ₹5 + 1× CHEESE (no buying price).
        // COGS is ₹10 for 1 unit and CHEESE is named, so the operator can see the P&L is
        // understating cost rather than being handed a silently low number.
        let day = day_28();
        let at = Utc.with_ymd_and_hms(2026, 7, 28, 6, 0, 0).unwrap();

        let mut boms = FakeBomRepo::default();
        boms.insert(
            "SANDWICH",
            dec!(1),
            &[("BREAD", dec!(2)), ("CHEESE", dec!(1))],
        );
        let mut prices = FakePriceRepo::default();
        prices.insert("BREAD", dec!(5));

        let report = compute_daily_pl(
            &day,
            3,
            &[invoice("INV-1", at, PosInvoiceStatus::Paid, dec!(80))],
            &[line("SANDWICH", "Sandwich", dec!(1), dec!(80))],
            &buying(),
            &NoBundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(report.cogs, Money::new(dec!(10)));
        assert_eq!(report.unset.bom_items, vec!["CHEESE"]);
        assert!(report.has_unset_items);
    }

    #[test]
    fn zero_revenue_day_omits_margin_but_still_reports_cogs() {
        let day = day_28();
        let mut prices = FakePriceRepo::default();
        prices.insert("COLA", dec!(15));

        let report = compute_daily_pl(
            &day,
            3,
            &[],
            &[line("COLA", "Cola", dec!(2), dec!(40))],
            &buying(),
            &NoBundles,
            &FakeBomRepo::default(),
            &prices,
        )
        .unwrap();

        assert_eq!(report.revenue, Money::ZERO);
        assert_eq!(report.cogs, Money::new(dec!(30)));
        assert_eq!(report.gross_profit, Money::new(dec!(-30)));
        assert_eq!(report.gross_margin_pct, None, "no margin without revenue");

        let json = serde_json::to_value(&report).unwrap();
        assert!(json.get("gross_margin_pct").is_none());
        assert_eq!(json["gross_profit"], "-30");
    }

    #[test]
    fn pl_money_fields_serialise_as_strings() {
        let day = day_28();
        let report = assemble_daily_pl(
            &day,
            3,
            RevenueSummary {
                revenue: Money::new(dec!(600.00)),
                round_off_total: Money::new(dec!(0.40)),
                invoice_count: 2,
                excluded_invoice_count: 1,
            },
            CogsAggregate {
                total: Money::new(dec!(35.00)),
                items: vec![],
                unset: UnsetItems::default(),
            },
        );

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["revenue"], "600.00");
        assert_eq!(json["cogs"], "35.00");
        assert_eq!(json["gross_profit"], "565.00");
        assert_eq!(json["round_off_total"], "0.40");
        assert_eq!(json["business_day"], "2026-07-28");
        assert_eq!(json["excluded_invoice_count"], 1);
    }

    // ---- Item costing ------------------------------------------------------

    #[test]
    fn item_costing_rows_carry_cost_basis_and_per_item_profit() {
        // THALI is a bundle (₹40/unit), SANDWICH has a BOM (₹10/unit with CHEESE unset),
        // COLA is plain (₹15/unit). One report, all three cost bases.
        struct OneBundle;
        impl ProductBundleRepo for OneBundle {
            fn find_by_new_item_code(
                &self,
                item: &ItemCode,
            ) -> peacock_core::Result<Option<ProductBundle>> {
                if item.as_str() == "THALI" {
                    Ok(Some(ProductBundle {
                        new_item_code: ItemCode::from("THALI"),
                        items: vec![
                            peacock_core::ports::ProductBundleLine {
                                item_code: ItemCode::from("ROTI"),
                                qty: dec!(2),
                            },
                            peacock_core::ports::ProductBundleLine {
                                item_code: ItemCode::from("DAL"),
                                qty: dec!(1),
                            },
                        ],
                    }))
                } else {
                    Ok(None)
                }
            }
        }

        let mut boms = FakeBomRepo::default();
        boms.insert(
            "SANDWICH",
            dec!(1),
            &[("BREAD", dec!(2)), ("CHEESE", dec!(1))],
        );

        let mut prices = FakePriceRepo::default();
        prices.insert("ROTI", dec!(5));
        prices.insert("DAL", dec!(30));
        prices.insert("BREAD", dec!(5));
        prices.insert("COLA", dec!(15));

        let lines = vec![
            line("THALI", "Thali", dec!(2), dec!(150)),
            line("SANDWICH", "Sandwich", dec!(1), dec!(80)),
            line("COLA", "Cola", dec!(3), dec!(40)),
        ];

        let aggregate = aggregate_cogs(&lines, &buying(), &OneBundle, &boms, &prices).unwrap();
        let report = build_item_costing(
            &day_28(),
            3,
            1,
            aggregate,
            &line_revenue_by_item(&lines),
        );

        // Sorted by item_code.
        let codes: Vec<&str> = report.items.iter().map(|r| r.item_code.as_str()).collect();
        assert_eq!(codes, vec!["COLA", "SANDWICH", "THALI"]);

        let cola = &report.items[0];
        assert_eq!(cola.cost_basis, crate::dto::reports::CostBasis::Plain);
        assert_eq!(cola.cogs, Money::new(dec!(45)));
        assert_eq!(cola.revenue, Money::new(dec!(120)));
        assert_eq!(cola.gross_profit, Money::new(dec!(75)));

        let sandwich = &report.items[1];
        assert_eq!(sandwich.cost_basis, crate::dto::reports::CostBasis::Bom);
        assert_eq!(sandwich.cogs, Money::new(dec!(10)));
        assert_eq!(sandwich.unset.bom_items, vec!["CHEESE"]);

        let thali = &report.items[2];
        assert_eq!(thali.cost_basis, crate::dto::reports::CostBasis::Bundle);
        assert_eq!(thali.cogs, Money::new(dec!(80)));
        assert_eq!(thali.revenue, Money::new(dec!(300)));

        // ₹45 + ₹10 + ₹80, and ₹120 + ₹80 + ₹300.
        assert_eq!(report.total_cogs, Money::new(dec!(135)));
        assert_eq!(report.total_line_revenue, Money::new(dec!(500)));
        assert!(report.has_unset_items);
        assert_eq!(report.unset.bom_items, vec!["CHEESE"]);
    }

    #[test]
    fn item_costing_total_cogs_equals_the_pl_cogs() {
        // Both endpoints must report the same cost for the same day; they share
        // `aggregate_cogs`, and this pins that they stay wired to it.
        let day = day_28();
        let at = Utc.with_ymd_and_hms(2026, 7, 28, 6, 0, 0).unwrap();

        let mut boms = FakeBomRepo::default();
        boms.insert(
            "MASALA-CHAI",
            dec!(10),
            &[("TEA-LEAVES", dec!(10)), ("MILK", dec!(100))],
        );
        let mut prices = FakePriceRepo::default();
        prices.insert("TEA-LEAVES", dec!(2.00));
        prices.insert("MILK", dec!(0.50));
        prices.insert("COLA", dec!(15));

        let lines = vec![
            line("MASALA-CHAI", "Masala Chai", dec!(5), dec!(120)),
            line("COLA", "Cola", dec!(2), dec!(40)),
        ];

        let pl = compute_daily_pl(
            &day,
            3,
            &[invoice("INV-1", at, PosInvoiceStatus::Paid, dec!(680))],
            &lines,
            &buying(),
            &NoBundles,
            &boms,
            &prices,
        )
        .unwrap();

        let aggregate = aggregate_cogs(&lines, &buying(), &NoBundles, &boms, &prices).unwrap();
        let costing = build_item_costing(&day, 3, 1, aggregate, &line_revenue_by_item(&lines));

        assert_eq!(costing.total_cogs, pl.cogs);
        assert_eq!(costing.total_cogs, Money::new(dec!(65.00)));
    }

    #[test]
    fn line_revenue_sums_rate_times_qty_per_item() {
        let revenue = line_revenue_by_item(&[
            line("COLA", "Cola", dec!(2), dec!(40)),
            line("COLA", "Cola", dec!(1), dec!(40)),
            line("CHAI", "Chai", dec!(4), dec!(30.50)),
        ]);

        assert_eq!(revenue["COLA"], Money::new(dec!(120)));
        assert_eq!(revenue["CHAI"], Money::new(dec!(122.00)));
    }

    // ---- Business-day resolution -------------------------------------------

    #[test]
    fn explicit_date_resolves_to_that_business_day() {
        let query = BusinessDayQuery {
            date: Some(NaiveDate::from_ymd_opt(2026, 7, 28).unwrap()),
            cutoff_hour: 3,
        };
        let day = resolve_business_day(&query, Utc::now()).unwrap();

        assert_eq!(day.label, NaiveDate::from_ymd_opt(2026, 7, 28).unwrap());
        // [28th 03:00 IST, 29th 03:00 IST) = [27th 21:30 UTC, 28th 21:30 UTC)
        assert_eq!(day.start, Utc.with_ymd_and_hms(2026, 7, 27, 21, 30, 0).unwrap());
        assert_eq!(day.end, Utc.with_ymd_and_hms(2026, 7, 28, 21, 30, 0).unwrap());
    }

    #[test]
    fn absent_date_buckets_now_by_the_cutoff() {
        // 01:30 IST on the 28th (20:00 UTC on the 27th) is still the 27th's business day
        // with cutoff 3 — the shift is mid-service, not over.
        let query = BusinessDayQuery {
            date: None,
            cutoff_hour: 3,
        };
        let day = resolve_business_day(
            &query,
            Utc.with_ymd_and_hms(2026, 7, 27, 20, 0, 0).unwrap(),
        )
        .unwrap();

        assert_eq!(day.label, NaiveDate::from_ymd_opt(2026, 7, 27).unwrap());
    }

    #[test]
    fn cutoff_zero_gives_calendar_days() {
        let query = BusinessDayQuery {
            date: Some(NaiveDate::from_ymd_opt(2026, 7, 28).unwrap()),
            cutoff_hour: 0,
        };
        let day = resolve_business_day(&query, Utc::now()).unwrap();

        assert_eq!(day.label, NaiveDate::from_ymd_opt(2026, 7, 28).unwrap());
        // Midnight IST is 18:30 UTC the previous day.
        assert_eq!(day.start, Utc.with_ymd_and_hms(2026, 7, 27, 18, 30, 0).unwrap());
    }

    #[test]
    fn out_of_range_cutoff_hour_is_rejected_not_asserted() {
        // `BusinessDay::for_instant` asserts on `cutoff_hour >= 24`, which would be a
        // panic mid-request. Validation happens before that call.
        let query = BusinessDayQuery {
            date: None,
            cutoff_hour: 25,
        };
        assert!(resolve_business_day(&query, Utc::now())
            .unwrap_err()
            .contains("cutoff_hour"));
    }

    // ---- HTTP contract -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn daily_pl_accepts_date_and_cutoff_and_reaches_the_storage_gap() {
        let response = send(
            Request::builder()
                .uri("/api/reports/daily-pl?date=2026-07-28&cutoff_hour=3")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        // Wired to Postgres (W1-C): valid query reaches storage and returns 200 (empty day → zeroed P&L).
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["business_day"], "2026-07-28");
        assert_eq!(json["cutoff_hour"], 3);
        assert_eq!(json["revenue"], "0");
        assert_eq!(json["cogs"], "0");
        // Money fields must be strings, never numbers.
        assert!(json["revenue"].is_string());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn daily_pl_defaults_to_todays_business_day() {
        let response = send(
            Request::builder()
                .uri("/api/reports/daily-pl")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        // No query → today's business day, still 200 (empty).
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json.get("business_day").is_some());
        assert_eq!(json["revenue"], "0");
    }

    #[tokio::test]
    async fn daily_pl_rejects_bad_cutoff_hour_with_400() {
        let response = send(
            Request::builder()
                .uri("/api/reports/daily-pl?cutoff_hour=99")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(detail(response).await.contains("cutoff_hour"));
    }

    #[tokio::test]
    async fn daily_pl_rejects_malformed_date_with_400() {
        let response = send(
            Request::builder()
                .uri("/api/reports/daily-pl?date=not-a-date")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn item_costing_accepts_the_same_query_shape() {
        let response = send(
            Request::builder()
                .uri("/api/reports/item-costing?date=2026-07-28&cutoff_hour=3")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["business_day"], "2026-07-28");
        assert_eq!(json["total_cogs"], "0");
        assert_eq!(json["total_line_revenue"], "0");
        assert_eq!(json["has_unset_items"], false);
    }

    #[tokio::test]
    async fn item_costing_rejects_bad_cutoff_hour_with_400() {
        let response = send(
            Request::builder()
                .uri("/api/reports/item-costing?cutoff_hour=24")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn report_routes_are_registered_and_get_only() {
        for uri in ["/api/reports/daily-pl", "/api/reports/item-costing"] {
            let get = send(Request::builder().uri(uri).body(Body::empty()).unwrap()).await;
            assert_ne!(
                get.status(),
                StatusCode::NOT_FOUND,
                "route {uri} must be registered"
            );

            let post = send(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
        }
    }

    #[tokio::test]
    async fn report_errors_are_problem_json_with_a_request_id() {
        let response = send(
            Request::builder()
                .uri("/api/reports/daily-pl?cutoff_hour=99")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
            crate::error::PROBLEM_JSON
        );
        assert!(response.headers().get("x-request-id").is_some());

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], 400);
        assert_eq!(json["instance"], "/api/reports/daily-pl");
    }
}
