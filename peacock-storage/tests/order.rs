//! Lane 2H acceptance tests — the order form.
//!
//! Each test gets its own freshly migrated scratch database (`support::TestDb`), so a
//! green run is also evidence that 007_order.sql applies cleanly on top of 001-006.
//!
//! Three things are under test:
//!
//! 1. CRUD on the form and its cart.
//! 2. The `last_invoice` FK to `invoices(name)` (Lane 2F) — enforced, and `SET NULL` on
//!    invoice delete rather than blocking it.
//! 3. Row-level locking. Two waiters updating the same form must serialise: one blocks,
//!    both succeed, and the row that lands is the second one's — not a mixture of the
//!    two. The lock is proved by *timing* (the loser's write cannot commit before the
//!    winner releases), not just by the absence of corruption, since an unlocked
//!    interleaving would often produce plausible-looking output by luck.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use peacock_core::ids::{CustomerName, InvoiceName, ItemCode, TableName, UserName};
use peacock_core::model::{OrderItem, UryOrderForm};
use peacock_core::money::Money;
use peacock_core::ports::OrderRepo;
use peacock_storage::repos::order::{OrderId, PgOrderRepo};
use peacock_storage::StorageError;
use rust_decimal_macros::dec;
use support::TestDb;

const BRANCH: &str = "Peacock HQ";
const RESTAURANT: &str = "Peacock Grand";
const ROOM: &str = "Main Hall";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A restaurant, a room, two tables and two items — the minimum graph the FKs on
/// `orders` and `order_items` require.
async fn seed(db: &TestDb) {
    db.seed_restaurant_and_room(RESTAURANT, ROOM, BRANCH).await;

    for table in ["T-01", "T-02"] {
        sqlx::query(
            "INSERT INTO tables (name, no_of_seats, restaurant, restaurant_room, branch)
             VALUES ($1, 4, $2, $3, $4)",
        )
        .bind(table)
        .bind(RESTAURANT)
        .bind(ROOM)
        .bind(BRANCH)
        .execute(db.pool())
        .await
        .expect("seed table");
    }

    for (code, name) in [("CURRY", "Chicken Curry"), ("NAAN", "Butter Naan")] {
        sqlx::query("INSERT INTO items (code, name, item_group) VALUES ($1, $2, 'Main Course')")
            .bind(code)
            .bind(name)
            .execute(db.pool())
            .await
            .expect("seed item");
    }
}

/// Insert an invoice directly. Lane 2F owns `PgInvoiceRepo`; this lane only needs a row
/// that satisfies the FK, so it writes the minimum the 005 constraints accept.
///
/// Every money column stays 0, which satisfies the tax invariants trivially
/// (`cgst + sgst + igst = total_tax`, `round_off = rounded_total - grand_total`, and so
/// on) without this lane restating Lane 2F's arithmetic.
async fn seed_invoice(db: &TestDb, name: &str, table: Option<&str>, printed: bool) {
    sqlx::query(
        "INSERT INTO invoices (
             name, naming_series, fiscal_year, series_number, status,
             restaurant, restaurant_table, restaurant_room, branch, customer,
             posted_at, business_day, supply_type, invoice_printed
         ) VALUES ($1, 'POS-', '2627', $2, 'Draft', $3, $4, $5, $6, 'Walk-in',
                   now(), current_date, 'Intrastate', $7)",
    )
    .bind(name)
    // The series number has to be unique per (series, fiscal_year); derive it from the
    // name so callers do not have to thread a counter through.
    .bind(name.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse::<i64>().unwrap_or(1))
    .bind(RESTAURANT)
    .bind(table)
    .bind(ROOM)
    .bind(BRANCH)
    .bind(printed)
    .execute(db.pool())
    .await
    .expect("seed invoice");
}

