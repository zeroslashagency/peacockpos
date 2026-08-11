//! Lane 2F acceptance tests — the invoice repository. **Money lane.**
//!
//! Each test gets a freshly migrated scratch database (`support::TestDb`).
//!
//! What these prove, in order of how much they matter:
//!
//! 1. **Gaplessness under concurrency** (CGST Rule 46(b)). 100 parallel creates
//!    produce 1..=100 with no gaps and no duplicates, and a rolled-back create burns
//!    no number.
//! 2. **Idempotency.** The same key replayed returns the same invoice and never moves
//!    the counter — including when the replays are concurrent.
//! 3. **Money survives storage.** The tax fixtures the parity harness validates are
//!    written and read back at full `Decimal` precision, and the schema refuses totals
//!    that contradict the domain's arithmetic.
//! 4. **Status transitions.** Only legal edges, enforced in the repository *and* by a
//!    trigger no code path can bypass.

mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use peacock_core::ids::{BranchName, InvoiceName, ItemCode, TableName};
use peacock_core::invoicing::{
    allocate_invoice_number, fiscal_year_code, MAX_INVOICE_NAME_LEN,
};
use peacock_core::model::PosInvoiceStatus;
use peacock_core::money::Money;
use peacock_core::tax::{compute_totals, DiscountBasis, InvoiceLine, SupplyType};
use peacock_storage::error::StorageError;
use peacock_storage::repos::invoice::{
    CreateOutcome, NewInvoice, NewInvoiceLine, PgInvoiceRepo, TxIdempotencyStore,
    TxSeriesAllocator,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::types::chrono::{NaiveDate, TimeZone, Utc};
use support::TestDb;
use uuid::Uuid;

const BRANCH: &str = "Peacock HQ";
const RESTAURANT: &str = "Peacock Grand";
const ROOM: &str = "Main Hall";
const SERIES: &str = "POS";
const FY: &str = "2627";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Restaurant, room, one table and the two items the sample lines reference.
async fn seed(db: &TestDb) {
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
    .expect("seed table");

    for (code, name) in [("CHAI", "Masala Chai"), ("NAAN", "Butter Naan")] {
        sqlx::query("INSERT INTO items (code, name, item_group) VALUES ($1, $2, 'Food')")
            .bind(code)
            .bind(name)
            .execute(db.pool())
            .await
            .expect("seed item");
    }
}

async fn repo(db: &TestDb) -> PgInvoiceRepo {
    let repo = PgInvoiceRepo::new(db.storage.clone());
    repo.register_series(SERIES, FY, 1)
        .await
        .expect("register series");
    repo
}

fn line(item: &str, qty: Decimal, rate: Decimal) -> NewInvoiceLine {
    NewInvoiceLine {
        item_code: ItemCode::from(item),
        item_name: format!("{item} item"),
        qty,
        rate: Money::new(rate),
        hsn_sac: Some("996331".to_owned()),
        course: None,
        comments: None,
        serve_priority: 0,
        indicate_course: false,
    }
}

/// The worked example from RUST_MIGRATION_PLAN_V2.md §5, which is also parity fixture
/// `01_tax_worked_example`: 4 × ₹100, 5% GST, ₹40 discount, intrastate.
/// Net 400, taxable 360, tax 18 (CGST 9 / SGST 9), grand 378.
fn worked_example() -> NewInvoice {
    invoice_with(
        vec![line("CHAI", dec!(4), dec!(100))],
        Money::new(dec!(40)),
        dec!(0.05),
        SupplyType::Intrastate,
        DiscountBasis::NetTotal,
    )
}

fn invoice_with(
    lines: Vec<NewInvoiceLine>,
    discount: Money,
    tax_rate: Decimal,
    supply_type: SupplyType,
    discount_basis: DiscountBasis,
) -> NewInvoice {
    // Totals come from the domain, never from the test. `compute_totals` is what the
    // parity harness validates against the Python oracle; recomputing them by hand here
    // would be a second, unvalidated implementation.
    let tax_lines: Vec<InvoiceLine> = lines
        .iter()
        .map(|l| InvoiceLine {
            item_name: l.item_name.clone(),
            quantity: l.qty,
            rate: l.rate,
            hsn_sac: l.hsn_sac.clone(),
        })
        .collect();

    let totals = compute_totals(&tax_lines, discount, tax_rate, supply_type, discount_basis)
        .expect("domain totals");

    NewInvoice {
        naming_series: SERIES.to_owned(),
        fiscal_year: FY.to_owned(),
        restaurant: Some(RESTAURANT.to_owned()),
        restaurant_table: Some(TableName::from("T-01")),
        restaurant_room: Some(ROOM.to_owned()),
        branch: BranchName::from(BRANCH),
        pos_profile: Some("Peacock POS".to_owned()),
        customer: "Walk-in".to_owned(),
        waiter: Some("waiter@peacock.test".to_owned()),
        cashier: Some("cashier@peacock.test".to_owned()),
        no_of_pax: 2,
        order_type: Some("Dine In".to_owned()),
        posted_at: Utc.with_ymd_and_hms(2026, 7, 15, 13, 30, 0).unwrap(),
        business_day: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        supply_type,
        discount_basis,
        tax_rate,
        paid_amount: totals.rounded_total,
        totals,
        change_amount: Money::ZERO,
        comments: None,
        lines,
    }
}

// ===========================================================================
// 1. Gapless numbering — CGST Rule 46(b)
// ===========================================================================

#[tokio::test]
async fn sequential_creates_produce_a_gapless_series() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let mut names = Vec::new();
    for _ in 0..10 {
        let created = repo
            .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
            .await
            .expect("create invoice");
        assert_eq!(created.outcome, CreateOutcome::Created);
        names.push(created.invoice.name.as_str().to_owned());
    }

    assert_eq!(names[0], "POS-2627-000001");
    assert_eq!(names[9], "POS-2627-000010");

    assert_eq!(
        repo.issued_numbers(SERIES, FY).await.unwrap(),
        (1..=10).collect::<Vec<u64>>()
    );
    assert!(
        repo.find_series_gaps(SERIES, FY).await.unwrap().is_empty(),
        "a sequential run produced gaps"
    );
}

