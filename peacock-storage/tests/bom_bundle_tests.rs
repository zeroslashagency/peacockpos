//! Lane 2D acceptance tests: `BomRepo` and `ProductBundleRepo` against real Postgres.
//!
//! The tests that matter most are the ones that re-run `peacock-core`'s own `cogs.rs`
//! cases with storage swapped in for the in-memory fakes. `cogs.rs` proves the arithmetic;
//! these prove the repositories feed it the same data, which is the half v1 got wrong.
//!
//! Every test builds its own freshly migrated database (`support::TestDb`), so a green run
//! is also evidence that `003_bom_bundle.sql` applies from empty.

mod support;

use std::collections::BTreeSet;

use peacock_core::cogs::{cogs_for_item, cogs_for_item_with_bundles, MAX_LEVEL};
use peacock_core::error::Error as DomainError;
use peacock_core::ids::{BomName, ItemCode, PriceListName};
use peacock_core::money::Money;
use peacock_core::ports::{BomRepo, PriceRepo, ProductBundleRepo};
use peacock_storage::repos::{PgBomRepo, PgProductBundleRepo};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use support::TestDb;

const BUYING: &str = "Standard Buying";

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// A `PriceRepo` over the `item_prices` table.
///
/// Lane 2C owns the real `PgPriceRepo`; this is the narrow slice these tests need to drive
/// `cogs` end to end without taking a dependency on that lane's in-flight code. It hits the
/// same table and the same `(item_code, price_list)` index.
struct TestPriceRepo {
    pool: sqlx::PgPool,
}

impl PriceRepo for TestPriceRepo {
    fn item_price(
        &self,
        item: &ItemCode,
        price_list: &PriceListName,
    ) -> peacock_core::error::Result<Option<Money>> {
        let pool = self.pool.clone();
        let item = item.as_str().to_owned();
        let list = price_list.as_str().to_owned();

        let rate: Option<Decimal> = block(async move {
            sqlx::query_scalar(
                "SELECT rate FROM item_prices
                 WHERE item_code = $1 AND price_list = $2 AND valid_from IS NULL",
            )
            .bind(&item)
            .bind(&list)
            .fetch_optional(&pool)
            .await
            .expect("item price lookup")
        });

        Ok(rate.map(Money::new))
    }
}

/// Drive a future from a sync context, the same way `peacock_storage::repos::blocking` does.
///
/// `block_in_place` hands the reactor to a sibling worker while this one parks, so the pool
/// connection the query is waiting on keeps being polled. Driving it on a private runtime
/// instead would leave that socket unpolled until the pool's acquire timeout fired — which
/// is why every test in this file is `flavor = "multi_thread"`.
fn block<F>(fut: F) -> F::Output
where
    F: std::future::Future,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

struct Fixture {
    db: TestDb,
}

impl Fixture {
    async fn new() -> Fixture {
        let db = TestDb::new().await;
        sqlx::query("INSERT INTO price_lists (name, buying) VALUES ($1, true)")
            .bind(BUYING)
            .execute(db.pool())
            .await
            .expect("seed price list");
        Fixture { db }
    }

    fn pool(&self) -> &sqlx::PgPool {
        self.db.pool()
    }

    fn boms(&self) -> PgBomRepo {
        PgBomRepo::new(self.pool().clone())
    }

    fn bundles(&self) -> PgProductBundleRepo {
        PgProductBundleRepo::new(self.pool().clone())
    }

    fn prices(&self) -> TestPriceRepo {
        TestPriceRepo {
            pool: self.pool().clone(),
        }
    }

    /// Idempotent so a test can name an item without tracking whether it already exists.
    async fn item(&self, code: &str) {
        sqlx::query(
            "INSERT INTO items (code, name) VALUES ($1, $1) ON CONFLICT (code) DO NOTHING",
        )
        .bind(code)
        .execute(self.pool())
        .await
        .expect("seed item");
    }

    async fn price(&self, code: &str, rate: Decimal) {
        self.item(code).await;
        sqlx::query("INSERT INTO item_prices (item_code, price_list, rate) VALUES ($1, $2, $3)")
            .bind(code)
            .bind(BUYING)
            .bind(rate)
            .execute(self.pool())
            .await
            .expect("seed price");
    }

    /// A submitted, active, default BOM: the shape `BomRepo::find_for_item` looks for.
    async fn bom(&self, item: &str, quantity: Decimal, lines: &[(&str, Decimal)]) -> String {
        self.bom_with_status(item, quantity, lines, "Submitted", true, true)
            .await
    }

    async fn bom_with_status(
        &self,
        item: &str,
        quantity: Decimal,
        lines: &[(&str, Decimal)],
        status: &str,
        is_active: bool,
        is_default: bool,
    ) -> String {
        self.item(item).await;
        let name = format!("BOM-{item}-{status}-{is_active}-{is_default}");

        sqlx::query(
            "INSERT INTO boms (name, item, quantity, status, is_active, is_default)
             VALUES ($1, $2, $3, $4::bom_status, $5, $6)",
        )
        .bind(&name)
        .bind(item)
        .bind(quantity)
        .bind(status)
        .bind(is_active)
        .bind(is_default)
        .execute(self.pool())
        .await
        .expect("seed bom");

        for (idx, (code, qty)) in lines.iter().enumerate() {
            self.item(code).await;
            sqlx::query(
                "INSERT INTO bom_lines (bom, idx, item_code, quantity) VALUES ($1, $2, $3, $4)",
            )
            .bind(&name)
            .bind(idx as i32 + 1)
            .bind(code)
            .bind(qty)
            .execute(self.pool())
            .await
            .expect("seed bom line");
        }

        name
    }

