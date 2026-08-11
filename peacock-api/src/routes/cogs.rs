//! COGS calculation endpoint (Lane 3I).
//!
//! - `POST /api/cogs/calculate` — COGS for one invoice or a business-day range
//!
//! ## Where the arithmetic lives
//!
//! Not here. Every rupee comes from [`peacock_core::cogs::cogs_for_item_with_bundles`],
//! the same entry point the parity harness drives against the Python oracle
//! (`peacock-parity/src/main.rs:281`). This module only groups invoice lines and shapes
//! the result for the wire, so the API layer cannot introduce a second cost basis.
//!
//! That buys three properties for free, all pinned by the tests below:
//!
//! - 2-level BOM explosion (`MAX_LEVEL = 2`); a level-3 BOM is priced as a leaf.
//! - `bom.quantity` normalisation — the `/ quantity` that v1 dropped and scaled COGS
//!   by the batch size (ury_daily_p_and_l.py:38).
//! - bundle → BOM → plain precedence as a partition, not a fallback chain.
//!
//! ## Why lines are grouped before costing
//!
//! [`aggregate_cogs`] sums quantities per `item_code` and costs each item once. Upstream
//! does the same thing in SQL (`SUM(b.qty)` grouped by item, ury_daily_p_and_l.py:73-172),
//! and it is arithmetically identical: per-unit cost does not depend on `qty`, and both
//! the guard (`if buying_price > 0`) and the extension (`× qty`) are applied after it, so
//! `cost(q1) + cost(q2) == cost(q1 + q2)` exactly in `Decimal`. Costing once per item
//! also means one BOM walk per item instead of one per line.
//!
//! ## Rounding
//!
//! None. `CogsResult::cost` is full-precision `Decimal` and stays that way through the
//! response. Rounding COGS at the API boundary would make this endpoint disagree with
//! the parity harness.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};

use peacock_core::cogs::cogs_for_item_with_bundles;
use peacock_core::ids::{ItemCode, PriceListName};
use peacock_core::model::OrderLine;
use peacock_core::money::Money;
use peacock_core::ports::{BomRepo, PriceRepo, ProductBundleRepo};
use rust_decimal::Decimal;

use crate::dto::reports::{
    CogsCalculateRequest, CogsCalculateResponse, CogsScope, CostBasis, ItemCogsBreakdown,
    UnsetItems,
};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/cogs/calculate", post(calculate_cogs))
}

/// The aggregated COGS of a set of invoice lines.
#[derive(Debug, Clone, PartialEq)]
pub struct CogsAggregate {
    /// Unrounded total cost.
    pub total: Money,
    /// One row per distinct `item_code`, sorted by code.
    pub items: Vec<ItemCogsBreakdown>,
    /// Union of the per-item unset lists, deduplicated and sorted, kept split by label.
    pub unset: UnsetItems,
}

impl CogsAggregate {
    pub fn has_unset_items(&self) -> bool {
        !self.unset.is_empty()
    }
}

/// Which of upstream's three cost bases applies to an item.
///
/// Bundle first, and it wins outright: the bundle query does not join `tabBOM` at all
/// and both other queries require `d.new_item_code IS NULL`
/// (ury_daily_p_and_l.py:147-172). An item that is both a Product Bundle and has an
/// active default BOM is priced as a bundle, and its own BOM is never consulted.
pub fn cost_basis_for(
    item: &ItemCode,
    bundles: &dyn ProductBundleRepo,
    boms: &dyn BomRepo,
) -> Result<CostBasis, peacock_core::Error> {
    if bundles.find_by_new_item_code(item)?.is_some() {
        return Ok(CostBasis::Bundle);
    }
    if boms.find_for_item(item)?.is_some() {
        return Ok(CostBasis::Bom);
    }
    Ok(CostBasis::Plain)
}

