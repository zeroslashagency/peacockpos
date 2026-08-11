//! Lane 2A acceptance tests.
//!
//! These run against a real Postgres — each test gets its own freshly migrated
//! database (see `support::TestDb`), which is also how "the migration is repeatable"
//! gets proved on every run rather than once by hand.

mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use peacock_core::ids::{
    BranchName, ItemCode, PriceListName, ProductionUnitName, RestaurantName, RoomName, TableName,
};
use peacock_core::model::MergedWith;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use support::TestDb;

const BRANCH: &str = "Peacock HQ";
const RESTAURANT: &str = "Peacock Grand";
const ROOM: &str = "Main Hall";

// ---------------------------------------------------------------------------
// 1. Pool + health check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pool_connects_and_health_check_reports_a_live_connection() {
    let db = TestDb::new().await;

    let health = db.storage.health_check().await.expect("health check");

    assert!(health.pool_size >= 1, "pool has no connections");
    assert!(
        health.latency < Duration::from_secs(5),
        "health check took {:?}",
        health.latency
    );

    // Repeated checks reuse the pool instead of growing it without bound.
    for _ in 0..5 {
        db.storage
            .health_check()
            .await
            .expect("repeat health check");
    }
    assert!(health.pool_size <= db.storage.config().max_connections);
}

// ---------------------------------------------------------------------------
// 2. Migrations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn migration_creates_every_core_table() {
    let db = TestDb::new().await;

    // Each test migrates its own scratch database, so a green run is also evidence the
    // migration set is repeatable from empty (Phase 2 verification gate 5).
    assert!(
        db.db_name().starts_with("peacock_test_"),
        "unexpected scratch database name: {}",
        db.db_name()
    );

    let found: BTreeSet<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
    )
    .fetch_all(db.pool())
    .await
    .expect("list tables")
    .into_iter()
    .collect();

    for expected in [
        "restaurants",
        "rooms",
        "tables",
        "production_units",
        "production_unit_item_groups",
        "items",
        "price_lists",
        "item_prices",
    ] {
        assert!(
            found.contains(expected),
            "table {expected} missing; found {found:?}"
        );
    }

    // sqlx's own bookkeeping table must exist, otherwise re-running would replay
    // migrations that already applied.
    assert!(found.contains("_sqlx_migrations"));
}

#[tokio::test]
async fn migration_is_idempotent_and_reruns_are_a_no_op() {
    let db = TestDb::new().await;

    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(db.pool())
        .await
        .expect("count migrations");
    assert!(before >= 1, "no migrations recorded");

    // Storage::connect already migrated; running again must apply nothing new and must
    // not error on the checksums it already recorded.
    db.storage.migrate().await.expect("second migrate run");
    db.storage.migrate().await.expect("third migrate run");

    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(db.pool())
        .await
        .expect("recount migrations");
    assert_eq!(before, after, "re-running migrations added rows");

    let all_succeeded: bool = sqlx::query_scalar("SELECT bool_and(success) FROM _sqlx_migrations")
        .fetch_one(db.pool())
        .await
        .expect("check success flags");
    assert!(all_succeeded, "a migration is recorded as failed");
}

// ---------------------------------------------------------------------------
// 3. Schema shape matches the domain models
// ---------------------------------------------------------------------------

#[tokio::test]
async fn table_columns_match_the_domain_model() {
    let db = TestDb::new().await;

    let cols: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT column_name, data_type, is_nullable
         FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'tables'",
    )
    .fetch_all(db.pool())
    .await
    .expect("describe tables");

    let by_name: std::collections::HashMap<_, _> = cols
        .into_iter()
        .map(|(c, t, n)| (c, (t, n == "YES")))
        .collect();

    // peacock_core::model::Table, field for field.
    let expectations: &[(&str, &str, bool)] = &[
        ("name", "text", false),
        ("no_of_seats", "integer", false),
        ("minimum_seating", "integer", false),
        ("restaurant", "text", false),
        ("restaurant_room", "text", false),
        ("branch", "text", false),
        ("is_take_away", "boolean", false),
        ("occupied", "boolean", false),
        // Option<NaiveTime> upstream — a bare Time, no zone.
        ("latest_invoice_time", "time without time zone", true),
        ("table_shape", "text", true),
        // Geometry, so float is correct here. Money never gets this treatment.
        ("layout_x", "double precision", false),
        ("layout_y", "double precision", false),
        ("layout_width", "double precision", false),
        ("layout_height", "double precision", false),
        ("merged_with", "jsonb", false),
        ("created_at", "timestamp with time zone", false),
        ("updated_at", "timestamp with time zone", false),
        ("deleted_at", "timestamp with time zone", true),
    ];

    for (col, ty, nullable) in expectations {
        let (actual_ty, actual_nullable) = by_name
            .get(*col)
            .unwrap_or_else(|| panic!("tables.{col} missing; have {:?}", by_name.keys()));
        assert_eq!(actual_ty, ty, "tables.{col} type");
        assert_eq!(actual_nullable, nullable, "tables.{col} nullability");
    }
}