fn form_for_table(table: &str) -> UryOrderForm {
    UryOrderForm {
        take_away: false,
        restaurant_table: Some(TableName::from(table)),
        customer_name: CustomerName::from("Walk-in"),
        no_of_pax: 2,
        grand_total: Money::ZERO,
        last_invoice: None,
        items: vec![],
        waiter: Some(UserName::from("waiter.a@peacock.test")),
        pos_profile: None,
        cashier: None,
        comments: None,
        modified_time: None,
    }
}

fn line(item: &str, name: &str, qty: i32, rate: rust_decimal::Decimal) -> OrderItem {
    OrderItem {
        item: ItemCode::from(item),
        item_name: name.to_owned(),
        qty,
        rate: Money::new(rate),
        comments: None,
    }
}

// ---------------------------------------------------------------------------
// 0. The migration itself
// ---------------------------------------------------------------------------

#[tokio::test]
async fn migration_007_applies_onto_a_fresh_database() {
    let db = TestDb::new().await;

    // The scratch database was created empty moments ago, so everything below is
    // evidence that 007 applies on top of 001-006 from nothing — Phase 2 gate 5,
    // and the reason this lane could not be written before Lane 2F landed.
    assert!(
        db.db_name().starts_with("peacock_test_"),
        "unexpected scratch database name: {}",
        db.db_name()
    );

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name IN ('orders', 'order_items')
         ORDER BY table_name",
    )
    .fetch_all(db.pool())
    .await
    .expect("list lane 2H tables");
    assert_eq!(tables, vec!["order_items", "orders"]);

    // The Lane 2F dependency, as the catalog sees it: `last_invoice` really does
    // reference `invoices`, and really is SET NULL rather than RESTRICT or CASCADE.
    let (target, delete_rule): (String, String) = sqlx::query_as(
        "SELECT ccu.table_name, rc.delete_rule
         FROM information_schema.table_constraints tc
         JOIN information_schema.constraint_column_usage ccu
              ON ccu.constraint_name = tc.constraint_name
         JOIN information_schema.referential_constraints rc
              ON rc.constraint_name = tc.constraint_name
         WHERE tc.constraint_name = 'orders_last_invoice_fkey'",
    )
    .fetch_one(db.pool())
    .await
    .expect("describe orders_last_invoice_fkey");

    assert_eq!(target, "invoices");
    assert_eq!(delete_rule, "SET NULL");

    // Money is NUMERIC, never float — the rule money.rs and the parity harness rest on.
    let money_types: Vec<String> = sqlx::query_scalar(
        "SELECT data_type FROM information_schema.columns
         WHERE table_schema = 'public'
           AND (table_name, column_name) IN (('orders', 'grand_total'), ('order_items', 'rate'))",
    )
    .fetch_all(db.pool())
    .await
    .expect("describe money columns");
    assert_eq!(money_types, vec!["numeric", "numeric"]);
}

// ---------------------------------------------------------------------------
// 1. CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_and_read_back_a_form_with_its_cart() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    let mut form = form_for_table("T-01");
    form.items = vec![
        line("CURRY", "Chicken Curry", 2, dec!(180.50)),
        line("NAAN", "Butter Naan", 4, dec!(45)),
    ];
    form.grand_total = Money::new(dec!(541.00));
    form.comments = Some("no onions".to_owned());

    let created = repo.create(&form).await.expect("create");

    assert_eq!(created.version, 1, "a fresh form starts at version 1");
    assert_eq!(created.form.items.len(), 2);

    let read = repo
        .get(created.id)
        .await
        .expect("get")
        .expect("form should exist");

    assert_eq!(read.id, created.id);
    assert_eq!(read.form.customer_name.as_str(), "Walk-in");
    assert_eq!(read.form.no_of_pax, 2);
    assert_eq!(read.form.grand_total, Money::new(dec!(541.00)));
    assert_eq!(read.form.comments.as_deref(), Some("no onions"));
    assert!(read.form.last_invoice.is_none(), "no invoice raised yet");

    // Cart order is the waiter's order, so `idx` has to preserve it.
    let codes: Vec<&str> = read.form.items.iter().map(|i| i.item.as_str()).collect();
    assert_eq!(codes, vec!["CURRY", "NAAN"]);
    assert_eq!(read.form.items[0].qty, 2);
    assert_eq!(read.form.items[0].rate, Money::new(dec!(180.50)));
}