    async fn bundle(&self, new_item_code: &str, lines: &[(&str, Decimal)]) -> String {
        self.item(new_item_code).await;
        let name = format!("PB-{new_item_code}");

        sqlx::query("INSERT INTO product_bundles (name, new_item_code) VALUES ($1, $2)")
            .bind(&name)
            .bind(new_item_code)
            .execute(self.pool())
            .await
            .expect("seed bundle");

        for (idx, (code, qty)) in lines.iter().enumerate() {
            self.item(code).await;
            sqlx::query(
                "INSERT INTO product_bundle_lines (bundle, idx, item_code, quantity)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(&name)
            .bind(idx as i32 + 1)
            .bind(code)
            .bind(qty)
            .execute(self.pool())
            .await
            .expect("seed bundle line");
        }

        name
    }
}

fn buying() -> PriceListName {
    PriceListName::from(BUYING)
}

// ---------------------------------------------------------------------------
// 1. Migration shape
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn migration_creates_the_bom_and_bundle_tables() {
    let fx = Fixture::new().await;

    let found: BTreeSet<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
    )
    .fetch_all(fx.pool())
    .await
    .expect("list tables")
    .into_iter()
    .collect();

    for expected in [
        "boms",
        "bom_lines",
        "product_bundles",
        "product_bundle_lines",
    ] {
        assert!(found.contains(expected), "table {expected} missing");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn quantity_columns_are_numeric_and_bom_batch_quantity_is_not_nullable() {
    // The schema half of the v1 defence. `boms.quantity` is the divisor
    // (ury_daily_p_and_l.py:38); if it were nullable a repository could read NULL and
    // "helpfully" default to 1, which is exactly the 10x bug. If it were float, the
    // division would drift off the paisa.
    let fx = Fixture::new().await;

    /// (table, is_nullable, numeric_precision, numeric_scale)
    type QuantityColumn = (String, String, Option<i32>, Option<i32>);

    let cols: Vec<QuantityColumn> = sqlx::query_as(
        "SELECT table_name, is_nullable, numeric_precision, numeric_scale
         FROM information_schema.columns
         WHERE table_schema = 'public'
           AND column_name = 'quantity'
           AND table_name IN ('boms','bom_lines','product_bundle_lines')",
    )
    .fetch_all(fx.pool())
    .await
    .expect("describe quantity columns");

    assert_eq!(cols.len(), 3, "expected three quantity columns, got {cols:?}");

    for (table, nullable, precision, scale) in cols {
        assert_eq!(nullable, "NO", "{table}.quantity must be NOT NULL");
        assert_eq!(precision, Some(18), "{table}.quantity precision");
        assert_eq!(scale, Some(6), "{table}.quantity scale");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bom_batch_quantity_must_be_strictly_positive() {
    // `cogs::bom_cost_per_unit` raises BomZeroQuantity rather than dividing by zero. A row
    // that would trigger it cannot be stored in the first place.
    let fx = Fixture::new().await;
    fx.item("BAD").await;

    for bad in [dec!(0), dec!(-1)] {
        let err = sqlx::query("INSERT INTO boms (name, item, quantity) VALUES ('B', 'BAD', $1)")
            .bind(bad)
            .execute(fx.pool())
            .await
            .expect_err("non-positive batch quantity accepted");
        assert_eq!(
            err.as_database_error().and_then(|e| e.constraint()),
            Some("boms_quantity_positive"),
            "wrong constraint for quantity {bad}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn child_tables_have_the_expected_fks_indexes_and_ordering() {
    let fx = Fixture::new().await;

    // FK + cascade: a BOM's lines are embed-only and go with it.
    let name = fx
        .bom("SANDWICH", dec!(1), &[("BREAD", dec!(2)), ("CHEESE", dec!(1))])
        .await;

    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM bom_lines WHERE bom = $1")
        .bind(&name)
        .fetch_one(fx.pool())
        .await
        .expect("count lines");
    assert_eq!(lines, 2);

    sqlx::query("DELETE FROM boms WHERE name = $1")
        .bind(&name)
        .execute(fx.pool())
        .await
        .expect("delete bom");

    let orphans: i64 = sqlx::query_scalar("SELECT count(*) FROM bom_lines WHERE bom = $1")
        .bind(&name)
        .fetch_one(fx.pool())
        .await
        .expect("count orphans");
    assert_eq!(orphans, 0, "bom_lines survived the parent delete");

    // A line pointing at a nonexistent item is rejected: `cogs` would otherwise flag a
    // phantom ingredient as an unset price.
    fx.item("REAL").await;
    sqlx::query("INSERT INTO boms (name, item, quantity) VALUES ('B2','REAL',1)")
        .execute(fx.pool())
        .await
        .expect("insert bom");
    let err = sqlx::query(
        "INSERT INTO bom_lines (bom, idx, item_code, quantity) VALUES ('B2',1,'GHOST',1)",
    )
    .execute(fx.pool())
    .await
    .expect_err("FK to a missing item accepted");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23503")
    );

    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE schemaname = 'public'")
            .fetch_all(fx.pool())
            .await
            .expect("list indexes");
    let has = |needle: &str| indexes.iter().any(|d| d.to_lowercase().contains(needle));

    // find_for_item runs once per BOM line during the explosion; it has to be an index hit.
    assert!(
        has("on public.boms using btree (item)"),
        "missing boms(item) index: {indexes:?}"
    );
    assert!(
        has("on public.bom_lines using btree (item_code)"),
        "missing bom_lines(item_code) index"
    );
    assert!(
        has("on public.product_bundles using btree (new_item_code)"),
        "missing product_bundles(new_item_code) index"
    );
    assert!(
        has("on public.product_bundle_lines using btree (item_code)"),
        "missing product_bundle_lines(item_code) index"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_component_lines_are_both_kept() {
    // ERPNext permits the same ingredient twice, and upstream costs each line separately.
    // Keying the child table on item_code would collapse them and halve that cost.
    let fx = Fixture::new().await;
    fx.price("SPICE", dec!(3)).await;
    fx.bom("MIX", dec!(1), &[("SPICE", dec!(2)), ("SPICE", dec!(5))])
        .await;

    let bom = fx
        .boms()
        .find_for_item(&ItemCode::from("MIX"))
        .expect("lookup")
        .expect("bom exists");

    assert_eq!(bom.items.len(), 2, "duplicate lines collapsed: {bom:?}");
    // 2x3 + 5x3 = 21, not 15 or 6.
    let result = cogs_for_item(
        &ItemCode::from("MIX"),
        dec!(1),
        &buying(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");
    assert_eq!(result.cost, Money::new(dec!(21)));
}

#[tokio::test(flavor = "multi_thread")]
async fn self_referencing_lines_are_rejected() {
    // An infinite explosion that MAX_LEVEL would silently truncate into an understated
    // cost rather than surface as an error.
    let fx = Fixture::new().await;
    fx.item("LOOP").await;
    sqlx::query("INSERT INTO boms (name, item, quantity) VALUES ('B','LOOP',1)")
        .execute(fx.pool())
        .await
        .expect("insert bom");

    let err = sqlx::query(
        "INSERT INTO bom_lines (bom, idx, item_code, quantity) VALUES ('B',1,'LOOP',1)",
    )
    .execute(fx.pool())
    .await
    .expect_err("self-referencing BOM line accepted");
    assert_eq!(
        err.as_database_error().and_then(|e| e.constraint()),
        Some("bom_lines_no_self_reference")
    );

    sqlx::query("INSERT INTO product_bundles (name, new_item_code) VALUES ('PB','LOOP')")
        .execute(fx.pool())
        .await
        .expect("insert bundle");
    let err = sqlx::query(
        "INSERT INTO product_bundle_lines (bundle, idx, item_code, quantity)
         VALUES ('PB',1,'LOOP',1)",
    )
    .execute(fx.pool())
    .await
    .expect_err("self-referencing bundle line accepted");
    assert_eq!(
        err.as_database_error().and_then(|e| e.constraint()),
        Some("product_bundle_lines_no_self_reference")
    );
}

// ---------------------------------------------------------------------------
// 2. BomRepo::find_for_item
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn missing_bom_returns_none_not_an_error() {
    // The plain-item bucket is reached exactly by this None (ury_daily_p_and_l.py:102-103).
    // Erroring here would fail every item that is not manufactured.
    let fx = Fixture::new().await;
    fx.item("PLAIN").await;

    let repo = fx.boms();
    assert!(repo
        .find_for_item(&ItemCode::from("PLAIN"))
        .expect("lookup must not fail")
        .is_none());
    // Nor for an item that does not exist at all.
    assert!(repo
        .find_for_item(&ItemCode::from("NO-SUCH-ITEM"))
        .expect("lookup must not fail")
        .is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn only_active_default_submitted_boms_are_visible() {
    // The BOM lookup filters is_active=1, is_default=1, docstatus=1 (:227). Each excluded
    // combination is checked separately so a partially-correct predicate cannot pass.
    let fx = Fixture::new().await;
    let repo = fx.boms();

    fx.bom_with_status("DRAFT-ONLY", dec!(1), &[("X", dec!(1))], "Draft", true, true)
        .await;
    fx.bom_with_status(
        "CANCELLED-ONLY",
        dec!(1),
        &[("X", dec!(1))],
        "Cancelled",
        true,
        true,
    )
    .await;
    fx.bom_with_status(
        "INACTIVE-ONLY",
        dec!(1),
        &[("X", dec!(1))],
        "Submitted",
        false,
        true,
    )
    .await;
    fx.bom_with_status(
        "NON-DEFAULT-ONLY",
        dec!(1),
        &[("X", dec!(1))],
        "Submitted",
        true,
        false,
    )
    .await;

    for hidden in [
        "DRAFT-ONLY",
        "CANCELLED-ONLY",
        "INACTIVE-ONLY",
        "NON-DEFAULT-ONLY",
    ] {
        assert!(
            repo.find_for_item(&ItemCode::from(hidden))
                .expect("lookup")
                .is_none(),
            "{hidden} should be invisible to find_for_item"
        );
    }

    // A submitted active default alongside the excluded rows is still found.
    fx.bom_with_status("DRAFT-ONLY", dec!(4), &[("X", dec!(1))], "Submitted", true, true)
        .await;
    let found = repo
        .find_for_item(&ItemCode::from("DRAFT-ONLY"))
        .expect("lookup")
        .expect("submitted bom should be found");
    assert_eq!(found.quantity, dec!(4));
}

#[tokio::test(flavor = "multi_thread")]
async fn at_most_one_active_default_bom_per_item() {
    // Upstream took boms[0] from an unordered result set, so with duplicates its COGS
    // depended on physical row order. The partial unique index removes the ambiguity.
    let fx = Fixture::new().await;
    fx.bom("CHAI", dec!(10), &[("TEA", dec!(1))]).await;

    let err = sqlx::query(
        "INSERT INTO boms (name, item, quantity, status) VALUES ('BOM-DUP','CHAI',5,'Submitted')",
    )
    .execute(fx.pool())
    .await
    .expect_err("second active default BOM accepted");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23505")
    );

    // A draft second BOM is fine — that is the normal "revision in progress" state.
    sqlx::query("INSERT INTO boms (name, item, quantity) VALUES ('BOM-REV','CHAI',5)")
        .execute(fx.pool())
        .await
        .expect("draft revision should be allowed alongside the submitted BOM");
}

#[tokio::test(flavor = "multi_thread")]
async fn bom_lines_read_back_in_idx_order_with_exact_quantities() {
    let fx = Fixture::new().await;
    // A fractional line quantity that a f64 round-trip would perturb.
    fx.bom(
        "BLEND",
        dec!(7),
        &[
            ("A", dec!(0.000001)),
            ("B", dec!(123456789012.123456)),
            ("C", dec!(0)),
        ],
    )
    .await;

    let bom = fx
        .boms()
        .find_for_item(&ItemCode::from("BLEND"))
        .expect("lookup")
        .expect("bom exists");

    assert_eq!(bom.name, BomName::from("BOM-BLEND-Submitted-true-true"));
    assert_eq!(bom.quantity, dec!(7));
    assert_eq!(
        bom.items
            .iter()
            .map(|l| (l.item_code.as_str().to_owned(), l.qty))
            .collect::<Vec<_>>(),
        vec![
            ("A".to_owned(), dec!(0.000001)),
            ("B".to_owned(), dec!(123456789012.123456)),
            ("C".to_owned(), dec!(0)),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn items_is_bom_flag_tracks_the_lookup_predicate() {
    // 001_core_tables.sql leaves `items.is_bom` for this lane to maintain. It is a cache of
    // the same predicate find_for_item uses, so the two must never disagree.
    let fx = Fixture::new().await;
    let repo = fx.boms();

    fx.item("CHAI").await;
    let flag = |code: &'static str| {
        let pool = fx.pool().clone();
        async move {
            sqlx::query_scalar::<_, bool>("SELECT is_bom FROM items WHERE code = $1")
                .bind(code)
                .fetch_one(&pool)
                .await
                .expect("read is_bom")
        }
    };

    assert!(!flag("CHAI").await, "no BOM yet");

    let name = fx.bom("CHAI", dec!(10), &[("TEA", dec!(1))]).await;
    assert!(flag("CHAI").await, "submitted BOM should set is_bom");
    assert!(repo
        .find_for_item(&ItemCode::from("CHAI"))
        .expect("lookup")
        .is_some());

    // Retiring the BOM must clear the flag, and the lookup must agree.
    sqlx::query("UPDATE boms SET is_active = false WHERE name = $1")
        .bind(&name)
        .execute(fx.pool())
        .await
        .expect("deactivate");
    assert!(!flag("CHAI").await, "inactive BOM should clear is_bom");
    assert!(repo
        .find_for_item(&ItemCode::from("CHAI"))
        .expect("lookup")
        .is_none());

    sqlx::query("UPDATE boms SET is_active = true WHERE name = $1")
        .bind(&name)
        .execute(fx.pool())
        .await
        .expect("reactivate");
    assert!(flag("CHAI").await);

    sqlx::query("DELETE FROM boms WHERE name = $1")
        .bind(&name)
        .execute(fx.pool())
        .await
        .expect("delete");
    assert!(!flag("CHAI").await, "deleting the BOM should clear is_bom");
}

// ---------------------------------------------------------------------------
// 3. The v1 bug: quantity normalisation
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn batch_bom_normalises_to_per_unit_cost() {
    // THE regression test, and the storage twin of `cogs::one_level_bom_with_quantity_not_one`
    // and parity fixture 07.
    //
    // By hand:
    //   BOM for MASALA-CHAI produces 10 cups (quantity = 10)
    //   TEA-LEAVES 10g @ 2.00/g  = 20
    //   MILK      100ml @ 0.50/ml = 50
    //   batch = 70  ->  per cup = 70 / 10 = 7
    //   5 cups ordered            -> 35
    //
    // v1 skipped the division and charged 70/cup: 350, wrong by exactly 10x.
    let fx = Fixture::new().await;
    fx.price("TEA-LEAVES", dec!(2.00)).await;
    fx.price("MILK", dec!(0.50)).await;
    fx.bom(
        "MASALA-CHAI",
        dec!(10),
        &[("TEA-LEAVES", dec!(10)), ("MILK", dec!(100))],
    )
    .await;

    // The divisor survives the round trip. If this is 1, everything below is 10x wrong.
    let bom = fx
        .boms()
        .find_for_item(&ItemCode::from("MASALA-CHAI"))
        .expect("lookup")
        .expect("bom exists");
    assert_eq!(bom.quantity, dec!(10), "batch quantity lost in storage");

    let result = cogs_for_item(
        &ItemCode::from("MASALA-CHAI"),
        dec!(5),
        &buying(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    assert_eq!(result.cost, Money::new(dec!(35.00)));
    assert_ne!(
        result.cost,
        Money::new(dec!(350.00)),
        "the v1 10x bug is back"
    );
    assert!(result.unset_bom_items.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn both_levels_normalise_independently() {
    // Storage twin of `cogs::two_level_bom_both_normalising`, and parity fixture 08.
    //
    //   BURGER BOM (batch 5): 5 x PATTY @10 + 5 x BUN @5 = 75 -> 15/unit
    //   COMBO  BOM (batch 1): 2 x BURGER + 1 x FRIES @20     = 50 -> 50/unit
    //   qty 3 -> 150
    let fx = Fixture::new().await;
    fx.price("PATTY", dec!(10)).await;
    fx.price("BUN", dec!(5)).await;
    fx.price("FRIES", dec!(20)).await;
    fx.bom("BURGER", dec!(5), &[("PATTY", dec!(5)), ("BUN", dec!(5))])
        .await;
    fx.bom(
        "COMBO-MEAL",
        dec!(1),
        &[("BURGER", dec!(2)), ("FRIES", dec!(1))],
    )
    .await;

    let result = cogs_for_item(
        &ItemCode::from("COMBO-MEAL"),
        dec!(3),
        &buying(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    assert_eq!(result.cost, Money::new(dec!(150)));
    assert!(result.unset_bom_items.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn fractional_normalisation_stays_exact() {
    // 10 / 3 has no finite decimal expansion. Storing quantities as NUMERIC keeps the
    // division on rust_decimal, so the result matches the domain's own arithmetic exactly
    // rather than to within a float epsilon.
    let fx = Fixture::new().await;
    fx.price("ITEM-Y", dec!(10)).await;
    fx.bom("ITEM-X", dec!(3), &[("ITEM-Y", dec!(1))]).await;

    let result = cogs_for_item(
        &ItemCode::from("ITEM-X"),
        dec!(0.5),
        &buying(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    assert_eq!(result.cost, Money::new(dec!(10) / dec!(3) / dec!(2)));
}

#[tokio::test(flavor = "multi_thread")]
async fn third_level_bom_is_priced_as_a_leaf() {
    // MAX_LEVEL = 2, matching upstream's two hardcoded functions. The third level exists in
    // storage and is deliberately never exploded — parity fixture 09.
    //
    //   PIZZA (batch 1) -> 1 x BASE
    //   BASE  (batch 2) -> 2 x DOUGH
    //   DOUGH (batch 10) -> 10 x FLOUR + 5 x WATER   <-- ignored, DOUGH priced at 30
    //
    //   BASE  = 2 x 30 = 60 / 2 = 30
    //   PIZZA = 1 x 30 = 30 / 1 = 30
    assert_eq!(MAX_LEVEL, 2);

    let fx = Fixture::new().await;
    fx.price("DOUGH", dec!(30)).await;
    fx.price("FLOUR", dec!(1)).await;
    fx.price("WATER", dec!(0.5)).await;
    fx.bom("PIZZA", dec!(1), &[("BASE", dec!(1))]).await;
    fx.bom("BASE", dec!(2), &[("DOUGH", dec!(2))]).await;
    fx.bom("DOUGH", dec!(10), &[("FLOUR", dec!(10)), ("WATER", dec!(5))])
        .await;

    // The level-3 BOM really is stored — otherwise this test would pass vacuously.
    assert!(fx
        .boms()
        .find_for_item(&ItemCode::from("DOUGH"))
        .expect("lookup")
        .is_some());

    let result = cogs_for_item(
        &ItemCode::from("PIZZA"),
        dec!(1),
        &buying(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    assert_eq!(result.cost, Money::new(dec!(30)));
    assert!(result.unset_bom_items.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_item_price_surfaces_in_unset_bom_items() {
    // Upstream accumulates every unpriced ingredient so the operator can see the gap
    // (:195). Dropping it turns a visible data problem into silently understated COGS.
    let fx = Fixture::new().await;
    fx.price("BREAD", dec!(5)).await;
    fx.item("CHEESE").await; // deliberately unpriced
    fx.bom("SANDWICH", dec!(1), &[("BREAD", dec!(2)), ("CHEESE", dec!(1))])
        .await;

    let result = cogs_for_item(
        &ItemCode::from("SANDWICH"),
        dec!(2),
        &buying(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    // 2 x 5 = 10 per unit, x2 = 20. CHEESE contributes nothing but is reported.
    assert_eq!(result.cost, Money::new(dec!(20)));
    assert_eq!(result.unset_bom_items, vec![ItemCode::from("CHEESE")]);
    assert!(result.unset_item_prices.is_empty());
    assert!(result.unset_bundle_items.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_item_missing_price_lands_in_the_items_list_not_the_bom_list() {
    // The three lists are labelled separately upstream (:262-264) and the label is the
    // actionable part. Parity fixture 22.
    let fx = Fixture::new().await;
    fx.item("NO-PRICE").await;

    let result = cogs_for_item(
        &ItemCode::from("NO-PRICE"),
        dec!(3),
        &buying(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    assert_eq!(result.cost, Money::ZERO);
    assert_eq!(result.unset_item_prices, vec![ItemCode::from("NO-PRICE")]);
    assert!(result.unset_bom_items.is_empty());
}

// ---------------------------------------------------------------------------
// 4. ProductBundleRepo
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn missing_bundle_returns_none() {
    let fx = Fixture::new().await;
    fx.item("PLAIN").await;

    let repo = fx.bundles();
    assert!(repo
        .find_by_new_item_code(&ItemCode::from("PLAIN"))
        .expect("lookup must not fail")
        .is_none());
    assert!(repo
        .find_by_new_item_code(&ItemCode::from("NO-SUCH-ITEM"))
        .expect("lookup must not fail")
        .is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn bundle_lines_read_back_in_idx_order() {
    let fx = Fixture::new().await;
    fx.bundle(
        "THALI",
        &[("ROTI", dec!(2)), ("DAL", dec!(1)), ("RICE", dec!(0.5))],
    )
    .await;

    let bundle = fx
        .bundles()
        .find_by_new_item_code(&ItemCode::from("THALI"))
        .expect("lookup")
        .expect("bundle exists");

    assert_eq!(bundle.new_item_code, ItemCode::from("THALI"));
    assert_eq!(
        bundle
            .items
            .iter()
            .map(|l| (l.item_code.as_str().to_owned(), l.qty))
            .collect::<Vec<_>>(),
        vec![
            ("ROTI".to_owned(), dec!(2)),
            ("DAL".to_owned(), dec!(1)),
            ("RICE".to_owned(), dec!(0.5)),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_one_live_bundle_per_sold_item_code() {
    // Two bundles selling the same code would make the cost basis depend on row order,
    // which is exactly what upstream's `pb_items[0]` did.
    let fx = Fixture::new().await;
    fx.bundle("THALI", &[("ROTI", dec!(2))]).await;

    let err = sqlx::query("INSERT INTO product_bundles (name, new_item_code) VALUES ('PB2','THALI')")
        .execute(fx.pool())
        .await
        .expect_err("duplicate bundle for one item code accepted");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23505")
    );

    // Soft-deleting the old bundle frees the code for a replacement.
    sqlx::query("UPDATE product_bundles SET deleted_at = now() WHERE name = 'PB-THALI'")
        .execute(fx.pool())
        .await
        .expect("retire bundle");
    sqlx::query("INSERT INTO product_bundles (name, new_item_code) VALUES ('PB2','THALI')")
        .execute(fx.pool())
        .await
        .expect("replacement bundle should be allowed");

    // And the retired one is invisible to the lookup.
    let bundle = fx
        .bundles()
        .find_by_new_item_code(&ItemCode::from("THALI"))
        .expect("lookup")
        .expect("replacement should be found");
    assert!(bundle.items.is_empty(), "read the wrong bundle: {bundle:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn draft_bundles_are_visible_because_upstream_applies_no_status_filter() {
    // ports.rs:104-106 is explicit: unlike the BOM lookup there is no docstatus or
    // is_active filter on Product Bundle (:222), so every stored bundle captures its item.
    // The schema has no status column at all, which is what this asserts.
    let fx = Fixture::new().await;

    let status_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'product_bundles'
           AND column_name IN ('status','docstatus','is_active','disabled')",
    )
    .fetch_all(fx.pool())
    .await
    .expect("look for a status column");

    assert!(
        status_columns.is_empty(),
        "product_bundles has status-like columns {status_columns:?}; upstream filters none \
         of them, so their presence invites a divergence"
    );
}

// ---------------------------------------------------------------------------
// 5. Bundle -> BOM -> plain precedence
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn bundle_of_plain_items() {
    // Parity fixture 15. THALI = 2 x ROTI @5 + 1 x DAL @30 = 40/unit, qty 3 -> 120.
    let fx = Fixture::new().await;
    fx.price("ROTI", dec!(5)).await;
    fx.price("DAL", dec!(30)).await;
    fx.bundle("THALI", &[("ROTI", dec!(2)), ("DAL", dec!(1))])
        .await;

    let result = cogs_for_item_with_bundles(
        &ItemCode::from("THALI"),
        dec!(3),
        &buying(),
        &fx.bundles(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    assert_eq!(result.cost, Money::new(dec!(120)));
    assert!(result.unset_bundle_items.is_empty());
    assert!(result.unset_bom_items.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn bundle_line_with_batch_bom_normalises() {
    // Parity fixture 16 — the normalisation case reached through a bundle.
    //
    //   MASALA-CHAI BOM (batch 10): 10 x TEA @2 + 100 x MILK @0.50 = 70 -> 7/cup
    //   COMBO bundle: 2 x MASALA-CHAI + 1 x SAMOSA @12 = 26/unit
    //   qty 4 -> 104.   Without the /10: 2x70 + 12 = 152/unit -> 608.
    let fx = Fixture::new().await;
    fx.price("TEA-LEAVES", dec!(2.00)).await;
    fx.price("MILK", dec!(0.50)).await;
    fx.price("SAMOSA", dec!(12)).await;
    fx.bom(
        "MASALA-CHAI",
        dec!(10),
        &[("TEA-LEAVES", dec!(10)), ("MILK", dec!(100))],
    )
    .await;
    fx.bundle("COMBO", &[("MASALA-CHAI", dec!(2)), ("SAMOSA", dec!(1))])
        .await;

    let result = cogs_for_item_with_bundles(
        &ItemCode::from("COMBO"),
        dec!(4),
        &buying(),
        &fx.bundles(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    assert_eq!(result.cost, Money::new(dec!(104)));
    assert_ne!(result.cost, Money::new(dec!(608)), "batch normalisation lost");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_adds_no_bom_depth() {
    // Parity fixture 17. Upstream calls the same `inner_bom_process` for a bundle line
    // (:231) as for a top-level BOM item (:201), so the walk still gets its two levels.
    //
    //   PLATTER bundle -> 1 x COMBO-MEAL
    //   COMBO-MEAL BOM (level 1, batch 1): 2 x BURGER + 1 x FRIES @20
    //   BURGER     BOM (level 2, batch 5): 5 x PATTY @10 + 5 x BUN @5 = 75 -> 15
    //   COMBO-MEAL = 2x15 + 20 = 50 -> bundle 50, qty 1 -> 50.
    //
    // If the bundle consumed a level, BURGER would be priced as a leaf; it has no Item
    // Price, so the cost would collapse to 20 and BURGER would appear in the unset list.
    let fx = Fixture::new().await;
    fx.price("PATTY", dec!(10)).await;
    fx.price("BUN", dec!(5)).await;
    fx.price("FRIES", dec!(20)).await;
    fx.bom("BURGER", dec!(5), &[("PATTY", dec!(5)), ("BUN", dec!(5))])
        .await;
    fx.bom(
        "COMBO-MEAL",
        dec!(1),
        &[("BURGER", dec!(2)), ("FRIES", dec!(1))],
    )
    .await;
    fx.bundle("PLATTER", &[("COMBO-MEAL", dec!(1))]).await;

    let via_bundle = cogs_for_item_with_bundles(
        &ItemCode::from("PLATTER"),
        dec!(1),
        &buying(),
        &fx.bundles(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    assert_eq!(via_bundle.cost, Money::new(dec!(50)));
    assert!(
        via_bundle.unset_bom_items.is_empty(),
        "BURGER got priced as a leaf, so the bundle ate a level: {via_bundle:?}"
    );

    // The same BOM reached from the top level costs the same — the direct proof.
    let direct = cogs_for_item(
        &ItemCode::from("COMBO-MEAL"),
        dec!(1),
        &buying(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");
    assert_eq!(direct.cost, via_bundle.cost);
}

#[tokio::test(flavor = "multi_thread")]
async fn bundle_wins_over_the_items_own_bom() {
    // Parity fixture 21. The precedence is a partition, not a fallback: the bundle query
    // joins no BOM table (:147-159) and both other buckets require the item not be a
    // bundle (:102, :139). So an item that is both is priced as a bundle and its own BOM is
    // never consulted.
    let fx = Fixture::new().await;
    fx.price("CHEAP-COMPONENT", dec!(1)).await;
    fx.price("EXPENSIVE-INGREDIENT", dec!(1000)).await;

    // A BOM that would cost 1000/unit if it were ever consulted.
    fx.bom("DUAL", dec!(1), &[("EXPENSIVE-INGREDIENT", dec!(1))])
        .await;
    // And a bundle for the same item that costs 2/unit.
    fx.bundle("DUAL", &[("CHEAP-COMPONENT", dec!(2))]).await;

    // Both really exist, so the test cannot pass by one of them being absent.
    assert!(fx
        .boms()
        .find_for_item(&ItemCode::from("DUAL"))
        .expect("lookup")
        .is_some());
    assert!(fx
        .bundles()
        .find_by_new_item_code(&ItemCode::from("DUAL"))
        .expect("lookup")
        .is_some());

    let result = cogs_for_item_with_bundles(
        &ItemCode::from("DUAL"),
        dec!(1),
        &buying(),
        &fx.bundles(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    assert_eq!(result.cost, Money::new(dec!(2)), "the BOM won; it must not");
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_bundle_child_is_priced_as_a_leaf() {
    // Parity fixture 20. Upstream never re-queries Product Bundle for a child line (:241),
    // so a bundle inside a bundle is priced from Item Price like any other leaf.
    //
    //   OUTER bundle -> 1 x INNER
    //   INNER is itself a bundle (of TOY @999) AND has an Item Price of 5.
    //   Expected: 5. Recursing would give 999.
    let fx = Fixture::new().await;
    fx.price("TOY", dec!(999)).await;
    fx.price("INNER", dec!(5)).await;
    fx.bundle("INNER", &[("TOY", dec!(1))]).await;
    fx.bundle("OUTER", &[("INNER", dec!(1))]).await;

    let result = cogs_for_item_with_bundles(
        &ItemCode::from("OUTER"),
        dec!(1),
        &buying(),
        &fx.bundles(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    assert_eq!(result.cost, Money::new(dec!(5)));
    assert_ne!(result.cost, Money::new(dec!(999)), "recursed into the nested bundle");
}

#[tokio::test(flavor = "multi_thread")]
async fn bundle_misses_are_labelled_separately_from_bom_misses() {
    // Parity fixture 18. A bundle child with no price is a BUNDLE SUB ITEM (:243); a miss
    // inside a bundle line's BOM is a BOM SUB ITEM (:236-237). Merging them would keep the
    // cost identical while stripping the routing information from the only place the gap
    // is surfaced.
    let fx = Fixture::new().await;
    fx.price("RICE", dec!(20)).await;
    fx.item("PICKLE").await; // bundle child, unpriced
    fx.price("BREAD", dec!(5)).await;
    fx.item("CHEESE").await; // BOM ingredient, unpriced
    fx.bom("SANDWICH", dec!(1), &[("BREAD", dec!(2)), ("CHEESE", dec!(1))])
        .await;
    fx.bundle(
        "MEAL",
        &[("RICE", dec!(1)), ("PICKLE", dec!(1)), ("SANDWICH", dec!(1))],
    )
    .await;

    let result = cogs_for_item_with_bundles(
        &ItemCode::from("MEAL"),
        dec!(2),
        &buying(),
        &fx.bundles(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    // 20 (RICE) + 0 (PICKLE) + 10 (SANDWICH) = 30/unit, x2 = 60.
    assert_eq!(result.cost, Money::new(dec!(60)));
    assert_eq!(result.unset_bundle_items, vec![ItemCode::from("PICKLE")]);
    assert_eq!(result.unset_bom_items, vec![ItemCode::from("CHEESE")]);
    assert!(result.unset_item_prices.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn fully_unpriced_bundle_contributes_zero_but_stays_visible() {
    // Parity fixture 19 and upstream's `if buying_price > 0` guard (:248): no cost row is
    // appended, so COGS gains nothing, but the gap must still be reported rather than
    // silently absorbed.
    let fx = Fixture::new().await;
    fx.item("UNKNOWN-A").await;
    fx.item("UNKNOWN-B").await;
    fx.bundle("MYSTERY", &[("UNKNOWN-A", dec!(1)), ("UNKNOWN-B", dec!(2))])
        .await;

    let result = cogs_for_item_with_bundles(
        &ItemCode::from("MYSTERY"),
        dec!(5),
        &buying(),
        &fx.bundles(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");

    assert_eq!(result.cost, Money::ZERO);
    assert_eq!(
        result.unset_bundle_items,
        vec![ItemCode::from("UNKNOWN-A"), ItemCode::from("UNKNOWN-B")]
    );
}

// ---------------------------------------------------------------------------
// 6. Prefetched snapshots
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn snapshots_give_the_same_answers_as_the_live_repositories() {
    // The prefetch path is the one a request handler should use, so it has to agree with
    // the per-lookup path to the paisa — otherwise COGS would depend on which one a caller
    // happened to pick.
    let fx = Fixture::new().await;
    fx.price("TEA-LEAVES", dec!(2.00)).await;
    fx.price("MILK", dec!(0.50)).await;
    fx.price("SAMOSA", dec!(12)).await;
    fx.price("PATTY", dec!(10)).await;
    fx.price("BUN", dec!(5)).await;
    fx.price("FRIES", dec!(20)).await;
    fx.bom(
        "MASALA-CHAI",
        dec!(10),
        &[("TEA-LEAVES", dec!(10)), ("MILK", dec!(100))],
    )
    .await;
    fx.bom("BURGER", dec!(5), &[("PATTY", dec!(5)), ("BUN", dec!(5))])
        .await;
    fx.bom(
        "COMBO-MEAL",
        dec!(1),
        &[("BURGER", dec!(2)), ("FRIES", dec!(1))],
    )
    .await;
    fx.bundle("COMBO", &[("MASALA-CHAI", dec!(2)), ("SAMOSA", dec!(1))])
        .await;

    let sold = vec![
        ItemCode::from("COMBO"),
        ItemCode::from("COMBO-MEAL"),
        ItemCode::from("SAMOSA"),
    ];

    let bundle_snapshot = fx.bundles().snapshot(&sold).await.expect("bundle snapshot");
    // A bundle line's BOM is walked at level 1, so its children are BOM roots too.
    let mut seeds = sold.clone();
    seeds.extend(bundle_snapshot.child_items());
    let bom_snapshot = fx
        .boms()
        .snapshot_for_items(&seeds)
        .await
        .expect("bom snapshot");

    assert_eq!(bundle_snapshot.len(), 1);
    // MASALA-CHAI and COMBO-MEAL as roots, BURGER as their level-2 child.
    assert_eq!(bom_snapshot.len(), 3, "closure not fully prefetched");

    for item in &sold {
        let live = cogs_for_item_with_bundles(
            item,
            dec!(3),
            &buying(),
            &fx.bundles(),
            &fx.boms(),
            &fx.prices(),
        )
        .expect("live cogs");

        let prefetched = cogs_for_item_with_bundles(
            item,
            dec!(3),
            &buying(),
            &bundle_snapshot,
            &bom_snapshot,
            &fx.prices(),
        )
        .expect("prefetched cogs");

        assert_eq!(live, prefetched, "snapshot diverged for {item}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_stops_at_max_level_and_prices_deeper_boms_as_leaves() {
    // The snapshot must not over-fetch either: prefetching a level-3 BOM would be dead
    // weight and would tempt a caller into exploding it, diverging from upstream.
    let fx = Fixture::new().await;
    fx.price("DOUGH", dec!(30)).await;
    fx.price("FLOUR", dec!(1)).await;
    fx.price("WATER", dec!(0.5)).await;
    fx.bom("PIZZA", dec!(1), &[("BASE", dec!(1))]).await;
    fx.bom("BASE", dec!(2), &[("DOUGH", dec!(2))]).await;
    fx.bom("DOUGH", dec!(10), &[("FLOUR", dec!(10)), ("WATER", dec!(5))])
        .await;

    let snapshot = fx
        .boms()
        .snapshot_for_items(&[ItemCode::from("PIZZA")])
        .await
        .expect("snapshot");

    assert!(snapshot.get(&ItemCode::from("PIZZA")).is_some());
    assert!(snapshot.get(&ItemCode::from("BASE")).is_some());
    assert!(
        snapshot.get(&ItemCode::from("DOUGH")).is_none(),
        "level-3 BOM prefetched; cogs never asks for it"
    );

    let prefetched = cogs_for_item(
        &ItemCode::from("PIZZA"),
        dec!(1),
        &buying(),
        &snapshot,
        &fx.prices(),
    )
    .expect("cogs");
    assert_eq!(prefetched.cost, Money::new(dec!(30)));

    let live = cogs_for_item(
        &ItemCode::from("PIZZA"),
        dec!(1),
        &buying(),
        &fx.boms(),
        &fx.prices(),
    )
    .expect("cogs");
    assert_eq!(live, prefetched);
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_input_and_unknown_items_are_handled_without_queries_failing() {
    let fx = Fixture::new().await;

    let boms = fx
        .boms()
        .snapshot_for_items(&[])
        .await
        .expect("empty bom snapshot");
    assert!(boms.is_empty());

    let bundles = fx.bundles().snapshot(&[]).await.expect("empty bundle snapshot");
    assert!(bundles.is_empty());

    let unknown = vec![ItemCode::from("NOPE")];
    assert!(fx
        .boms()
        .snapshot_for_items(&unknown)
        .await
        .expect("unknown bom snapshot")
        .is_empty());
    assert!(fx
        .bundles()
        .snapshot(&unknown)
        .await
        .expect("unknown bundle snapshot")
        .is_empty());

    // And an item outside the snapshot's closure reads as "no BOM", which is the same
    // answer the live repository gives.
    assert!(boms
        .find_for_item(&ItemCode::from("NOPE"))
        .expect("lookup")
        .is_none());
}

// ---------------------------------------------------------------------------
// 7. Failure modes
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn repository_lookups_work_from_a_sync_context_inside_a_runtime() {
    // The port traits are synchronous while sqlx is async, and `block_in_place` panics
    // under the current-thread runtime a bare `#[tokio::test]` builds. Every test in this
    // file exercises the bridge implicitly; this one says so out loud, including from a
    // plain thread with no runtime at all.
    let fx = Fixture::new().await;
    fx.bom("CHAI", dec!(10), &[("TEA", dec!(1))]).await;
    fx.bundle("THALI", &[("ROTI", dec!(1))]).await;

    let boms = fx.boms();
    let bundles = fx.bundles();

    assert!(boms
        .find_for_item(&ItemCode::from("CHAI"))
        .expect("lookup inside the runtime")
        .is_some());

    let out = std::thread::spawn(move || {
        (
            boms.find_for_item(&ItemCode::from("CHAI")).map(|o| o.is_some()),
            bundles
                .find_by_new_item_code(&ItemCode::from("THALI"))
                .map(|o| o.is_some()),
        )
    })
    .join()
    .expect("thread panicked");

    assert!(out.0.expect("bom lookup off-runtime"));
    assert!(out.1.expect("bundle lookup off-runtime"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_pool_surfaces_as_an_error_rather_than_a_missing_bom() {
    // A missing BOM and an unreachable database must not look the same: the first is the
    // plain-item bucket, the second would silently understate COGS for every BOM item.
    let fx = Fixture::new().await;
    fx.bom("CHAI", dec!(10), &[("TEA", dec!(1))]).await;

    let repo = fx.boms();
    assert!(repo
        .find_for_item(&ItemCode::from("CHAI"))
        .expect("lookup")
        .is_some());

    repo.pool().close().await;

    let err = repo
        .find_for_item(&ItemCode::from("CHAI"))
        .expect_err("closed pool reported success");
    assert!(
        !matches!(err, DomainError::BomZeroQuantity(_)),
        "infrastructure failure masqueraded as a domain error: {err:?}"
    );
}
