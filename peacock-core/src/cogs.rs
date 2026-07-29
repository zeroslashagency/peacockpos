//! Cost of goods sold (COGS).
//!
//! Ported from `_upstream/ury-ury/ury/ury/doctype/ury_daily_p_and_l/ury_daily_p_and_l.py`.
//!
//! ## Critical arithmetic (all three defects from v1 avoided)
//!
//! ### 1. Exactly TWO levels, not depth-3 recursion
//! `inner_bom_process` → `inner_inner_bom_process`, which stops.
//! - ury_daily_p_and_l.py:10 (`inner_bom_process`)
//! - ury_daily_p_and_l.py:42 (`inner_inner_bom_process`)
//!
//! ### 2. Divide by `bom.quantity` to normalise batch BOM to per-unit cost
//! - ury_daily_p_and_l.py:38: `bom_buying_price = bom_buying_price / bom.quantity`
//!
//! v1 dropped this entirely. A BOM with `quantity=10` (produces 10 units) would
//! scale COGS by 10×, meaning a ₹50 batch BOM would be priced as ₹500/unit instead
//! of ₹5/unit.
//!
//! **Worked example:**
//! - Item "Masala Chai" has a BOM:
//!   - 10g Tea Leaves @ ₹2/g = ₹20
//!   - 100ml Milk @ ₹0.50/ml = ₹50
//!   - BOM `quantity = 10` (batch produces 10 cups)
//! - Total batch cost = ₹70
//! - **Per-unit cost = ₹70 / 10 = ₹7/cup**
//! - Without the division: ₹70/cup (silently wrong by 10×)
//!
//! ### 3. Price from `Item Price` on the configured `buying_price_list`
//! - ury_daily_p_and_l.py:30: `filters={'price_list': buying_price_list, 'item_code': ...}`
//!
//! v1 used `valuation_repo.get_latest()`, a different cost basis (moving-average
//! stock valuation) that diverges from the operator's buying prices.
//!
//! ### 4. Accumulate `unset_bom_items`
//! Upstream accumulates every ingredient with no price so the operator sees which
//! data is missing. Dropping this turns a visible data gap into silently understated
//! COGS.
//!
//! ## The three cost bases, and which one wins
//!
//! `cogs_sold` partitions every invoice line into exactly one of three buckets with
//! three mutually exclusive SQL queries:
//!
//! | Bucket | Query | Predicates | Priced by |
//! |--------|-------|-----------|-----------|
//! | plain | `non_pb_item_sales` (:73) | `d.new_item_code IS NULL` (:102) AND `e.item IS NULL` (:103) | `Item Price` (:179) |
//! | BOM | `bom_item_sales` (:110) | `d.new_item_code IS NULL` (:139) AND `e.item IS NOT NULL` (:140) | `inner_bom_process` (:201) |
//! | bundle | `pb_item_sales` (:147) | `d.new_item_code IS NOT NULL` (:170) | per-line walk (:219-248) |
//!
//! `d` is `tabProduct Bundle` joined on `d.new_item_code = b.item_code`; `e` is an
//! active/default/submitted `tabBOM` joined on `e.item = b.item_code`.
//!
//! **Product Bundle wins.** The bundle query does not join `tabBOM` at all (:147-159
//! has no `e`), and both other queries require `d.new_item_code IS NULL`. So an item
//! that is *both* a Product Bundle and has an active default BOM is priced as a
//! bundle and never as a BOM — its own BOM is ignored entirely. Precedence is
//! bundle → BOM → plain, and it is a partition, not a fallback chain.
//!
//! ## A bundle is not an extra level of BOM depth
//!
//! For a bundle line that has a BOM, upstream calls `inner_bom_process` (:231) —
//! the very same function, at the very same entry point, as a top-level BOM item
//! (:201). It does **not** wrap it in another level. So a bundle line's BOM walk
//! still gets the full two levels (`inner_bom_process` → `inner_inner_bom_process`),
//! and `MAX_LEVEL` is unchanged at 2. Treating the bundle as a level would silently
//! truncate the walk to one level and understate COGS.
//!
//! ## No recursion into nested bundles
//!
//! A bundle line is checked for a BOM (:227) and otherwise priced from `Item Price`
//! (:241). Upstream never re-queries `Product Bundle` for a bundle line, so a bundle
//! whose child is itself a bundle prices that child as a leaf item. See
//! `bundle_of_bundle_inner_priced_as_leaf`.
//!
//! ## The `if buying_price > 0` guard
//!
//! Both the BOM bucket (:208) and the bundle bucket (:248) skip the cost row when
//! the computed per-unit cost is not strictly positive; the plain-item bucket
//! (:181-193) has **no such guard**. Consequences, all preserved here:
//!
//! - A fully unpriced bundle (every line missing a price) contributes exactly zero
//!   to COGS, while its missing items still surface in the unset lists. The gap is
//!   visible, not silently absorbed.
//! - A net-negative per-unit cost (possible with a negative `Item Price`) is clamped
//!   to zero in the BOM and bundle buckets, but passes through for a plain item.
//!   That asymmetry is upstream's, not ours.

use crate::error::{Error, Result};
use crate::ids::*;
use crate::model::OrderLine;
use crate::money::Money;
use crate::ports::{BomRepo, PriceRepo, ProductBundleRepo};
use rust_decimal::Decimal;
use std::collections::HashSet;