/// Groups lines by `item_code`, costs each item once, and aggregates.
///
/// Quantities are summed before costing; see the module docs for why that is exact.
/// The `item_name` of the first line seen for an item wins — upstream reports one name
/// per item and the code is the identity.
pub fn aggregate_cogs(
    lines: &[OrderLine],
    buying_price_list: &PriceListName,
    bundles: &dyn ProductBundleRepo,
    boms: &dyn BomRepo,
    prices: &dyn PriceRepo,
) -> Result<CogsAggregate, peacock_core::Error> {
    // BTreeMap: deterministic `item_code` ordering without a sort pass, and stable
    // output is what makes the response diffable between runs.
    let mut grouped: BTreeMap<ItemCode, (String, Decimal)> = BTreeMap::new();
    for line in lines {
        let entry = grouped
            .entry(line.item_code.clone())
            .or_insert_with(|| (line.item_name.clone(), Decimal::ZERO));
        entry.1 += line.qty;
    }

    let mut total = Money::ZERO;
    let mut items = Vec::with_capacity(grouped.len());
    let mut union = peacock_core::cogs::CogsResult::default();

    for (item_code, (item_name, qty)) in grouped {
        let result =
            cogs_for_item_with_bundles(&item_code, qty, buying_price_list, bundles, boms, prices)?;
        let cost_basis = cost_basis_for(&item_code, bundles, boms)?;

        total = total + result.cost;
        items.push(ItemCogsBreakdown {
            item_code: item_code.as_str().to_owned(),
            item_name,
            qty,
            cogs: result.cost,
            cost_basis,
            unset: UnsetItems::from_cogs(&result),
        });

        // `merge` unions and sorts each of the three lists; its cost addition is
        // discarded because `total` already accumulated it above.
        union = union.merge(peacock_core::cogs::CogsResult {
            cost: Money::ZERO,
            ..result
        });
    }

    Ok(CogsAggregate {
        total,
        items,
        unset: UnsetItems::from_cogs(&union),
    })
}