#[tokio::test]
async fn update_replaces_the_cart_and_advances_the_version() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    let mut form = form_for_table("T-01");
    form.items = vec![line("CURRY", "Chicken Curry", 1, dec!(180.50))];
    let created = repo.create(&form).await.expect("create");

    // The client sends the whole cart, so an update is a replacement, not a merge.
    let mut edited = created.form.clone();
    edited.items = vec![
        line("NAAN", "Butter Naan", 3, dec!(45)),
        line("CURRY", "Chicken Curry", 2, dec!(180.50)),
    ];
    edited.no_of_pax = 5;
    edited.grand_total = Money::new(dec!(496.00));

    let updated = repo.update(created.id, &edited).await.expect("update");

    assert_eq!(updated.version, 2, "version must advance on write");
    assert_eq!(updated.form.no_of_pax, 5);
    let codes: Vec<&str> = updated.form.items.iter().map(|i| i.item.as_str()).collect();
    assert_eq!(codes, vec!["NAAN", "CURRY"], "new cart order preserved");

    // No leftovers from the replaced cart.
    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM order_items WHERE order_id = $1")
        .bind(updated.id.get())
        .fetch_one(db.pool())
        .await
        .expect("count lines");
    assert_eq!(lines, 2, "the old cart line survived the replacement");
}

#[tokio::test]
async fn delete_takes_the_cart_with_it_and_is_idempotent() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    let mut form = form_for_table("T-01");
    form.items = vec![line("CURRY", "Chicken Curry", 1, dec!(180.50))];
    let created = repo.create(&form).await.expect("create");

    assert!(repo.delete(created.id).await.expect("delete"));

    assert!(
        repo.get(created.id).await.expect("get after delete").is_none(),
        "form survived its own delete"
    );

    // ON DELETE CASCADE on order_items.order_id — Frappe child-table semantics.
    let orphans: i64 = sqlx::query_scalar("SELECT count(*) FROM order_items WHERE order_id = $1")
        .bind(created.id.get())
        .fetch_one(db.pool())
        .await
        .expect("count orphans");
    assert_eq!(orphans, 0, "cart lines outlived their parent");

    // A second delete is a no-op, not an error: settling twice must not 500.
    assert!(!repo.delete(created.id).await.expect("second delete"));
}

#[tokio::test]
async fn one_live_form_per_table_and_get_or_create_reuses_it() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    let first = repo.create(&form_for_table("T-01")).await.expect("create");

    // Two rival carts on one table is the bug the unique partial index prevents.
    let err = repo
        .create(&form_for_table("T-01"))
        .await
        .expect_err("second live form on the same table was accepted");
    match &err {
        StorageError::Constraint { constraint, .. } => {
            assert_eq!(constraint, "orders_one_live_form_per_table_idx");
        }
        other => panic!("expected a unique violation, got {other:?}"),
    }

    // The open-or-reuse path returns the existing row rather than colliding.
    let reused = repo
        .get_or_create_for_table(&TableName::from("T-01"), &form_for_table("T-01"))
        .await
        .expect("get_or_create on an occupied table");
    assert_eq!(reused.id, first.id);

    // A different table is a different form.
    let other_table = repo
        .get_or_create_for_table(&TableName::from("T-02"), &form_for_table("T-02"))
        .await
        .expect("get_or_create on a free table");
    assert_ne!(other_table.id, first.id);

    // Take-away forms carry no table. NULLs are distinct in the partial index, so any
    // number of them coexist.
    for _ in 0..3 {
        let mut takeaway = form_for_table("T-01");
        takeaway.take_away = true;
        takeaway.restaurant_table = None;
        repo.create(&takeaway).await.expect("take-away form");
    }
    assert_eq!(repo.list_take_away().await.expect("list").len(), 3);
}