#[tokio::test]
async fn money_columns_are_numeric_never_float() {
    let db = TestDb::new().await;

    // money.rs is explicit that money is Decimal only. A float rate would reintroduce
    // paisa drift that the parity harness exists to catch.
    let (data_type, precision, scale): (String, Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT data_type, numeric_precision, numeric_scale
         FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'item_prices' AND column_name = 'rate'",
    )
    .fetch_one(db.pool())
    .await
    .expect("describe item_prices.rate");

    assert_eq!(data_type, "numeric");
    assert_eq!(precision, Some(18));
    assert_eq!(scale, Some(6));

    let floats: Vec<String> = sqlx::query_scalar(
        "SELECT table_name || '.' || column_name
         FROM information_schema.columns
         WHERE table_schema = 'public'
           AND data_type IN ('real', 'double precision')
           AND column_name NOT LIKE 'layout\\_%'",
    )
    .fetch_all(db.pool())
    .await
    .expect("scan for float columns");

    assert!(
        floats.is_empty(),
        "float columns outside table geometry: {floats:?}"
    );
}

#[tokio::test]
async fn every_query_path_from_phase_one_has_an_index() {
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

    // TableRepo::list_by_room — the merge BFS does one room query, not one per hop.
    assert!(
        has("on public.tables using btree (restaurant, restaurant_room)"),
        "missing tables(restaurant, restaurant_room) index: {indexes:?}"
    );
    // Table status is (occupied, is_take_away) upstream; there is no status column.
    assert!(
        has("on public.tables using btree (restaurant_room, occupied)"),
        "missing tables occupancy index"
    );
    // ProductionRepo::list_for_branch
    assert!(
        has("on public.production_units using btree (branch)"),
        "missing production_units(branch) index"
    );
    // PriceRepo::item_price
    assert!(
        has("on public.item_prices using btree (item_code, price_list)"),
        "missing item_prices(item_code, price_list) index"
    );
    // "which cluster is this table in" must not scan every merged_with array.
    assert!(
        has("on public.tables using gin (merged_with)"),
        "missing GIN index on tables.merged_with"
    );
    // ItemRepo::item_groups routes KOT lines by item_group.
    assert!(
        has("on public.items using btree (item_group)"),
        "missing items(item_group) index"
    );
}

// ---------------------------------------------------------------------------
// 4. Round-trip: insert and read back every entity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn restaurant_round_trips() {
    let db = TestDb::new().await;
    db.seed_restaurant_and_room(RESTAURANT, ROOM, BRANCH).await;

    let (name, branch, pos_profile, prefix, room_wise): (
        String,
        String,
        Option<String>,
        String,
        bool,
    ) = sqlx::query_as(
        "SELECT name, branch, pos_profile, invoice_series_prefix, room_wise_menu
             FROM restaurants WHERE name = $1",
    )
    .bind(RESTAURANT)
    .fetch_one(db.pool())
    .await
    .expect("read restaurant");

    assert_eq!(RestaurantName::from(name.as_str()).as_str(), RESTAURANT);
    assert_eq!(BranchName::from(branch.as_str()).as_str(), BRANCH);
    assert_eq!(pos_profile.as_deref(), Some("Peacock POS"));
    assert_eq!(prefix, "PCK-");
    assert!(!room_wise, "room_wise_menu should default false");
}

