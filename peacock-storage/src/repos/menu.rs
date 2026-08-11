//! PostgreSQL implementation of [`MenuRepo`] and [`MenuResolutionRepo`].
//!
//! Two traits, because the domain has two distinct needs:
//!
//! * [`MenuRepo::courses_for_menu`] (`ports.rs:45`) — KOT routing. The room and the item
//!   codes are already known; it wants the course per item, batched
//!   (`ury_kot_generate.py:72`, which queried per item and is bug 6/7 in GROUND-TRUTH.md).
//! * [`MenuResolutionRepo`] (`menu.rs:121`) — the POS menu screen. Resolve *which* menu
//!   applies, then list its items.
//!
//! # Restaurant scoping
//!
//! Every resolution path is restaurant-scoped upstream: `getRestaurantMenu` derives the
//! restaurant from the branch (`restaurant = frappe.db.get_value("URY Restaurant",
//! {"branch": branch_name}, "name")`, api.py:33) and then reads the child tables with
//! `{"parent": restaurant, ...}`. The `MenuResolutionRepo` trait methods carry no
//! restaurant argument, so this repository is constructed *for* one restaurant
//! ([`PgMenuResolutionRepo::new`]) and that name is the `parent` in every child-table
//! query. A repository scoped to nothing would resolve a room's menu from another
//! branch's restaurant the moment a second branch existed.
//!
//! # The `room_wise_menu` / `order_type_wise_menu` flags
//!
//! Upstream checks the flag before reading the child table (api.py:36–46, :50–62): with
//! `room_wise_menu` off, the room mapping is not consulted at all and the default menu
//! wins even if a mapping row exists. That check lives here rather than in the domain,
//! because the flags are columns on `restaurants` and `resolve_menu` takes a strategy the
//! caller already chose. So [`PgMenuResolutionRepo::menu_for_room`] returns `None` when
//! the flag is off, which sends `resolve_menu` down its documented fallback to
//! `default_menu` (`menu.rs:205–211`) — the same outcome as upstream, reached through the
//! port's own vocabulary.
//!
//! # Course sequence
//!
//! `menu.rs` is explicit that upstream stores no sequence on `URY Menu Course` and that
//! the port must either derive one or add it to the schema. 002_menu_tables.sql adds a
//! nullable `menu_courses.idx`, and [`course_sequences`](PgMenuResolutionRepo::course_sequences)
//! returns only the rows that have one. A course with `idx IS NULL` is deliberately
//! absent from the map, which is the contract the trait documents ("If a course is not in
//! the map, its sequence is undefined and items in that course sort by name only") and
//! what `menu.rs::course_with_no_sequence_sorts_by_name_only` asserts.

use std::collections::HashMap;

use peacock_core::error::Result;
use peacock_core::ids::{ItemCode, MenuCourseName, MenuName, RestaurantName, RoomName};
use peacock_core::menu::{MenuResolutionRepo, ResolvedMenuItem};
use peacock_core::money::Money;
use peacock_core::ports::MenuRepo;
use rust_decimal::Decimal;
use sqlx::PgPool;

use crate::error::{StorageError, StorageResult};
use crate::repos::blocking::block_on;
use crate::repos::to_domain_error;

/// [`MenuRepo`] over a Postgres pool — the KOT-routing half.
#[derive(Clone, Debug)]
pub struct PgMenuRepo {
    pool: PgPool,
}

