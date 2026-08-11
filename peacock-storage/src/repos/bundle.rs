//! PostgreSQL implementation of [`ProductBundleRepo`] — Lane 2D.
//!
//! Backs the bundle cost basis in `peacock_core::cogs`. Short module, because a bundle is
//! a flat list: `cogs_for_item_with_bundles` asks this repository once per sold item and
//! then prices each child line through `BomRepo` or `Item Price`.
//!
//! # Bundle wins, and it is a partition rather than a fallback
//!
//! Upstream splits invoice lines with three mutually exclusive queries. The bundle bucket
//! is `d.new_item_code IS NOT NULL` (ury_daily_p_and_l.py:170) and joins no BOM table at
//! all; the plain and BOM buckets both require `d.new_item_code IS NULL` (:102, :139). So
//! an item that is *both* a Product Bundle and has an active default BOM is priced as a
//! bundle, and its own BOM is never consulted.
//!
//! `cogs_for_item_with_bundles` implements that by asking here first and only falling
//! through to `cogs_for_item` on `None` (`cogs.rs:296-298`). What this module contributes
//! is that the answer cannot be ambiguous: `product_bundles.new_item_code` is uniquely
//! indexed among live rows, so the lookup is one bundle or none. Upstream took
//! `pb_items[0]` from an unordered result, so with duplicates its cost basis depended on
//! physical row order.
//!
//! Fixture `21_cogs_bundle_wins_over_bom.json` is the one that pins this.
//!
//! # No status filter, deliberately
//!
//! The BOM lookup filters `is_active=1, is_default=1, docstatus=1` (:227). The Product
//! Bundle lookup filters nothing (:222) — a draft bundle still captures the item, which
//! `ports.rs:104-106` calls out. So `product_bundles` has no status column at all, rather
//! than an unused one a later change might start filtering on. The only exclusion is
//! `deleted_at`, the soft delete every table in this schema carries.
//!
//! # A bundle adds no BOM depth
//!
//! For a bundle line that has a BOM, upstream calls `inner_bom_process` (:231) — the same
//! function, at the same entry point, as a top-level BOM item (:201). The walk therefore
//! still gets its full two levels. Nothing in this module participates in that: it
//! returns a flat list and the domain does the rest, which is precisely why the bundle
//! cannot accidentally consume a level. Fixture
//! `17_cogs_bundle_adds_no_bom_depth.json` is the check.
//!
//! # No recursion into nested bundles
//!
//! A bundle line is checked for a BOM and otherwise priced from `Item Price` (:241).
//! Upstream never re-queries Product Bundle for a child line, so a bundle whose child is
//! itself a bundle prices that child as a leaf. This repository offers no "expand
//! recursively" method, so there is no way to do it by accident
//! (`20_cogs_bundle_of_bundle_leaf.json`).

use std::collections::HashMap;

use rust_decimal::Decimal;
use sqlx::PgPool;

use peacock_core::error::Result as DomainResult;
use peacock_core::ids::ItemCode;
use peacock_core::ports::{ProductBundle, ProductBundleLine, ProductBundleRepo};

use super::blocking::block_on;
use super::to_domain_error;
use crate::error::StorageResult;

/// PostgreSQL-backed [`ProductBundleRepo`].
#[derive(Clone, Debug)]
pub struct PgProductBundleRepo {
    pool: PgPool,
}

/// One `product_bundles` row, before its lines are attached.
#[derive(Debug, sqlx::FromRow)]
struct BundleRow {
    name: String,
    new_item_code: String,
}

/// One `product_bundle_lines` row. `bundle` is carried so a multi-bundle fetch can group
/// by parent.
#[derive(Debug, sqlx::FromRow)]
struct BundleLineRow {
    bundle: String,
    item_code: String,
    quantity: Decimal,
}