#[tokio::test]
async fn hundred_concurrent_creates_are_gapless_and_unique() {
    // The headline requirement. 100 parallel creates, one series, no gaps, no
    // duplicates. A `nextval()` implementation passes this too — until something
    // rolls back, which is what the next test covers.
    let db = TestDb::with_config(|c| {
        c.with_max_connections(16)
            .with_min_connections(4)
            .with_acquire_timeout(Duration::from_secs(30))
    })
    .await;
    seed(&db).await;
    let repo = repo(&db).await;

    const N: usize = 100;
    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let repo = repo.clone();
        handles.push(tokio::spawn(async move {
            repo.create_invoice_idempotent(Uuid::new_v4(), &worked_example())
                .await
                .map(|c| c.invoice.series_number)
        }));
    }

    let mut numbers = Vec::with_capacity(N);
    for h in handles {
        numbers.push(h.await.expect("task panicked").expect("create failed"));
    }

    numbers.sort_unstable();
    assert_eq!(
        numbers,
        (1..=N as u64).collect::<Vec<u64>>(),
        "concurrent allocation did not produce exactly 1..={N}"
    );

    let unique: BTreeSet<u64> = numbers.iter().copied().collect();
    assert_eq!(unique.len(), N, "duplicate invoice numbers issued");

    assert!(
        repo.find_series_gaps(SERIES, FY).await.unwrap().is_empty(),
        "100 concurrent creates gapped the series"
    );
    assert_eq!(repo.peek_series(SERIES, FY).await.unwrap(), Some(101));

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, N as i64);
}

#[tokio::test]
async fn a_failed_insert_does_not_burn_a_number() {
    // This is why the counter is a row and not a sequence. `nextval()` is exempt from
    // rollback, so under a sequence the failed attempt below would consume 1 and the
    // next good invoice would be 000002 — a permanent gap Rule 46(b) forbids.
    // Pinned in the domain by `invoicing.rs::rolled_back_allocation_does_not_burn_number`.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    // FK violation: no such item. Fails after the counter has been incremented.
    let mut doomed = worked_example();
    doomed.lines = vec![line("NO-SUCH-ITEM", dec!(1), dec!(100))];

    let err = repo
        .create_invoice_idempotent(Uuid::new_v4(), &doomed)
        .await
        .expect_err("insert with a bad item FK should fail");
    assert!(
        matches!(err, StorageError::Constraint { .. }),
        "expected a constraint error, got {err:?}"
    );

    // The counter is back where it started.
    assert_eq!(repo.peek_series(SERIES, FY).await.unwrap(), Some(1));

    let good = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .expect("create after rollback");
    assert_eq!(
        good.invoice.name.as_str(),
        "POS-2627-000001",
        "the rolled-back attempt burned a number"
    );
    assert!(repo.find_series_gaps(SERIES, FY).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_over_long_series_is_rejected_without_burning_a_number() {
    // The Rule 46(b) length guard fires after the counter moves but inside the
    // transaction, so the rejection must leave the counter untouched. Mirrors
    // `invoicing.rs::over_long_series_does_not_burn_a_number`.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgInvoiceRepo::new(db.storage.clone());
    repo.register_series("TOOLONG", FY, 7).await.unwrap();

    let mut over_long = worked_example();
    over_long.naming_series = "TOOLONG".to_owned();

    let err = repo
        .create_invoice_idempotent(Uuid::new_v4(), &over_long)
        .await
        .expect_err("a 19-character name should be refused");
    assert!(
        matches!(
            err,
            StorageError::Domain(peacock_core::Error::InvoiceNameTooLong { .. })
        ),
        "expected InvoiceNameTooLong, got {err:?}"
    );

    assert_eq!(
        repo.peek_series("TOOLONG", FY).await.unwrap(),
        Some(7),
        "counter advanced despite a rejected allocation"
    );
}

#[tokio::test]
async fn an_unregistered_series_reports_the_domain_error() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgInvoiceRepo::new(db.storage.clone()); // no register_series

    let err = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .expect_err("unregistered series should fail");

    match err {
        StorageError::Domain(peacock_core::Error::SeriesNotConfigured(s, fy)) => {
            assert_eq!(s, SERIES);
            assert_eq!(fy, FY);
        }
        other => panic!("expected SeriesNotConfigured, got {other:?}"),
    }
}

#[tokio::test]
async fn the_series_number_cannot_be_reused_under_a_different_name() {
    // `name` being the PK stops a duplicate string. This stops the subtler forgery:
    // the same counter value issued twice under differently formatted names.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    repo.create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .expect("first invoice");

    let err = sqlx::query(
        "INSERT INTO invoices
             (name, naming_series, fiscal_year, series_number, branch, customer,
              posted_at, business_day, supply_type)
         VALUES ('POS-2627-00001', $1, $2, 1, $3, 'Walk-in', now(), CURRENT_DATE, 'Intrastate')",
    )
    .bind(SERIES)
    .bind(FY)
    .bind(BRANCH)
    .execute(db.pool())
    .await
    .expect_err("reusing series_number 1 should be refused");

    assert_eq!(
        err.as_database_error().and_then(|e| e.constraint()),
        Some("invoices_series_number_unique_idx")
    );
}

#[tokio::test]
async fn a_name_over_sixteen_characters_cannot_be_stored_at_all() {
    // The domain guard is the first line of defence; this is the schema backstop for
    // any write that does not come through the repository.
    let db = TestDb::new().await;
    seed(&db).await;

    let err = sqlx::query(
        "INSERT INTO invoices
             (name, naming_series, fiscal_year, series_number, branch, customer,
              posted_at, business_day, supply_type)
         VALUES ('WAY-TOO-LONG-2627-000001', 'WAY', $1, 1, $2, 'Walk-in',
                 now(), CURRENT_DATE, 'Intrastate')",
    )
    .bind(FY)
    .bind(BRANCH)
    .execute(db.pool())
    .await
    .expect_err("a 24-character invoice name should be refused");

    assert_eq!(
        err.as_database_error().and_then(|e| e.constraint()),
        Some("invoices_name_within_rule_46b")
    );
}

// ===========================================================================
// 2. Idempotency
// ===========================================================================

#[tokio::test]
async fn the_same_key_ten_times_yields_one_invoice() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let key = Uuid::new_v4();
    let first = repo
        .create_invoice_idempotent(key, &worked_example())
        .await
        .expect("first create");
    assert_eq!(first.outcome, CreateOutcome::Created);

    for attempt in 0..10 {
        let replay = repo
            .create_invoice_idempotent(key, &worked_example())
            .await
            .unwrap_or_else(|e| panic!("replay {attempt} failed: {e}"));
        assert_eq!(replay.outcome, CreateOutcome::Replayed);
        assert_eq!(replay.invoice.name, first.invoice.name);
        assert_eq!(replay.invoice.series_number, first.invoice.series_number);
        // Not just the number: the whole document, lines included.
        assert_eq!(replay.invoice, first.invoice);
    }

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "replays created extra invoices");

    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM invoice_lines")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(lines, 1, "replays duplicated invoice lines");

    // The counter moved exactly once. This is the invariant that keeps a retried
    // submit from gapping the series.
    assert_eq!(repo.peek_series(SERIES, FY).await.unwrap(), Some(2));

    // A fresh key gets the next number, not the 12th.
    let next = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .expect("fresh key");
    assert_eq!(next.invoice.name.as_str(), "POS-2627-000002");
}