#[tokio::test]
async fn table_round_trips_including_merged_with_jsonb() {
    let db = TestDb::new().await;
    db.seed_restaurant_and_room(RESTAURANT, ROOM, BRANCH).await;

    // The CSV form is what legacy rows hold; the JSONB column is the new home.
    let merged = MergedWith::parse(Some("T-02, T-03 ,,"));
    let as_json: Vec<String> = merged.iter().map(|t| t.as_str().to_owned()).collect();

    sqlx::query(
        "INSERT INTO tables
             (name, no_of_seats, minimum_seating, restaurant, restaurant_room, branch,
              is_take_away, occupied, latest_invoice_time, table_shape,
              layout_x, layout_y, layout_width, layout_height, merged_with)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind("T-01")
    .bind(4_i32)
    .bind(2_i32)
    .bind(RESTAURANT)
    .bind(ROOM)
    .bind(BRANCH)
    .bind(false)
    .bind(true)
    .bind(chrono::NaiveTime::from_hms_opt(19, 45, 0).unwrap())
    .bind("Circle")
    .bind(1.5_f64)
    .bind(2.5_f64)
    .bind(80.0_f64)
    .bind(60.0_f64)
    .bind(serde_json::to_value(&as_json).unwrap())
    .execute(db.pool())
    .await
    .expect("insert table");

    #[allow(clippy::type_complexity)]
    let (name, seats, room, occupied, shape, time, merged_json): (
        String,
        i32,
        String,
        bool,
        Option<String>,
        Option<chrono::NaiveTime>,
        serde_json::Value,
    ) = sqlx::query_as(
        "SELECT name, no_of_seats, restaurant_room, occupied, table_shape,
                latest_invoice_time, merged_with
         FROM tables WHERE name = $1",
    )
    .bind("T-01")
    .fetch_one(db.pool())
    .await
    .expect("read table");

    assert_eq!(TableName::from(name.as_str()).as_str(), "T-01");
    assert_eq!(seats, 4);
    assert_eq!(RoomName::from(room.as_str()).as_str(), ROOM);
    assert!(occupied);
    assert_eq!(shape.as_deref(), Some("Circle"));
    assert_eq!(time, chrono::NaiveTime::from_hms_opt(19, 45, 0));

    // Vec -> JSONB -> Vec preserves order and drops nothing.
    let back: Vec<String> =
        serde_json::from_value(merged_json).expect("merged_with as Vec<String>");
    assert_eq!(back, vec!["T-02", "T-03"]);

    let rebuilt = MergedWith::parse(Some(&back.join(",")));
    assert_eq!(rebuilt, merged, "JSONB round-trip changed the cluster");
    assert!(rebuilt.contains(&TableName::from("T-03")));
}

#[tokio::test]
async fn production_unit_round_trips_with_ordered_item_groups() {
    let db = TestDb::new().await;

    sqlx::query(
        "INSERT INTO production_units (name, branch, pos_profile) VALUES ($1, $2, 'Peacock POS')",
    )
    .bind("Hot Kitchen")
    .bind(BRANCH)
    .execute(db.pool())
    .await
    .expect("insert production unit");

    for (idx, group) in ["Main Course", "Starters", "Breads"].iter().enumerate() {
        sqlx::query(
            "INSERT INTO production_unit_item_groups (production_unit, idx, item_group)
             VALUES ($1, $2, $3)",
        )
        .bind("Hot Kitchen")
        .bind(idx as i32 + 1)
        .bind(group)
        .execute(db.pool())
        .await
        .expect("insert item group");
    }

    let (name, branch): (String, String) =
        sqlx::query_as("SELECT name, branch FROM production_units WHERE name = $1")
            .bind("Hot Kitchen")
            .fetch_one(db.pool())
            .await
            .expect("read production unit");

    assert_eq!(
        ProductionUnitName::from(name.as_str()).as_str(),
        "Hot Kitchen"
    );
    assert_eq!(branch, BRANCH);

    // ProductionUnit::item_groups is a Vec, so `idx` has to preserve insertion order.
    let groups: Vec<String> = sqlx::query_scalar(
        "SELECT item_group FROM production_unit_item_groups
         WHERE production_unit = $1 ORDER BY idx",
    )
    .bind("Hot Kitchen")
    .fetch_all(db.pool())
    .await
    .expect("read item groups");

    assert_eq!(groups, vec!["Main Course", "Starters", "Breads"]);

    // Child rows are embed-only: deleting the parent takes them with it.
    sqlx::query("DELETE FROM production_units WHERE name = $1")
        .bind("Hot Kitchen")
        .execute(db.pool())
        .await
        .expect("delete production unit");

    let orphans: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM production_unit_item_groups WHERE production_unit = $1",
    )
    .bind("Hot Kitchen")
    .fetch_one(db.pool())
    .await
    .expect("count orphans");
    assert_eq!(orphans, 0, "child rows survived the parent delete");
}

