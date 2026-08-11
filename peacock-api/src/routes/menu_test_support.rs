//! Test fixture shared by the `menu.rs` and `items.rs` unit tests — Lane W1-B.
//!
//! A throwaway `peacock_menu_test_<uuid>` database per test, migrated on connect and
//! dropped on `Drop`. Same shape as `peacock-storage/tests/support/mod.rs` and
//! `peacock-api/tests/support/mod.rs`; it lives in `src` rather than `tests` because these
//! are `#[cfg(test)]` unit tests inside the crate and a `tests/` module is not reachable
//! from there.
//!
//! # Why a real database
//!
//! Everything worth proving about these two endpoints is in SQL. The `room_wise_menu` and
//! `order_type_wise_menu` flags are enforced inside the repository's queries, the price
//! precedence is an `ORDER BY`, the soft-delete filters are `WHERE` clauses, and the
//! cross-branch check is a join. A mocked repository would assert that the handler calls
//! the method — which is not the part that can be wrong.
//!
//! # Skipping
//!
//! [`MenuFixture::try_new`] returns `None` when no server is reachable, and every test
//! returns early on it, so a bare checkout can still run `cargo test`. Set
//! `TEST_DATABASE_URL` (or `DATABASE_URL`) to any database on the target server; it is used
//! only to issue `CREATE DATABASE`.
//!
//! # The seeded world
//!
//! One restaurant, one branch, three menus, four items. The numbers are chosen so a
//! confusion between the three price sources is visible rather than plausible:
//!
//! | Item | `menu_items.rate` (selling) | `item_prices` buying | `item_prices` selling |
//! |---|---|---|---|
//! | BIRYANI | **250** | **99** | **260** |
//!
//! Three distinct values for one item. A response showing 99 where 250 belongs means a
//! handler read the COGS basis; 260 where 250 belongs means it read `Item Price` instead of
//! the menu child table. Both are the bug GROUND-TRUTH.md warns about, and both are now a
//! failing assertion rather than a plausible-looking number.

#![cfg(test)]

use std::time::Duration;

use peacock_storage::{DbConfig, Storage};
use sqlx::{Connection, Executor, PgConnection};

pub const RESTAURANT: &str = "Peacock Grand";
pub const BRANCH: &str = "Peacock - Main";
pub const ROOM: &str = "Main Hall";

const DEFAULT_ADMIN_URL: &str = "postgres://localhost:5432/postgres";

pub struct MenuFixture {
    storage: Storage,
    admin_url: String,
    db_name: String,
}

impl MenuFixture {
    /// The restaurant's `active_menu` — strategy 3.
    pub const DEFAULT_MENU: &'static str = "Menu-Default";
    /// Mapped to [`ROOM`] in `menu_for_room` — strategy 1.
    pub const ROOM_MENU: &'static str = "Menu-Room";
    /// Mapped to order type `Delivery` in `order_type_menu` — strategy 2.
    pub const DELIVERY_MENU: &'static str = "Menu-Delivery";
    /// A menu on a different branch, for the cross-branch scope check.
    pub const FOREIGN_MENU: &'static str = "Menu-Other-Branch";
    /// The buying list — the COGS basis (`ury_daily_p_and_l.py:30`).
    pub const BUYING_LIST: &'static str = "Peacock Buying";
    /// The selling list `item_prices` also carries, to prove the menu rate is not read
    /// from here either.
    pub const SELLING_LIST: &'static str = "Standard Selling";