#[tokio::test]
async fn concurrent_replays_of_one_key_still_yield_one_invoice() {
    // The race the retry exists for: 20 requests carrying the same key, launched at
    // once. Several miss the lookup, all but one lose the insert on
    // `idempotency_keys_pkey`, and each loser's rollback restores the counter.
    let db = TestDb::with_config(|c| {
        c.with_max_connections(12)
            .with_acquire_timeout(Duration::from_secs(30))
    })
    .await;
    seed(&db).await;
    let repo = repo(&db).await;

    let key = Uuid::new_v4();
    let mut handles = Vec::new();
    for _ in 0..20 {
        let repo = repo.clone();
        handles.push(tokio::spawn(async move {
            repo.create_invoice_idempotent(key, &worked_example()).await
        }));
    }

    let mut names = BTreeSet::new();
    let mut created = 0;
    for h in handles {
        let res = h.await.expect("task panicked").expect("create failed");
        if res.outcome == CreateOutcome::Created {
            created += 1;
        }
        names.insert(res.invoice.name.as_str().to_owned());
    }

    assert_eq!(created, 1, "more than one caller believed it created");
    assert_eq!(names.len(), 1, "callers disagreed on the invoice: {names:?}");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);

    // Every loser rolled back, so exactly one number was consumed.
    assert_eq!(repo.peek_series(SERIES, FY).await.unwrap(), Some(2));
    assert!(repo.find_series_gaps(SERIES, FY).await.unwrap().is_empty());
}

#[tokio::test]
async fn different_keys_get_different_invoices() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let a = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap();
    let b = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap();

    assert_ne!(a.invoice.name, b.invoice.name);
    assert_eq!(a.invoice.series_number + 1, b.invoice.series_number);
}

#[tokio::test]
async fn an_idempotency_key_resolves_to_its_invoice() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let key = Uuid::new_v4();
    let created = repo
        .create_invoice_idempotent(key, &worked_example())
        .await
        .unwrap();

    assert_eq!(
        repo.lookup_idempotency_key(key).await.unwrap(),
        Some(created.invoice.name.clone())
    );
    assert_eq!(
        repo.lookup_idempotency_key(Uuid::new_v4()).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn idempotency_keys_expire_after_twenty_four_hours() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let key = Uuid::new_v4();
    repo.create_invoice_idempotent(key, &worked_example())
        .await
        .unwrap();

    let (created_at, expires_at): (
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
    ) = sqlx::query_as("SELECT created_at, expires_at FROM idempotency_keys WHERE key = $1")
        .bind(key)
        .fetch_one(db.pool())
        .await
        .expect("read key row");

    assert_eq!(
        (expires_at - created_at).num_hours(),
        24,
        "default expiry is not 24 hours"
    );

    // Expiry is advisory: nothing purges on a timer, so an unexpired key survives.
    assert_eq!(repo.purge_expired_idempotency_keys().await.unwrap(), 0);
    assert!(repo.lookup_idempotency_key(key).await.unwrap().is_some());

    // Age the row into the past to simulate a key written 25 hours ago. Both
    // timestamps have to move: `expires_at > created_at` is a CHECK, so pulling only
    // `expires_at` back would violate it rather than expire the key.
    sqlx::query(
        "UPDATE idempotency_keys
            SET created_at = now() - INTERVAL '25 hours',
                expires_at = now() - INTERVAL '1 hour'",
    )
    .execute(db.pool())
    .await
    .unwrap();

    assert_eq!(repo.purge_expired_idempotency_keys().await.unwrap(), 1);
    assert!(repo.lookup_idempotency_key(key).await.unwrap().is_none());

    let invoices: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(invoices, 1, "purging a key deleted the invoice");

    // A replay after purge writes a NEW invoice. That is a duplicate, not a gap, so
    // Rule 46(b) still holds — the documented consequence of purging.
    let after = repo
        .create_invoice_idempotent(key, &worked_example())
        .await
        .unwrap();
    assert_eq!(after.outcome, CreateOutcome::Created);
    assert_eq!(after.invoice.name.as_str(), "POS-2627-000002");
    assert!(repo.find_series_gaps(SERIES, FY).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_idempotency_key_cannot_point_at_a_missing_invoice() {
    let db = TestDb::new().await;
    seed(&db).await;

    let err = sqlx::query("INSERT INTO idempotency_keys (key, invoice) VALUES ($1, 'POS-2627-000099')")
        .bind(Uuid::new_v4())
        .execute(db.pool())
        .await
        .expect_err("a key for a nonexistent invoice should be refused");

    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23503"),
        "the key -> invoice link is not enforced"
    );
}

// ===========================================================================
// 3. Money: storage must not move a paisa
// ===========================================================================

#[tokio::test]
async fn the_worked_example_round_trips_to_the_paisa() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let input = worked_example();
    let created = repo
        .create_invoice_idempotent(Uuid::new_v4(), &input)
        .await
        .unwrap();

    // Parity fixture 01_tax_worked_example, straight out of the database.
    let t = &created.invoice.totals;
    assert_eq!(t.net_total, Money::new(dec!(400)));
    assert_eq!(t.discount, Money::new(dec!(40)));
    assert_eq!(t.taxable_value, Money::new(dec!(360)));
    assert_eq!(t.tax.total_tax, Money::new(dec!(18)));
    assert_eq!(t.tax.cgst, Money::new(dec!(9)));
    assert_eq!(t.tax.sgst, Money::new(dec!(9)));
    assert_eq!(t.tax.igst, Money::ZERO);
    assert_eq!(t.grand_total, Money::new(dec!(378)));
    assert_eq!(t.rounded_total, Money::new(dec!(378)));
    assert_eq!(t.round_off, Money::ZERO);

    // Read back through a separate call: identical, not merely equal-looking.
    let fetched = repo.get(&created.invoice.name).await.unwrap();
    assert_eq!(fetched, created.invoice);
    assert_eq!(fetched.totals, input.totals);
}