/// `POST /api/cogs/calculate`
///
/// Calculates COGS for a single invoice or a business-day range.
///
/// ## Request
/// ```json
/// { "invoice": "ACC-PSINV-2026-00042" }
/// ```
/// or
/// ```json
/// { "from_date": "2026-07-01", "to_date": "2026-07-31", "cutoff_hour": 3 }
/// ```
///
/// Exactly one scope is required. Supplying both, neither, or half a date range is a 400.
///
/// ## Response
/// Total COGS, a per-item breakdown with each item's cost basis, and the three
/// unset-price lists. `has_unset_items` is the flag to surface in the UI: when true the
/// COGS figure understates cost by whatever the listed items would have contributed.
async fn calculate_cogs(
    State(state): State<AppState>,
    Json(req): Json<CogsCalculateRequest>,
) -> ApiResult<Json<CogsCalculateResponse>> {
    let scope = req.scope().map_err(ApiError::invalid_input)?;

    let storage = state.storage();
    let invoice_repo = storage.invoice_repo();

    // Collect lines from invoices in scope, and determine invoice_count.
    // Both use half-open [start, end) via BusinessDay so a midnight invoice belongs to exactly one day.
    let (lines, invoice_count): (Vec<OrderLine>, usize) = match &scope {
        CogsScope::Invoice(name) => {
            let invoice = invoice_repo
                .get(&peacock_core::ids::InvoiceName::from(name.as_str()))
                .await?;
            let lines = invoice
                .lines
                .into_iter()
                .map(|line| OrderLine {
                    item_code: line.item_code,
                    item_name: line.item_name,
                    qty: line.qty,
                    rate: line.rate,
                    comments: line.comments,
                    serve_priority: line.serve_priority,
                    indicate_course: line.indicate_course,
                })
                .collect::<Vec<_>>();
            (lines, 1)
        }
        CogsScope::DateRange { from, to } => {
            use peacock_core::businessday::BusinessDay;
            use crate::routes::reports::REPORT_TZ;

            let start_day = BusinessDay::for_instant(
                from.and_hms_opt(req.cutoff_hour, 0, 0)
                    .ok_or_else(|| ApiError::invalid_input("invalid from_date"))?
                    .and_local_timezone(REPORT_TZ)
                    .earliest()
                    .ok_or_else(|| ApiError::invalid_input("from_date does not exist in IST"))?
                    .with_timezone(&chrono::Utc),
                req.cutoff_hour,
                REPORT_TZ,
            );
            let end_day = BusinessDay::for_instant(
                to.and_hms_opt(req.cutoff_hour, 0, 0)
                    .ok_or_else(|| ApiError::invalid_input("invalid to_date"))?
                    .and_local_timezone(REPORT_TZ)
                    .earliest()
                    .ok_or_else(|| ApiError::invalid_input("to_date does not exist in IST"))?
                    .with_timezone(&chrono::Utc),
                req.cutoff_hour,
                REPORT_TZ,
            );

            let start = start_day.start;
            let end = end_day.end;

            // Lines already filtered to REVENUE statuses at SQL layer.
            let lines = invoice_repo.revenue_lines_between(start, end).await?;
            let summaries = invoice_repo.summaries_between(start, end).await?;
            let count = summaries
                .iter()
                .filter(|s| s.status.counts_as_revenue())
                .count();
            (lines, count)
        }
    };

    // --- Snapshot prefetch: bounded queries, no per-line blocking ---------------
    // Cost basis precedence is bundle → BOM → plain. A bundle child that has a BOM
    // must still be priced at level 1 (the bundle adds no depth), so the BOM seed
    // includes both the sold items and the bundle's children.
    let distinct: Vec<ItemCode> = {
        use std::collections::HashSet;
        let mut seen: HashSet<ItemCode> = HashSet::new();
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
    // Dedup seed for the BOM snapshot (contains distinct + children, may overlap)
    {
        use std::collections::HashSet;
        let mut seen: HashSet<ItemCode> = HashSet::new();
        bom_seed.retain(|c| seen.insert(c.clone()));
    }
    let bom_snapshot = storage.bom_repo().snapshot_for_items(&bom_seed).await?;

    // Price repo remains blocking per leaf (multi_thread runtime required), but
    // BOM and bundle are now in-memory. If needed, this can be prefetched via
    // PgPriceRepo::item_prices_batch_async into an in-memory map to eliminate all blocking.
    let price_repo = storage.price_repo();

    let aggregate = aggregate_cogs(
        &lines,
        &state.config().buying_price_list,
        &bundle_snapshot,
        &bom_snapshot,
        &price_repo,
    )?;

    Ok(Json(response_for(scope, invoice_count, aggregate)))
}

/// Shapes an aggregate into the wire response for a scope.
///
/// Split out from the handler so the response contract is testable before storage lands.
pub fn response_for(
    scope: CogsScope,
    invoice_count: usize,
    aggregate: CogsAggregate,
) -> CogsCalculateResponse {
    let has_unset_items = aggregate.has_unset_items();
    let (scope_label, invoice, from_date, to_date) = match scope {
        CogsScope::Invoice(name) => ("invoice", Some(name), None, None),
        CogsScope::DateRange { from, to } => ("date_range", None, Some(from), Some(to)),
    };

    CogsCalculateResponse {
        scope: scope_label.to_owned(),
        invoice,
        from_date,
        to_date,
        invoice_count,
        cogs: aggregate.total,
        items: aggregate.items,
        unset: aggregate.unset,
        has_unset_items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app;
    use crate::config::Config;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::NaiveDate;
    use http_body_util::BodyExt;
    use peacock_core::ports::{Bom, BomLine, ProductBundle, ProductBundleLine};
    use rust_decimal_macros::dec;
    use std::collections::HashMap;
    use tower::ServiceExt;

    // ---- In-memory repos, mirroring `peacock_core::cogs`'s test fakes -------

    #[derive(Default)]
    struct FakeBomRepo {
        boms: HashMap<ItemCode, Bom>,
    }

    impl FakeBomRepo {
        fn insert(&mut self, item: &str, quantity: Decimal, lines: &[(&str, Decimal)]) {
            self.boms.insert(
                ItemCode::from(item),
                Bom {
                    name: peacock_core::ids::BomName::new(format!("BOM-{item}")),
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
    struct FakeBundleRepo {
        bundles: HashMap<ItemCode, ProductBundle>,
    }

    impl FakeBundleRepo {
        fn insert(&mut self, new_item_code: &str, lines: &[(&str, Decimal)]) {
            self.bundles.insert(
                ItemCode::from(new_item_code),
                ProductBundle {
                    new_item_code: ItemCode::from(new_item_code),
                    items: lines
                        .iter()
                        .map(|(code, qty)| ProductBundleLine {
                            item_code: ItemCode::from(*code),
                            qty: *qty,
                        })
                        .collect(),
                },
            );
        }
    }

    impl ProductBundleRepo for FakeBundleRepo {
        fn find_by_new_item_code(
            &self,
            item: &ItemCode,
        ) -> peacock_core::Result<Option<ProductBundle>> {
            Ok(self.bundles.get(item).cloned())
        }
    }

    pub(crate) fn line(item: &str, name: &str, qty: Decimal, rate: Decimal) -> OrderLine {
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

    fn buying() -> PriceListName {
        PriceListName::from("Buying")
    }

    async fn send(request: Request<Body>) -> axum::response::Response {
        app::build(Config::default()).oneshot(request).await.unwrap()
    }

    async fn post_calculate(body: serde_json::Value) -> axum::response::Response {
        send(
            Request::builder()
                .method("POST")
                .uri("/api/cogs/calculate")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
    }

    async fn detail(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        json["detail"].as_str().unwrap_or_default().to_owned()
    }

    // ---- Arithmetic: the money tests ---------------------------------------

    #[test]
    fn bom_quantity_is_normalised_the_10x_bug() {
        // The v1 defect this lane exists to keep out of the API layer.
        //
        // MASALA-CHAI BOM produces 10 cups: 10g TEA @ ₹2 + 100ml MILK @ ₹0.50 = ₹70
        // batch → ₹7/cup. Selling 5 cups is ₹35.
        //
        // Without `/ bom.quantity` the answer is ₹350 — wrong by exactly 10×.
        let mut boms = FakeBomRepo::default();
        boms.insert(
            "MASALA-CHAI",
            dec!(10),
            &[("TEA-LEAVES", dec!(10)), ("MILK", dec!(100))],
        );

        let mut prices = FakePriceRepo::default();
        prices.insert("TEA-LEAVES", dec!(2.00));
        prices.insert("MILK", dec!(0.50));

        let aggregate = aggregate_cogs(
            &[line("MASALA-CHAI", "Masala Chai", dec!(5), dec!(30))],
            &buying(),
            &FakeBundleRepo::default(),
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(aggregate.total, Money::new(dec!(35.00)));
        assert_ne!(aggregate.total, Money::new(dec!(350.00)), "the 10x bug");
        assert_eq!(aggregate.items[0].cost_basis, CostBasis::Bom);
        assert!(!aggregate.has_unset_items());
    }

    #[test]
    fn two_level_bom_explodes_both_levels() {
        // Matches parity fixture 08_cogs_two_level_bom to the paisa.
        //
        // BURGER batch of 5: 5×₹10 PATTY + 5×₹5 BUN = ₹75 → ₹15/unit
        // COMBO-MEAL (qty 1): 2×₹15 + 1×₹20 FRIES = ₹50/unit; 3 sold → ₹150.
        let mut boms = FakeBomRepo::default();
        boms.insert(
            "COMBO-MEAL",
            dec!(1),
            &[("BURGER", dec!(2)), ("FRIES", dec!(1))],
        );
        boms.insert("BURGER", dec!(5), &[("PATTY", dec!(5)), ("BUN", dec!(5))]);

        let mut prices = FakePriceRepo::default();
        prices.insert("PATTY", dec!(10));
        prices.insert("BUN", dec!(5));
        prices.insert("FRIES", dec!(20));

        let aggregate = aggregate_cogs(
            &[line("COMBO-MEAL", "Combo Meal", dec!(3), dec!(200))],
            &buying(),
            &FakeBundleRepo::default(),
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(aggregate.total, Money::new(dec!(150)));
    }

    #[test]
    fn third_bom_level_is_priced_as_a_leaf_max_level_two() {
        // MAX_LEVEL = 2. DOUGH has a BOM but sits at max depth, so it is priced from
        // `Item Price` (₹30) and FLOUR/WATER are never consulted.
        //
        // BASE batch of 2: 2×₹30 = ₹60 → ₹30/unit; PIZZA (qty 1): 1×₹30 = ₹30.
        assert_eq!(peacock_core::cogs::MAX_LEVEL, 2);

        let mut boms = FakeBomRepo::default();
        boms.insert("PIZZA", dec!(1), &[("BASE", dec!(1))]);
        boms.insert("BASE", dec!(2), &[("DOUGH", dec!(2))]);
        boms.insert("DOUGH", dec!(10), &[("FLOUR", dec!(10)), ("WATER", dec!(5))]);

        let mut prices = FakePriceRepo::default();
        prices.insert("DOUGH", dec!(30));
        prices.insert("FLOUR", dec!(1));
        prices.insert("WATER", dec!(0.5));

        let aggregate = aggregate_cogs(
            &[line("PIZZA", "Pizza", dec!(1), dec!(250))],
            &buying(),
            &FakeBundleRepo::default(),
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(aggregate.total, Money::new(dec!(30)));
        assert!(aggregate.unset.bom_items.is_empty());
    }

    #[test]
    fn bundle_wins_over_bom_precedence_is_a_partition() {
        // THALI is both a Product Bundle and has an active default BOM. Upstream prices
        // it as a bundle and ignores the BOM entirely (ury_daily_p_and_l.py:170).
        //
        // Bundle: 2× ROTI @ ₹5 + 1× DAL @ ₹30 = ₹40/unit; 2 sold → ₹80.
        // Its BOM would have produced ₹1000 — the number that appears if precedence
        // degrades into a fallback chain.
        let mut bundles = FakeBundleRepo::default();
        bundles.insert("THALI", &[("ROTI", dec!(2)), ("DAL", dec!(1))]);

        let mut boms = FakeBomRepo::default();
        boms.insert("THALI", dec!(1), &[("GOLD-LEAF", dec!(1))]);

        let mut prices = FakePriceRepo::default();
        prices.insert("ROTI", dec!(5));
        prices.insert("DAL", dec!(30));
        prices.insert("GOLD-LEAF", dec!(500));

        let aggregate = aggregate_cogs(
            &[line("THALI", "Thali", dec!(2), dec!(150))],
            &buying(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(aggregate.total, Money::new(dec!(80)));
        assert_eq!(aggregate.items[0].cost_basis, CostBasis::Bundle);
    }

    #[test]
    fn bundle_adds_no_bom_depth() {
        // A bundle line's BOM enters at level 1, the same entry point as a top-level BOM
        // item (:231 calls the same function as :201), so it still gets both levels.
        //
        // PLATTER bundle → 1× COMBO-MEAL, whose 2-level BOM costs ₹50/unit.
        // If the bundle consumed a level, BURGER would be priced as a leaf; it has no
        // `Item Price`, so the cost would collapse to ₹20 and BURGER would surface as
        // unset. The empty unset list is what pins this.
        let mut bundles = FakeBundleRepo::default();
        bundles.insert("PLATTER", &[("COMBO-MEAL", dec!(1))]);

        let mut boms = FakeBomRepo::default();
        boms.insert(
            "COMBO-MEAL",
            dec!(1),
            &[("BURGER", dec!(2)), ("FRIES", dec!(1))],
        );
        boms.insert("BURGER", dec!(5), &[("PATTY", dec!(5)), ("BUN", dec!(5))]);

        let mut prices = FakePriceRepo::default();
        prices.insert("PATTY", dec!(10));
        prices.insert("BUN", dec!(5));
        prices.insert("FRIES", dec!(20));

        let aggregate = aggregate_cogs(
            &[line("PLATTER", "Platter", dec!(1), dec!(400))],
            &buying(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(aggregate.total, Money::new(dec!(50)));
        assert!(aggregate.unset.bom_items.is_empty());
    }

    #[test]
    fn unset_bom_items_surface_and_contribute_zero() {
        // SANDWICH BOM: 2× BREAD @ ₹5 + 1× CHEESE (no price).
        // Cost = ₹10/unit × 2 = ₹20; CHEESE stays visible under BOM SUB ITEMS rather
        // than being silently absorbed.
        let mut boms = FakeBomRepo::default();
        boms.insert(
            "SANDWICH",
            dec!(1),
            &[("BREAD", dec!(2)), ("CHEESE", dec!(1))],
        );

        let mut prices = FakePriceRepo::default();
        prices.insert("BREAD", dec!(5));

        let aggregate = aggregate_cogs(
            &[line("SANDWICH", "Sandwich", dec!(2), dec!(80))],
            &buying(),
            &FakeBundleRepo::default(),
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(aggregate.total, Money::new(dec!(20)));
        assert_eq!(aggregate.unset.bom_items, vec!["CHEESE"]);
        assert!(aggregate.unset.item_prices.is_empty());
        assert!(aggregate.unset.bundle_items.is_empty());
        assert!(aggregate.has_unset_items());
        // And the row itself carries its own gap, so a UI can flag the exact line.
        assert_eq!(aggregate.items[0].unset.bom_items, vec!["CHEESE"]);
    }

    #[test]
    fn the_three_unset_lists_stay_separate_by_label() {
        // One order hitting all three buckets at once:
        // - NO-PRICE   plain item, no price      → item_prices  (ITEMS)
        // - MEAL       bundle child PICKLE       → bundle_items (BUNDLE SUB ITEMS)
        // - SANDWICH   BOM ingredient CHEESE     → bom_items    (BOM SUB ITEMS)
        let mut bundles = FakeBundleRepo::default();
        bundles.insert("MEAL", &[("RICE", dec!(1)), ("PICKLE", dec!(1))]);

        let mut boms = FakeBomRepo::default();
        boms.insert(
            "SANDWICH",
            dec!(1),
            &[("BREAD", dec!(2)), ("CHEESE", dec!(1))],
        );

        let mut prices = FakePriceRepo::default();
        prices.insert("RICE", dec!(20));
        prices.insert("BREAD", dec!(5));

        let aggregate = aggregate_cogs(
            &[
                line("NO-PRICE", "Mystery", dec!(1), dec!(10)),
                line("MEAL", "Meal", dec!(1), dec!(100)),
                line("SANDWICH", "Sandwich", dec!(1), dec!(80)),
            ],
            &buying(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(aggregate.unset.item_prices, vec!["NO-PRICE"]);
        assert_eq!(aggregate.unset.bundle_items, vec!["PICKLE"]);
        assert_eq!(aggregate.unset.bom_items, vec!["CHEESE"]);
        // ₹20 RICE + ₹10 BREAD, NO-PRICE contributes nothing.
        assert_eq!(aggregate.total, Money::new(dec!(30)));
    }

    #[test]
    fn repeated_item_lines_are_grouped_and_costed_once() {
        // Three lines of the same item must equal one line of the summed quantity.
        // `cost(q1) + cost(q2) == cost(q1 + q2)` is what makes the grouping safe.
        let mut boms = FakeBomRepo::default();
        boms.insert(
            "MASALA-CHAI",
            dec!(10),
            &[("TEA-LEAVES", dec!(10)), ("MILK", dec!(100))],
        );
        let mut prices = FakePriceRepo::default();
        prices.insert("TEA-LEAVES", dec!(2.00));
        prices.insert("MILK", dec!(0.50));

        let split = aggregate_cogs(
            &[
                line("MASALA-CHAI", "Masala Chai", dec!(2), dec!(30)),
                line("MASALA-CHAI", "Masala Chai", dec!(1), dec!(30)),
                line("MASALA-CHAI", "Masala Chai", dec!(2), dec!(30)),
            ],
            &buying(),
            &FakeBundleRepo::default(),
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(split.items.len(), 1, "lines must collapse to one row");
        assert_eq!(split.items[0].qty, dec!(5));
        assert_eq!(split.total, Money::new(dec!(35.00)));
    }

    #[test]
    fn breakdown_rows_are_sorted_by_item_code() {
        let mut prices = FakePriceRepo::default();
        for code in ["ZEBRA", "APPLE", "MANGO"] {
            prices.insert(code, dec!(10));
        }

        let aggregate = aggregate_cogs(
            &[
                line("ZEBRA", "Zebra", dec!(1), dec!(20)),
                line("APPLE", "Apple", dec!(1), dec!(20)),
                line("MANGO", "Mango", dec!(1), dec!(20)),
            ],
            &buying(),
            &FakeBundleRepo::default(),
            &FakeBomRepo::default(),
            &prices,
        )
        .unwrap();

        let codes: Vec<&str> = aggregate.items.iter().map(|i| i.item_code.as_str()).collect();
        assert_eq!(codes, vec!["APPLE", "MANGO", "ZEBRA"]);
        assert_eq!(aggregate.total, Money::new(dec!(30)));
    }

    #[test]
    fn fractional_quantities_keep_full_decimal_precision() {
        // ₹10 batch / 3 = 3.333... per unit. No rounding anywhere in this layer, so the
        // repeating decimal survives to the response exactly as `cogs.rs` produced it.
        let mut boms = FakeBomRepo::default();
        boms.insert("ITEM-X", dec!(3), &[("ITEM-Y", dec!(1))]);
        let mut prices = FakePriceRepo::default();
        prices.insert("ITEM-Y", dec!(10));

        let aggregate = aggregate_cogs(
            &[line("ITEM-X", "Item X", dec!(0.5), dec!(20))],
            &buying(),
            &FakeBundleRepo::default(),
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(aggregate.total, Money::new(dec!(10) / dec!(3) / dec!(2)));
    }

    #[test]
    fn empty_line_set_is_zero_not_an_error() {
        let aggregate = aggregate_cogs(
            &[],
            &buying(),
            &FakeBundleRepo::default(),
            &FakeBomRepo::default(),
            &FakePriceRepo::default(),
        )
        .unwrap();

        assert_eq!(aggregate.total, Money::ZERO);
        assert!(aggregate.items.is_empty());
        assert!(!aggregate.has_unset_items());
    }

    #[test]
    fn zero_quantity_bom_is_a_domain_error_not_a_silent_zero() {
        // Dividing by `bom.quantity` cannot be skipped, so a zero batch size must fail
        // loudly. `ApiError` maps this to 500: stored data the caller cannot fix.
        let mut boms = FakeBomRepo::default();
        boms.insert("BAD-BOM", dec!(0), &[("ITEM-A", dec!(1))]);

        let err = aggregate_cogs(
            &[line("BAD-BOM", "Bad", dec!(1), dec!(10))],
            &buying(),
            &FakeBundleRepo::default(),
            &boms,
            &FakePriceRepo::default(),
        )
        .unwrap_err();

        let api: ApiError = err.into();
        assert_eq!(api.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn cost_basis_detection_covers_all_three_buckets() {
        let mut bundles = FakeBundleRepo::default();
        bundles.insert("THALI", &[("ROTI", dec!(1))]);
        let mut boms = FakeBomRepo::default();
        boms.insert("SANDWICH", dec!(1), &[("BREAD", dec!(2))]);

        assert_eq!(
            cost_basis_for(&ItemCode::from("THALI"), &bundles, &boms).unwrap(),
            CostBasis::Bundle
        );
        assert_eq!(
            cost_basis_for(&ItemCode::from("SANDWICH"), &bundles, &boms).unwrap(),
            CostBasis::Bom
        );
        assert_eq!(
            cost_basis_for(&ItemCode::from("COLA"), &bundles, &boms).unwrap(),
            CostBasis::Plain
        );
    }

    // ---- Response shaping --------------------------------------------------

    #[test]
    fn response_for_invoice_scope_omits_date_fields() {
        let aggregate = CogsAggregate {
            total: Money::new(dec!(35.00)),
            items: vec![],
            unset: UnsetItems::default(),
        };
        let response = response_for(
            CogsScope::Invoice("ACC-PSINV-2026-00042".into()),
            1,
            aggregate,
        );

        assert_eq!(response.scope, "invoice");
        assert_eq!(response.invoice.as_deref(), Some("ACC-PSINV-2026-00042"));
        assert_eq!(response.from_date, None);
        assert!(!response.has_unset_items);

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["cogs"], "35.00");
        assert!(json.get("from_date").is_none());
    }

    #[test]
    fn response_for_date_range_scope_carries_both_dates_and_unset_flag() {
        let aggregate = CogsAggregate {
            total: Money::new(dec!(12.50)),
            items: vec![],
            unset: UnsetItems {
                item_prices: vec![],
                bundle_items: vec![],
                bom_items: vec!["CHEESE".into()],
            },
        };
        let response = response_for(
            CogsScope::DateRange {
                from: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                to: NaiveDate::from_ymd_opt(2026, 7, 31).unwrap(),
            },
            17,
            aggregate,
        );

        assert_eq!(response.scope, "date_range");
        assert_eq!(response.invoice, None);
        assert_eq!(response.to_date, NaiveDate::from_ymd_opt(2026, 7, 31));
        assert_eq!(response.invoice_count, 17);
        assert!(
            response.has_unset_items,
            "a missing buying price must never be silent"
        );
    }

    // ---- HTTP contract -----------------------------------------------------

    #[tokio::test]
    async fn calculate_rejects_both_scopes() {
        let response = post_calculate(serde_json::json!({
            "invoice": "ACC-PSINV-2026-00042",
            "from_date": "2026-07-01",
            "to_date": "2026-07-31"
        }))
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(detail(response).await.contains("not both"));
    }

    #[tokio::test]
    async fn calculate_rejects_missing_scope() {
        let response = post_calculate(serde_json::json!({})).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(detail(response).await.contains("required"));
    }

    #[tokio::test]
    async fn calculate_rejects_half_a_date_range() {
        let response = post_calculate(serde_json::json!({ "from_date": "2026-07-01" })).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(detail(response).await.contains("both"));
    }

    #[tokio::test]
    async fn calculate_rejects_inverted_date_range() {
        let response = post_calculate(serde_json::json!({
            "from_date": "2026-07-31",
            "to_date": "2026-07-01"
        }))
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(detail(response).await.contains("must not be after"));
    }

    #[tokio::test]
    async fn calculate_rejects_out_of_range_cutoff_hour() {
        let response = post_calculate(serde_json::json!({
            "invoice": "ACC-PSINV-2026-00042",
            "cutoff_hour": 24
        }))
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(detail(response).await.contains("cutoff_hour"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn calculate_accepts_a_valid_scope_and_reaches_the_storage_gap() {
        // Storage is now wired (W1-A + W1-C). Missing invoice is 409 (Conflict: "no such invoice"),
        // date range with no data is 200 with zero COGS — both prove the handler reached Postgres
        // rather than the old stub.
        let invoice_resp = post_calculate(serde_json::json!({ "invoice": "ACC-PSINV-2026-00042" })).await;
        // Missing invoice maps to Conflict (409) via peacock-storage's missing_invoice_domain.
        assert_eq!(invoice_resp.status(), StatusCode::CONFLICT);
        let detail = detail(invoice_resp).await;
        assert!(!detail.contains("Phase 2"), "internal detail must not leak");
        assert!(
            detail.contains("no such invoice") || detail.contains("ACC-PSINV-2026-00042"),
            "detail should mention missing invoice, got {detail:?}"
        );

        let range_resp =
            post_calculate(serde_json::json!({ "from_date": "2026-07-01", "to_date": "2026-07-31" })).await;
        assert_eq!(range_resp.status(), StatusCode::OK);
        let bytes = range_resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["scope"], "date_range");
        // No invoices in the test DB for that window → zero COGS, flag false.
        assert_eq!(json["cogs"], "0");
        assert_eq!(json["has_unset_items"], false);
    }

    #[tokio::test]
    async fn calculate_route_is_registered_and_post_only() {
        let get = send(
            Request::builder()
                .uri("/api/cogs/calculate")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn errors_are_problem_json_with_a_request_id() {
        let response = post_calculate(serde_json::json!({})).await;

        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
            crate::error::PROBLEM_JSON
        );
        assert!(response.headers().get("x-request-id").is_some());

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], 400);
        assert_eq!(json["instance"], "/api/cogs/calculate");
    }
}