impl PgMenuRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Course per item for the room's menu, in one query.
    ///
    /// Resolves the room's menu the way `ury_kot_generate.py:64–69` does — the room's
    /// `Menu for Room` row, else the restaurant's `active_menu` — then reads the courses
    /// for all `codes` at once with `= ANY($2)`.
    ///
    /// Items absent from the menu, or present with no course, are simply not in the map.
    /// `kot.rs` treats a missing key as "no course", so there is nothing to signal.
    pub async fn courses_for_menu_async(
        &self,
        room: &RoomName,
        codes: &[ItemCode],
    ) -> StorageResult<HashMap<ItemCode, MenuCourseName>> {
        // Nothing to look up, and `= ANY('{}')` would still cost a round trip.
        if codes.is_empty() {
            return Ok(HashMap::new());
        }

        let wanted: Vec<String> = codes.iter().map(|c| c.as_str().to_owned()).collect();

        // One statement, not two. The CTE resolves the menu (room mapping first, then the
        // restaurant's active_menu) and the outer query reads that menu's courses, so a
        // 12-item order costs one query rather than 1 + 12.
        //
        // `room_wise_menu` is honoured here for the same reason as in the resolution
        // repo: with the flag off, upstream never reads the mapping.
        let rows: Vec<(String, String)> = sqlx::query_as(
            r#"
            WITH resolved AS (
                SELECT COALESCE(
                           (SELECT mfr.menu
                            FROM menu_for_room mfr
                            JOIN restaurants r ON r.name = mfr.restaurant
                            WHERE mfr.room = $1
                              AND r.room_wise_menu
                              AND r.deleted_at IS NULL
                            LIMIT 1),
                           (SELECT r.active_menu
                            FROM restaurants r
                            JOIN rooms rm ON rm.name = $1
                            WHERE r.branch = rm.branch
                              AND r.deleted_at IS NULL
                            LIMIT 1)
                       ) AS menu
            )
            SELECT mi.item, mi.course
            FROM menu_items mi
            JOIN resolved ON resolved.menu = mi.menu
            WHERE mi.item = ANY($2)
              AND mi.course IS NOT NULL
              AND NOT mi.disabled
            "#,
        )
        .bind(room.as_str())
        .bind(&wanted)
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(rows
            .into_iter()
            .map(|(item, course)| (ItemCode::new(item), MenuCourseName::new(course)))
            .collect())
    }
}

impl MenuRepo for PgMenuRepo {
    fn courses_for_menu(
        &self,
        room: &RoomName,
        codes: &[ItemCode],
    ) -> Result<HashMap<ItemCode, MenuCourseName>> {
        block_on(self.courses_for_menu_async(room, codes)).map_err(to_domain_error)
    }
}

/// [`MenuResolutionRepo`] over a Postgres pool, scoped to one restaurant.
///
/// The scope is a constructor argument because the trait's methods have no restaurant
/// parameter and every child-table lookup upstream is `{"parent": restaurant, ...}`.
#[derive(Clone, Debug)]
pub struct PgMenuResolutionRepo {
    pool: PgPool,
    restaurant: RestaurantName,
}

impl PgMenuResolutionRepo {
    pub fn new(pool: PgPool, restaurant: RestaurantName) -> Self {
        Self { pool, restaurant }
    }

