//! PostgreSQL implementation of [`BomRepo`] — Lane 2D.
//!
//! Backs the BOM cost basis in `peacock_core::cogs`. The arithmetic all lives in the
//! domain; this module's only job is to hand it a `Bom` whose `quantity` and whose line
//! `qty`s are exactly what the operator entered.
//!
//! # The v1 bug, and where it was actually possible
//!
//! Upstream normalises a batch BOM to a per-unit cost by dividing by the BOM's own batch
//! size (ury_daily_p_and_l.py:38, :57). v1 dropped that division and priced every unit at
//! the whole batch cost — a 70 rupee batch of 10 cups of chai became 70 per cup instead
//! of 7.
//!
//! `cogs::bom_cost_per_unit` performs the division unconditionally, so the domain cannot
//! reintroduce it. What a repository *can* still do is feed the wrong divisor:
//!
//! * select the batch quantity and silently default it to 1 when the column is NULL,
//! * read `bom_lines.quantity` into the batch field or vice versa,
//! * lose precision by round-tripping either through `f64`.
//!
//! All three are closed here. `boms.quantity` is `NOT NULL CHECK (> 0)` and read into a
//! non-`Option` `Decimal`, so there is no default to fall back on. The two quantities
//! come from different tables and are named `quantity` in both, but the row structs keep
//! them apart by type. And nothing in this file mentions a float: `NUMERIC(18,6)` maps
//! straight to `rust_decimal::Decimal`, which is what `Money` is built on.
//!
//! # Two levels is the walk's business, not this module's
//!
//! `cogs::MAX_LEVEL` is 2, and the level-2 call site simply does not ask for a child BOM
//! (`cogs.rs:186-190`). So this repository does not know or care how deep it is being
//! called from: it answers "the active default BOM for this item" and the walk decides
//! whether to ask again. A third-level BOM is stored perfectly happily and then never
//! looked up, which is exactly what fixture `09_cogs_three_level_max_depth.json` asserts.
//!
//! # Missing BOM is not an error
//!
//! `find_for_item` returns `Ok(None)` for an item with no BOM, an item with only draft or
//! non-default BOMs, and an item that does not exist at all. The plain-item bucket
//! (ury_daily_p_and_l.py:102-103) is reached precisely by that `None`, so turning it into
//! an error would make every non-BOM item fail.
//!
//! # Query count
//!
//! The trait is one lookup per call, which under COGS means one query per BOM line. For
//! anything on a request path use [`PgBomRepo::snapshot_for_items`], which loads the
//! whole two-level closure in a bounded number of queries and then serves
//! `find_for_item` from memory.

use std::collections::{HashMap, HashSet};

use rust_decimal::Decimal;
use sqlx::PgPool;

use peacock_core::error::Result as DomainResult;
use peacock_core::ids::{BomName, ItemCode};
use peacock_core::ports::{Bom, BomLine, BomRepo};

use super::blocking::block_on;
use super::to_domain_error;
use crate::error::StorageResult;

/// The predicate that defines "the BOM" for an item.
///
/// `is_active = 1 AND is_default = 1 AND docstatus = 1` upstream
/// (ury_daily_p_and_l.py:19, :227), with `docstatus` modelled as `bom_status` per the
/// plan's enum rule. Written once and shared by every query in this file so the lookup,
/// the prefetch and `items.is_bom` can never drift apart.
const ACTIVE_DEFAULT: &str =
    "is_active AND is_default AND status = 'Submitted' AND deleted_at IS NULL";

/// PostgreSQL-backed [`BomRepo`].
#[derive(Clone, Debug)]
pub struct PgBomRepo {
    pool: PgPool,
}

/// One `boms` row, before its lines are attached.
#[derive(Debug, sqlx::FromRow)]
struct BomRow {
    name: String,
    item: String,
    /// The divisor. `Decimal`, not `f64`, and not `Option` — see the module docs.
    quantity: Decimal,
}

/// One `bom_lines` row. `bom` is carried so a multi-BOM fetch can group by parent.
#[derive(Debug, sqlx::FromRow)]
struct BomLineRow {
    bom: String,
    item_code: String,
    quantity: Decimal,
}