#[tokio::test]
async fn the_odd_paisa_cgst_split_survives_storage() {
    // Parity fixture 02_tax_intrastate_odd_paisa: tax 18.01, CGST 9.005 rounds to 9.01,
    // SGST = 18.01 - 9.01 = 9.00. The pair must still sum to total_tax after a round
    // trip — a paisa lost in storage is a paisa the harness cannot see.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let input = invoice_with(
        vec![line("CHAI", dec!(1), dec!(360.2))],
        Money::ZERO,
        dec!(0.05),
        SupplyType::Intrastate,
        DiscountBasis::NetTotal,
    );

    let stored = repo
        .create_invoice_idempotent(Uuid::new_v4(), &input)
        .await
        .unwrap()
        .invoice;

    let t = &stored.totals;
    assert_eq!(t.tax.total_tax, Money::new(dec!(18.01)));
    assert_eq!(t.tax.cgst, Money::new(dec!(9.01)));
    assert_eq!(t.tax.sgst, Money::new(dec!(9.00)));
    assert_eq!(t.tax.cgst + t.tax.sgst, t.tax.total_tax, "a paisa went missing");
    assert_eq!(t.round_off, t.rounded_total - t.grand_total);
}

#[tokio::test]
async fn every_parity_tax_shape_round_trips_unchanged() {
    // A sweep over the tax shapes the parity fixtures cover: interstate IGST, both
    // discount bases, both round-off signs, the rupee midpoint, multi-line, and the
    // 100%-discount edge. Each is computed by the domain, stored, read back, and
    // compared field by field. Any divergence here would be a storage bug the parity
    // harness cannot catch, because the harness never touches the database.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let cases: Vec<(&str, NewInvoice)> = vec![
        (
            "interstate igst",
            invoice_with(
                vec![line("CHAI", dec!(4), dec!(100))],
                Money::new(dec!(40)),
                dec!(0.05),
                SupplyType::Interstate,
                DiscountBasis::NetTotal,
            ),
        ),
        (
            "discount basis grand total",
            invoice_with(
                vec![line("CHAI", dec!(1), dec!(100))],
                Money::new(dec!(10)),
                dec!(0.05),
                SupplyType::Intrastate,
                DiscountBasis::GrandTotal,
            ),
        ),
        (
            "round off negative",
            invoice_with(
                vec![line("CHAI", dec!(1), dec!(360.38))],
                Money::ZERO,
                dec!(0.05),
                SupplyType::Intrastate,
                DiscountBasis::NetTotal,
            ),
        ),
        (
            "rupee midpoint round off",
            invoice_with(
                vec![line("CHAI", dec!(1), dec!(408.10))],
                Money::ZERO,
                dec!(0.05),
                SupplyType::Intrastate,
                DiscountBasis::NetTotal,
            ),
        ),
        (
            "multi line",
            invoice_with(
                vec![
                    line("CHAI", dec!(2), dec!(100.11)),
                    line("NAAN", dec!(3), dec!(50.22)),
                    line("CHAI", dec!(1), dec!(200.33)),
                ],
                Money::new(dec!(50)),
                dec!(0.05),
                SupplyType::Intrastate,
                DiscountBasis::NetTotal,
            ),
        ),
        (
            "hundred percent discount",
            invoice_with(
                vec![line("CHAI", dec!(1), dec!(100))],
                Money::new(dec!(100)),
                dec!(0.05),
                SupplyType::Intrastate,
                DiscountBasis::NetTotal,
            ),
        ),
    ];

    for (label, input) in cases {
        let created = repo
            .create_invoice_idempotent(Uuid::new_v4(), &input)
            .await
            .unwrap_or_else(|e| panic!("{label}: create failed: {e}"));
        let stored = repo.get(&created.invoice.name).await.unwrap();

        assert_eq!(stored.totals, input.totals, "{label}: totals moved in storage");
        assert_eq!(stored.tax_rate, input.tax_rate, "{label}: tax rate moved");
        assert_eq!(stored.supply_type, input.supply_type, "{label}");
        assert_eq!(stored.discount_basis, input.discount_basis, "{label}");

        // The ledger invariant, re-checked on the stored copy.
        assert_eq!(
            stored.totals.round_off,
            stored.totals.rounded_total - stored.totals.grand_total,
            "{label}: round_off is not the residual"
        );
        assert_eq!(
            stored.totals.tax.cgst + stored.totals.tax.sgst + stored.totals.tax.igst,
            stored.totals.tax.total_tax,
            "{label}: tax components do not sum to the total"
        );

        // net_total is the sum of the lines, still exact.
        let line_sum: Money = stored.lines.iter().map(|l| l.amount).sum();
        assert_eq!(
            line_sum, stored.totals.net_total,
            "{label}: lines do not sum to net_total"
        );
    }
}