#[tokio::test]
async fn list_queries_batch_the_cart_read() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    let waiter = UserName::from("waiter.a@peacock.test");

    let mut t1 = form_for_table("T-01");
    t1.items = vec![line("CURRY", "Chicken Curry", 1, dec!(180.50))];
    repo.create(&t1).await.expect("create T-01");

    let mut t2 = form_for_table("T-02");
    t2.items = vec![
        line("NAAN", "Butter Naan", 2, dec!(45)),
        line("CURRY", "Chicken Curry", 1, dec!(180.50)),
    ];
    repo.create(&t2).await.expect("create T-02");

    // A form belonging to someone else must not appear.
    let mut other = form_for_table("T-01");
    other.take_away = true;
    other.restaurant_table = None;
    other.waiter = Some(UserName::from("waiter.b@peacock.test"));
    repo.create(&other).await.expect("create other waiter form");

    let mine = repo.list_for_waiter(&waiter).await.expect("list_for_waiter");
    assert_eq!(mine.len(), 2);

    // Each form keeps its own lines: the batch read must not cross-contaminate.
    let counts: Vec<usize> = mine.iter().map(|o| o.form.items.len()).collect();
    assert_eq!(counts, vec![1, 2]);
    assert_eq!(mine[1].form.items[0].item.as_str(), "NAAN");
}

#[tokio::test]
async fn form_level_constraints_reject_impossible_rows() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    // Neither a table nor the take-away flag: unreachable in the UI, so it can only be
    // a bug (`orders_has_a_binding`).
    let mut unbound = form_for_table("T-01");
    unbound.restaurant_table = None;
    let err = repo
        .create(&unbound)
        .await
        .expect_err("a form bound to nothing was accepted");
    assert!(
        matches!(&err, StorageError::Constraint { constraint, .. } if constraint == "orders_has_a_binding"),
        "unexpected error: {err:?}"
    );

    // no_of_pax is `reqd: 1` upstream; zero pax is not a covered table.
    let mut no_pax = form_for_table("T-01");
    no_pax.no_of_pax = 0;
    let err = repo
        .create(&no_pax)
        .await
        .expect_err("zero pax was accepted");
    assert!(
        matches!(&err, StorageError::Constraint { constraint, .. } if constraint == "orders_no_of_pax_positive"),
        "unexpected error: {err:?}"
    );

    // The table FK is real: a form cannot bind to a table that does not exist.
    let err = repo
        .create(&form_for_table("T-404"))
        .await
        .expect_err("FK to a missing table was accepted");
    assert!(
        matches!(&err, StorageError::Constraint { constraint, .. } if constraint == "orders_restaurant_table_fkey"),
        "unexpected error: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. last_invoice — the Lane 2F relationship
// ---------------------------------------------------------------------------

#[tokio::test]
async fn last_invoice_fk_is_enforced() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    let created = repo.create(&form_for_table("T-01")).await.expect("create");

    // An invoice number the ledger has never issued must not be storable: the whole
    // point of the pointer is that it resolves.
    let err = repo
        .set_last_invoice(created.id, Some(&InvoiceName::from("POS-2627-999999")))
        .await
        .expect_err("dangling last_invoice was accepted");
    match &err {
        StorageError::Constraint { constraint, .. } => {
            assert_eq!(constraint, "orders_last_invoice_fkey");
        }
        other => panic!("expected a foreign key violation, got {other:?}"),
    }

    // The same rejection has to hold on the create path, not only on the setter.
    let mut with_bad_invoice = form_for_table("T-02");
    with_bad_invoice.last_invoice = Some(InvoiceName::from("POS-2627-999998"));
    let err = repo
        .create(&with_bad_invoice)
        .await
        .expect_err("create with a dangling last_invoice was accepted");
    assert!(
        matches!(&err, StorageError::Constraint { constraint, .. } if constraint == "orders_last_invoice_fkey"),
        "unexpected error: {err:?}"
    );

    // A real invoice is accepted and round-trips.
    seed_invoice(&db, "POS-2627-000001", Some("T-01"), false).await;
    let linked = repo
        .set_last_invoice(created.id, Some(&InvoiceName::from("POS-2627-000001")))
        .await
        .expect("link a real invoice");
    assert_eq!(
        linked.form.last_invoice.as_ref().map(|i| i.as_str()),
        Some("POS-2627-000001")
    );
    assert_eq!(linked.version, 2, "linking is a write and bumps version");

    // Clearing it is how a settled form is released.
    let cleared = repo
        .set_last_invoice(created.id, None)
        .await
        .expect("clear last_invoice");
    assert!(cleared.form.last_invoice.is_none());
}