/// Maximum BOM explosion depth. Matches upstream's two-function hardcoded walk.
/// - Level 1: `inner_bom_process` (ury_daily_p_and_l.py:10)
/// - Level 2: `inner_inner_bom_process` (ury_daily_p_and_l.py:42) — stops here
///
/// An item WITH a BOM at max depth is treated as a leaf and priced directly from
/// `Item Price`, not exploded. This is verified against the Python and matches it.
pub const MAX_LEVEL: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CogsResult {
    pub cost: Money,
    /// Sold items priced directly from `Item Price` that have none — upstream's
    /// `unset_item_prices`, shown to the operator under the label `ITEMS`
    /// (ury_daily_p_and_l.py:177, :262).
    pub unset_item_prices: Vec<ItemCode>,
    /// Product Bundle child lines with no `Item Price` — upstream's
    /// `unset_pb_item_prices`, label `BUNDLE SUB ITEMS` (:220, :263).
    pub unset_bundle_items: Vec<ItemCode>,
    /// BOM ingredients with no `Item Price` — upstream's `unset_bom_item_prices`,
    /// label `BOM SUB ITEMS` (:195, :264).
    ///
    /// Deduplicated and sorted for deterministic output (upstream accumulated in
    /// encounter order with manual `if item not in …` guards; we use a HashSet
    /// then sort).
    pub unset_bom_items: Vec<ItemCode>,
}

/// The three lists stay separate because upstream renders them as three labelled
/// sections in `remarks` (:261-266) and the label is the actionable part: an
/// operator fixing "BOM SUB ITEMS: [MILK]" edits a BOM ingredient's buying price,
/// whereas "BUNDLE SUB ITEMS: [MILK]" points at a bundle component. Merging them
/// would keep the cost identical but strip that routing information from the only
/// place the data gap is ever surfaced.
///
/// One deliberate difference from upstream: these hold `item_code`, not
/// `item_name` (:34, :182, :240). Upstream's choice of the display name makes the
/// remarks unactionable when two items share a name, and this crate has no
/// `Item.item_name` port. The set of flagged items is the same either way.
impl CogsResult {
    /// Merge two results, combining costs and all three unset-item lists.
    pub fn merge(self, other: CogsResult) -> CogsResult {
        fn union(a: Vec<ItemCode>, b: Vec<ItemCode>) -> Vec<ItemCode> {
            let mut set: HashSet<ItemCode> = a.into_iter().collect();
            set.extend(b);
            let mut v: Vec<_> = set.into_iter().collect();
            v.sort();
            v
        }

        CogsResult {
            cost: self.cost + other.cost,
            unset_item_prices: union(self.unset_item_prices, other.unset_item_prices),
            unset_bundle_items: union(self.unset_bundle_items, other.unset_bundle_items),
            unset_bom_items: union(self.unset_bom_items, other.unset_bom_items),
        }
    }
}

fn sorted(set: HashSet<ItemCode>) -> Vec<ItemCode> {
    let mut v: Vec<_> = set.into_iter().collect();
    v.sort();
    v
}

/// Per-unit cost of one BOM, normalised by its batch quantity.
///
/// `level` is the level of **this** BOM in upstream's two-function walk:
/// - `level = 1` → `inner_bom_process` (ury_daily_p_and_l.py:10), which looks up a
///   BOM for each of its lines and descends.
/// - `level = 2` → `inner_inner_bom_process` (:42), which prices every line from
///   `Item Price` and never descends.
///
/// The `level < MAX_LEVEL` gate is what makes that asymmetry explicit: at level 2
/// no BOM lookup happens at all, so a level-3 BOM is priced as a leaf.
fn bom_cost_per_unit(
    bom: &crate::ports::Bom,
    buying_price_list: &PriceListName,
    boms: &dyn BomRepo,
    prices: &dyn PriceRepo,
    level: u8,
) -> Result<(Money, HashSet<ItemCode>)> {
    // Upstream divides unconditionally (:38, :57) and would raise ZeroDivisionError.
    if bom.quantity.is_zero() {
        return Err(Error::BomZeroQuantity(bom.name.clone()));
    }

    let mut batch_cost = Money::ZERO;
    let mut unset: HashSet<ItemCode> = HashSet::new();

    for line in &bom.items {
        let child_bom = if level < MAX_LEVEL {
            boms.find_for_item(&line.item_code)?
        } else {
            None
        };

        match child_bom {
            Some(child) => {
                let (child_per_unit, child_unset) =
                    bom_cost_per_unit(&child, buying_price_list, boms, prices, level + 1)?;
                batch_cost = batch_cost + child_per_unit * line.qty;
                unset.extend(child_unset);
            }
            None => match prices.item_price(&line.item_code, buying_price_list)? {
                Some(price) => batch_cost = batch_cost + price * line.qty,
                None => {
                    unset.insert(line.item_code.clone());
                }
            },
        }
    }

    // Normalise batch to per-unit cost (ury_daily_p_and_l.py:38, :57).
    Ok((Money::new(batch_cost.inner() / bom.quantity), unset))
}

/// Upstream's `if buying_price > 0` guard (ury_daily_p_and_l.py:208, :248).
///
/// Applies to the BOM and bundle buckets only. A non-positive per-unit cost means
/// no cost row is appended and nothing is added to `cogs`, so the item contributes
/// exactly zero — the missing prices still show up in the unset lists.
fn guarded_extension(per_unit: Money, qty: Decimal) -> Money {
    if per_unit.inner() > Decimal::ZERO {
        per_unit * qty
    } else {
        Money::ZERO
    }
}