impl PgProductBundleRepo {
    pub fn new(pool: PgPool) -> Self {
        PgProductBundleRepo { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The bundle sold under `item`, or `None`.
    ///
    /// `None` covers "not a bundle" and "no such item" alike, and both mean the same
    /// thing to `cogs`: fall through to the BOM and plain buckets.
    pub async fn find_by_new_item_code_async(
        &self,
        item: &ItemCode,
    ) -> StorageResult<Option<ProductBundle>> {
        let Some(header) = sqlx::query_as::<_, BundleRow>(
            "SELECT name, new_item_code FROM product_bundles
             WHERE new_item_code = $1 AND deleted_at IS NULL",
        )
        .bind(item.as_str())
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };

        let lines = sqlx::query_as::<_, BundleLineRow>(
            "SELECT bundle, item_code, quantity FROM product_bundle_lines
             WHERE bundle = $1 ORDER BY idx",
        )
        .bind(&header.name)
        .fetch_all(&self.pool)
        .await?;

        Ok(Some(assemble(header, lines).1))
    }

    /// Every live bundle among `items`, keyed by `new_item_code`, in two queries.
    pub async fn find_by_new_item_codes_async(
        &self,
        items: &[ItemCode],
    ) -> StorageResult<HashMap<ItemCode, ProductBundle>> {
        if items.is_empty() {
            return Ok(HashMap::new());
        }

        let codes: Vec<String> = items.iter().map(|i| i.as_str().to_owned()).collect();

        let headers = sqlx::query_as::<_, BundleRow>(
            "SELECT name, new_item_code FROM product_bundles
             WHERE new_item_code = ANY($1) AND deleted_at IS NULL",
        )
        .bind(&codes)
        .fetch_all(&self.pool)
        .await?;

        if headers.is_empty() {
            return Ok(HashMap::new());
        }

        let names: Vec<String> = headers.iter().map(|h| h.name.clone()).collect();
        let lines = sqlx::query_as::<_, BundleLineRow>(
            "SELECT bundle, item_code, quantity FROM product_bundle_lines
             WHERE bundle = ANY($1) ORDER BY bundle, idx",
        )
        .bind(&names)
        .fetch_all(&self.pool)
        .await?;

        // Grouped, not filtered per header: filtering would be quadratic in line count.
        let mut by_bundle: HashMap<String, Vec<BundleLineRow>> = HashMap::new();
        for line in lines {
            by_bundle.entry(line.bundle.clone()).or_default().push(line);
        }

        Ok(headers
            .into_iter()
            .map(|header| {
                let lines = by_bundle.remove(&header.name).unwrap_or_default();
                assemble(header, lines)
            })
            .collect())
    }

    /// Prefetch every bundle among `items` and serve the port from memory.
    ///
    /// Two queries total, versus one per sold item through the trait. Unlike the BOM
    /// snapshot there is no closure to walk: upstream never expands a bundle's children as
    /// bundles, so one round is the whole answer.
    pub async fn snapshot(&self, items: &[ItemCode]) -> StorageResult<BundleSnapshot> {
        Ok(BundleSnapshot {
            bundles: self.find_by_new_item_codes_async(items).await?,
        })
    }

    /// The item codes appearing as child lines of the given bundles.
    ///
    /// The set `BomRepo` must be primed with when prefetching: a bundle line's BOM is
    /// walked at level 1 (:231), so those children are BOM roots in their own right and
    /// would otherwise be missing from a snapshot built only from the sold items.
    pub fn child_items(bundles: &HashMap<ItemCode, ProductBundle>) -> Vec<ItemCode> {
        let mut out: Vec<ItemCode> = Vec::new();
        for bundle in bundles.values() {
            for line in &bundle.items {
                if !out.contains(&line.item_code) {
                    out.push(line.item_code.clone());
                }
            }
        }
        out
    }
}

impl ProductBundleRepo for PgProductBundleRepo {
    /// Blocking bridge over [`Self::find_by_new_item_code_async`]. See `super::blocking`.
    fn find_by_new_item_code(&self, item: &ItemCode) -> DomainResult<Option<ProductBundle>> {
        block_on(self.find_by_new_item_code_async(item)).map_err(to_domain_error)
    }
}

/// An in-memory [`ProductBundleRepo`], prefetched by [`PgProductBundleRepo::snapshot`].
#[derive(Clone, Debug, Default)]
pub struct BundleSnapshot {
    bundles: HashMap<ItemCode, ProductBundle>,
}

impl BundleSnapshot {
    pub fn from_map(bundles: HashMap<ItemCode, ProductBundle>) -> Self {
        BundleSnapshot { bundles }
    }

    pub fn len(&self) -> usize {
        self.bundles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bundles.is_empty()
    }

    pub fn get(&self, item: &ItemCode) -> Option<&ProductBundle> {
        self.bundles.get(item)
    }

    /// The child item codes across every bundle in the snapshot — the BOM prefetch seed.
    pub fn child_items(&self) -> Vec<ItemCode> {
        PgProductBundleRepo::child_items(&self.bundles)
    }
}

impl ProductBundleRepo for BundleSnapshot {
    fn find_by_new_item_code(&self, item: &ItemCode) -> DomainResult<Option<ProductBundle>> {
        Ok(self.bundles.get(item).cloned())
    }
}

/// Attach lines to a header, returning the sold item code alongside the bundle.
fn assemble(header: BundleRow, lines: Vec<BundleLineRow>) -> (ItemCode, ProductBundle) {
    let sold = ItemCode::from(header.new_item_code.as_str());
    let bundle = ProductBundle {
        new_item_code: sold.clone(),
        items: lines
            .into_iter()
            .map(|l| ProductBundleLine {
                item_code: ItemCode::from(l.item_code.as_str()),
                qty: l.quantity,
            })
            .collect(),
    };
    (sold, bundle)
}