#[tokio::test]
async fn line_amounts_and_order_survive_the_round_trip() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let input = invoice_with(
        vec![
            line("CHAI", dec!(2.5), dec!(40.123456)),
            line("NAAN", dec!(3), dec!(19.5)),
        ],
        Money::ZERO,
        dec!(0.05),
        SupplyType::Intrastate,
        DiscountBasis::NetTotal,
    );

    let stored = repo
        .create_invoice_idempotent(Uuid::new_v4(), &input)
        .await
        .unwrap()
        .invoice;

    assert_eq!(stored.lines.len(), 2);
    // 1-based `idx`, matching Frappe child tables, and insertion order preserved.
    assert_eq!(stored.lines[0].idx, 1);
    assert_eq!(stored.lines[1].idx, 2);
    assert_eq!(stored.lines[0].item_code.as_str(), "CHAI");
    assert_eq!(stored.lines[1].item_code.as_str(), "NAAN");

    // Six decimals of rate, and the product, both exact.
    assert_eq!(stored.lines[0].rate, Money::new(dec!(40.123456)));
    assert_eq!(stored.lines[0].qty, dec!(2.5));
    assert_eq!(stored.lines[0].amount, Money::new(dec!(100.308640)));
    assert_eq!(stored.lines[1].amount, Money::new(dec!(58.5)));
    assert_eq!(stored.lines[0].hsn_sac.as_deref(), Some("996331"));
}

#[tokio::test]
async fn totals_that_contradict_the_domain_are_refused_by_the_schema() {
    // The parity harness proves the arithmetic; these constraints prove storage cannot
    // then persist something that disagrees with it.
    let db = TestDb::new().await;
    seed(&db).await;

    let base = "INSERT INTO invoices
        (name, naming_series, fiscal_year, series_number, branch, customer,
         posted_at, business_day, supply_type, discount_basis, tax_rate,
         net_total, discount, taxable_value, cgst, sgst, igst, total_tax,
         grand_total, rounded_total, round_off)
        VALUES ($1, 'POS', '2627', $2, $3, 'Walk-in', now(), CURRENT_DATE,";

    // CGST + SGST must equal total_tax. 9.00 + 9.00 <> 18.01 loses a paisa.
    let cases: Vec<(&str, &str, &str)> = vec![
        (
            "tax components must sum to total_tax",
            "invoices_tax_components_sum_to_total",
            "'Intrastate', 'NetTotal', 0.05, 360.20, 0, 360.20, 9.00, 9.00, 0, 18.01, 378.21, 378, -0.21)",
        ),
        (
            "intrastate cannot carry IGST",
            "invoices_intrastate_has_no_igst",
            "'Intrastate', 'NetTotal', 0.05, 360, 0, 360, 0, 0, 18, 18, 378, 378, 0)",
        ),
        (
            "interstate cannot carry CGST/SGST",
            "invoices_interstate_has_no_cgst_or_sgst",
            "'Interstate', 'NetTotal', 0.05, 360, 0, 360, 9, 9, 0, 18, 378, 378, 0)",
        ),
        (
            "NetTotal basis: taxable_value = net_total - discount",
            "invoices_taxable_value_follows_discount_basis",
            "'Intrastate', 'NetTotal', 0.05, 400, 40, 400, 10, 10, 0, 20, 420, 420, 0)",
        ),
        (
            "grand_total must follow the basis",
            "invoices_grand_total_follows_discount_basis",
            "'Intrastate', 'NetTotal', 0.05, 400, 40, 360, 9, 9, 0, 18, 999, 999, 0)",
        ),
        (
            "round_off must be the exact residual",
            "invoices_round_off_is_exact",
            "'Intrastate', 'NetTotal', 0.05, 360.38, 0, 360.38, 9.01, 9.01, 0, 18.02, 378.40, 378, 0)",
        ),
        (
            "rounded_total must be whole rupees",
            "invoices_rounded_total_is_whole_rupees",
            "'Intrastate', 'NetTotal', 0.05, 360.38, 0, 360.38, 9.01, 9.01, 0, 18.02, 378.40, 378.40, 0)",
        ),
        (
            "rounding is to the NEAREST rupee",
            "invoices_round_off_within_half_a_rupee",
            "'Intrastate', 'NetTotal', 0.05, 360.38, 0, 360.38, 9.01, 9.01, 0, 18.02, 378.40, 380, 1.60)",
        ),
    ];

    for (i, (label, constraint, tail)) in cases.iter().enumerate() {
        let sql = format!("{base} {tail}");
        let err = sqlx::query(&sql)
            .bind(format!("POS-2627-{:06}", i + 900))
            .bind(i as i64 + 900)
            .bind(BRANCH)
            .execute(db.pool())
            .await
            .err()
            .unwrap_or_else(|| panic!("{label}: the schema accepted a bad total"));

        assert_eq!(
            err.as_database_error().and_then(|e| e.constraint()),
            Some(*constraint),
            "{label}: wrong constraint fired"
        );
    }
}

#[tokio::test]
async fn a_line_amount_that_is_not_qty_times_rate_is_refused() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let created = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap();

    let err = sqlx::query(
        "INSERT INTO invoice_lines (invoice, idx, item_code, item_name, qty, rate, amount)
         VALUES ($1, 99, 'CHAI', 'Masala Chai', 2, 50, 999)",
    )
    .bind(created.invoice.name.as_str())
    .execute(db.pool())
    .await
    .expect_err("a line whose amount is not qty * rate should be refused");

    assert_eq!(
        err.as_database_error().and_then(|e| e.constraint()),
        Some("invoice_lines_amount_is_qty_times_rate")
    );
}

#[tokio::test]
async fn deleting_an_invoice_takes_its_lines_and_keys_with_it() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let key = Uuid::new_v4();
    let created = repo
        .create_invoice_idempotent(key, &worked_example())
        .await
        .unwrap();

    sqlx::query("DELETE FROM invoices WHERE name = $1")
        .bind(created.invoice.name.as_str())
        .execute(db.pool())
        .await
        .expect("delete invoice");

    let orphan_lines: i64 =
        sqlx::query_scalar("SELECT count(*) FROM invoice_lines WHERE invoice = $1")
            .bind(created.invoice.name.as_str())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(orphan_lines, 0, "child lines survived the parent delete");

    // The key cascades too: it is a request-dedup record with no independent meaning,
    // so it must not outlive the invoice and point at nothing.
    assert!(repo.lookup_idempotency_key(key).await.unwrap().is_none());
}

// ===========================================================================
// 4. Status transitions
// ===========================================================================