#[tokio::test]
async fn deleting_an_invoice_nulls_the_pointer_instead_of_being_blocked() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    seed_invoice(&db, "POS-2627-000002", Some("T-01"), false).await;
    let created = repo.create(&form_for_table("T-01")).await.expect("create");
    repo.set_last_invoice(created.id, Some(&InvoiceName::from("POS-2627-000002")))
        .await
        .expect("link invoice");

    // ON DELETE SET NULL, not RESTRICT: a transient UI form must never be able to pin a
    // row in the ledger. The invoice is the senior record; the pointer is a convenience.
    sqlx::query("DELETE FROM invoices WHERE name = $1")
        .bind("POS-2627-000002")
        .execute(db.pool())
        .await
        .expect("a stale order form blocked an invoice delete");

    let after = repo
        .get(created.id)
        .await
        .expect("get")
        .expect("form should still exist");

    assert!(
        after.form.last_invoice.is_none(),
        "expected SET NULL, found {:?}",
        after.form.last_invoice
    );
    // The form itself survives — losing the link costs a UI convenience, not the cart.
    assert_eq!(
        after.form.restaurant_table.as_ref().map(|t| t.as_str()),
        Some("T-01")
    );
}

// ---------------------------------------------------------------------------
// 3. count_separate_active — the merge guard, against `invoices`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn count_separate_active_counts_unprinted_draft_invoices() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    let tables = vec![TableName::from("T-01"), TableName::from("T-02")];

    assert_eq!(
        repo.count_separate_active_async(&tables).await.expect("count"),
        0,
        "no invoices at all means no active orders"
    );

    // An open form with no invoice is NOT an active order: a waiter can browse the menu
    // and walk away, and that must not block a merge (ury_order.py:223-233 probes POS
    // Invoice, not the form).
    repo.create(&form_for_table("T-01")).await.expect("create");
    assert_eq!(
        repo.count_separate_active_async(&tables).await.expect("count"),
        0,
        "an order form without an invoice was counted as active"
    );

    // An unprinted Draft invoice is active.
    seed_invoice(&db, "POS-2627-000011", Some("T-01"), false).await;
    assert_eq!(
        repo.count_separate_active_async(&tables).await.expect("count"),
        1
    );

    // Two unprinted drafts on the *same* table are still one active table — the guard
    // counts separate orders, and `DISTINCT restaurant_table` is what makes that true.
    seed_invoice(&db, "POS-2627-000012", Some("T-01"), false).await;
    assert_eq!(
        repo.count_separate_active_async(&tables).await.expect("count"),
        1,
        "two drafts on one table counted twice"
    );

    // A printed invoice is no longer active: it is a bill awaiting payment, and merging
    // is what upstream allows after printing.
    seed_invoice(&db, "POS-2627-000013", Some("T-02"), true).await;
    assert_eq!(
        repo.count_separate_active_async(&tables).await.expect("count"),
        1,
        "a printed invoice was counted as active"
    );

    // A second table with its own unprinted draft is the case the merge guard rejects
    // (Error::MultipleActiveOrders).
    seed_invoice(&db, "POS-2627-000014", Some("T-02"), false).await;
    assert_eq!(
        repo.count_separate_active_async(&tables).await.expect("count"),
        2
    );

    // An empty member set is 0 without a query.
    assert_eq!(
        repo.count_separate_active_async(&[]).await.expect("count"),
        0
    );
}