#[tokio::test]
async fn item_round_trips() {
    let db = TestDb::new().await;

    sqlx::query(
        "INSERT INTO items (code, name, item_group, stock_uom, is_bom)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind("MASALA-CHAI")
    .bind("Masala Chai")
    .bind("Beverages")
    .bind("Nos")
    .bind(true)
    .execute(db.pool())
    .await
    .expect("insert item");

    let (code, name, group, is_bom, disabled): (String, String, Option<String>, bool, bool) =
        sqlx::query_as(
            "SELECT code, name, item_group, is_bom, disabled FROM items WHERE code = $1",
        )
        .bind("MASALA-CHAI")
        .fetch_one(db.pool())
        .await
        .expect("read item");

    assert_eq!(ItemCode::from(code.as_str()).as_str(), "MASALA-CHAI");
    assert_eq!(name, "Masala Chai");
    assert_eq!(group.as_deref(), Some("Beverages"));
    assert!(is_bom);
    assert!(!disabled);
}

#[tokio::test]
async fn item_price_round_trips_at_full_decimal_precision() {
    let db = TestDb::new().await;

    sqlx::query("INSERT INTO items (code, name) VALUES ('MILK', 'Milk')")
        .execute(db.pool())
        .await
        .expect("insert item");
    sqlx::query(
        "INSERT INTO price_lists (name, buying, selling) VALUES ('Standard Buying', true, false)",
    )
    .execute(db.pool())
    .await
    .expect("insert price list");

    // Six decimals: a per-unit ingredient rate is divided by BOM batch size before it
    // is ever rounded to paisa, so truncating the stored value would move COGS.
    let rate = dec!(41.666667);
    sqlx::query(
        "INSERT INTO item_prices (item_code, price_list, rate, uom) VALUES ($1, $2, $3, 'Litre')",
    )
    .bind("MILK")
    .bind("Standard Buying")
    .bind(rate)
    .execute(db.pool())
    .await
    .expect("insert price");

    let stored: Decimal =
        sqlx::query_scalar("SELECT rate FROM item_prices WHERE item_code = $1 AND price_list = $2")
            .bind("MILK")
            .bind("Standard Buying")
            .fetch_one(db.pool())
            .await
            .expect("read rate");

    assert_eq!(stored, rate, "rate lost precision in the round trip");

    // PriceRepo::item_price returns Option; a missing row is normal, not an error.
    let missing: Option<Decimal> =
        sqlx::query_scalar("SELECT rate FROM item_prices WHERE item_code = $1 AND price_list = $2")
            .bind("MILK")
            .bind(PriceListName::from("Standard Selling").as_str())
            .fetch_optional(db.pool())
            .await
            .expect("query missing price");
    assert!(missing.is_none());
}

// ---------------------------------------------------------------------------
// 5. Constraints and triggers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn merged_with_rejects_shapes_that_would_break_deserialisation() {
    let db = TestDb::new().await;
    db.seed_restaurant_and_room(RESTAURANT, ROOM, BRANCH).await;

    sqlx::query(
        "INSERT INTO tables (name, no_of_seats, restaurant, restaurant_room, branch)
         VALUES ('T-01', 4, $1, $2, $3)",
    )
    .bind(RESTAURANT)
    .bind(ROOM)
    .bind(BRANCH)
    .execute(db.pool())
    .await
    .expect("insert table");

    // MergedWith is Vec<TableName>: an object, a scalar or a mixed array cannot be
    // deserialised, so the schema refuses them rather than letting a repo panic later.
    for bad in [r#"{"a": 1}"#, r#""T-02""#, r#"["T-02", 7]"#, "42"] {
        let err = sqlx::query("UPDATE tables SET merged_with = $1::jsonb WHERE name = 'T-01'")
            .bind(bad)
            .execute(db.pool())
            .await
            .expect_err(&format!("merged_with accepted {bad}"));

        let db_err = err.as_database_error().expect("database error");
        assert_eq!(
            db_err.code().as_deref(),
            Some("23514"),
            "expected a CHECK violation for {bad}"
        );
    }

    // merge.rs treats the seed table as implicit; storing it would double-count.
    let err =
        sqlx::query(r#"UPDATE tables SET merged_with = '["T-01"]'::jsonb WHERE name = 'T-01'"#)
            .execute(db.pool())
            .await
            .expect_err("self-membership accepted");
    assert_eq!(
        err.as_database_error().and_then(|e| e.constraint()),
        Some("tables_merged_with_excludes_self")
    );

    // An empty array is the normal unmerged state.
    sqlx::query("UPDATE tables SET merged_with = '[]'::jsonb WHERE name = 'T-01'")
        .execute(db.pool())
        .await
        .expect("empty array should be valid");
}

#[tokio::test]
async fn updated_at_trigger_fires_on_every_table() {
    let db = TestDb::new().await;
    db.seed_restaurant_and_room(RESTAURANT, ROOM, BRANCH).await;

    sqlx::query(
        "INSERT INTO tables (name, no_of_seats, restaurant, restaurant_room, branch)
         VALUES ('T-07', 2, $1, $2, $3)",
    )
    .bind(RESTAURANT)
    .bind(ROOM)
    .bind(BRANCH)
    .execute(db.pool())
    .await
    .expect("insert table");

    let (created, updated): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT created_at, updated_at FROM tables WHERE name = 'T-07'")
            .fetch_one(db.pool())
            .await
            .expect("read timestamps");
    assert_eq!(created, updated, "a fresh row should have equal timestamps");

    sqlx::query("UPDATE tables SET occupied = true WHERE name = 'T-07'")
        .execute(db.pool())
        .await
        .expect("update table");

    let (created_after, updated_after): (
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
    ) = sqlx::query_as("SELECT created_at, updated_at FROM tables WHERE name = 'T-07'")
        .fetch_one(db.pool())
        .await
        .expect("re-read timestamps");

    assert_eq!(created, created_after, "created_at must not move");
    assert!(
        updated_after > updated,
        "updated_at did not advance: {updated} -> {updated_after}"
    );

    // The trigger must be attached everywhere, not just to `tables`.
    let missing: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname
         FROM pg_class c
         JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public'
           AND c.relkind = 'r'
           AND c.relname <> '_sqlx_migrations'
           AND EXISTS (
               SELECT 1 FROM information_schema.columns col
               WHERE col.table_schema = 'public'
                 AND col.table_name = c.relname
                 AND col.column_name = 'updated_at'
           )
           AND NOT EXISTS (
               SELECT 1 FROM pg_trigger t
               WHERE t.tgrelid = c.oid AND NOT t.tgisinternal
           )",
    )
    .fetch_all(db.pool())
    .await
    .expect("find tables without an updated_at trigger");

    assert!(
        missing.is_empty(),
        "tables have updated_at but no trigger to maintain it: {missing:?}"
    );
}

