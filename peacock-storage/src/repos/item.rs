//! Item master reads — `GET /api/items/:item_code`.
//!
//! # Why this is not another `ItemRepo`
//!
//! `peacock_core::ports::ItemRepo` already exists (`ports.rs:29`) and has exactly one
//! method, `item_groups`, batched to kill the per-item `frappe.db.get_value("Item", …)`
//! N+1 at `ury_kot_generate.py:154`/`:214` (bugs 6 and 7 in GROUND-TRUTH.md).
//! [`super::routing::PgItemRepo`] implements it and [`super::routing::RoutingSnapshot`]
//! serves it from memory. Nothing here changes that.
//!
//! What the API needs is different in kind: one item, all of its display columns, for a
//! detail screen. That is not a port the domain has — no domain rule reads `stock_uom` or
//! `is_bom` for a single item — so inventing a trait for it would put an HTTP concern in
//! `peacock-core`. This is a plain repository returning a plain row.
//!
//! # Price is not here
//!
//! [`ItemDetails`] carries **no rate**, and that is the point. There are two prices in
//! this system and confusing them is a real bug that has already been caught once:
//!
//! * **Selling price** is `menu_items.rate` (`ury_menu_item.rate`, api.py:79, 87). It is
//!   per *menu*, so an item has as many selling prices as it has menus and a
//!   single-item lookup cannot name one. It is served by
//!   [`PgMenuResolutionRepo::menu_items_async`](super::menu::PgMenuResolutionRepo::menu_items_async).
//! * **Buying price** is `item_prices` on a *buying* price list, which is the COGS basis
//!   (`ury_daily_p_and_l.py:30`). It is served by [`super::price::PgPriceRepo`], and the
//!   caller has to name the list.
//!
//! ERPNext's `Item.standard_rate` — the obvious third candidate — is not in the schema
//! (001_core_tables.sql keeps only the columns Peacock reads) precisely so that no
//! handler can reach for it and quietly bill from the item master.
//!
//! # Soft deletes and disabled items
//!
//! `items` carries both `deleted_at` and `disabled`. They are not the same thing:
//!
//! * `deleted_at IS NOT NULL` is a retired row. It reads as absent, so the API answers
//!   404. Historical invoice lines still reference it by code; this is a read filter.
//! * `disabled` is a live row an operator has withdrawn from sale. It is **returned**,
//!   with the flag set, because a detail screen looking at an old order line has to be
//!   able to show what that line was. Routing already treats a disabled item as
//!   unroutable (`routing.rs`), which is where that decision belongs.

use peacock_core::ids::{ItemCode, ItemGroupName};
use sqlx::PgPool;

use crate::error::{StorageError, StorageResult};

/// One `items` row.
///
/// Mirrors the table exactly (001_core_tables.sql) rather than a wire shape: the API's
/// DTO does the renaming, so a column added to the schema does not silently change a
/// response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemDetails {
    pub code: ItemCode,
    /// `items.name` — the display name. Not the primary key; `code` is.
    pub name: String,
    /// Nullable upstream, and the absence is meaningful: an item with no group routes to
    /// no kitchen station (`routing.rs`).
    pub item_group: Option<ItemGroupName>,
    pub stock_uom: String,
    /// Cache of "a default active BOM exists". 001_core_tables.sql says not to treat it
    /// as authoritative on its own, so it is reported, never branched on here.
    pub is_bom: bool,
    pub disabled: bool,
}

/// Read-only access to the item master.
#[derive(Clone, Debug)]
pub struct PgItemDetailsRepo {
    pool: PgPool,
}

impl PgItemDetailsRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// One item by code. `None` when it does not exist or is soft-deleted.
    pub async fn find_async(&self, item: &ItemCode) -> StorageResult<Option<ItemDetails>> {
        #[allow(clippy::type_complexity)]
        let row: Option<(String, String, Option<String>, String, bool, bool)> = sqlx::query_as(
            r#"
            SELECT code, name, item_group, stock_uom, is_bom, disabled
            FROM items
            WHERE code = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(item.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(row.map(
            |(code, name, item_group, stock_uom, is_bom, disabled)| ItemDetails {
                code: ItemCode::new(code),
                name,
                item_group: item_group.map(ItemGroupName::new),
                stock_uom,
                is_bom,
                disabled,
            },
        ))
    }
}
