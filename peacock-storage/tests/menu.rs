//! Lane 2C acceptance tests: menu resolution, course ordering, price precedence.
//!
//! Each test gets its own freshly migrated database (`support::TestDb`), so a green run
//! also proves 002_menu_tables.sql applies from empty on top of 001.
//!
//! # Why these tests are `multi_thread`
//!
//! The port traits are synchronous and their impls block (see
//! `peacock_storage::repos::blocking`). Tokio refuses `block_on` from inside an async task
//! on a current-thread runtime — correctly, since blocking the only thread would deadlock
//! the query being awaited. So every test that drives a *sync* trait method uses
//! `flavor = "multi_thread"` and, where it calls domain code, `spawn_blocking`. That is
//! also the shape an Axum handler takes when it reaches domain logic, so the tests
//! exercise the real calling convention rather than a convenient one.
//!
//! Tests that only need the async methods use a plain `#[tokio::test]`.

mod support;

use std::collections::HashMap;

use peacock_core::error::Error as DomainError;
use peacock_core::ids::{
    ItemCode, MenuCourseName, MenuName, PriceListName, RestaurantName, RoomName,
};
use peacock_core::menu::{resolve_menu, MenuStrategy, ResolvedMenuItem};
use peacock_core::money::Money;
use peacock_core::ports::{MenuRepo, PriceRepo};
use peacock_storage::repos::{PgMenuRepo, PgMenuResolutionRepo, PgPriceRepo};
use rust_decimal_macros::dec;
use support::TestDb;

const BRANCH: &str = "Peacock HQ";
const RESTAURANT: &str = "Peacock Grand";
const ROOM: &str = "Main Hall";
const OTHER_ROOM: &str = "Terrace";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Seed the graph menu resolution needs: rooms, a restaurant, and the menu tables.
///
/// Mirrors the fixtures in `peacock_core::menu`'s own tests so the assertions can be
/// compared line by line against the domain expectations.
struct MenuFixture<'a> {
    db: &'a TestDb,
}

impl<'a> MenuFixture<'a> {
    async fn new(db: &'a TestDb) -> MenuFixture<'a> {
        db.seed_restaurant_and_room(RESTAURANT, ROOM, BRANCH).await;
        sqlx::query("INSERT INTO rooms (name, branch, room_type) VALUES ($1, $2, 'NON-AC')")
            .bind(OTHER_ROOM)
            .bind(BRANCH)
            .execute(db.pool())
            .await
            .expect("seed second room");
        MenuFixture { db }
    }

    fn pool(&self) -> &sqlx::PgPool {
        self.db.pool()
    }

    async fn menu(&self, name: &str) -> &Self {
        sqlx::query("INSERT INTO menus (name, branch) VALUES ($1, $2)")
            .bind(name)
            .bind(BRANCH)
            .execute(self.pool())
            .await
            .expect("insert menu");
        self
    }

    async fn set_active_menu(&self, menu: Option<&str>) -> &Self {
        sqlx::query("UPDATE restaurants SET active_menu = $2 WHERE name = $1")
            .bind(RESTAURANT)
            .bind(menu)
            .execute(self.pool())
            .await
            .expect("set active menu");
        self
    }

    async fn set_flags(&self, room_wise: bool, order_type_wise: bool) -> &Self {
        sqlx::query(
            "UPDATE restaurants SET room_wise_menu = $2, order_type_wise_menu = $3
             WHERE name = $1",
        )
        .bind(RESTAURANT)
        .bind(room_wise)
        .bind(order_type_wise)
        .execute(self.pool())
        .await
        .expect("set menu flags");
        self
    }

    async fn map_room(&self, idx: i32, room: &str, menu: &str) -> &Self {
        sqlx::query(
            "INSERT INTO menu_for_room (restaurant, idx, room, menu) VALUES ($1, $2, $3, $4)",
        )
        .bind(RESTAURANT)
        .bind(idx)
        .bind(room)
        .bind(menu)
        .execute(self.pool())
        .await
        .expect("map room to menu");
        self
    }

    async fn map_order_type(&self, idx: i32, order_type: &str, menu: &str) -> &Self {
        sqlx::query(
            "INSERT INTO order_type_menu (restaurant, idx, order_type, menu)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(RESTAURANT)
        .bind(idx)
        .bind(order_type)
        .bind(menu)
        .execute(self.pool())
        .await
        .expect("map order type to menu");
        self
    }

    async fn course(&self, name: &str, idx: Option<i32>) -> &Self {
        sqlx::query("INSERT INTO menu_courses (name, idx) VALUES ($1, $2)")
            .bind(name)
            .bind(idx)
            .execute(self.pool())
            .await
            .expect("insert course");
        self
    }

    async fn item(&self, code: &str, name: &str) -> &Self {
        sqlx::query("INSERT INTO items (code, name, item_group) VALUES ($1, $2, 'Food')")
            .bind(code)
            .bind(name)
            .execute(self.pool())
            .await
            .expect("insert item");
        self
    }

    /// One `menu_items` row. `item_name` is passed through as-is so the COALESCE
    /// fallback can be exercised with `None` and with a blank string.
    #[allow(clippy::too_many_arguments)]
    async fn menu_item(
        &self,
        menu: &str,
        idx: i32,
        item: &str,
        item_name: Option<&str>,
        rate: rust_decimal::Decimal,
        course: Option<&str>,
        disabled: bool,
    ) -> &Self {
        sqlx::query(
            "INSERT INTO menu_items
                 (menu, idx, item, item_name, rate, special_dish, disabled, course)
             VALUES ($1, $2, $3, $4, $5, false, $6, $7)",
        )
        .bind(menu)
        .bind(idx)
        .bind(item)
        .bind(item_name)
        .bind(rate)
        .bind(disabled)
        .bind(course)
        .execute(self.pool())
        .await
        .expect("insert menu item");
        self
    }

    fn resolution_repo(&self) -> PgMenuResolutionRepo {
        PgMenuResolutionRepo::new(self.pool().clone(), RestaurantName::new(RESTAURANT))
    }

    fn menu_repo(&self) -> PgMenuRepo {
        PgMenuRepo::new(self.pool().clone())
    }
}

/// Seed a price list and an item, for the price tests.
async fn seed_price_list(db: &TestDb, name: &str, buying: bool, selling: bool) {
    sqlx::query("INSERT INTO price_lists (name, buying, selling) VALUES ($1, $2, $3)")
        .bind(name)
        .bind(buying)
        .bind(selling)
        .execute(db.pool())
        .await
        .expect("insert price list");
}

async fn seed_item(db: &TestDb, code: &str) {
    sqlx::query("INSERT INTO items (code, name) VALUES ($1, $1)")
        .bind(code)
        .execute(db.pool())
        .await
        .expect("insert item");
}

async fn seed_price(
    db: &TestDb,
    item: &str,
    list: &str,
    rate: rust_decimal::Decimal,
    valid_from: Option<chrono::NaiveDate>,
    valid_upto: Option<chrono::NaiveDate>,
) {
    sqlx::query(
        "INSERT INTO item_prices (item_code, price_list, rate, valid_from, valid_upto)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(item)
    .bind(list)
    .bind(rate)
    .bind(valid_from)
    .bind(valid_upto)
    .execute(db.pool())
    .await
    .expect("insert item price");
}

/// Run `f` off the runtime's worker threads.
///
/// The sync trait impls block, so domain code must not be called from an async task —
/// this is the `spawn_blocking` hop an Axum handler would make.
async fn in_blocking<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .expect("blocking task panicked")
}

// ===========================================================================
// 1. Migration shape
// ===========================================================================

#[tokio::test]
async fn migration_creates_every_menu_table() {
    let db = TestDb::new().await;

    let found: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
    )
    .fetch_all(db.pool())
    .await
    .expect("list tables");

    for expected in [
        "menus",
        "menu_items",
        "menu_courses",
        "menu_for_room",
        "order_type_menu",
    ] {
        assert!(
            found.iter().any(|t| t == expected),
            "table {expected} missing; found {found:?}"
        );
    }
}

#[tokio::test]
async fn menu_item_rate_is_numeric_not_float() {
    // The selling price (api.py:79) goes through the same Decimal discipline as
    // item_prices.rate. A float column here would reintroduce paisa drift.
    let db = TestDb::new().await;

    let (data_type, precision, scale): (String, Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT data_type, numeric_precision, numeric_scale
         FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'menu_items' AND column_name = 'rate'",
    )
    .fetch_one(db.pool())
    .await
    .expect("describe menu_items.rate");

    assert_eq!(data_type, "numeric");
    assert_eq!(precision, Some(18));
    assert_eq!(scale, Some(6));
}

#[tokio::test]
async fn active_menu_is_a_real_foreign_key() {
    // 001_core_tables.sql left restaurants.active_menu without a FK and deferred it to
    // this lane. The default-menu strategy reads that column, so a dangling name would
    // resolve to a menu with no items rather than failing.
    let db = TestDb::new().await;
    db.seed_restaurant_and_room(RESTAURANT, ROOM, BRANCH).await;

    let err = sqlx::query("UPDATE restaurants SET active_menu = 'No-Such-Menu' WHERE name = $1")
        .bind(RESTAURANT)
        .execute(db.pool())
        .await
        .expect_err("dangling active_menu was accepted");

    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23503"),
        "expected a FK violation"
    );
}