/// `multi_thread` is required, not cosmetic. The sync port method bridges to async with
/// `block_in_place` + `Handle::block_on`, which tokio only supports on the
/// multi-threaded runtime — the flavor `peacock-api` actually runs. On the
/// single-threaded test runtime the blocking call would have nothing left to drive the
/// reactor and would hang instead of failing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sync_port_method_matches_the_async_one() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    seed_invoice(&db, "POS-2627-000021", Some("T-01"), false).await;
    seed_invoice(&db, "POS-2627-000022", Some("T-02"), false).await;

    let tables = vec![TableName::from("T-01"), TableName::from("T-02")];

    // `merge.rs` calls the trait, not the async inherent method, so the blocking bridge
    // is the path that actually matters.
    let repo_for_blocking = repo.clone();
    let tables_for_blocking = tables.clone();
    let via_trait = tokio::task::spawn_blocking(move || {
        OrderRepo::count_separate_active(&repo_for_blocking, &tables_for_blocking)
    })
    .await
    .expect("blocking task panicked")
    .expect("count_separate_active");

    assert_eq!(via_trait, 2);
    assert_eq!(
        via_trait,
        repo.count_separate_active_async(&tables).await.expect("async count")
    );
}

// ---------------------------------------------------------------------------
// 4. Row-level locking
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_waiters_updating_the_same_form_serialise() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = Arc::new(PgOrderRepo::new(db.storage.clone()));

    let created = repo.create(&form_for_table("T-01")).await.expect("create");

    // Waiter A takes the row lock by hand and holds it, standing in for a slow
    // read-modify-write. Waiter B goes through the repository and must block.
    let mut holder = db.storage.begin().await.expect("begin holder tx");
    let locked: i64 = sqlx::query_scalar("SELECT version FROM orders WHERE id = $1 FOR UPDATE")
        .bind(created.id.get())
        .fetch_one(&mut *holder)
        .await
        .expect("take the lock");
    assert_eq!(locked, 1);

    let hold_for = Duration::from_millis(600);
    let started = Instant::now();

    let waiter_b = {
        let repo = Arc::clone(&repo);
        let id = created.id;
        let mut edit = created.form.clone();
        edit.no_of_pax = 8;
        edit.items = vec![line("CURRY", "Chicken Curry", 3, dec!(180.50))];
        tokio::spawn(async move { repo.update(id, &edit).await })
    };

    // Give B a moment to reach the lock, then confirm it is genuinely stuck: nothing it
    // wrote can be visible while A holds the row.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let seen_by_a_third_party: i32 =
        sqlx::query_scalar("SELECT no_of_pax FROM orders WHERE id = $1")
            .bind(created.id.get())
            .fetch_one(db.pool())
            .await
            .expect("read while locked");
    assert_eq!(
        seen_by_a_third_party, 2,
        "waiter B's write landed while the row was locked"
    );
    assert!(!waiter_b.is_finished(), "waiter B did not block on the lock");

    // A finishes.
    sqlx::query("UPDATE orders SET comments = 'waiter A was here', version = version + 1 WHERE id = $1")
        .bind(created.id.get())
        .execute(&mut *holder)
        .await
        .expect("waiter A writes");
    tokio::time::sleep(hold_for.saturating_sub(started.elapsed())).await;
    holder.commit().await.expect("commit holder tx");

    // Now B proceeds, and only now.
    let b_result = waiter_b.await.expect("waiter B panicked").expect("waiter B update");
    assert!(
        started.elapsed() >= hold_for,
        "waiter B committed after {:?}, before the lock was released",
        started.elapsed()
    );

    // B read under the lock, so it saw A's version (2) and produced 3 — not 2, which is
    // what an interleaved write would leave.
    assert_eq!(
        b_result.version, 3,
        "version did not account for both writes"
    );
    assert_eq!(b_result.form.no_of_pax, 8);
    assert_eq!(b_result.form.items.len(), 1);

    // And here is the limit of what a row lock buys, stated rather than glossed over:
    // `update` is a whole-form replace (the client sends the entire cart), so B's
    // snapshot — taken before A wrote — puts `comments` back to NULL. A's field is gone.
    //
    // The lock guarantees serialisation and an intact row. It does NOT merge fields, and
    // no lock can: B's payload simply does not contain A's comment. A caller that cannot
    // accept last-writer-wins must use `update_if_version`, which turns exactly this
    // situation into Error::Conflict — see
    // `update_if_version_turns_a_lost_update_into_a_conflict`.
    assert_eq!(
        b_result.form.comments, None,
        "expected whole-form replace semantics"
    );
}