    /// Scope to the restaurant that serves `branch`.
    ///
    /// Mirrors api.py:33. `None` when the branch has no restaurant, which the caller
    /// should surface rather than defaulting to some other branch's menu.
    pub async fn for_branch(pool: PgPool, branch: &str) -> StorageResult<Option<Self>> {
        let name: Option<String> = sqlx::query_scalar(
            "SELECT name FROM restaurants
             WHERE branch = $1 AND deleted_at IS NULL
             ORDER BY name
             LIMIT 1",
        )
        .bind(branch)
        .fetch_optional(&pool)
        .await
        .map_err(StorageError::from)?;

        Ok(name.map(|n| Self::new(pool, RestaurantName::new(n))))
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn restaurant(&self) -> &RestaurantName {
        &self.restaurant
    }

    /// Strategy 1: the room's menu (api.py:40–46).
    ///
    /// `None` when `room_wise_menu` is off or no mapping row exists; `resolve_menu` then
    /// falls back to [`default_menu`](Self::default_menu_async).
    pub async fn menu_for_room_async(&self, room: &RoomName) -> StorageResult<Option<MenuName>> {
        let menu: Option<String> = sqlx::query_scalar(
            r#"
            SELECT mfr.menu
            FROM menu_for_room mfr
            JOIN restaurants r ON r.name = mfr.restaurant
            WHERE mfr.restaurant = $1
              AND mfr.room = $2
              AND r.room_wise_menu
              AND r.deleted_at IS NULL
            LIMIT 1
            "#,
        )
        .bind(self.restaurant.as_str())
        .bind(room.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(menu.map(MenuName::new))
    }

    /// Strategy 2: the order type's menu (api.py:56–62).
    ///
    /// The cashier-role check from api.py:27–29 is an authorization concern and belongs
    /// to the API layer (Phase 5); this only answers whether a mapping exists.
    pub async fn menu_for_order_type_async(
        &self,
        order_type: &str,
    ) -> StorageResult<Option<MenuName>> {
        let menu: Option<String> = sqlx::query_scalar(
            r#"
            SELECT otm.menu
            FROM order_type_menu otm
            JOIN restaurants r ON r.name = otm.restaurant
            WHERE otm.restaurant = $1
              AND otm.order_type = $2
              AND r.order_type_wise_menu
              AND r.deleted_at IS NULL
            LIMIT 1
            "#,
        )
        .bind(self.restaurant.as_str())
        .bind(order_type)
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(menu.map(MenuName::new))
    }

    /// Strategy 3: `URY Restaurant.active_menu` (api.py:48, 65, 69).
    ///
    /// `None` when unset, which `resolve_menu` turns into `Error::NoActiveMenu`
    /// (api.py:71–72).
    pub async fn default_menu_async(&self) -> StorageResult<Option<MenuName>> {
        let menu: Option<String> = sqlx::query_scalar(
            "SELECT active_menu FROM restaurants
             WHERE name = $1 AND deleted_at IS NULL",
        )
        .bind(self.restaurant.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::from)?
        // Two different NULLs collapse here: no restaurant row, and a restaurant whose
        // active_menu is unset. Both mean "no default menu", and `resolve_menu` maps both
        // to NoActiveMenu, so flattening loses nothing the caller could act on.
        .flatten();

        Ok(menu.map(MenuName::new))
    }

    /// All enabled items on `menu`, with their course.
    ///
    /// Mirrors api.py:76–80: `filters={"parent": menu, "disabled": 0}`.
    ///
    /// `course_sequence` is left `None` here by design — `resolve_menu` materialises it
    /// from [`course_sequences`](Self::course_sequences_async) and sorts
    /// (`menu.rs:230–245`). Filling it in here would duplicate that logic in SQL and give
    /// two places to disagree.
    ///
    /// **rate is `menu_items.rate`** (api.py:79, 87) — the authoritative selling price.
    /// Not `Item Price`, not `Item.standard_rate`.
    pub async fn menu_items_async(&self, menu: &MenuName) -> StorageResult<Vec<ResolvedMenuItem>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(String, Option<String>, Decimal, bool, Option<String>)> = sqlx::query_as(
            r#"
            SELECT
                mi.item,
                -- item_name is `fetch_from: item.item_name` upstream, so the child row
                -- holds a copy that can be blank or stale. Fall back to the item master,
                -- then to the code itself: resolve_menu sorts on item_name, and a NULL
                -- would make that ordering arbitrary.
                COALESCE(NULLIF(btrim(mi.item_name), ''), NULLIF(btrim(i.name), ''), mi.item)
                    AS item_name,
                mi.rate,
                mi.special_dish,
                mi.course
            FROM menu_items mi
            JOIN items i ON i.code = mi.item
            WHERE mi.menu = $1
              AND NOT mi.disabled
            ORDER BY mi.idx
            "#,
        )
        .bind(menu.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(rows
            .into_iter()
            .map(|(item, item_name, rate, special_dish, course)| ResolvedMenuItem {
                item: ItemCode::new(item),
                // COALESCE guarantees a value; the Option is only sqlx's view of a
                // nullable expression.
                item_name: item_name.unwrap_or_default(),
                rate: Money::new(rate),
                special_dish,
                course: course.map(MenuCourseName::new),
                course_sequence: None,
            })
            .collect())
    }

    /// Course → sequence, for the courses that have one.
    ///
    /// Courses with `idx IS NULL` are omitted on purpose: the trait says an absent course
    /// has an undefined sequence and its items sort by name only.
    pub async fn course_sequences_async(&self) -> StorageResult<HashMap<MenuCourseName, i32>> {
        let rows: Vec<(String, i32)> =
            sqlx::query_as("SELECT name, idx FROM menu_courses WHERE idx IS NOT NULL")
                .fetch_all(&self.pool)
                .await
                .map_err(StorageError::from)?;

        Ok(rows
            .into_iter()
            .map(|(name, idx)| (MenuCourseName::new(name), idx))
            .collect())
    }

    /// Does `menu` belong to the branch this repository's restaurant serves?
    ///
    /// `Some(true)` yes; `Some(false)` the menu exists but under another branch; `None`
    /// no such menu (or it is soft-deleted).
    ///
    /// `GET /api/menu/:menu_id/items` needs this because `menu_items` has no restaurant
    /// column — it is a child of `menus`, which carries `branch` (002_menu_tables.sql) —
    /// so nothing in the item query itself stops a caller reading another branch's menu
    /// and its prices by naming it. Every other method here is scoped by
    /// `self.restaurant` through a child-table `parent` filter; this one closes the same
    /// hole for the one lookup that is keyed by menu alone.
    ///
    /// The three outcomes are distinguished here rather than collapsed to a bool so an
    /// operator-facing caller can tell "typo" from "wrong branch" without a second query
    /// shape. The public API deliberately collapses them: telling an unauthenticated
    /// caller which menus exist elsewhere is free enumeration.
    pub async fn menu_belongs_to_scope_async(
        &self,
        menu: &MenuName,
    ) -> StorageResult<Option<bool>> {
        let belongs: Option<bool> = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM restaurants r
                WHERE r.name = $2
                  AND r.branch = m.branch
                  AND r.deleted_at IS NULL
            )
            FROM menus m
            WHERE m.name = $1 AND m.deleted_at IS NULL
            "#,
        )
        .bind(menu.as_str())
        .bind(self.restaurant.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(belongs)
    }

    /// Every course, ordered by `idx` then name — `getMenuCourses` (api.py:106–108).
    ///
    /// Unsequenced courses come last, alphabetically, so the list is stable rather than
    /// left to the planner.
    pub async fn list_courses_async(&self) -> StorageResult<Vec<MenuCourseName>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM menu_courses ORDER BY idx ASC NULLS LAST, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(rows.into_iter().map(MenuCourseName::new).collect())
    }
}

impl MenuResolutionRepo for PgMenuResolutionRepo {
    fn menu_for_room(&self, room: &RoomName) -> Result<Option<MenuName>> {
        block_on(self.menu_for_room_async(room)).map_err(to_domain_error)
    }

    fn menu_for_order_type(&self, order_type: &str) -> Result<Option<MenuName>> {
        block_on(self.menu_for_order_type_async(order_type)).map_err(to_domain_error)
    }

    fn default_menu(&self) -> Result<Option<MenuName>> {
        block_on(self.default_menu_async()).map_err(to_domain_error)
    }

    fn menu_items(&self, menu: &MenuName) -> Result<Vec<ResolvedMenuItem>> {
        block_on(self.menu_items_async(menu)).map_err(to_domain_error)
    }

    fn course_sequences(&self) -> Result<HashMap<MenuCourseName, i32>> {
        block_on(self.course_sequences_async()).map_err(to_domain_error)
    }
}