#[tokio::test]
async fn menu_tables_have_the_indexes_resolution_depends_on() {
    let db = TestDb::new().await;

    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE schemaname = 'public'")
            .fetch_all(db.pool())
            .await
            .expect("list indexes");

    let has = |needle: &str| {
        indexes
            .iter()
            .any(|d| d.to_lowercase().contains(&needle.to_lowercase()))
    };

    // menu_items(menu) for the enabled-items read (api.py:76–80).
    assert!(
        has("on public.menu_items using btree (menu) where (not disabled)"),
        "missing menu_items enabled index: {indexes:?}"
    );
    // The course grouping used by courses_for_menu and the API's course view.
    assert!(
        has("on public.menu_items using btree (menu, course)"),
        "missing menu_items(menu, course) index"
    );
    // Child ordering must be unique per parent.
    assert!(
        has("menu_items_order_idx") && has("unique"),
        "missing unique menu_items(menu, idx) index"
    );
    // Room-wise strategy arriving from the room side (no restaurant in hand).
    assert!(
        has("on public.menu_for_room using btree (room)"),
        "missing menu_for_room(room) index"
    );
}

#[tokio::test]
async fn child_rows_die_with_their_parent_menu() {
    // menu_items is a child table (istable=1): embed-only, no life of its own.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-A").await;
    fx.item("ITEM-001", "Item 1").await;
    fx.menu_item("Menu-A", 1, "ITEM-001", Some("Item 1"), dec!(10), None, false)
        .await;

    sqlx::query("DELETE FROM menus WHERE name = 'Menu-A'")
        .execute(db.pool())
        .await
        .expect("delete menu");

    let orphans: i64 = sqlx::query_scalar("SELECT count(*) FROM menu_items WHERE menu = 'Menu-A'")
        .fetch_one(db.pool())
        .await
        .expect("count orphans");
    assert_eq!(orphans, 0, "menu_items survived the parent delete");
}

#[tokio::test]
async fn one_row_per_item_per_menu() {
    // Two rows for one item mean two rates, and the resolved menu would show whichever
    // the sort happened to surface.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-A").await;
    fx.item("ITEM-001", "Item 1").await;
    fx.menu_item("Menu-A", 1, "ITEM-001", Some("Item 1"), dec!(10), None, false)
        .await;

    let err = sqlx::query(
        "INSERT INTO menu_items (menu, idx, item, rate) VALUES ('Menu-A', 2, 'ITEM-001', 20)",
    )
    .execute(db.pool())
    .await
    .expect_err("duplicate item on one menu was accepted");

    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23505")
    );
}

#[tokio::test]
async fn two_courses_cannot_claim_one_sequence() {
    // A tie would make resolve_menu's output order depend on the sort's tie-break
    // rather than on configuration.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.course("Starters", Some(1)).await;

    let err = sqlx::query("INSERT INTO menu_courses (name, idx) VALUES ('Mains', 1)")
        .execute(db.pool())
        .await
        .expect_err("duplicate course sequence was accepted");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23505")
    );

    // Unsequenced courses coexist freely: the partial index only covers idx IS NOT NULL.
    fx.course("Unknown-A", None).await;
    fx.course("Unknown-B", None).await;
}