#[tokio::test]
async fn parallel_updates_all_land_and_leave_a_consistent_row() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = Arc::new(PgOrderRepo::new(db.storage.clone()));

    let created = repo.create(&form_for_table("T-01")).await.expect("create");

    // Eight waiters, same form, no coordination. Every update must land, and the row
    // that remains has to be exactly one of them — never a blend.
    let mut handles = Vec::new();
    for pax in 1..=8 {
        let repo = Arc::clone(&repo);
        let id = created.id;
        let mut edit = created.form.clone();
        edit.no_of_pax = pax;
        edit.comments = Some(format!("waiter-{pax}"));
        edit.grand_total = Money::new(dec!(100) * rust_decimal::Decimal::from(pax));
        edit.items = (0..pax)
            .map(|_| line("CURRY", "Chicken Curry", 1, dec!(180.50)))
            .collect();
        handles.push(tokio::spawn(async move { repo.update(id, &edit).await }));
    }

    let mut versions = Vec::new();
    for h in handles {
        let stored = h
            .await
            .expect("task panicked")
            .expect("a concurrent update failed instead of blocking");
        versions.push(stored.version);
    }

    // Serialised, so the versions are 2..=9 with no repeats: a repeat would mean two
    // writers read the same version, which is the lost update the lock prevents.
    versions.sort_unstable();
    assert_eq!(
        versions,
        (2..=9).collect::<Vec<i64>>(),
        "versions overlapped, so a write was lost"
    );

    let final_state = repo
        .get(created.id)
        .await
        .expect("get")
        .expect("form should exist");

    assert_eq!(final_state.version, 9);

    // The winner's fields all come from the same writer. A torn row would pair one
    // waiter's pax count with another's cart.
    let winner = final_state.form.comments.as_deref().expect("comments");
    let pax: i32 = winner
        .strip_prefix("waiter-")
        .expect("comment marker")
        .parse()
        .expect("pax from marker");
    assert_eq!(final_state.form.no_of_pax, pax, "row is torn across writers");
    assert_eq!(
        final_state.form.items.len(),
        pax as usize,
        "cart does not match the winning writer"
    );
    assert_eq!(
        final_state.form.grand_total,
        Money::new(dec!(100) * rust_decimal::Decimal::from(pax)),
        "total does not match the winning writer"
    );
}