    /// `None` when no server is reachable, so the caller can skip rather than fail.
    pub async fn try_new() -> Option<MenuFixture> {
        let admin_url = admin_url();
        let db_name = format!("peacock_menu_test_{}", uuid::Uuid::new_v4().simple());

        let mut admin = match PgConnection::connect(&admin_url).await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!(
                    "skipping: no Postgres at {} ({e}). Set TEST_DATABASE_URL to run these.",
                    redact(&admin_url)
                );
                return None;
            }
        };

        // The identifier is a uuid we generated, but quote it anyway: CREATE DATABASE takes
        // no bind parameters, so quoting is the only defence if the name ever becomes
        // caller-supplied.
        admin
            .execute(format!(r#"CREATE DATABASE "{db_name}""#).as_str())
            .await
            .unwrap_or_else(|e| panic!("CREATE DATABASE {db_name} failed: {e}"));
        admin.close().await.ok();

        let url = swap_database(&admin_url, &db_name);
        let config = DbConfig::from_url(&url)
            .expect("test url should be valid")
            .with_acquire_timeout(Duration::from_secs(5));
        let storage = Storage::connect(config)
            .await
            .unwrap_or_else(|e| panic!("connect + migrate on {db_name} failed: {e}"));

        let fixture = MenuFixture {
            storage,
            admin_url,
            db_name,
        };
        fixture.seed().await;
        Some(fixture)
    }

    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    pub fn pool(&self) -> &sqlx::PgPool {
        self.storage.pool()
    }

    #[allow(dead_code)]
    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    /// Turn on `room_wise_menu`.
    ///
    /// Off in the seed on purpose: the default state must be the one where the mapping is
    /// ignored, so a test that forgets to enable the flag and still sees the room menu is a
    /// failure rather than a pass.
    pub async fn enable_room_wise(&self) {
        sqlx::query("UPDATE restaurants SET room_wise_menu = TRUE WHERE name = $1")
            .bind(RESTAURANT)
            .execute(self.pool())
            .await
            .expect("enable room_wise_menu");
    }

    pub async fn enable_order_type_wise(&self) {
        sqlx::query("UPDATE restaurants SET order_type_wise_menu = TRUE WHERE name = $1")
            .bind(RESTAURANT)
            .execute(self.pool())
            .await
            .expect("enable order_type_wise_menu");
    }

    /// A menu belonging to another branch, with an item on it.
    pub async fn seed_foreign_menu(&self) {
        sqlx::query("INSERT INTO menus (name, branch) VALUES ($1, 'Peacock - Second')")
            .bind(Self::FOREIGN_MENU)
            .execute(self.pool())
            .await
            .expect("seed the foreign menu");
        sqlx::query(
            "INSERT INTO menu_items (menu, idx, item, item_name, rate)
             VALUES ($1, 1, 'BIRYANI', 'Chicken Biryani', 999.00)",
        )
        .bind(Self::FOREIGN_MENU)
        .execute(self.pool())
        .await
        .expect("seed a foreign menu item");
    }

    /// A menu whose name needs percent-encoding in a URL.
    pub async fn seed_spaced_menu(&self) {
        sqlx::query("INSERT INTO menus (name, branch) VALUES ('Menu With Spaces', $1)")
            .bind(BRANCH)
            .execute(self.pool())
            .await
            .expect("seed the spaced menu");
    }

    /// The whole seeded world. See the module docs for the price table.
    async fn seed(&self) {
        let pool = self.pool();

        sqlx::query("INSERT INTO rooms (name, branch, room_type) VALUES ($1, $2, 'AC')")
            .bind(ROOM)
            .bind(BRANCH)
            .execute(pool)
            .await
            .expect("seed room");

        // Both `*_wise_menu` flags default to FALSE here, matching the column defaults.
        sqlx::query(
            "INSERT INTO restaurants
                 (name, company, branch, pos_profile, invoice_series_prefix, default_room)
             VALUES ($1, 'Peacock Foods', $2, 'Peacock POS', 'PCK-', $3)",
        )
        .bind(RESTAURANT)
        .bind(BRANCH)
        .bind(ROOM)
        .execute(pool)
        .await
        .expect("seed restaurant");

        for (code, name, group) in [
            ("BIRYANI", "Chicken Biryani", "Main Course"),
            ("DOSA", "Masala Dosa", "Main Course"),
            ("TEA", "Masala Tea", "Beverages"),
            ("STICKER", "Peacock Sticker", "Merchandise"),
        ] {
            sqlx::query("INSERT INTO items (code, name, item_group) VALUES ($1, $2, $3)")
                .bind(code)
                .bind(name)
                .bind(group)
                .execute(pool)
                .await
                .expect("seed item");
        }

        // Courses. `Mains` before `Beverages` by idx, so the expected output order is not
        // the insertion order and not alphabetical — a sort that silently did nothing would
        // show up.
        for (course, idx) in [("Mains", 1), ("Beverages", 2)] {
            sqlx::query("INSERT INTO menu_courses (name, idx) VALUES ($1, $2)")
                .bind(course)
                .bind(idx)
                .execute(pool)
                .await
                .expect("seed course");
        }

        for menu in [Self::DEFAULT_MENU, Self::ROOM_MENU, Self::DELIVERY_MENU] {
            sqlx::query("INSERT INTO menus (name, branch) VALUES ($1, $2)")
                .bind(menu)
                .bind(BRANCH)
                .execute(pool)
                .await
                .expect("seed menu");
        }

        // The default menu. Inserted in an order that is neither the expected output order
        // nor alphabetical: TEA (Beverages) first, the uncoursed STICKER in the middle.
        // Expected out: BIRYANI, DOSA (Mains, by name), TEA (Beverages), STICKER (last).
        for (idx, item, item_name, rate, course) in [
            (1, "TEA", "Masala Tea", "20.00", Some("Beverages")),
            (2, "STICKER", "Peacock Sticker", "50.00", None),
            (3, "DOSA", "Masala Dosa", "120.00", Some("Mains")),
            // 250 is the selling price the guest pays. Deliberately different from both
            // `item_prices` rows below.
            (4, "BIRYANI", "Chicken Biryani", "250.00", Some("Mains")),
        ] {
            sqlx::query(
                "INSERT INTO menu_items (menu, idx, item, item_name, rate, course)
                 VALUES ($1, $2, $3, $4, $5::numeric, $6)",
            )
            .bind(Self::DEFAULT_MENU)
            .bind(idx)
            .bind(item)
            .bind(item_name)
            .bind(rate)
            .bind(course)
            .execute(pool)
            .await
            .expect("seed default menu item");
        }

        // The room and delivery menus carry one item each: these tests assert on *which*
        // menu resolved, not on its contents.
        for menu in [Self::ROOM_MENU, Self::DELIVERY_MENU] {
            sqlx::query(
                "INSERT INTO menu_items (menu, idx, item, item_name, rate, course)
                 VALUES ($1, 1, 'BIRYANI', 'Chicken Biryani', 300.00, 'Mains')",
            )
            .bind(menu)
            .execute(pool)
            .await
            .expect("seed scoped menu item");
        }

        sqlx::query("UPDATE restaurants SET active_menu = $2 WHERE name = $1")
            .bind(RESTAURANT)
            .bind(Self::DEFAULT_MENU)
            .execute(pool)
            .await
            .expect("set active_menu");

        sqlx::query(
            "INSERT INTO menu_for_room (restaurant, idx, room, menu) VALUES ($1, 1, $2, $3)",
        )
        .bind(RESTAURANT)
        .bind(ROOM)
        .bind(Self::ROOM_MENU)
        .execute(pool)
        .await
        .expect("seed menu_for_room");

        sqlx::query(
            "INSERT INTO order_type_menu (restaurant, idx, order_type, menu)
             VALUES ($1, 1, 'Delivery', $2)",
        )
        .bind(RESTAURANT)
        .bind(Self::DELIVERY_MENU)
        .execute(pool)
        .await
        .expect("seed order_type_menu");

        // Price lists. `buying`/`selling` are separate booleans upstream and a list must
        // have a direction (001_core_tables.sql).
        sqlx::query("INSERT INTO price_lists (name, buying, selling) VALUES ($1, TRUE, FALSE)")
            .bind(Self::BUYING_LIST)
            .execute(pool)
            .await
            .expect("seed buying list");
        sqlx::query("INSERT INTO price_lists (name, buying, selling) VALUES ($1, FALSE, TRUE)")
            .bind(Self::SELLING_LIST)
            .execute(pool)
            .await
            .expect("seed selling list");

        // 99 = cost. If this ever appears as a menu rate, a handler read the COGS basis.
        sqlx::query("INSERT INTO item_prices (item_code, price_list, rate) VALUES ('BIRYANI', $1, 99.00)")
            .bind(Self::BUYING_LIST)
            .execute(pool)
            .await
            .expect("seed buying price");
        // 260 = `Item Price` on a selling list. If this appears as a menu rate, a handler
        // read `Item Price` instead of `menu_items.rate`.
        sqlx::query("INSERT INTO item_prices (item_code, price_list, rate) VALUES ('BIRYANI', $1, 260.00)")
            .bind(Self::SELLING_LIST)
            .execute(pool)
            .await
            .expect("seed selling price");
        // STICKER is priced on no list at all — the `Option`/404 path.
    }
}