// ===========================================================================
// 2. Menu resolution — the three strategies, against real storage
//
// These mirror `peacock_core::menu`'s own tests one for one, driving `resolve_menu`
// with PgMenuResolutionRepo in place of the in-memory fake.
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_strategy_resolves_to_room_menu() {
    // api.py:40–46, and menu.rs::room_strategy_resolves_to_room_menu.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Room-A").await;
    fx.menu("Menu-Default").await;
    fx.set_active_menu(Some("Menu-Default")).await;
    fx.set_flags(true, false).await;
    fx.map_room(1, ROOM, "Menu-Room-A").await;
    fx.item("ITEM-001", "Item 1").await;
    fx.menu_item(
        "Menu-Room-A",
        1,
        "ITEM-001",
        Some("Item 1"),
        dec!(100.00),
        None,
        false,
    )
    .await;

    let repo = fx.resolution_repo();
    let resolved = in_blocking(move || {
        resolve_menu(
            MenuStrategy::Room(RoomName::new(ROOM)),
            chrono::Utc::now(),
            &repo,
        )
    })
    .await
    .expect("resolve room menu");

    assert_eq!(resolved.menu.as_str(), "Menu-Room-A");
    assert_eq!(resolved.items.len(), 1);
    assert_eq!(resolved.items[0].rate, Money::new(dec!(100.00)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_strategy_falls_back_to_default_when_no_mapping() {
    // api.py:46, and menu.rs::room_strategy_falls_back_to_default_when_no_mapping.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Default").await;
    fx.set_active_menu(Some("Menu-Default")).await;
    fx.set_flags(true, false).await;
    // No menu_for_room row for OTHER_ROOM.
    fx.item("ITEM-002", "Item 2").await;
    fx.menu_item(
        "Menu-Default",
        1,
        "ITEM-002",
        Some("Item 2"),
        dec!(200.00),
        None,
        false,
    )
    .await;

    let repo = fx.resolution_repo();
    let resolved = in_blocking(move || {
        resolve_menu(
            MenuStrategy::Room(RoomName::new(OTHER_ROOM)),
            chrono::Utc::now(),
            &repo,
        )
    })
    .await
    .expect("resolve fallback menu");

    assert_eq!(resolved.menu.as_str(), "Menu-Default");
    assert_eq!(resolved.items.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_wise_flag_off_ignores_an_existing_mapping() {
    // api.py:36–47: the flag is checked BEFORE the child table is read, so a mapping row
    // that exists while the flag is off must not win. This is the case the flag exists
    // for — an operator configuring rooms ahead of enabling the feature.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Room-A").await;
    fx.menu("Menu-Default").await;
    fx.set_active_menu(Some("Menu-Default")).await;
    fx.map_room(1, ROOM, "Menu-Room-A").await;
    fx.set_flags(false, false).await; // flag OFF, mapping present

    let repo = fx.resolution_repo();
    let resolved = in_blocking(move || {
        resolve_menu(
            MenuStrategy::Room(RoomName::new(ROOM)),
            chrono::Utc::now(),
            &repo,
        )
    })
    .await
    .expect("resolve with flag off");

    assert_eq!(
        resolved.menu.as_str(),
        "Menu-Default",
        "room mapping was consulted despite room_wise_menu = false"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn order_type_strategy_resolves_correctly() {
    // api.py:56–62, and menu.rs::order_type_strategy_resolves_correctly.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Delivery").await;
    fx.menu("Menu-Default").await;
    fx.set_active_menu(Some("Menu-Default")).await;
    fx.set_flags(false, true).await;
    fx.map_order_type(1, "Delivery", "Menu-Delivery").await;
    fx.item("ITEM-003", "Item 3").await;
    fx.menu_item(
        "Menu-Delivery",
        1,
        "ITEM-003",
        Some("Item 3"),
        dec!(150.00),
        None,
        false,
    )
    .await;

    let repo = fx.resolution_repo();
    let resolved = in_blocking(move || {
        resolve_menu(
            MenuStrategy::OrderType("Delivery".to_owned()),
            chrono::Utc::now(),
            &repo,
        )
    })
    .await
    .expect("resolve order type menu");

    assert_eq!(resolved.menu.as_str(), "Menu-Delivery");
    assert_eq!(resolved.items.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn order_type_strategy_falls_back_to_default() {
    // api.py:62, and menu.rs::order_type_strategy_falls_back_to_default.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Default").await;
    fx.set_active_menu(Some("Menu-Default")).await;
    fx.set_flags(false, true).await;
    // Mapped "Phone In", asked for "Delivery".
    fx.menu("Menu-Phone").await;
    fx.map_order_type(1, "Phone In", "Menu-Phone").await;

    let repo = fx.resolution_repo();
    let resolved = in_blocking(move || {
        resolve_menu(
            MenuStrategy::OrderType("Delivery".to_owned()),
            chrono::Utc::now(),
            &repo,
        )
    })
    .await
    .expect("resolve order type fallback");

    assert_eq!(resolved.menu.as_str(), "Menu-Default");
    assert!(
        resolved.items.is_empty(),
        "an empty menu is valid, not an error"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn order_type_is_not_pinned_to_the_upstream_select_options() {
    // The Select options ("\nPhone In\nTake Away\nDelivery") are a UI hint, not a
    // constraint: they are edited in the doctype and existing rows are never migrated.
    // A CHECK pinning them would reject a fourth order type the moment one was added.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Catering").await;
    fx.menu("Menu-Default").await;
    fx.set_active_menu(Some("Menu-Default")).await;
    fx.set_flags(false, true).await;
    fx.map_order_type(1, "Catering", "Menu-Catering").await;

    let repo = fx.resolution_repo();
    let resolved = in_blocking(move || {
        resolve_menu(
            MenuStrategy::OrderType("Catering".to_owned()),
            chrono::Utc::now(),
            &repo,
        )
    })
    .await
    .expect("resolve a novel order type");

    assert_eq!(resolved.menu.as_str(), "Menu-Catering");

    // Blank is still refused: it cannot be what anyone configured.
    let err = sqlx::query(
        "INSERT INTO order_type_menu (restaurant, idx, order_type, menu)
         VALUES ($1, 2, '   ', 'Menu-Default')",
    )
    .bind(RESTAURANT)
    .execute(db.pool())
    .await
    .expect_err("blank order type was accepted");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23514")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_strategy_uses_active_menu() {
    // api.py:69, and menu.rs::default_strategy_uses_active_menu.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Default").await;
    fx.set_active_menu(Some("Menu-Default")).await;
    fx.item("ITEM-004", "Item 4").await;
    fx.menu_item(
        "Menu-Default",
        1,
        "ITEM-004",
        Some("Item 4"),
        dec!(250.00),
        None,
        false,
    )
    .await;

    let repo = fx.resolution_repo();
    let resolved = in_blocking(move || {
        resolve_menu(MenuStrategy::Default, chrono::Utc::now(), &repo)
    })
    .await
    .expect("resolve default menu");

    assert_eq!(resolved.menu.as_str(), "Menu-Default");
    assert_eq!(resolved.items[0].rate, Money::new(dec!(250.00)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_active_menu_returns_the_domain_error() {
    // api.py:71–72, and menu.rs::no_active_menu_returns_error.
    //
    // This also pins the error mapping end to end: NoActiveMenu has to survive the trip
    // through `to_domain_error` intact, because peacock-api maps it to 404 while a
    // generic storage failure becomes 500.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.set_active_menu(None).await;

    let repo = fx.resolution_repo();
    let err = in_blocking(move || resolve_menu(MenuStrategy::Default, chrono::Utc::now(), &repo))
        .await
        .expect_err("resolution without an active menu should fail");

    assert!(
        matches!(err, DomainError::NoActiveMenu),
        "expected NoActiveMenu, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_with_no_mapping_and_no_default_returns_error() {
    // menu.rs::room_with_no_mapping_and_no_default_returns_error.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.set_flags(true, false).await;
    fx.set_active_menu(None).await;

    let repo = fx.resolution_repo();
    let err = in_blocking(move || {
        resolve_menu(
            MenuStrategy::Room(RoomName::new(OTHER_ROOM)),
            chrono::Utc::now(),
            &repo,
        )
    })
    .await
    .expect_err("no mapping and no default should fail");

    assert!(matches!(err, DomainError::NoActiveMenu));
}

#[tokio::test]
async fn resolution_is_scoped_to_its_restaurant() {
    // The trait methods carry no restaurant argument, so the repo is constructed for one.
    // Without that scoping, a second branch's mapping would answer for the first.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Ours").await;
    fx.menu("Menu-Theirs").await;
    fx.set_active_menu(Some("Menu-Ours")).await;
    fx.set_flags(true, false).await;
    fx.map_room(1, ROOM, "Menu-Ours").await;

    // A second restaurant on another branch, mapping the same room to its own menu.
    sqlx::query(
        "INSERT INTO restaurants (name, company, branch, invoice_series_prefix,
                                  active_menu, room_wise_menu)
         VALUES ('Other Grand', 'Peacock Foods', 'Other Branch', 'OTH-', 'Menu-Theirs', true)",
    )
    .execute(db.pool())
    .await
    .expect("seed second restaurant");
    sqlx::query(
        "INSERT INTO menu_for_room (restaurant, idx, room, menu)
         VALUES ('Other Grand', 1, $1, 'Menu-Theirs')",
    )
    .bind(ROOM)
    .execute(db.pool())
    .await
    .expect("map room for the other restaurant");

    let ours = fx.resolution_repo();
    assert_eq!(
        ours.menu_for_room_async(&RoomName::new(ROOM))
            .await
            .expect("room lookup")
            .map(|m| m.as_str().to_owned()),
        Some("Menu-Ours".to_owned())
    );

    let theirs = PgMenuResolutionRepo::new(db.pool().clone(), RestaurantName::new("Other Grand"));
    assert_eq!(
        theirs
            .menu_for_room_async(&RoomName::new(ROOM))
            .await
            .expect("room lookup")
            .map(|m| m.as_str().to_owned()),
        Some("Menu-Theirs".to_owned())
    );
}

#[tokio::test]
async fn for_branch_finds_the_restaurant_or_says_it_cannot() {
    // api.py:33. A branch with no restaurant must be visible to the caller rather than
    // silently resolving against some other branch.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Default").await;
    fx.set_active_menu(Some("Menu-Default")).await;

    let found = PgMenuResolutionRepo::for_branch(db.pool().clone(), BRANCH)
        .await
        .expect("branch lookup")
        .expect("restaurant for the seeded branch");
    assert_eq!(found.restaurant().as_str(), RESTAURANT);
    assert_eq!(
        found
            .default_menu_async()
            .await
            .expect("default menu")
            .map(|m| m.as_str().to_owned()),
        Some("Menu-Default".to_owned())
    );

    assert!(
        PgMenuResolutionRepo::for_branch(db.pool().clone(), "Nowhere")
            .await
            .expect("branch lookup")
            .is_none(),
        "a branch with no restaurant should return None"
    );
}

#[tokio::test]
async fn a_missing_restaurant_has_no_default_menu_rather_than_failing() {
    // Both "no restaurant row" and "restaurant with active_menu unset" mean the same
    // thing to resolve_menu, and both must arrive as Ok(None) so it can raise
    // NoActiveMenu rather than a storage error.
    let db = TestDb::new().await;
    let repo = PgMenuResolutionRepo::new(db.pool().clone(), RestaurantName::new("Ghost"));

    assert_eq!(repo.default_menu_async().await.expect("default menu"), None);
}

// ===========================================================================
// 3. Course ordering
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn items_ordered_by_course_sequence_then_name() {
    // menu.rs::items_ordered_by_course_sequence_then_name, same fixture and expectations.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Main").await;
    fx.set_active_menu(Some("Menu-Main")).await;
    fx.course("Starters", Some(1)).await;
    fx.course("Mains", Some(2)).await;
    fx.course("Desserts", Some(3)).await;

    for (code, name) in [
        ("ITEM-001", "Soup A"),
        ("ITEM-002", "Soup B"),
        ("ITEM-003", "Dessert Item"),
        ("ITEM-004", "Main Dish Z"),
    ] {
        fx.item(code, name).await;
    }

    // Inserted in an order that does NOT match the expected output, so a pass cannot be
    // an accident of insertion order.
    fx.menu_item(
        "Menu-Main",
        1,
        "ITEM-003",
        Some("Dessert Item"),
        dec!(300.00),
        Some("Desserts"),
        false,
    )
    .await;
    fx.menu_item(
        "Menu-Main",
        2,
        "ITEM-001",
        Some("Soup A"),
        dec!(100.00),
        Some("Starters"),
        false,
    )
    .await;
    fx.menu_item(
        "Menu-Main",
        3,
        "ITEM-004",
        Some("Main Dish Z"),
        dec!(400.00),
        Some("Mains"),
        false,
    )
    .await;
    fx.menu_item(
        "Menu-Main",
        4,
        "ITEM-002",
        Some("Soup B"),
        dec!(150.00),
        Some("Starters"),
        false,
    )
    .await;

    let repo = fx.resolution_repo();
    let resolved = in_blocking(move || {
        resolve_menu(MenuStrategy::Default, chrono::Utc::now(), &repo)
    })
    .await
    .expect("resolve menu");

    // Starters (Soup A, Soup B), Mains (Main Dish Z), Desserts (Dessert Item).
    let order: Vec<&str> = resolved.items.iter().map(|i| i.item.as_str()).collect();
    assert_eq!(order, vec!["ITEM-001", "ITEM-002", "ITEM-004", "ITEM-003"]);

    // The sequences came from menu_courses.idx, not from insertion order.
    assert_eq!(resolved.items[0].course_sequence, Some(1));
    assert_eq!(resolved.items[1].course_sequence, Some(1));
    assert_eq!(resolved.items[2].course_sequence, Some(2));
    assert_eq!(resolved.items[3].course_sequence, Some(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn items_with_no_course_appear_last() {
    // menu.rs::items_with_no_course_appear_last.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Mixed").await;
    fx.set_active_menu(Some("Menu-Mixed")).await;
    fx.course("Starters", Some(1)).await;

    for (code, name) in [
        ("ITEM-001", "Starter A"),
        ("ITEM-002", "No Course B"),
        ("ITEM-003", "No Course A"),
    ] {
        fx.item(code, name).await;
    }

    fx.menu_item(
        "Menu-Mixed",
        1,
        "ITEM-002",
        Some("No Course B"),
        dec!(200.00),
        None,
        false,
    )
    .await;
    fx.menu_item(
        "Menu-Mixed",
        2,
        "ITEM-001",
        Some("Starter A"),
        dec!(100.00),
        Some("Starters"),
        false,
    )
    .await;
    fx.menu_item(
        "Menu-Mixed",
        3,
        "ITEM-003",
        Some("No Course A"),
        dec!(300.00),
        None,
        false,
    )
    .await;

    let repo = fx.resolution_repo();
    let resolved = in_blocking(move || {
        resolve_menu(MenuStrategy::Default, chrono::Utc::now(), &repo)
    })
    .await
    .expect("resolve menu");

    let order: Vec<&str> = resolved.items.iter().map(|i| i.item.as_str()).collect();
    // Starter A first, then the no-course items by name: A before B.
    assert_eq!(order, vec!["ITEM-001", "ITEM-003", "ITEM-002"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn course_with_no_sequence_sorts_by_name_only() {
    // menu.rs::course_with_no_sequence_sorts_by_name_only.
    //
    // This is why menu_courses.idx is nullable: a NOT NULL column would make the domain's
    // "course not in the map" branch unreachable from real storage.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Partial").await;
    fx.set_active_menu(Some("Menu-Partial")).await;
    fx.course("Unknown", None).await; // present, but unsequenced

    fx.item("ITEM-001", "Unknown A").await;
    fx.item("ITEM-002", "Unknown B").await;
    fx.menu_item(
        "Menu-Partial",
        1,
        "ITEM-002",
        Some("Unknown B"),
        dec!(200.00),
        Some("Unknown"),
        false,
    )
    .await;
    fx.menu_item(
        "Menu-Partial",
        2,
        "ITEM-001",
        Some("Unknown A"),
        dec!(100.00),
        Some("Unknown"),
        false,
    )
    .await;

    let repo = fx.resolution_repo();
    let resolved = in_blocking(move || {
        resolve_menu(MenuStrategy::Default, chrono::Utc::now(), &repo)
    })
    .await
    .expect("resolve menu");

    let order: Vec<&str> = resolved.items.iter().map(|i| i.item.as_str()).collect();
    assert_eq!(order, vec!["ITEM-001", "ITEM-002"]);
    assert!(
        resolved.items.iter().all(|i| i.course_sequence.is_none()),
        "an unsequenced course must not acquire a sequence"
    );
}

#[tokio::test]
async fn course_sequences_omits_unsequenced_courses() {
    // The trait contract: a course absent from the map has an undefined sequence.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.course("Starters", Some(1)).await;
    fx.course("Mains", Some(2)).await;
    fx.course("Unsequenced", None).await;

    let sequences: HashMap<MenuCourseName, i32> = fx
        .resolution_repo()
        .course_sequences_async()
        .await
        .expect("course sequences");

    assert_eq!(sequences.get(&MenuCourseName::new("Starters")), Some(&1));
    assert_eq!(sequences.get(&MenuCourseName::new("Mains")), Some(&2));
    assert!(
        !sequences.contains_key(&MenuCourseName::new("Unsequenced")),
        "an unsequenced course must not appear in the map"
    );
}

#[tokio::test]
async fn list_courses_orders_by_idx_with_unsequenced_last() {
    // getMenuCourses (api.py:106–108), made deterministic.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.course("Desserts", Some(3)).await;
    fx.course("Starters", Some(1)).await;
    fx.course("Zed", None).await;
    fx.course("Mains", Some(2)).await;
    fx.course("Alpha", None).await;

    let courses = fx
        .resolution_repo()
        .list_courses_async()
        .await
        .expect("list courses");
    let names: Vec<&str> = courses.iter().map(|c| c.as_str()).collect();

    assert_eq!(
        names,
        vec!["Starters", "Mains", "Desserts", "Alpha", "Zed"],
        "sequenced courses in idx order, then unsequenced alphabetically"
    );
}

#[tokio::test]
async fn empty_menu_resolves_without_error() {
    // menu.rs::empty_menu_resolves_without_error — an empty menu is valid.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Empty").await;
    fx.set_active_menu(Some("Menu-Empty")).await;

    let items = fx
        .resolution_repo()
        .menu_items_async(&MenuName::new("Menu-Empty"))
        .await
        .expect("menu items");
    assert!(items.is_empty());
}

#[tokio::test]
async fn disabled_items_are_excluded() {
    // api.py:76–80 filters `disabled: 0`. A disabled item still has a row, because the
    // operator's rate survives a temporary removal from the menu.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-A").await;
    fx.item("ITEM-ON", "Available").await;
    fx.item("ITEM-OFF", "Withdrawn").await;
    fx.menu_item(
        "Menu-A",
        1,
        "ITEM-ON",
        Some("Available"),
        dec!(10),
        None,
        false,
    )
    .await;
    fx.menu_item(
        "Menu-A",
        2,
        "ITEM-OFF",
        Some("Withdrawn"),
        dec!(20),
        None,
        true,
    )
    .await;

    let items = fx
        .resolution_repo()
        .menu_items_async(&MenuName::new("Menu-A"))
        .await
        .expect("menu items");

    let codes: Vec<&str> = items.iter().map(|i| i.item.as_str()).collect();
    assert_eq!(codes, vec!["ITEM-ON"]);
}

#[tokio::test]
async fn item_name_falls_back_to_the_item_master_then_the_code() {
    // `item_name` is `fetch_from: item.item_name` upstream, i.e. a denormalised copy that
    // can be blank. resolve_menu sorts on it, so a NULL would make the order arbitrary.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-A").await;
    fx.item("ITEM-COPY", "Master Name").await;
    fx.item("ITEM-BLANK", "Blank Copy Master").await;

    fx.menu_item("Menu-A", 1, "ITEM-COPY", None, dec!(10), None, false)
        .await; // NULL copy
    fx.menu_item("Menu-A", 2, "ITEM-BLANK", Some("   "), dec!(20), None, false)
        .await; // blank copy

    let items = fx
        .resolution_repo()
        .menu_items_async(&MenuName::new("Menu-A"))
        .await
        .expect("menu items");

    let by_code: HashMap<&str, &ResolvedMenuItem> =
        items.iter().map(|i| (i.item.as_str(), i)).collect();
    assert_eq!(by_code["ITEM-COPY"].item_name, "Master Name");
    assert_eq!(by_code["ITEM-BLANK"].item_name, "Blank Copy Master");
    assert!(
        items.iter().all(|i| !i.item_name.is_empty()),
        "item_name must never be empty; resolve_menu sorts on it"
    );
}

#[tokio::test]
async fn rate_comes_from_menu_items_not_item_price() {
    // menu.rs::rate_comes_from_ury_menu_item_not_item_price, api.py:79 and :87.
    //
    // The item is deliberately given a *different* Item Price so a repository that read
    // the wrong source would fail rather than coincidentally agree.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Price").await;
    fx.item("ITEM-RATE", "Rate Test Item").await;
    fx.menu_item(
        "Menu-Price",
        1,
        "ITEM-RATE",
        Some("Rate Test Item"),
        dec!(123.45),
        None,
        false,
    )
    .await;

    seed_price_list(&db, "Standard Selling", false, true).await;
    seed_price(&db, "ITEM-RATE", "Standard Selling", dec!(999.99), None, None).await;

    let items = fx
        .resolution_repo()
        .menu_items_async(&MenuName::new("Menu-Price"))
        .await
        .expect("menu items");

    assert_eq!(items[0].rate, Money::new(dec!(123.45)));
    assert_ne!(
        items[0].rate,
        Money::new(dec!(999.99)),
        "rate must come from menu_items.rate, not Item Price"
    );
}

#[tokio::test]
async fn menu_item_rate_keeps_full_decimal_precision() {
    // Money is Decimal end to end (money.rs). A float column would round 6 dp to garbage.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-A").await;
    fx.item("ITEM-P", "Precise").await;
    fx.menu_item(
        "Menu-A",
        1,
        "ITEM-P",
        Some("Precise"),
        dec!(41.666667),
        None,
        false,
    )
    .await;

    let items = fx
        .resolution_repo()
        .menu_items_async(&MenuName::new("Menu-A"))
        .await
        .expect("menu items");
    assert_eq!(items[0].rate, Money::new(dec!(41.666667)));
}

#[tokio::test]
async fn a_negative_rate_is_refused() {
    // A negative selling price is not a discount, it is a data-entry accident that would
    // flow into an invoice line.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-A").await;
    fx.item("ITEM-N", "Negative").await;

    let err = sqlx::query(
        "INSERT INTO menu_items (menu, idx, item, rate) VALUES ('Menu-A', 1, 'ITEM-N', -1)",
    )
    .execute(db.pool())
    .await
    .expect_err("negative rate was accepted");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23514")
    );
}

// ===========================================================================
// 4. MenuRepo::courses_for_menu — the KOT-routing path
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn courses_for_menu_returns_the_course_per_item() {
    // ury_kot_generate.py:72, but batched: one query for all codes instead of one per
    // item (bugs 6 and 7, GROUND-TRUTH.md).
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Hall").await;
    fx.set_flags(true, false).await;
    fx.map_room(1, ROOM, "Menu-Hall").await;
    fx.course("Starters", Some(1)).await;
    fx.course("Mains", Some(2)).await;

    fx.item("ITEM-S", "Soup").await;
    fx.item("ITEM-M", "Curry").await;
    fx.item("ITEM-X", "Water").await; // on the menu, no course
    fx.item("ITEM-Z", "Off Menu").await; // not on the menu at all

    fx.menu_item(
        "Menu-Hall",
        1,
        "ITEM-S",
        Some("Soup"),
        dec!(80),
        Some("Starters"),
        false,
    )
    .await;
    fx.menu_item(
        "Menu-Hall",
        2,
        "ITEM-M",
        Some("Curry"),
        dec!(220),
        Some("Mains"),
        false,
    )
    .await;
    fx.menu_item("Menu-Hall", 3, "ITEM-X", Some("Water"), dec!(20), None, false)
        .await;

    let repo = fx.menu_repo();
    let codes = vec![
        ItemCode::new("ITEM-S"),
        ItemCode::new("ITEM-M"),
        ItemCode::new("ITEM-X"),
        ItemCode::new("ITEM-Z"),
    ];
    let courses =
        in_blocking(move || repo.courses_for_menu(&RoomName::new(ROOM), &codes))
            .await
            .expect("courses for menu");

    assert_eq!(
        courses.get(&ItemCode::new("ITEM-S")),
        Some(&MenuCourseName::new("Starters"))
    );
    assert_eq!(
        courses.get(&ItemCode::new("ITEM-M")),
        Some(&MenuCourseName::new("Mains"))
    );
    // No course, and not on the menu, are both simply absent. kot.rs reads a missing key
    // as "no course", so there is nothing to distinguish.
    assert!(!courses.contains_key(&ItemCode::new("ITEM-X")));
    assert!(!courses.contains_key(&ItemCode::new("ITEM-Z")));
    assert_eq!(courses.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn courses_for_menu_falls_back_to_the_active_menu() {
    // ury_kot_generate.py:64–69: no room mapping, so the restaurant's active_menu
    // supplies the courses.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Default").await;
    fx.set_active_menu(Some("Menu-Default")).await;
    fx.set_flags(true, false).await; // flag on, but no mapping for OTHER_ROOM
    fx.course("Mains", Some(1)).await;
    fx.item("ITEM-M", "Curry").await;
    fx.menu_item(
        "Menu-Default",
        1,
        "ITEM-M",
        Some("Curry"),
        dec!(220),
        Some("Mains"),
        false,
    )
    .await;

    let repo = fx.menu_repo();
    let codes = vec![ItemCode::new("ITEM-M")];
    let courses = in_blocking(move || repo.courses_for_menu(&RoomName::new(OTHER_ROOM), &codes))
        .await
        .expect("courses via fallback");

    assert_eq!(
        courses.get(&ItemCode::new("ITEM-M")),
        Some(&MenuCourseName::new("Mains"))
    );
}

#[tokio::test]
async fn courses_for_menu_skips_the_query_for_an_empty_code_list() {
    // A KOT with no lines must not cost a round trip.
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;

    let courses = fx
        .menu_repo()
        .courses_for_menu_async(&RoomName::new(ROOM), &[])
        .await
        .expect("empty code list");
    assert!(courses.is_empty());
}

#[tokio::test]
async fn courses_for_menu_excludes_disabled_items() {
    let db = TestDb::new().await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Hall").await;
    fx.set_flags(true, false).await;
    fx.map_room(1, ROOM, "Menu-Hall").await;
    fx.course("Starters", Some(1)).await;
    fx.item("ITEM-OFF", "Withdrawn").await;
    fx.menu_item(
        "Menu-Hall",
        1,
        "ITEM-OFF",
        Some("Withdrawn"),
        dec!(80),
        Some("Starters"),
        true,
    )
    .await;

    let courses = fx
        .menu_repo()
        .courses_for_menu_async(&RoomName::new(ROOM), &[ItemCode::new("ITEM-OFF")])
        .await
        .expect("courses for menu");
    assert!(courses.is_empty());
}

#[tokio::test]
async fn courses_for_menu_is_one_query_regardless_of_item_count() {
    // The query budget kot.rs depends on: one batched lookup, not one per item. Upstream
    // issued one course lookup per item (ury_kot_generate.py:72) — bugs 6 and 7.
    //
    // `max_connections(1)` matters: `pg_stat_force_next_flush()` flushes only the calling
    // backend's pending statistics, so the query and the flush have to share a connection
    // or the count reads as zero.
    let db = TestDb::with_config(|c| c.with_max_connections(1).with_min_connections(1)).await;
    let fx = MenuFixture::new(&db).await;
    fx.menu("Menu-Big").await;
    fx.set_flags(true, false).await;
    fx.map_room(1, ROOM, "Menu-Big").await;
    fx.course("Starters", Some(1)).await;

    let mut codes = Vec::new();
    for i in 0..60 {
        let code = format!("ITEM-{i:03}");
        fx.item(&code, &format!("Item {i}")).await;
        fx.menu_item(
            "Menu-Big",
            i + 1,
            &code,
            Some(&format!("Item {i}")),
            dec!(100),
            Some("Starters"),
            false,
        )
        .await;
        codes.push(ItemCode::new(code));
    }

    let before = menu_items_scans(db.pool()).await;
    let courses = fx
        .menu_repo()
        .courses_for_menu_async(&RoomName::new(ROOM), &codes)
        .await
        .expect("batched courses");
    let after = menu_items_scans(db.pool()).await;

    assert_eq!(courses.len(), 60, "every item should have a course");
    assert_eq!(
        after - before,
        1,
        "60 items should cost one scan of menu_items, not one per item"
    );
}

/// Scans of `menu_items` recorded by the statistics collector.
///
/// One statement that reads the table increments this by one, so the delta across a call
/// is that call's query count for this table — which is the N+1 regression the KOT path
/// cares about (`kot.rs` §"Query budget", bugs 6 and 7 in GROUND-TRUTH.md).
///
/// `pg_stat_statements` would be more direct but it is an extension and may not be
/// installed; `pg_stat_all_tables` is always present. Stats are flushed asynchronously,
/// hence the explicit flush before reading.
async fn menu_items_scans(pool: &sqlx::PgPool) -> i64 {
    sqlx::query("SELECT pg_stat_force_next_flush()")
        .execute(pool)
        .await
        .expect("flush statistics");

    sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COALESCE(seq_scan, 0) + COALESCE(idx_scan, 0)
         FROM pg_stat_all_tables
         WHERE schemaname = 'public' AND relname = 'menu_items'",
    )
    .fetch_one(pool)
    .await
    .expect("read menu_items scan count")
    .unwrap_or(0)
}

// ===========================================================================
// 5. PriceRepo — precedence, missing prices, multi-pricelist
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_price_reads_the_base_rate() {
    let db = TestDb::new().await;
    seed_item(&db, "MILK").await;
    seed_price_list(&db, "Standard Buying", true, false).await;
    seed_price(&db, "MILK", "Standard Buying", dec!(41.666667), None, None).await;

    let repo = PgPriceRepo::new(db.pool().clone());
    let price = in_blocking(move || {
        repo.item_price(
            &ItemCode::new("MILK"),
            &PriceListName::new("Standard Buying"),
        )
    })
    .await
    .expect("item price");

    // Full 6 dp survives: COGS divides this by BOM batch size before rounding to paisa.
    assert_eq!(price, Some(Money::new(dec!(41.666667))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_price_returns_none_and_does_not_fail() {
    // The contract that matters most: upstream accumulates unset prices for the operator
    // rather than aborting the P&L (ury_daily_p_and_l.py).
    let db = TestDb::new().await;
    seed_item(&db, "TEA").await;
    seed_price_list(&db, "Standard Buying", true, false).await;
    // No item_prices row at all.

    let repo = PgPriceRepo::new(db.pool().clone());
    let price = in_blocking(move || {
        repo.item_price(&ItemCode::new("TEA"), &PriceListName::new("Standard Buying"))
    })
    .await
    .expect("a missing price must not be an error");

    assert_eq!(price, None);
}

#[tokio::test]
async fn a_missing_price_is_distinct_from_a_zero_price() {
    // Collapsing the two would value an unpriced ingredient at nothing and overstate
    // margin. Zero is a real price meaning "free".
    let db = TestDb::new().await;
    seed_item(&db, "FREEBIE").await;
    seed_item(&db, "UNPRICED").await;
    seed_price_list(&db, "Standard Buying", true, false).await;
    seed_price(&db, "FREEBIE", "Standard Buying", dec!(0), None, None).await;

    let repo = PgPriceRepo::new(db.pool().clone());

    assert_eq!(
        repo.item_price_async(
            &ItemCode::new("FREEBIE"),
            &PriceListName::new("Standard Buying")
        )
        .await
        .expect("zero price"),
        Some(Money::ZERO)
    );
    assert_eq!(
        repo.item_price_async(
            &ItemCode::new("UNPRICED"),
            &PriceListName::new("Standard Buying")
        )
        .await
        .expect("missing price"),
        None
    );
}

#[tokio::test]
async fn an_unknown_price_list_returns_none_rather_than_failing() {
    let db = TestDb::new().await;
    seed_item(&db, "MILK").await;
    seed_price_list(&db, "Standard Buying", true, false).await;
    seed_price(&db, "MILK", "Standard Buying", dec!(40), None, None).await;

    let repo = PgPriceRepo::new(db.pool().clone());
    let price = repo
        .item_price_async(&ItemCode::new("MILK"), &PriceListName::new("No Such List"))
        .await
        .expect("unknown price list must not be an error");
    assert_eq!(price, None);
}

#[tokio::test]
async fn a_dated_override_beats_the_open_ended_base_rate() {
    // valid_from NULL is the base rate; a dated row is the override that supersedes it.
    let db = TestDb::new().await;
    seed_item(&db, "MILK").await;
    seed_price_list(&db, "Standard Buying", true, false).await;

    let today = chrono::Utc::now().date_naive();
    seed_price(&db, "MILK", "Standard Buying", dec!(40), None, None).await;
    seed_price(
        &db,
        "MILK",
        "Standard Buying",
        dec!(45.50),
        Some(today - chrono::Duration::days(5)),
        None,
    )
    .await;

    let repo = PgPriceRepo::new(db.pool().clone());
    let price = repo
        .item_price_async(
            &ItemCode::new("MILK"),
            &PriceListName::new("Standard Buying"),
        )
        .await
        .expect("item price");

    assert_eq!(price, Some(Money::new(dec!(45.50))));
}

#[tokio::test]
async fn the_most_recent_applicable_rate_wins() {
    let db = TestDb::new().await;
    seed_item(&db, "MILK").await;
    seed_price_list(&db, "Standard Buying", true, false).await;

    let today = chrono::Utc::now().date_naive();
    for (days_ago, rate) in [(30, dec!(38)), (10, dec!(42)), (2, dec!(46))] {
        seed_price(
            &db,
            "MILK",
            "Standard Buying",
            rate,
            Some(today - chrono::Duration::days(days_ago)),
            None,
        )
        .await;
    }

    let repo = PgPriceRepo::new(db.pool().clone());
    assert_eq!(
        repo.item_price_async(
            &ItemCode::new("MILK"),
            &PriceListName::new("Standard Buying")
        )
        .await
        .expect("item price"),
        Some(Money::new(dec!(46)))
    );
}

#[tokio::test]
async fn a_future_rate_does_not_apply_yet() {
    let db = TestDb::new().await;
    seed_item(&db, "MILK").await;
    seed_price_list(&db, "Standard Buying", true, false).await;

    let today = chrono::Utc::now().date_naive();
    seed_price(&db, "MILK", "Standard Buying", dec!(40), None, None).await;
    seed_price(
        &db,
        "MILK",
        "Standard Buying",
        dec!(77),
        Some(today + chrono::Duration::days(5)),
        None,
    )
    .await;

    let repo = PgPriceRepo::new(db.pool().clone());
    assert_eq!(
        repo.item_price_async(
            &ItemCode::new("MILK"),
            &PriceListName::new("Standard Buying")
        )
        .await
        .expect("item price"),
        Some(Money::new(dec!(40))),
        "a rate whose valid_from is in the future must not win"
    );
}

#[tokio::test]
async fn an_expired_rate_does_not_apply() {
    let db = TestDb::new().await;
    seed_item(&db, "MILK").await;
    seed_price_list(&db, "Standard Buying", true, false).await;

    let today = chrono::Utc::now().date_naive();
    // Expired: its window closed 10 days ago, even though valid_from is the latest.
    seed_price(
        &db,
        "MILK",
        "Standard Buying",
        dec!(99),
        Some(today - chrono::Duration::days(30)),
        Some(today - chrono::Duration::days(10)),
    )
    .await;
    seed_price(&db, "MILK", "Standard Buying", dec!(40), None, None).await;

    let repo = PgPriceRepo::new(db.pool().clone());
    assert_eq!(
        repo.item_price_async(
            &ItemCode::new("MILK"),
            &PriceListName::new("Standard Buying")
        )
        .await
        .expect("item price"),
        Some(Money::new(dec!(40))),
        "an expired rate must not win"
    );
}

#[tokio::test]
async fn a_price_can_be_read_as_of_a_past_date() {
    // COGS runs for a business day, which may not be today.
    let db = TestDb::new().await;
    seed_item(&db, "MILK").await;
    seed_price_list(&db, "Standard Buying", true, false).await;

    let today = chrono::Utc::now().date_naive();
    seed_price(&db, "MILK", "Standard Buying", dec!(40), None, None).await;
    seed_price(
        &db,
        "MILK",
        "Standard Buying",
        dec!(46),
        Some(today - chrono::Duration::days(3)),
        None,
    )
    .await;

    let repo = PgPriceRepo::new(db.pool().clone());
    let item = ItemCode::new("MILK");
    let list = PriceListName::new("Standard Buying");

    // Before the override existed: the base rate.
    assert_eq!(
        repo.item_price_on_async(&item, &list, Some(today - chrono::Duration::days(10)))
            .await
            .expect("historical price"),
        Some(Money::new(dec!(40)))
    );
    // After it took effect.
    assert_eq!(
        repo.item_price_on_async(&item, &list, Some(today))
            .await
            .expect("current price"),
        Some(Money::new(dec!(46)))
    );
}

#[tokio::test]
async fn buying_and_selling_lists_are_independent() {
    // COGS reads the buying list (ury_daily_p_and_l.py:30), never stock valuation and
    // never the selling list. Reading the wrong one silently changes the cost basis.
    let db = TestDb::new().await;
    seed_item(&db, "MILK").await;
    seed_price_list(&db, "Standard Buying", true, false).await;
    seed_price_list(&db, "Standard Selling", false, true).await;
    seed_price(&db, "MILK", "Standard Buying", dec!(40), None, None).await;
    seed_price(&db, "MILK", "Standard Selling", dec!(95), None, None).await;

    let repo = PgPriceRepo::new(db.pool().clone());
    let item = ItemCode::new("MILK");

    assert_eq!(
        repo.item_price_async(&item, &PriceListName::new("Standard Buying"))
            .await
            .expect("buying price"),
        Some(Money::new(dec!(40)))
    );
    assert_eq!(
        repo.item_price_async(&item, &PriceListName::new("Standard Selling"))
            .await
            .expect("selling price"),
        Some(Money::new(dec!(95)))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_specific_price_list_takes_precedence_over_the_default() {
    // Multi-pricelist precedence, with the caller stating the order.
    let db = TestDb::new().await;
    seed_item(&db, "MILK").await;
    seed_price_list(&db, "Branch Buying", true, false).await;
    seed_price_list(&db, "Standard Buying", true, false).await;
    seed_price(&db, "MILK", "Branch Buying", dec!(38.50), None, None).await;
    seed_price(&db, "MILK", "Standard Buying", dec!(40), None, None).await;

    let repo = PgPriceRepo::new(db.pool().clone());
    let chain = vec![
        PriceListName::new("Branch Buying"),
        PriceListName::new("Standard Buying"),
    ];
    let found = in_blocking(move || repo.item_price_with_fallback(&ItemCode::new("MILK"), &chain))
        .await
        .expect("price with fallback")
        .expect("a price should be found");

    assert_eq!(found.0.as_str(), "Branch Buying");
    assert_eq!(found.1, Money::new(dec!(38.50)));
}

#[tokio::test]
async fn the_default_price_list_is_used_when_the_specific_one_has_no_row() {
    let db = TestDb::new().await;
    seed_item(&db, "MILK").await;
    seed_price_list(&db, "Branch Buying", true, false).await;
    seed_price_list(&db, "Standard Buying", true, false).await;
    // Only the default list prices it.
    seed_price(&db, "MILK", "Standard Buying", dec!(40), None, None).await;

    let repo = PgPriceRepo::new(db.pool().clone());
    let found = repo
        .item_price_with_fallback_async(
            &ItemCode::new("MILK"),
            &[
                PriceListName::new("Branch Buying"),
                PriceListName::new("Standard Buying"),
            ],
            None,
        )
        .await
        .expect("price with fallback")
        .expect("the default list should supply the price");

    assert_eq!(found.0.as_str(), "Standard Buying");
    assert_eq!(found.1, Money::new(dec!(40)));
}

#[tokio::test]
async fn a_fallback_chain_that_prices_nothing_returns_none() {
    let db = TestDb::new().await;
    seed_item(&db, "TEA").await;
    seed_price_list(&db, "Branch Buying", true, false).await;
    seed_price_list(&db, "Standard Buying", true, false).await;

    let repo = PgPriceRepo::new(db.pool().clone());
    let found = repo
        .item_price_with_fallback_async(
            &ItemCode::new("TEA"),
            &[
                PriceListName::new("Branch Buying"),
                PriceListName::new("Standard Buying"),
            ],
            None,
        )
        .await
        .expect("empty chain result must not be an error");
    assert_eq!(found, None);

    // An empty chain is also not an error.
    assert_eq!(
        repo.item_price_with_fallback_async(&ItemCode::new("TEA"), &[], None)
            .await
            .expect("empty chain"),
        None
    );
}

#[tokio::test]
async fn batched_prices_apply_the_same_precedence_as_the_scalar_lookup() {
    // The COGS path prices every ingredient of every BOM; per-item lookups in a loop are
    // the N+1 shape bugs 6 and 7 are.
    let db = TestDb::new().await;
    for code in ["MILK", "SUGAR", "TEA", "UNPRICED"] {
        seed_item(&db, code).await;
    }
    seed_price_list(&db, "Standard Buying", true, false).await;

    let today = chrono::Utc::now().date_naive();
    seed_price(&db, "MILK", "Standard Buying", dec!(40), None, None).await;
    seed_price(
        &db,
        "MILK",
        "Standard Buying",
        dec!(45.50),
        Some(today - chrono::Duration::days(5)),
        None,
    )
    .await;
    seed_price(&db, "SUGAR", "Standard Buying", dec!(45), None, None).await;
    seed_price(
        &db,
        "TEA",
        "Standard Buying",
        dec!(500),
        Some(today - chrono::Duration::days(60)),
        Some(today - chrono::Duration::days(30)),
    )
    .await; // expired

    let repo = PgPriceRepo::new(db.pool().clone());
    let items: Vec<ItemCode> = ["MILK", "SUGAR", "TEA", "UNPRICED"]
        .iter()
        .map(|c| ItemCode::new(*c))
        .collect();

    let prices = repo
        .item_prices_batch_async(&items, &PriceListName::new("Standard Buying"), None)
        .await
        .expect("batched prices");

    // Dated override wins, exactly as in the scalar lookup.
    assert_eq!(
        prices.get(&ItemCode::new("MILK")),
        Some(&Money::new(dec!(45.50)))
    );
    assert_eq!(
        prices.get(&ItemCode::new("SUGAR")),
        Some(&Money::new(dec!(45)))
    );
    // Expired and never-priced items are absent, not zero.
    assert!(!prices.contains_key(&ItemCode::new("TEA")));
    assert!(!prices.contains_key(&ItemCode::new("UNPRICED")));
    assert_eq!(prices.len(), 2);

    // And the batch agrees with the scalar lookup item by item.
    for item in &items {
        let scalar = repo
            .item_price_async(item, &PriceListName::new("Standard Buying"))
            .await
            .expect("scalar price");
        assert_eq!(
            prices.get(item).copied(),
            scalar,
            "batch and scalar disagree for {item}"
        );
    }
}

#[tokio::test]
async fn an_empty_batch_costs_no_query() {
    let db = TestDb::new().await;
    seed_price_list(&db, "Standard Buying", true, false).await;

    let repo = PgPriceRepo::new(db.pool().clone());
    let prices = repo
        .item_prices_batch_async(&[], &PriceListName::new("Standard Buying"), None)
        .await
        .expect("empty batch");
    assert!(prices.is_empty());
}

// ===========================================================================
// 6. The sync/async bridge
// ===========================================================================

#[test]
fn sync_trait_impls_work_with_no_ambient_runtime() {
    // A CLI or a test harness with no #[tokio::main] still has to be able to call the
    // port traits — the fallback runtime in `repos::blocking` covers that.
    let rt = tokio::runtime::Runtime::new().expect("build runtime");
    let db = rt.block_on(TestDb::new());
    rt.block_on(seed_item(&db, "MILK"));
    rt.block_on(seed_price_list(&db, "Standard Buying", true, false));
    rt.block_on(seed_price(
        &db,
        "MILK",
        "Standard Buying",
        dec!(40),
        None,
        None,
    ));
    let pool = db.pool().clone();

    // Deliberately outside any runtime context.
    let repo = PgPriceRepo::new(pool);
    let price = repo
        .item_price(
            &ItemCode::new("MILK"),
            &PriceListName::new("Standard Buying"),
        )
        .expect("item price without an ambient runtime");
    assert_eq!(price, Some(Money::new(dec!(40))));

    drop(db);
}