#[tokio::test]
async fn update_if_version_turns_a_lost_update_into_a_conflict() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    let created = repo.create(&form_for_table("T-01")).await.expect("create");

    // Waiter A's tablet read version 1 and then went to sleep.
    let stale = created.form.clone();

    // Waiter B updates in the meantime.
    let mut b_edit = created.form.clone();
    b_edit.no_of_pax = 6;
    let after_b = repo
        .update(created.id, &b_edit)
        .await
        .expect("waiter B update");
    assert_eq!(after_b.version, 2);

    // A wakes up and writes with the version it remembers. `update` would win the race
    // it should have lost; `update_if_version` refuses.
    let err = repo
        .update_if_version(created.id, created.version, &stale)
        .await
        .expect_err("stale write was accepted");

    match &err {
        StorageError::Domain(peacock_core::error::Error::Conflict { expected, actual }) => {
            assert_eq!(expected, "1");
            assert_eq!(actual, "2");
        }
        other => panic!("expected a domain Conflict, got {other:?}"),
    }

    // The refusal is total: B's value is untouched.
    let current = repo
        .get(created.id)
        .await
        .expect("get")
        .expect("form should exist");
    assert_eq!(current.form.no_of_pax, 6);
    assert_eq!(current.version, 2, "the rejected write still bumped version");

    // With the right version it goes through.
    let ok = repo
        .update_if_version(created.id, current.version, &stale)
        .await
        .expect("update with the current version");
    assert_eq!(ok.version, 3);
    assert_eq!(ok.form.no_of_pax, 2);
}

#[tokio::test]
async fn concurrent_first_open_of_one_table_yields_a_single_form() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = Arc::new(PgOrderRepo::new(db.storage.clone()));

    // Six tablets hitting "open T-01" at once, with no form there yet. `FOR UPDATE`
    // locks nothing when it matches no row, so the unique partial index is the backstop:
    // at most one winner, and the losers see either the winner's row or a 23505.
    let mut handles = Vec::new();
    for _ in 0..6 {
        let repo = Arc::clone(&repo);
        handles.push(tokio::spawn(async move {
            repo.get_or_create_for_table(&TableName::from("T-01"), &form_for_table("T-01"))
                .await
        }));
    }

    let mut ids = Vec::new();
    let mut rejections = 0;
    for h in handles {
        match h.await.expect("task panicked") {
            Ok(stored) => ids.push(stored.id),
            Err(StorageError::Constraint { constraint, .. }) => {
                assert_eq!(constraint, "orders_one_live_form_per_table_idx");
                rejections += 1;
            }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    assert!(!ids.is_empty(), "every opener failed");
    assert_eq!(
        ids.len() + rejections,
        6,
        "an opener neither succeeded nor was rejected"
    );

    // Whatever the split, exactly one row exists and every success points at it.
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM orders WHERE restaurant_table = $1")
        .bind("T-01")
        .fetch_one(db.pool())
        .await
        .expect("count forms");
    assert_eq!(rows, 1, "the table ended up with {rows} rival carts");

    let unique: std::collections::BTreeSet<i64> = ids.iter().map(|i| i.get()).collect();
    assert_eq!(unique.len(), 1, "successes disagreed on the form id: {ids:?}");
}

#[tokio::test]
async fn updating_a_form_that_is_gone_is_an_error_not_a_silent_insert() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgOrderRepo::new(db.storage.clone());

    let created = repo.create(&form_for_table("T-01")).await.expect("create");
    let form = created.form.clone();
    repo.delete(created.id).await.expect("delete");

    // The lock query finds no row, so there is nothing to update — and a waiter whose
    // form was settled underneath them has to be told, not handed a fresh row.
    let err = repo
        .update(created.id, &form)
        .await
        .expect_err("update on a deleted form was accepted");
    assert!(
        matches!(&err, StorageError::Constraint { constraint, .. } if constraint == "not_found"),
        "unexpected error: {err:?}"
    );

    let err = repo
        .set_last_invoice(OrderId(999_999), None)
        .await
        .expect_err("set_last_invoice on a missing form was accepted");
    assert!(
        matches!(&err, StorageError::Constraint { constraint, .. } if constraint == "not_found"),
        "unexpected error: {err:?}"
    );

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM orders")
        .fetch_one(db.pool())
        .await
        .expect("count forms");
    assert_eq!(rows, 0, "a failed update inserted a row");
}