impl PgBomRepo {
    pub fn new(pool: PgPool) -> Self {
        PgBomRepo { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The active default BOM for `item`, or `None`.
    ///
    /// Two queries: the header, then its lines ordered by `idx`. Splitting them keeps
    /// the batch quantity on a single row — a join would repeat it per line and invite a
    /// reader to treat a line quantity as the divisor.
    pub async fn find_for_item_async(&self, item: &ItemCode) -> StorageResult<Option<Bom>> {
        let sql = format!(
            "SELECT name, item, quantity FROM boms WHERE item = $1 AND {ACTIVE_DEFAULT}"
        );
        let Some(header) = sqlx::query_as::<_, BomRow>(&sql)
            .bind(item.as_str())
            .fetch_optional(&self.pool)
            .await?
        else {
            // No BOM, only drafts, or no such item: all three are the plain-item bucket.
            return Ok(None);
        };

        let lines = sqlx::query_as::<_, BomLineRow>(
            "SELECT bom, item_code, quantity FROM bom_lines WHERE bom = $1 ORDER BY idx",
        )
        .bind(&header.name)
        .fetch_all(&self.pool)
        .await?;

        Ok(Some(assemble(header, lines).1))
    }

    /// Every active default BOM keyed by the item it produces, in one pair of queries.
    ///
    /// Used by [`Self::snapshot_for_items`] and by callers that already know the full set
    /// of items they will price.
    pub async fn find_for_items_async(
        &self,
        items: &[ItemCode],
    ) -> StorageResult<HashMap<ItemCode, Bom>> {
        if items.is_empty() {
            return Ok(HashMap::new());
        }

        let codes: Vec<String> = items.iter().map(|i| i.as_str().to_owned()).collect();

        let sql = format!(
            "SELECT name, item, quantity FROM boms WHERE item = ANY($1) AND {ACTIVE_DEFAULT}"
        );
        let headers = sqlx::query_as::<_, BomRow>(&sql)
            .bind(&codes)
            .fetch_all(&self.pool)
            .await?;

        if headers.is_empty() {
            return Ok(HashMap::new());
        }

        let bom_names: Vec<String> = headers.iter().map(|h| h.name.clone()).collect();
        let lines = sqlx::query_as::<_, BomLineRow>(
            "SELECT bom, item_code, quantity FROM bom_lines
             WHERE bom = ANY($1) ORDER BY bom, idx",
        )
        .bind(&bom_names)
        .fetch_all(&self.pool)
        .await?;

        // Grouped rather than filtered per header: filtering would be quadratic in the
        // number of lines, and a large menu's closure is mostly lines.
        let mut by_bom: HashMap<String, Vec<BomLineRow>> = HashMap::new();
        for line in lines {
            by_bom.entry(line.bom.clone()).or_default().push(line);
        }

        Ok(headers
            .into_iter()
            .map(|header| {
                let lines = by_bom.remove(&header.name).unwrap_or_default();
                assemble(header, lines)
            })
            .collect())
    }

    /// Load everything `cogs` could ask for while pricing `items`, then serve it from
    /// memory.
    ///
    /// The explosion reaches at most `cogs::MAX_LEVEL` levels, so the closure is bounded:
    /// the roots' BOMs, then their lines' BOMs. That is two rounds of two queries
    /// regardless of how many items or how wide the BOMs are, versus one query per line
    /// through the trait.
    ///
    /// A third round is deliberately not fetched. `cogs` never asks for a level-3 BOM
    /// (`cogs.rs:186-190`), so fetching one would be dead weight that also invited the
    /// mistake of exploding it.
    pub async fn snapshot_for_items(&self, items: &[ItemCode]) -> StorageResult<BomSnapshot> {
        let mut boms: HashMap<ItemCode, Bom> = HashMap::new();
        let mut frontier: Vec<ItemCode> = dedup(items.to_vec());

        for _ in 0..peacock_core::cogs::MAX_LEVEL {
            if frontier.is_empty() {
                break;
            }
            let found = self.find_for_items_async(&frontier).await?;
            if found.is_empty() {
                break;
            }

            // Next frontier is every ingredient we have not already resolved a BOM for.
            let mut next: Vec<ItemCode> = Vec::new();
            for (item, bom) in found {
                for line in &bom.items {
                    if !boms.contains_key(&line.item_code) {
                        next.push(line.item_code.clone());
                    }
                }
                boms.insert(item, bom);
            }
            frontier = dedup(next)
                .into_iter()
                .filter(|i| !boms.contains_key(i))
                .collect();
        }

        Ok(BomSnapshot { boms })
    }
}

impl BomRepo for PgBomRepo {
    /// Blocking bridge over [`Self::find_for_item_async`]. See `super::blocking`.
    fn find_for_item(&self, item: &ItemCode) -> DomainResult<Option<Bom>> {
        block_on(self.find_for_item_async(item)).map_err(to_domain_error)
    }
}

/// An in-memory [`BomRepo`], prefetched by [`PgBomRepo::snapshot_for_items`].
///
/// Same answers as the live repository for every item within the snapshot's closure, and
/// `None` outside it — which is the correct answer there too, because `cogs` only reaches
/// outside the closure at a depth where it would not have asked for a BOM anyway.
#[derive(Clone, Debug, Default)]
pub struct BomSnapshot {
    boms: HashMap<ItemCode, Bom>,
}

impl BomSnapshot {
    /// Build a snapshot directly, for tests and for callers assembling BOMs elsewhere.
    pub fn from_map(boms: HashMap<ItemCode, Bom>) -> Self {
        BomSnapshot { boms }
    }

    pub fn len(&self) -> usize {
        self.boms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.boms.is_empty()
    }

    pub fn get(&self, item: &ItemCode) -> Option<&Bom> {
        self.boms.get(item)
    }
}

impl BomRepo for BomSnapshot {
    fn find_for_item(&self, item: &ItemCode) -> DomainResult<Option<Bom>> {
        Ok(self.boms.get(item).cloned())
    }
}

/// Attach lines to a header, returning the produced item alongside the BOM.
///
/// The only place a `Bom` is constructed in this module, so the batch quantity has
/// exactly one path into `Bom::quantity`. `ports::Bom` does not carry the item it
/// produces — nothing in the COGS arithmetic reads it — so it comes back as the tuple's
/// first element for keying a map.
fn assemble(header: BomRow, lines: Vec<BomLineRow>) -> (ItemCode, Bom) {
    let item = ItemCode::from(header.item.as_str());
    let bom = Bom {
        name: BomName::from(header.name.as_str()),
        // `header.quantity` — never a line's. This single assignment is the divisor
        // `cogs::bom_cost_per_unit` divides by.
        quantity: header.quantity,
        items: lines
            .into_iter()
            .map(|l| BomLine {
                item_code: ItemCode::from(l.item_code.as_str()),
                qty: l.quantity,
            })
            .collect(),
    };
    (item, bom)
}

fn dedup(items: Vec<ItemCode>) -> Vec<ItemCode> {
    let mut seen: HashSet<ItemCode> = HashSet::new();
    items.into_iter().filter(|i| seen.insert(i.clone())).collect()
}