/// Compute COGS for a single item at the given quantity, ignoring Product Bundles.
///
/// Equivalent to `cogs_for_item_with_bundles` with an empty bundle repo: it resolves
/// BOM → plain. Use it only where the caller knows the item cannot be a bundle;
/// otherwise prefer [`cogs_for_item_with_bundles`], because a bundle resolved as a
/// BOM (or as a plain item) is priced on the wrong basis.
///
/// ## Algorithm (ury_daily_p_and_l.py:10, :42, :196-218)
/// 1. Look up the active default BOM for the item.
/// 2. No BOM → price directly from `Item Price` (the plain bucket, :179).
/// 3. BOM → explode two levels, dividing each level by its `bom.quantity`.
/// 4. Apply the `buying_price > 0` guard, then multiply by `qty`.
/// 5. Accumulate missing prices into the matching unset list (contributes zero cost).
///
/// ## Errors
/// - `BomZeroQuantity` if any exploded `bom.quantity == 0` (would divide by zero).
pub fn cogs_for_item(
    item: &ItemCode,
    qty: Decimal,
    buying_price_list: &PriceListName,
    boms: &dyn BomRepo,
    prices: &dyn PriceRepo,
) -> Result<CogsResult> {
    if let Some(bom) = boms.find_for_item(item)? {
        let (per_unit, unset) = bom_cost_per_unit(&bom, buying_price_list, boms, prices, 1)?;
        return Ok(CogsResult {
            cost: guarded_extension(per_unit, qty),
            unset_bom_items: sorted(unset),
            ..Default::default()
        });
    }

    // Plain bucket (:178-193): no `buying_price > 0` guard here, and a missing price
    // lands under the `ITEMS` label rather than `BOM SUB ITEMS`.
    match prices.item_price(item, buying_price_list)? {
        Some(price) => Ok(CogsResult {
            cost: price * qty,
            ..Default::default()
        }),
        None => Ok(CogsResult {
            cost: Money::ZERO,
            unset_item_prices: vec![item.clone()],
            ..Default::default()
        }),
    }
}

/// Compute COGS for a single item, resolving all three of upstream's cost bases.
///
/// Precedence is **bundle → BOM → plain**, and it is a partition rather than a
/// fallback chain: an item that is both a Product Bundle and has an active default
/// BOM is priced as a bundle, and its own BOM is never consulted. See the module
/// docs for the SQL that establishes this (`d.new_item_code IS NOT NULL` at
/// ury_daily_p_and_l.py:170, with no BOM join in that query).
///
/// Bundle pricing (:221-258), per child line:
/// - line has an active default BOM → `inner_bom_process` at level 1, exactly as a
///   top-level BOM item would be (:231 calls the same function as :201), then
///   `× line.qty`. The bundle is not an extra level of depth.
/// - otherwise → `Item Price × line.qty`, with a miss recorded under
///   `unset_bundle_items` (:243).
///
/// Then the `buying_price > 0` guard (:248) and `× qty`.
pub fn cogs_for_item_with_bundles(
    item: &ItemCode,
    qty: Decimal,
    buying_price_list: &PriceListName,
    bundles: &dyn ProductBundleRepo,
    boms: &dyn BomRepo,
    prices: &dyn PriceRepo,
) -> Result<CogsResult> {
    let Some(bundle) = bundles.find_by_new_item_code(item)? else {
        return cogs_for_item(item, qty, buying_price_list, boms, prices);
    };

    let mut buying_price = Money::ZERO;
    let mut unset_bom: HashSet<ItemCode> = HashSet::new();
    let mut unset_bundle: HashSet<ItemCode> = HashSet::new();

    for line in &bundle.items {
        match boms.find_for_item(&line.item_code)? {
            Some(bom) => {
                let (per_unit, unset) =
                    bom_cost_per_unit(&bom, buying_price_list, boms, prices, 1)?;
                buying_price = buying_price + per_unit * line.qty;
                unset_bom.extend(unset);
            }
            // No BOM: priced as a leaf, even if this child is itself a bundle
            // (upstream never re-queries `Product Bundle` here).
            None => match prices.item_price(&line.item_code, buying_price_list)? {
                Some(price) => buying_price = buying_price + price * line.qty,
                None => {
                    unset_bundle.insert(line.item_code.clone());
                }
            },
        }
    }

    Ok(CogsResult {
        cost: guarded_extension(buying_price, qty),
        unset_item_prices: vec![],
        unset_bundle_items: sorted(unset_bundle),
        unset_bom_items: sorted(unset_bom),
    })
}

/// Compute COGS across an entire order, aggregating all three unset-item lists.
///
/// Bundle-unaware; see [`cogs_for_order_with_bundles`].
pub fn cogs_for_order(
    lines: &[OrderLine],
    buying_price_list: &PriceListName,
    boms: &dyn BomRepo,
    prices: &dyn PriceRepo,
) -> Result<CogsResult> {
    lines
        .iter()
        .map(|line| cogs_for_item(&line.item_code, line.qty, buying_price_list, boms, prices))
        .try_fold(CogsResult::default(), |acc, res| Ok(acc.merge(res?)))
}