#[tokio::test]
async fn a_new_invoice_starts_as_draft() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let created = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap();
    assert_eq!(created.invoice.status, PosInvoiceStatus::Draft);
    assert!(!created.invoice.status.counts_as_revenue());
}

#[tokio::test]
async fn the_legal_lifecycle_is_draft_paid_consolidated() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let name = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap()
        .invoice
        .name;

    let paid = repo.set_status(&name, PosInvoiceStatus::Paid).await.unwrap();
    assert_eq!(paid.status, PosInvoiceStatus::Paid);
    assert!(paid.status.counts_as_revenue());

    let consolidated = repo
        .set_status(&name, PosInvoiceStatus::Consolidated)
        .await
        .unwrap();
    assert_eq!(consolidated.status, PosInvoiceStatus::Consolidated);
    assert!(consolidated.status.counts_as_revenue());
}

#[tokio::test]
async fn paid_can_be_returned_and_return_is_terminal() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let name = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap()
        .invoice
        .name;

    repo.set_status(&name, PosInvoiceStatus::Paid).await.unwrap();
    let returned = repo
        .set_status(&name, PosInvoiceStatus::Return)
        .await
        .unwrap();
    assert_eq!(returned.status, PosInvoiceStatus::Return);
    // A Return is not revenue — the bug 4 definition.
    assert!(!returned.status.counts_as_revenue());

    assert!(repo.set_status(&name, PosInvoiceStatus::Paid).await.is_err());
}

#[tokio::test]
async fn illegal_transitions_are_refused() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let draft = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap()
        .invoice
        .name;

    // Draft -> Consolidated would let unpaid revenue into the P&L.
    let err = repo
        .set_status(&draft, PosInvoiceStatus::Consolidated)
        .await
        .expect_err("Draft -> Consolidated should be refused");
    assert!(
        matches!(err, StorageError::Domain(peacock_core::Error::Conflict { .. })),
        "expected a domain Conflict, got {err:?}"
    );
    assert!(repo
        .set_status(&draft, PosInvoiceStatus::Return)
        .await
        .is_err());

    // Consolidated is terminal: no un-consolidating, no un-paying.
    repo.set_status(&draft, PosInvoiceStatus::Paid).await.unwrap();
    repo.set_status(&draft, PosInvoiceStatus::Consolidated)
        .await
        .unwrap();
    assert!(repo.set_status(&draft, PosInvoiceStatus::Paid).await.is_err());
    assert!(repo
        .set_status(&draft, PosInvoiceStatus::Draft)
        .await
        .is_err());

    // The status did not move on any failed attempt.
    assert_eq!(
        repo.get(&draft).await.unwrap().status,
        PosInvoiceStatus::Consolidated
    );
}

#[tokio::test]
async fn re_applying_the_same_status_is_a_no_op_not_an_error() {
    // A retried "mark paid" must not fail; the POS client cannot know whether its
    // previous request landed.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let name = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap()
        .invoice
        .name;

    repo.set_status(&name, PosInvoiceStatus::Paid).await.unwrap();
    for _ in 0..3 {
        let again = repo.set_status(&name, PosInvoiceStatus::Paid).await.unwrap();
        assert_eq!(again.status, PosInvoiceStatus::Paid);
    }
}

#[tokio::test]
async fn the_trigger_refuses_an_illegal_transition_written_directly() {
    // The repository check is a courtesy; the trigger is the guarantee. A migration
    // script or a psql session must be held to the same rule.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let name = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap()
        .invoice
        .name;

    let err = sqlx::query("UPDATE invoices SET status = 'Consolidated' WHERE name = $1")
        .bind(name.as_str())
        .execute(db.pool())
        .await
        .expect_err("raw SQL bypassed the transition rule");

    assert_eq!(
        err.as_database_error().and_then(|e| e.constraint()),
        Some("invoices_status_transition")
    );
}

#[tokio::test]
async fn an_issued_invoice_cannot_be_renumbered() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let name = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap()
        .invoice
        .name;

    for sql in [
        "UPDATE invoices SET series_number = 99 WHERE name = $1",
        "UPDATE invoices SET fiscal_year = '2728' WHERE name = $1",
        "UPDATE invoices SET naming_series = 'XXX' WHERE name = $1",
        "UPDATE invoices SET name = 'POS-2627-000042' WHERE name = $1",
    ] {
        let err = sqlx::query(sql)
            .bind(name.as_str())
            .execute(db.pool())
            .await
            .err()
            .unwrap_or_else(|| panic!("renumbering succeeded: {sql}"));
        assert_eq!(
            err.as_database_error().and_then(|e| e.constraint()),
            Some("invoices_serial_is_immutable"),
            "{sql}"
        );
    }
}

#[tokio::test]
async fn totals_freeze_once_the_invoice_leaves_draft() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let name = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap()
        .invoice
        .name;

    // While Draft, a correction is legitimate.
    sqlx::query(
        "UPDATE invoices
            SET net_total = 400, discount = 40, taxable_value = 360,
                cgst = 9, sgst = 9, total_tax = 18,
                grand_total = 378, rounded_total = 378, round_off = 0
          WHERE name = $1",
    )
    .bind(name.as_str())
    .execute(db.pool())
    .await
    .expect("a draft invoice should be editable");

    repo.set_status(&name, PosInvoiceStatus::Paid).await.unwrap();

    // After payment, the printed document and the database row are the same thing.
    let err = sqlx::query("UPDATE invoices SET grand_total = 1, rounded_total = 1, round_off = 0 WHERE name = $1")
        .bind(name.as_str())
        .execute(db.pool())
        .await
        .expect_err("a paid invoice's totals should be frozen");

    assert_eq!(
        err.as_database_error().and_then(|e| e.constraint()),
        Some("invoices_totals_frozen_after_draft")
    );

    // Non-money fields still move: a bill can be reprinted.
    repo.mark_printed(&name).await.expect("mark printed");
    assert!(repo.get(&name).await.unwrap().invoice_printed);
}