impl Drop for MenuFixture {
    fn drop(&mut self) {
        // `Drop` is sync and the cleanup is async, and the pool's runtime is going away with
        // us, so the teardown gets its own short-lived one.
        let admin_url = self.admin_url.clone();
        let db_name = self.db_name.clone();
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async {
                if let Ok(mut admin) = PgConnection::connect(&admin_url).await {
                    let _ = admin
                        .execute(
                            format!(r#"DROP DATABASE IF EXISTS "{db_name}" WITH (FORCE)"#).as_str(),
                        )
                        .await;
                    let _ = admin.close().await;
                }
            });
        })
        .join();
    }
}

fn admin_url() -> String {
    std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_ADMIN_URL.to_owned())
}

/// Replace the database component of a URL, preserving query parameters such as `sslmode`.
fn swap_database(url: &str, db: &str) -> String {
    let (scheme, rest) = url.split_once("://").expect("url should have a scheme");
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let query = tail.find('?').map(|q| &tail[q..]).unwrap_or("");
    format!("{scheme}://{authority}/{db}{query}")
}

fn redact(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => match rest.rfind('@') {
            Some(at) => format!("{scheme}://***@{}", &rest[at + 1..]),
            None => url.to_owned(),
        },
        None => "<redacted>".to_owned(),
    }
}