/// Compute COGS across an entire order, resolving all three cost bases.
pub fn cogs_for_order_with_bundles(
    lines: &[OrderLine],
    buying_price_list: &PriceListName,
    bundles: &dyn ProductBundleRepo,
    boms: &dyn BomRepo,
    prices: &dyn PriceRepo,
) -> Result<CogsResult> {
    lines
        .iter()
        .map(|line| {
            cogs_for_item_with_bundles(
                &line.item_code,
                line.qty,
                buying_price_list,
                bundles,
                boms,
                prices,
            )
        })
        .try_fold(CogsResult::default(), |acc, res| Ok(acc.merge(res?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{Bom, BomLine, ProductBundle, ProductBundleLine};
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    // --- Hand-rolled in-memory fake repos (no mocking crate, no database) ---

    struct FakeBomRepo {
        boms: HashMap<ItemCode, Bom>,
    }

    impl FakeBomRepo {
        fn new() -> Self {
            FakeBomRepo {
                boms: HashMap::new(),
            }
        }

        fn insert(&mut self, item: &str, quantity: Decimal, lines: Vec<(&str, Decimal)>) {
            let bom_name = format!("BOM-{}", item);
            self.boms.insert(
                ItemCode::from(item),
                Bom {
                    name: BomName::from(bom_name.as_str()),
                    quantity,
                    items: lines
                        .into_iter()
                        .map(|(code, qty)| BomLine {
                            item_code: ItemCode::from(code),
                            qty,
                        })
                        .collect(),
                },
            );
        }
    }

    impl BomRepo for FakeBomRepo {
        fn find_for_item(&self, item: &ItemCode) -> Result<Option<Bom>> {
            Ok(self.boms.get(item).cloned())
        }
    }

    struct FakePriceRepo {
        prices: HashMap<ItemCode, Money>,
    }

    impl FakePriceRepo {
        fn new() -> Self {
            FakePriceRepo {
                prices: HashMap::new(),
            }
        }

        fn insert(&mut self, item: &str, price: Money) {
            self.prices.insert(ItemCode::from(item), price);
        }
    }

    impl PriceRepo for FakePriceRepo {
        fn item_price(&self, item: &ItemCode, _price_list: &PriceListName) -> Result<Option<Money>> {
            Ok(self.prices.get(item).copied())
        }
    }

    struct FakeBundleRepo {
        bundles: HashMap<ItemCode, ProductBundle>,
    }

    impl FakeBundleRepo {
        fn new() -> Self {
            FakeBundleRepo {
                bundles: HashMap::new(),
            }
        }

        fn insert(&mut self, new_item_code: &str, lines: Vec<(&str, Decimal)>) {
            self.bundles.insert(
                ItemCode::from(new_item_code),
                ProductBundle {
                    new_item_code: ItemCode::from(new_item_code),
                    items: lines
                        .into_iter()
                        .map(|(code, qty)| ProductBundleLine {
                            item_code: ItemCode::from(code),
                            qty,
                        })
                        .collect(),
                },
            );
        }
    }

    impl ProductBundleRepo for FakeBundleRepo {
        fn find_by_new_item_code(&self, item: &ItemCode) -> Result<Option<ProductBundle>> {
            Ok(self.bundles.get(item).cloned())
        }
    }

    fn buying_list() -> PriceListName {
        PriceListName::from("Buying")
    }

    #[test]
    fn flat_item_with_price_no_bom() {
        let boms = FakeBomRepo::new();
        let mut prices = FakePriceRepo::new();
        prices.insert("ITEM-001", Money::new(dec!(10.00)));

        let result = cogs_for_item(
            &ItemCode::from("ITEM-001"),
            dec!(3),
            &buying_list(),
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::new(dec!(30.00)));
        assert!(result.unset_bom_items.is_empty());
    }

    #[test]
    fn one_level_bom_with_quantity_not_one() {
        // THE test that catches v1's bug: `bom.quantity != 1`.
        //
        // Expected calculation (BY HAND):
        // - BOM for "Masala Chai" produces 10 cups (quantity=10)
        // - Tea: 10g @ ₹2/g = ₹20
        // - Milk: 100ml @ ₹0.50/ml = ₹50
        // - Total batch cost = ₹70
        // - Per-unit cost = ₹70 / 10 = ₹7/cup
        // - Order qty = 5 cups
        // - Total COGS = ₹7 × 5 = ₹35
        //
        // Without division by bom.quantity: ₹70 × 5 = ₹350 (wrong by 10×)

        let mut boms = FakeBomRepo::new();
        boms.insert(
            "MASALA-CHAI",
            dec!(10), // batch produces 10 cups
            vec![("TEA-LEAVES", dec!(10)), ("MILK", dec!(100))],
        );

        let mut prices = FakePriceRepo::new();
        prices.insert("TEA-LEAVES", Money::new(dec!(2.00)));  // ₹2/g
        prices.insert("MILK", Money::new(dec!(0.50)));        // ₹0.50/ml

        let result = cogs_for_item(
            &ItemCode::from("MASALA-CHAI"),
            dec!(5),
            &buying_list(),
            &boms,
            &prices,
        )
        .unwrap();

        // ₹70 batch / 10 = ₹7/unit × 5 = ₹35
        assert_eq!(result.cost, Money::new(dec!(35.00)));
        assert!(result.unset_bom_items.is_empty());
    }

    #[test]
    fn two_level_bom_both_normalising() {
        // Level 1: "COMBO-MEAL" (qty=1) → "BURGER" (2×), "FRIES" (1×)
        // Level 2: "BURGER" (qty=5) → "PATTY" (5×), "BUN" (5×)
        // Prices: PATTY=₹10, BUN=₹5, FRIES=₹20
        //
        // Calculation:
        // - BURGER batch: (5×₹10 + 5×₹5) = ₹75 / 5 = ₹15/unit
        // - COMBO: 2×₹15 + 1×₹20 = ₹50 / 1 = ₹50/unit
        // - Order qty=3 → ₹150

        let mut boms = FakeBomRepo::new();
        boms.insert(
            "COMBO-MEAL",
            dec!(1),
            vec![("BURGER", dec!(2)), ("FRIES", dec!(1))],
        );
        boms.insert(
            "BURGER",
            dec!(5),
            vec![("PATTY", dec!(5)), ("BUN", dec!(5))],
        );

        let mut prices = FakePriceRepo::new();
        prices.insert("PATTY", Money::new(dec!(10)));
        prices.insert("BUN", Money::new(dec!(5)));
        prices.insert("FRIES", Money::new(dec!(20)));

        let result = cogs_for_item(
            &ItemCode::from("COMBO-MEAL"),
            dec!(3),
            &buying_list(),
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::new(dec!(150)));
        assert!(result.unset_bom_items.is_empty());
    }

    #[test]
    fn three_level_bom_third_level_priced_as_leaf() {
        // Proves MAX_LEVEL = 2 matches upstream: third level is NOT exploded.
        //
        // Level 1: "PIZZA" (qty=1) → "BASE" (1×)
        // Level 2: "BASE" (qty=2) → "DOUGH" (2×)
        // Level 3: "DOUGH" (qty=10) → "FLOUR" (10×), "WATER" (5×) — IGNORED
        //
        // At max depth, "DOUGH" is priced directly at ₹30.
        //
        // Calculation:
        // - BASE batch: 2×₹30 = ₹60 / 2 = ₹30/unit
        // - PIZZA: 1×₹30 = ₹30 / 1 = ₹30
        // - Order qty=1 → ₹30

        let mut boms = FakeBomRepo::new();
        boms.insert("PIZZA", dec!(1), vec![("BASE", dec!(1))]);
        boms.insert("BASE", dec!(2), vec![("DOUGH", dec!(2))]);
        boms.insert(
            "DOUGH",
            dec!(10),
            vec![("FLOUR", dec!(10)), ("WATER", dec!(5))],
        );

        let mut prices = FakePriceRepo::new();
        prices.insert("DOUGH", Money::new(dec!(30)));
        prices.insert("FLOUR", Money::new(dec!(1)));  // not used
        prices.insert("WATER", Money::new(dec!(0.5))); // not used

        let result = cogs_for_item(
            &ItemCode::from("PIZZA"),
            dec!(1),
            &buying_list(),
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::new(dec!(30)));
        assert!(result.unset_bom_items.is_empty());
    }

    #[test]
    fn missing_item_price_lands_in_unset_and_contributes_zero() {
        let mut boms = FakeBomRepo::new();
        boms.insert("SANDWICH", dec!(1), vec![("BREAD", dec!(2)), ("CHEESE", dec!(1))]);

        let mut prices = FakePriceRepo::new();
        prices.insert("BREAD", Money::new(dec!(5)));
        // CHEESE has no price

        let result = cogs_for_item(
            &ItemCode::from("SANDWICH"),
            dec!(2),
            &buying_list(),
            &boms,
            &prices,
        )
        .unwrap();

        // Cost = 2×₹5 = ₹10, then ×2 = ₹20 (CHEESE contributes zero)
        assert_eq!(result.cost, Money::new(dec!(20)));
        assert_eq!(result.unset_bom_items, vec![ItemCode::from("CHEESE")]);
    }

    #[test]
    fn bom_zero_quantity_returns_error() {
        let mut boms = FakeBomRepo::new();
        boms.insert("BAD-BOM", dec!(0), vec![("ITEM-A", dec!(1))]);

        let prices = FakePriceRepo::new();

        let err = cogs_for_item(
            &ItemCode::from("BAD-BOM"),
            dec!(1),
            &buying_list(),
            &boms,
            &prices,
        )
        .unwrap_err();

        assert_eq!(err, Error::BomZeroQuantity(BomName::from("BOM-BAD-BOM")));
    }

    #[test]
    fn fractional_quantities_produce_exact_decimal_results() {
        // No float drift: 0.333 × 3 in Decimal stays exact within precision.
        let mut boms = FakeBomRepo::new();
        boms.insert("ITEM-X", dec!(3), vec![("ITEM-Y", dec!(1))]);

        let mut prices = FakePriceRepo::new();
        prices.insert("ITEM-Y", Money::new(dec!(10)));

        let result = cogs_for_item(
            &ItemCode::from("ITEM-X"),
            dec!(0.5),
            &buying_list(),
            &boms,
            &prices,
        )
        .unwrap();

        // Batch cost = ₹10, per-unit = ₹10/3 = 3.333..., qty=0.5 → ₹1.666...
        // Exact to Decimal precision (no float drift).
        assert_eq!(result.cost, Money::new(dec!(10) / dec!(3) / dec!(2)));
        assert!(result.unset_bom_items.is_empty());
    }

    #[test]
    fn bom_line_with_zero_qty() {
        // Edge case: a BOM line with qty=0 contributes zero cost.
        let mut boms = FakeBomRepo::new();
        boms.insert("ITEM-Z", dec!(1), vec![("ITEM-A", dec!(0)), ("ITEM-B", dec!(2))]);

        let mut prices = FakePriceRepo::new();
        prices.insert("ITEM-A", Money::new(dec!(100))); // not used
        prices.insert("ITEM-B", Money::new(dec!(5)));

        let result = cogs_for_item(
            &ItemCode::from("ITEM-Z"),
            dec!(1),
            &buying_list(),
            &boms,
            &prices,
        )
        .unwrap();

        // 0×₹100 + 2×₹5 = ₹10 / 1 = ₹10
        assert_eq!(result.cost, Money::new(dec!(10)));
        assert!(result.unset_bom_items.is_empty());
    }

    #[test]
    fn aggregation_across_order_merges_unset_lists_without_duplicates() {
        let mut boms = FakeBomRepo::new();
        boms.insert("ITEM-1", dec!(1), vec![("MISSING-A", dec!(1))]);
        boms.insert("ITEM-2", dec!(1), vec![("MISSING-A", dec!(1)), ("MISSING-B", dec!(1))]);

        let prices = FakePriceRepo::new();

        let lines = vec![
            OrderLine {
                item_code: ItemCode::from("ITEM-1"),
                item_name: "Item 1".to_owned(),
                qty: dec!(1),
                rate: Money::ZERO,
                comments: None,
                serve_priority: 0,
                indicate_course: false,
            },
            OrderLine {
                item_code: ItemCode::from("ITEM-2"),
                item_name: "Item 2".to_owned(),
                qty: dec!(1),
                rate: Money::ZERO,
                comments: None,
                serve_priority: 0,
                indicate_course: false,
            },
        ];

        let result = cogs_for_order(&lines, &buying_list(), &boms, &prices).unwrap();

        assert_eq!(result.cost, Money::ZERO);
        // MISSING-A appears twice but should be deduplicated, then sorted.
        assert_eq!(
            result.unset_bom_items,
            vec![ItemCode::from("MISSING-A"), ItemCode::from("MISSING-B")]
        );
    }

    #[test]
    fn deliberately_deep_bom_cost_stays_exact() {
        // Wide BOM at level 2 with many lines, verifying no accumulation drift.
        let mut boms = FakeBomRepo::new();
        boms.insert("ROOT", dec!(1), vec![("INTERMEDIATE", dec!(1))]);
        boms.insert(
            "INTERMEDIATE",
            dec!(1),
            vec![
                ("LEAF-1", dec!(1)),
                ("LEAF-2", dec!(2)),
                ("LEAF-3", dec!(3)),
                ("LEAF-4", dec!(4)),
                ("LEAF-5", dec!(5)),
                ("LEAF-6", dec!(6)),
                ("LEAF-7", dec!(7)),
                ("LEAF-8", dec!(8)),
                ("LEAF-9", dec!(9)),
                ("LEAF-10", dec!(10)),
            ],
        );

        let mut prices = FakePriceRepo::new();
        for i in 1..=10 {
            prices.insert(&format!("LEAF-{}", i), Money::new(Decimal::from(i)));
        }

        let result = cogs_for_item(
            &ItemCode::from("ROOT"),
            dec!(1),
            &buying_list(),
            &boms,
            &prices,
        )
        .unwrap();

        // Sum = 1×1 + 2×2 + 3×3 + ... + 10×10 = 385
        let expected = (1..=10).map(|i| i * i).sum::<i32>();
        assert_eq!(result.cost, Money::new(Decimal::from(expected)));
        assert!(result.unset_bom_items.is_empty());
    }

    #[test]
    fn plain_item_missing_price_lands_in_unset_item_prices_not_bom() {
        // Upstream keeps three separate lists under three labels (:262-264).
        // A sold item with no price is `ITEMS`, not `BOM SUB ITEMS`.
        let boms = FakeBomRepo::new();
        let prices = FakePriceRepo::new();

        let result = cogs_for_item(
            &ItemCode::from("NO-PRICE"),
            dec!(3),
            &buying_list(),
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::ZERO);
        assert_eq!(result.unset_item_prices, vec![ItemCode::from("NO-PRICE")]);
        assert!(result.unset_bom_items.is_empty());
        assert!(result.unset_bundle_items.is_empty());
    }

    // ---------------- Product Bundle (ury_daily_p_and_l.py:219-258) ----------------

    #[test]
    fn bundle_of_plain_items() {
        // THALI = 2× ROTI @ ₹5 + 1× DAL @ ₹30 → ₹40/unit, qty 3 → ₹120.
        let mut bundles = FakeBundleRepo::new();
        bundles.insert("THALI", vec![("ROTI", dec!(2)), ("DAL", dec!(1))]);

        let boms = FakeBomRepo::new();
        let mut prices = FakePriceRepo::new();
        prices.insert("ROTI", Money::new(dec!(5)));
        prices.insert("DAL", Money::new(dec!(30)));

        let result = cogs_for_item_with_bundles(
            &ItemCode::from("THALI"),
            dec!(3),
            &buying_list(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::new(dec!(120)));
        assert!(result.unset_bundle_items.is_empty());
        assert!(result.unset_bom_items.is_empty());
    }

    #[test]
    fn bundle_line_with_bom_quantity_not_one_normalises() {
        // The batch-normalisation case inside a bundle.
        //
        // COMBO bundle: 2× MASALA-CHAI + 1× SAMOSA @ ₹12
        // MASALA-CHAI BOM: batch of 10, 10g TEA @ ₹2 + 100ml MILK @ ₹0.50 = ₹70
        //   → ₹70 / 10 = ₹7/cup
        // Bundle per-unit = 2×₹7 + 1×₹12 = ₹26; qty 4 → ₹104.
        //
        // Dropping the /10 would give 2×₹70 + ₹12 = ₹152/unit → ₹608.
        let mut bundles = FakeBundleRepo::new();
        bundles.insert("COMBO", vec![("MASALA-CHAI", dec!(2)), ("SAMOSA", dec!(1))]);

        let mut boms = FakeBomRepo::new();
        boms.insert(
            "MASALA-CHAI",
            dec!(10),
            vec![("TEA-LEAVES", dec!(10)), ("MILK", dec!(100))],
        );

        let mut prices = FakePriceRepo::new();
        prices.insert("TEA-LEAVES", Money::new(dec!(2.00)));
        prices.insert("MILK", Money::new(dec!(0.50)));
        prices.insert("SAMOSA", Money::new(dec!(12)));

        let result = cogs_for_item_with_bundles(
            &ItemCode::from("COMBO"),
            dec!(4),
            &buying_list(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::new(dec!(104)));
        assert!(result.unset_bundle_items.is_empty());
        assert!(result.unset_bom_items.is_empty());
    }

    #[test]
    fn bundle_line_bom_gets_full_two_levels_not_truncated_by_the_bundle() {
        // A bundle is NOT an extra level of BOM depth: :231 calls the same
        // `inner_bom_process` entry point as the top-level BOM bucket at :201.
        //
        // PLATTER bundle: 1× COMBO-MEAL
        // COMBO-MEAL BOM (level 1, qty=1): 2× BURGER + 1× FRIES @ ₹20
        // BURGER BOM (level 2, qty=5): 5× PATTY @ ₹10 + 5× BUN @ ₹5 = ₹75 → ₹15/unit
        // COMBO-MEAL = 2×₹15 + ₹20 = ₹50/unit → bundle ₹50, qty 1 → ₹50.
        //
        // If the bundle consumed a level, BURGER would be priced as a leaf. It has
        // no `Item Price`, so the cost would collapse to ₹20 and BURGER would show
        // up in the unset list. Asserting the empty unset list is what pins this.
        let mut bundles = FakeBundleRepo::new();
        bundles.insert("PLATTER", vec![("COMBO-MEAL", dec!(1))]);

        let mut boms = FakeBomRepo::new();
        boms.insert(
            "COMBO-MEAL",
            dec!(1),
            vec![("BURGER", dec!(2)), ("FRIES", dec!(1))],
        );
        boms.insert("BURGER", dec!(5), vec![("PATTY", dec!(5)), ("BUN", dec!(5))]);

        let mut prices = FakePriceRepo::new();
        prices.insert("PATTY", Money::new(dec!(10)));
        prices.insert("BUN", Money::new(dec!(5)));
        prices.insert("FRIES", Money::new(dec!(20)));

        let result = cogs_for_item_with_bundles(
            &ItemCode::from("PLATTER"),
            dec!(1),
            &buying_list(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::new(dec!(50)));
        assert!(result.unset_bom_items.is_empty());
        // The identical BOM reached from the top level costs the same — proof the
        // bundle added no depth.
        let direct = cogs_for_item(
            &ItemCode::from("COMBO-MEAL"),
            dec!(1),
            &buying_list(),
            &boms,
            &prices,
        )
        .unwrap();
        assert_eq!(direct.cost, result.cost);
    }

    #[test]
    fn bundle_line_missing_price_lands_in_bundle_unset_list() {
        // MEAL = 1× RICE @ ₹20 + 1× PICKLE (no price)
        // Cost = ₹20/unit × 2 = ₹40; PICKLE stays visible under BUNDLE SUB ITEMS.
        let mut bundles = FakeBundleRepo::new();
        bundles.insert("MEAL", vec![("RICE", dec!(1)), ("PICKLE", dec!(1))]);

        let boms = FakeBomRepo::new();
        let mut prices = FakePriceRepo::new();
        prices.insert("RICE", Money::new(dec!(20)));

        let result = cogs_for_item_with_bundles(
            &ItemCode::from("MEAL"),
            dec!(2),
            &buying_list(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::new(dec!(40)));
        assert_eq!(result.unset_bundle_items, vec![ItemCode::from("PICKLE")]);
        // A bundle child miss is NOT a BOM ingredient miss; upstream labels them
        // differently (:263 vs :264).
        assert!(result.unset_bom_items.is_empty());
        assert!(result.unset_item_prices.is_empty());
    }

    #[test]
    fn bundle_bom_ingredient_miss_lands_in_bom_unset_list_not_bundle() {
        // A miss *inside* a bundle line's BOM is a BOM SUB ITEM (:236-237), which
        // upstream appends to `unset_bom_item_prices` — the same list the top-level
        // BOM bucket uses — not to `unset_pb_item_prices`.
        let mut bundles = FakeBundleRepo::new();
        bundles.insert("BOX", vec![("SANDWICH", dec!(1))]);

        let mut boms = FakeBomRepo::new();
        boms.insert(
            "SANDWICH",
            dec!(1),
            vec![("BREAD", dec!(2)), ("CHEESE", dec!(1))],
        );

        let mut prices = FakePriceRepo::new();
        prices.insert("BREAD", Money::new(dec!(5)));

        let result = cogs_for_item_with_bundles(
            &ItemCode::from("BOX"),
            dec!(1),
            &buying_list(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::new(dec!(10)));
        assert_eq!(result.unset_bom_items, vec![ItemCode::from("CHEESE")]);
        assert!(result.unset_bundle_items.is_empty());
    }

    #[test]
    fn fully_unpriced_bundle_contributes_zero_but_stays_visible() {
        // Upstream's `if buying_price > 0` guard (:248): no cost row is appended,
        // so COGS gains nothing. The gap must still be reported, never silently
        // zeroed away.
        let mut bundles = FakeBundleRepo::new();
        bundles.insert("MYSTERY", vec![("UNKNOWN-A", dec!(1)), ("UNKNOWN-B", dec!(2))]);

        let boms = FakeBomRepo::new();
        let prices = FakePriceRepo::new();

        let result = cogs_for_item_with_bundles(
            &ItemCode::from("MYSTERY"),
            dec!(7),
            &buying_list(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::ZERO);
        assert_eq!(
            result.unset_bundle_items,
            vec![ItemCode::from("UNKNOWN-A"), ItemCode::from("UNKNOWN-B")]
        );
    }

    #[test]
    fn bundle_of_bundle_inner_priced_as_leaf() {
        // Upstream does NOT support nested bundles. The bundle line loop (:225-246)
        // checks only for a BOM (:227) and otherwise reads `Item Price` (:241) — it
        // never re-queries `Product Bundle`. So the inner bundle is priced as an
        // ordinary item from its own `Item Price`, and its children are ignored.
        //
        // OUTER = 1× INNER + 1× DRINK @ ₹15
        // INNER is itself a bundle (2× X @ ₹100 each) but has its own Item Price ₹40.
        // Cost = ₹40 + ₹15 = ₹55, NOT ₹200 + ₹15.
        let mut bundles = FakeBundleRepo::new();
        bundles.insert("OUTER", vec![("INNER", dec!(1)), ("DRINK", dec!(1))]);
        bundles.insert("INNER", vec![("X", dec!(2))]);

        let boms = FakeBomRepo::new();
        let mut prices = FakePriceRepo::new();
        prices.insert("INNER", Money::new(dec!(40)));
        prices.insert("DRINK", Money::new(dec!(15)));
        prices.insert("X", Money::new(dec!(100))); // never reached

        let result = cogs_for_item_with_bundles(
            &ItemCode::from("OUTER"),
            dec!(1),
            &buying_list(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::new(dec!(55)));
        assert!(result.unset_bundle_items.is_empty());
    }

    #[test]
    fn bundle_wins_over_bom_for_the_same_item() {
        // The precedence rule, straight from the SQL partition: `pb_item_sales`
        // (:147) selects on `d.new_item_code IS NOT NULL` (:170) and never joins
        // `tabBOM`, while `bom_item_sales` requires `d.new_item_code IS NULL` (:139).
        // An item that is both is priced as a bundle; its BOM is ignored entirely.
        //
        // DUAL as bundle: 1× CHEAP @ ₹1 → ₹1/unit
        // DUAL as BOM:    1× DEAR @ ₹999 → ₹999/unit
        let mut bundles = FakeBundleRepo::new();
        bundles.insert("DUAL", vec![("CHEAP", dec!(1))]);

        let mut boms = FakeBomRepo::new();
        boms.insert("DUAL", dec!(1), vec![("DEAR", dec!(1))]);

        let mut prices = FakePriceRepo::new();
        prices.insert("CHEAP", Money::new(dec!(1)));
        prices.insert("DEAR", Money::new(dec!(999)));

        let result = cogs_for_item_with_bundles(
            &ItemCode::from("DUAL"),
            dec!(1),
            &buying_list(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap();

        assert_eq!(result.cost, Money::new(dec!(1)));
    }

    #[test]
    fn bundle_zero_quantity_bom_propagates_the_error() {
        // The zero-divisor guard reaches through the bundle path too. Reusing
        // `Error::BomZeroQuantity` rather than adding a bundle-specific variant:
        // the failing document really is the BOM, and the bundle is only how we
        // arrived at it.
        let mut bundles = FakeBundleRepo::new();
        bundles.insert("BAD-BUNDLE", vec![("BAD-BOM", dec!(1))]);

        let mut boms = FakeBomRepo::new();
        boms.insert("BAD-BOM", dec!(0), vec![("ITEM-A", dec!(1))]);

        let prices = FakePriceRepo::new();

        let err = cogs_for_item_with_bundles(
            &ItemCode::from("BAD-BUNDLE"),
            dec!(1),
            &buying_list(),
            &bundles,
            &boms,
            &prices,
        )
        .unwrap_err();

        assert_eq!(err, Error::BomZeroQuantity(BomName::from("BOM-BAD-BOM")));
    }

    #[test]
    fn order_with_bundles_keeps_the_three_unset_lists_apart() {
        let mut bundles = FakeBundleRepo::new();
        bundles.insert("MEAL", vec![("RICE", dec!(1)), ("PICKLE", dec!(1))]);

        let mut boms = FakeBomRepo::new();
        boms.insert("SOUP", dec!(1), vec![("STOCK", dec!(1))]);

        let mut prices = FakePriceRepo::new();
        prices.insert("RICE", Money::new(dec!(20)));
        // PICKLE (bundle child), STOCK (BOM ingredient) and LOOSE (plain) unpriced.

        let line = |code: &str, qty: Decimal| OrderLine {
            item_code: ItemCode::from(code),
            item_name: code.to_owned(),
            qty,
            rate: Money::ZERO,
            comments: None,
            serve_priority: 0,
            indicate_course: false,
        };

        let lines = vec![line("MEAL", dec!(1)), line("SOUP", dec!(1)), line("LOOSE", dec!(1))];

        let result =
            cogs_for_order_with_bundles(&lines, &buying_list(), &bundles, &boms, &prices).unwrap();

        // Only MEAL contributes: ₹20. SOUP is guarded to zero, LOOSE has no price.
        assert_eq!(result.cost, Money::new(dec!(20)));
        assert_eq!(result.unset_bundle_items, vec![ItemCode::from("PICKLE")]);
        assert_eq!(result.unset_bom_items, vec![ItemCode::from("STOCK")]);
        assert_eq!(result.unset_item_prices, vec![ItemCode::from("LOOSE")]);
    }
}