#[tokio::test]
async fn a_cancellation_reason_is_recorded_for_the_audit_trail() {
    // `invoicing.rs`: "A gap only appears for a deliberately cancelled invoice, which
    // must carry a logged void reason for the audit trail."
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let name = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap()
        .invoice
        .name;

    repo.record_cancel_reason(&name, "customer walked out; table released")
        .await
        .expect("record reason");

    let stored = repo.get(&name).await.unwrap();
    assert_eq!(
        stored.cancel_reason.as_deref(),
        Some("customer walked out; table released")
    );

    // A blank reason is not a reason.
    assert!(repo.record_cancel_reason(&name, "   ").await.is_err());
    assert!(repo
        .record_cancel_reason(&InvoiceName::from("POS-2627-999999"), "nope")
        .await
        .is_err());
}

// ===========================================================================
// 5. Queries
// ===========================================================================

#[tokio::test]
async fn invoices_are_queryable_by_date_status_and_table() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    sqlx::query(
        "INSERT INTO tables (name, no_of_seats, restaurant, restaurant_room, branch)
         VALUES ('T-02', 2, $1, $2, $3)",
    )
    .bind(RESTAURANT)
    .bind(ROOM)
    .bind(BRANCH)
    .execute(db.pool())
    .await
    .unwrap();

    // Three invoices: two on T-01 (one paid), one on T-02.
    let mut names = Vec::new();
    for table in ["T-01", "T-01", "T-02"] {
        let mut inv = worked_example();
        inv.restaurant_table = Some(TableName::from(table));
        names.push(
            repo.create_invoice_idempotent(Uuid::new_v4(), &inv)
                .await
                .unwrap()
                .invoice
                .name,
        );
    }
    repo.set_status(&names[0], PosInvoiceStatus::Paid)
        .await
        .unwrap();

    // By table.
    assert_eq!(
        repo.list_by_table(&TableName::from("T-01")).await.unwrap().len(),
        2
    );
    assert_eq!(
        repo.list_by_table(&TableName::from("T-02")).await.unwrap().len(),
        1
    );

    // By status.
    assert_eq!(
        repo.list_by_status(PosInvoiceStatus::Paid).await.unwrap().len(),
        1
    );
    assert_eq!(
        repo.list_by_status(PosInvoiceStatus::Draft).await.unwrap().len(),
        2
    );

    // By date: the half-open window that fixes bug 2.
    let posted = Utc.with_ymd_and_hms(2026, 7, 15, 13, 30, 0).unwrap();
    assert_eq!(
        repo.list_by_posted_range(posted, posted + chrono::Duration::seconds(1))
            .await
            .unwrap()
            .len(),
        3
    );
    // `end` is exclusive: a window ending exactly at `posted` must see nothing.
    assert!(repo
        .list_by_posted_range(posted - chrono::Duration::hours(1), posted)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn the_revenue_query_uses_the_single_status_definition() {
    // Bug 4: shift close counted only Paid, the P&L counted Paid + Consolidated. One
    // definition now — `PosInvoiceStatus::REVENUE` — and this query reads it.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let mut names = Vec::new();
    for _ in 0..4 {
        names.push(
            repo.create_invoice_idempotent(Uuid::new_v4(), &worked_example())
                .await
                .unwrap()
                .invoice
                .name,
        );
    }

    // names[0] Draft, [1] Paid, [2] Consolidated, [3] Return.
    repo.set_status(&names[1], PosInvoiceStatus::Paid).await.unwrap();
    repo.set_status(&names[2], PosInvoiceStatus::Paid).await.unwrap();
    repo.set_status(&names[2], PosInvoiceStatus::Consolidated)
        .await
        .unwrap();
    repo.set_status(&names[3], PosInvoiceStatus::Paid).await.unwrap();
    repo.set_status(&names[3], PosInvoiceStatus::Return).await.unwrap();

    let revenue = repo
        .list_revenue_for_business_day(
            &BranchName::from(BRANCH),
            NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(revenue.len(), 2, "revenue set is not Paid + Consolidated");
    for inv in &revenue {
        assert!(
            inv.status.counts_as_revenue(),
            "{:?} is not a revenue status",
            inv.status
        );
    }

    // And the revenue figure is `rounded_total` (bug 3), summed in Decimal.
    let total: Money = revenue.iter().map(|i| i.totals.rounded_total).sum();
    assert_eq!(total, Money::new(dec!(756)));

    // A different business day sees nothing.
    assert!(repo
        .list_revenue_for_business_day(
            &BranchName::from(BRANCH),
            NaiveDate::from_ymd_opt(2026, 7, 16).unwrap()
        )
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_missing_invoice_is_none_rather_than_an_error() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    assert!(repo
        .find(&InvoiceName::from("POS-2627-000404"))
        .await
        .unwrap()
        .is_none());
    assert!(repo.get(&InvoiceName::from("POS-2627-000404")).await.is_err());

    let created = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap();
    assert!(repo.find(&created.invoice.name).await.unwrap().is_some());
}

// ===========================================================================
// 6. Schema shape
// ===========================================================================

#[tokio::test]
async fn every_invoice_money_column_is_numeric_never_float() {
    let db = TestDb::new().await;

    let cols: Vec<(String, String, Option<i32>, Option<i32>)> = sqlx::query_as(
        "SELECT table_name, column_name, numeric_precision, numeric_scale
         FROM information_schema.columns
         WHERE table_schema = 'public'
           AND table_name IN ('invoices', 'invoice_lines')
           AND column_name IN (
               'net_total','discount','taxable_value','cgst','sgst','igst','total_tax',
               'grand_total','rounded_total','round_off','paid_amount','change_amount',
               'qty','rate','amount'
           )",
    )
    .fetch_all(db.pool())
    .await
    .expect("describe money columns");

    assert_eq!(cols.len(), 15, "expected 15 money columns, got {cols:?}");
    for (table, column, precision, scale) in &cols {
        // The Lane 2A standard. A float here would reintroduce the paisa drift the
        // parity harness exists to catch.
        assert_eq!(*precision, Some(18), "{table}.{column} precision");
        assert_eq!(*scale, Some(6), "{table}.{column} scale");
    }
}

#[tokio::test]
async fn the_invoice_query_paths_all_have_indexes() {
    let db = TestDb::new().await;

    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT indexdef FROM pg_indexes
         WHERE schemaname = 'public'
           AND tablename IN ('invoices', 'invoice_lines', 'idempotency_keys',
                             'invoice_naming_series')",
    )
    .fetch_all(db.pool())
    .await
    .expect("list indexes");

    let has = |needle: &str| {
        indexes
            .iter()
            .any(|d| d.to_lowercase().contains(&needle.to_lowercase()))
    };

    // Rule 46(b) uniqueness: no counter value twice per series per FY.
    assert!(
        has("unique index invoices_series_number_unique_idx")
            && has("(naming_series, fiscal_year, series_number)"),
        "missing the series-number uniqueness index: {indexes:?}"
    );

    // By date — both the instant scan and the rollup key.
    assert!(has("on public.invoices using btree (posted_at)"), "missing posted_at index");
    assert!(
        has("on public.invoices using btree (business_day)"),
        "missing business_day index"
    );
    // By status.
    assert!(
        has("on public.invoices using btree (status, posted_at)"),
        "missing status index"
    );
    // By table.
    assert!(
        has("on public.invoices using btree (restaurant_table, posted_at)"),
        "missing restaurant_table index"
    );
    // The P&L / shift-close path, partial on the revenue statuses.
    assert!(
        has("(branch, business_day, posted_at)") && has("'Paid'"),
        "missing the partial revenue index"
    );
    // Lines in order — the only read path that matters.
    assert!(
        has("unique index invoice_lines_invoice_idx_key"),
        "missing the (invoice, idx) unique index"
    );
    // The purge scan.
    assert!(
        has("on public.idempotency_keys using btree (expires_at)"),
        "missing expires_at index"
    );
}

#[tokio::test]
async fn the_invoice_status_enum_matches_the_domain_enum() {
    let db = TestDb::new().await;

    let labels: Vec<String> = sqlx::query_scalar(
        "SELECT e.enumlabel
         FROM pg_enum e JOIN pg_type t ON t.oid = e.enumtypid
         WHERE t.typname = 'invoice_status'
         ORDER BY e.enumsortorder",
    )
    .fetch_all(db.pool())
    .await
    .expect("read enum labels");

    // Every `PosInvoiceStatus` variant, and nothing else. A label the domain does not
    // know would read back as an error; a variant the schema lacks could not be stored.
    assert_eq!(labels, vec!["Draft", "Paid", "Consolidated", "Return"]);
}

// ===========================================================================
// 7. The domain ports, driven directly
// ===========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn the_domain_allocator_runs_against_postgres() {
    // `invoicing::allocate_invoice_number` is the domain's own entry point, exercised
    // here through the real transaction-scoped ports rather than the in-memory fakes it
    // is unit-tested with. Same function, same assertions, real database.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let fy = fiscal_year_code(NaiveDate::from_ymd_opt(2026, 7, 15).unwrap());
    assert_eq!(fy, FY, "fiscal year code drifted from the series fixture");

    let mut tx = db.storage.begin().await.unwrap();
    let mut allocator = TxSeriesAllocator::new(&mut tx);
    let names: Vec<InvoiceName> = (0..3)
        .map(|_| {
            let mut store = NoopStore;
            allocate_invoice_number(&mut allocator, &mut store, SERIES, &fy, Uuid::new_v4())
                .expect("domain allocation")
        })
        .collect();
    tx.commit().await.unwrap();

    assert_eq!(names[0].as_str(), "POS-2627-000001");
    assert_eq!(names[1].as_str(), "POS-2627-000002");
    assert_eq!(names[2].as_str(), "POS-2627-000003");
    for n in &names {
        assert!(n.as_str().len() <= MAX_INVOICE_NAME_LEN);
    }
    assert_eq!(repo.peek_series(SERIES, FY).await.unwrap(), Some(4));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_domain_idempotency_store_writes_through_to_postgres() {
    use peacock_core::invoicing::IdempotencyStore;

    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    // The store's FK requires the invoice to exist first, so create one the normal way.
    let created = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap();

    let key = Uuid::new_v4();
    let mut tx = db.storage.begin().await.unwrap();
    {
        let mut store = TxIdempotencyStore::new(&mut tx);
        assert!(store.get(key).is_none(), "a fresh key must not be found");
        store
            .record(key, created.invoice.name.clone())
            .expect("record key");
        // Written-through cache: `get` after `record` is consistent inside the
        // transaction, which is what `allocate_invoice_number`'s replay check needs.
        assert_eq!(store.get(key), Some(created.invoice.name.clone()));
    }
    tx.commit().await.unwrap();

    assert_eq!(
        repo.lookup_idempotency_key(key).await.unwrap(),
        Some(created.invoice.name)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rolled_back_domain_allocation_burns_nothing() {
    // The same guarantee as the async path, through the domain port: abandon the
    // transaction and the counter returns to where it was.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let mut tx = db.storage.begin().await.unwrap();
    {
        let mut allocator = TxSeriesAllocator::new(&mut tx);
        let mut store = NoopStore;
        let name =
            allocate_invoice_number(&mut allocator, &mut store, SERIES, FY, Uuid::new_v4())
                .unwrap();
        assert_eq!(name.as_str(), "POS-2627-000001");
    }
    tx.rollback().await.unwrap();

    assert_eq!(
        repo.peek_series(SERIES, FY).await.unwrap(),
        Some(1),
        "the rollback did not restore the counter"
    );

    // And the number is still available.
    let created = repo
        .create_invoice_idempotent(Uuid::new_v4(), &worked_example())
        .await
        .unwrap();
    assert_eq!(created.invoice.name.as_str(), "POS-2627-000001");
}

/// An `IdempotencyStore` that records nothing.
///
/// Used where the test drives `allocate_invoice_number` purely to observe the counter.
/// A real `TxIdempotencyStore` cannot be used there: its FK to `invoices` requires an
/// invoice that, by construction, has not been written yet.
struct NoopStore;

impl peacock_core::invoicing::IdempotencyStore for NoopStore {
    fn get(&self, _key: Uuid) -> Option<InvoiceName> {
        None
    }
    fn record(&mut self, _key: Uuid, _invoice_name: InvoiceName) -> peacock_core::Result<()> {
        Ok(())
    }
}