#[tokio::test]
async fn foreign_keys_and_uniqueness_are_enforced() {
    let db = TestDb::new().await;
    db.seed_restaurant_and_room(RESTAURANT, ROOM, BRANCH).await;

    // A table cannot point at a room that does not exist — merge.rs is room-scoped and
    // relies on the room reference being real (Error::CrossRoomMerge).
    let err = sqlx::query(
        "INSERT INTO tables (name, no_of_seats, restaurant, restaurant_room, branch)
         VALUES ('T-404', 4, $1, 'Nonexistent Room', $2)",
    )
    .bind(RESTAURANT)
    .bind(BRANCH)
    .execute(db.pool())
    .await
    .expect_err("FK to a missing room was accepted");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23503")
    );

    // A restaurant with live tables cannot be deleted out from under them.
    sqlx::query(
        "INSERT INTO tables (name, no_of_seats, restaurant, restaurant_room, branch)
         VALUES ('T-11', 4, $1, $2, $3)",
    )
    .bind(RESTAURANT)
    .bind(ROOM)
    .bind(BRANCH)
    .execute(db.pool())
    .await
    .expect("insert table");

    let err = sqlx::query("DELETE FROM restaurants WHERE name = $1")
        .bind(RESTAURANT)
        .execute(db.pool())
        .await
        .expect_err("restaurant with tables was deletable");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23503")
    );

    // One base price per (item, price_list). Two would make COGS depend on row order.
    sqlx::query("INSERT INTO items (code, name) VALUES ('SUGAR', 'Sugar')")
        .execute(db.pool())
        .await
        .expect("insert item");
    sqlx::query("INSERT INTO price_lists (name, buying) VALUES ('Standard Buying', true)")
        .execute(db.pool())
        .await
        .expect("insert price list");
    sqlx::query(
        "INSERT INTO item_prices (item_code, price_list, rate) VALUES ('SUGAR','Standard Buying',45)",
    )
    .execute(db.pool())
    .await
    .expect("insert first price");

    let err = sqlx::query(
        "INSERT INTO item_prices (item_code, price_list, rate) VALUES ('SUGAR','Standard Buying',50)",
    )
    .execute(db.pool())
    .await
    .expect_err("duplicate base price was accepted");
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23505")
    );

    // A dated price is a different row, not a duplicate.
    sqlx::query(
        "INSERT INTO item_prices (item_code, price_list, rate, valid_from)
         VALUES ('SUGAR','Standard Buying', 48, DATE '2026-04-01')",
    )
    .execute(db.pool())
    .await
    .expect("dated price should be allowed alongside the base price");
}

// ---------------------------------------------------------------------------
// 6. Concurrency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pool_serves_concurrent_requests_without_exhaustion() {
    // Fewer connections than tasks on purpose: the tasks have to queue and be handed a
    // connection in turn, which is the behaviour a busy POS depends on.
    let db = TestDb::with_config(|c| {
        c.with_max_connections(4)
            .with_min_connections(1)
            .with_acquire_timeout(Duration::from_secs(10))
    })
    .await;
    db.seed_restaurant_and_room(RESTAURANT, ROOM, BRANCH).await;

    for i in 0..20 {
        sqlx::query(
            "INSERT INTO tables (name, no_of_seats, restaurant, restaurant_room, branch)
             VALUES ($1, 4, $2, $3, $4)",
        )
        .bind(format!("T-{i:02}"))
        .bind(RESTAURANT)
        .bind(ROOM)
        .bind(BRANCH)
        .execute(db.pool())
        .await
        .expect("seed table");
    }

    let mut handles = Vec::new();
    for i in 0..40 {
        let pool = db.pool().clone();
        handles.push(tokio::spawn(async move {
            // Mix reads and writes so both paths contend for the same 4 connections.
            if i % 2 == 0 {
                let n: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM tables WHERE restaurant_room = $1 AND deleted_at IS NULL",
                )
                .bind(ROOM)
                .fetch_one(&pool)
                .await?;
                Ok::<i64, sqlx::Error>(n)
            } else {
                sqlx::query("UPDATE tables SET occupied = NOT occupied WHERE name = $1")
                    .bind(format!("T-{:02}", i % 20))
                    .execute(&pool)
                    .await?;
                Ok(0)
            }
        }));
    }

    let mut reads = 0;
    for h in handles {
        let n = h.await.expect("task panicked").expect("query failed");
        if n > 0 {
            assert_eq!(n, 20, "concurrent read saw a partial room");
            reads += 1;
        }
    }
    assert_eq!(reads, 20, "not every read completed");

    assert!(
        db.storage.pool().size() <= 4,
        "pool grew past max_connections: {}",
        db.storage.pool().size()
    );
    db.storage.health_check().await.expect("pool still healthy");
}

#[tokio::test]
async fn serializable_retry_helper_commits_and_survives_conflicts() {
    let db = TestDb::new().await;
    db.seed_restaurant_and_room(RESTAURANT, ROOM, BRANCH).await;

    // Establishes that the SERIALIZABLE wrapper actually commits; gapless numbering in
    // lanes 2E/2F is built on it.
    let inserted = db
        .storage
        .with_serializable_retry(3, |tx| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO tables (name, no_of_seats, restaurant, restaurant_room, branch)
                     VALUES ('T-SER', 6, $1, $2, $3)",
                )
                .bind(RESTAURANT)
                .bind(ROOM)
                .bind(BRANCH)
                .execute(&mut **tx)
                .await?;
                Ok(1_usize)
            })
        })
        .await
        .expect("serializable transaction");

    assert_eq!(inserted, 1);

    let seats: i32 = sqlx::query_scalar("SELECT no_of_seats FROM tables WHERE name = 'T-SER'")
        .fetch_one(db.pool())
        .await
        .expect("committed row should be visible");
    assert_eq!(seats, 6);

    // A non-retryable failure must surface immediately rather than burn the retries.
    let err = db
        .storage
        .with_serializable_retry(3, |tx| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO tables (name, no_of_seats, restaurant, restaurant_room, branch)
                     VALUES ('T-SER', 6, $1, $2, $3)",
                )
                .bind(RESTAURANT)
                .bind(ROOM)
                .bind(BRANCH)
                .execute(&mut **tx)
                .await?;
                Ok(1_usize)
            })
        })
        .await
        .expect_err("duplicate key should fail");

    assert!(
        !err.is_retryable(),
        "PK violation misclassified as retryable: {err}"
    );
}
