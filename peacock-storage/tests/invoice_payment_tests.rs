//! Invoice payment + station routing integration tests — Lane 4A-3. **Money lane.**
//!
//! Covers what `010_invoice_payments.sql` and the Lane 4A-3 repository additions own:
//!
//! 1. Split tender accumulates and settles against `rounded_total`, not `grand_total`
//!    (`businessday.rs` bug 3).
//! 2. `invoices.paid_amount` is derived from the payment rows by trigger, so no code path
//!    can compute it and get it wrong.
//! 3. Overpayment is refused at the application *and* the database, because the
//!    application check can lose a race.
//! 4. Station routing reads real production units and real item groups, in a bounded
//!    number of queries.
//!
//! Every test runs against a throwaway database ([`support::TestDb`]) with the full
//! migration set applied, so the constraints and triggers under test are the deployed
//! ones.

mod support;

use peacock_core::ids::{
    BranchName, ItemCode, ItemGroupName, InvoiceName, ProductionUnitName, RoomName, TableName,
};
use peacock_core::kot::{required_item_codes, route_items_to_stations, unrouted_item_codes, KotContext};
use peacock_core::model::{KotType, OrderLine, PosInvoiceStatus};
use peacock_core::money::Money;
use peacock_core::ports::{ItemRepo, ProductionRepo};
use peacock_core::tax::{compute_totals, DiscountBasis, InvoiceLine, SupplyType};
use peacock_storage::error::StorageError;
use peacock_storage::repos::invoice::{
    NewInvoice, NewInvoiceLine, NewPayment, PaymentMethod, PgInvoiceRepo,
};
use peacock_storage::repos::routing::{PgItemRepo, PgProductionRepo, RoutingSnapshot};
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

/// Restaurant, room, a table, and the items the sample lines reference.
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

    for (code, name, group) in [
        ("CHAI", "Masala Chai", "Beverages"),
        ("NAAN", "Butter Naan", "Breads"),
        ("BIRYANI", "Chicken Biryani", "Main Course"),
        ("ORPHAN", "Unrouted Thing", "Stationery"),
    ] {
        sqlx::query("INSERT INTO items (code, name, item_group) VALUES ($1, $2, $3)")
            .bind(code)
            .bind(name)
            .bind(group)
            .execute(db.pool())
            .await
            .expect("seed item");
    }
}

/// Two stations: the grill takes food, the bar takes drinks.
async fn seed_production_units(db: &TestDb) {
    for (unit, groups) in [
        ("Hot Kitchen", vec!["Main Course", "Breads"]),
        ("Bar", vec!["Beverages"]),
    ] {
        sqlx::query("INSERT INTO production_units (name, branch) VALUES ($1, $2)")
            .bind(unit)
            .bind(BRANCH)
            .execute(db.pool())
            .await
            .expect("seed production unit");

        for (position, group) in groups.iter().enumerate() {
            sqlx::query(
                "INSERT INTO production_unit_item_groups (production_unit, idx, item_group)
                 VALUES ($1, $2, $3)",
            )
            .bind(unit)
            .bind(position as i32 + 1)
            .bind(group)
            .execute(db.pool())
            .await
            .expect("seed item group");
        }
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

/// The §5 worked example, unpaid: 4 × ₹100, ₹40 discount, 5% GST → rounded total ₹378.
///
/// `paid_amount` starts at zero, unlike the Lane 2F fixture: these tests are about the
/// payments arriving, so the invoice has to begin owing the full amount.
fn unpaid_worked_example() -> NewInvoice {
    let lines = vec![line("CHAI", dec!(4), dec!(100))];

    // Totals come from the domain. Recomputing them here would be a second, unvalidated
    // implementation of the arithmetic the parity harness pins.
    let totals = compute_totals(
        &lines
            .iter()
            .map(|l| InvoiceLine {
                item_name: l.item_name.clone(),
                quantity: l.qty,
                rate: l.rate,
                hsn_sac: l.hsn_sac.clone(),
            })
            .collect::<Vec<_>>(),
        Money::new(dec!(40)),
        dec!(0.05),
        SupplyType::Intrastate,
        DiscountBasis::NetTotal,
    )
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
        waiter: None,
        cashier: None,
        no_of_pax: 2,
        order_type: Some("ORD-001".to_owned()),
        posted_at: Utc.with_ymd_and_hms(2026, 7, 15, 13, 30, 0).unwrap(),
        business_day: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        supply_type: SupplyType::Intrastate,
        discount_basis: DiscountBasis::NetTotal,
        tax_rate: dec!(0.05),
        totals,
        paid_amount: Money::ZERO,
        change_amount: Money::ZERO,
        comments: None,
        lines,
    }
}

fn cash(amount: Decimal) -> NewPayment {
    NewPayment {
        method: PaymentMethod::Cash,
        amount: Money::new(amount),
        reference: None,
        paid_at: Utc.with_ymd_and_hms(2026, 7, 15, 14, 0, 0).unwrap(),
    }
}

/// Create the unpaid worked example and hand back its allocated name.
async fn create_unpaid(repo: &PgInvoiceRepo) -> InvoiceName {
    repo.create_invoice_idempotent(Uuid::new_v4(), &unpaid_worked_example())
        .await
        .expect("create invoice")
        .invoice
        .name
}

// ===========================================================================
// 1. Payments settle against rounded_total
// ===========================================================================

#[tokio::test]
async fn a_full_payment_settles_the_invoice_and_marks_it_paid() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;
    let name = create_unpaid(&repo).await;

    let invoice = repo
        .record_payment(&name, &cash(dec!(378)))
        .await
        .expect("record payment");

    assert_eq!(invoice.status, PosInvoiceStatus::Paid);
    assert_eq!(invoice.paid_amount, Money::new(dec!(378)));
    assert_eq!(invoice.outstanding_amount(), Money::ZERO);
    assert!(invoice.is_settled());
    assert_eq!(invoice.payments.len(), 1);
    assert_eq!(invoice.payments[0].idx, 1);
    assert_eq!(invoice.payments[0].method, PaymentMethod::Cash);
}

#[tokio::test]
async fn settlement_is_measured_against_rounded_total_not_grand_total() {
    // businessday.rs bug 3: upstream settled against grand_total (378.00) while the
    // customer paid rounded_total (378). On a bill where the two differ, paying the
    // rounded figure must settle it — otherwise every cash bill leaves a sub-rupee
    // residue that reconciles against nothing.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    // 1 × ₹360.20 at 5% → grand 378.21, rounded 378, round_off -0.21.
    let lines = vec![line("CHAI", dec!(1), dec!(360.20))];
    let totals = compute_totals(
        &[InvoiceLine {
            item_name: "CHAI item".to_owned(),
            quantity: dec!(1),
            rate: Money::new(dec!(360.20)),
            hsn_sac: None,
        }],
        Money::ZERO,
        dec!(0.05),
        SupplyType::Intrastate,
        DiscountBasis::NetTotal,
    )
    .unwrap();

    assert_ne!(
        totals.grand_total, totals.rounded_total,
        "this test is only meaningful when the two figures differ"
    );

    let rounded_total = totals.rounded_total;

    let mut new_invoice = unpaid_worked_example();
    new_invoice.lines = lines;
    new_invoice.totals = totals;

    let name = repo
        .create_invoice_idempotent(Uuid::new_v4(), &new_invoice)
        .await
        .expect("create")
        .invoice
        .name;

    let paid = repo
        .record_payment(&name, &cash(rounded_total.inner()))
        .await
        .expect("pay the rounded total");

    assert_eq!(
        paid.status,
        PosInvoiceStatus::Paid,
        "paying rounded_total must settle the bill"
    );
    assert_eq!(paid.outstanding_amount(), Money::ZERO);
}

#[tokio::test]
async fn a_short_payment_leaves_the_invoice_draft_with_a_balance() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;
    let name = create_unpaid(&repo).await;

    let invoice = repo
        .record_payment(&name, &cash(dec!(300)))
        .await
        .expect("partial payment");

    assert_eq!(
        invoice.status,
        PosInvoiceStatus::Draft,
        "a short payment must not settle the bill"
    );
    assert_eq!(invoice.paid_amount, Money::new(dec!(300)));
    assert_eq!(invoice.outstanding_amount(), Money::new(dec!(78)));
    assert!(!invoice.is_settled());
}

#[tokio::test]
async fn split_tender_accumulates_then_settles() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;
    let name = create_unpaid(&repo).await;

    let part = repo
        .record_payment(
            &name,
            &NewPayment {
                method: PaymentMethod::Card,
                amount: Money::new(dec!(300)),
                reference: Some("txn-4242".to_owned()),
                paid_at: Utc.with_ymd_and_hms(2026, 7, 15, 14, 0, 0).unwrap(),
            },
        )
        .await
        .expect("card leg");
    assert_eq!(part.status, PosInvoiceStatus::Draft);

    let rest = repo
        .record_payment(&name, &cash(dec!(78)))
        .await
        .expect("cash leg");

    assert_eq!(rest.status, PosInvoiceStatus::Paid);
    assert_eq!(rest.paid_amount, Money::new(dec!(378)));
    assert_eq!(rest.payments.len(), 2);

    // idx is 1-based and ordered, so "the second tender on this bill" is answerable.
    assert_eq!(rest.payments[0].idx, 1);
    assert_eq!(rest.payments[0].method, PaymentMethod::Card);
    assert_eq!(rest.payments[0].reference.as_deref(), Some("txn-4242"));
    assert_eq!(rest.payments[1].idx, 2);
    assert_eq!(rest.payments[1].method, PaymentMethod::Cash);

    // Only the cash leg counts toward the CGST Rule 56 drawer total.
    assert_eq!(rest.cash_total(), Money::new(dec!(78)));
}

// ===========================================================================
// 2. paid_amount is derived, not asserted
// ===========================================================================

#[tokio::test]
async fn paid_amount_is_maintained_by_the_trigger_from_the_payment_rows() {
    // The guarantee: no code path computes paid_amount. A handler that added it up itself
    // would be one missed update away from a bill that refuses a legitimate final payment.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;
    let name = create_unpaid(&repo).await;

    repo.record_payment(&name, &cash(dec!(100))).await.unwrap();
    repo.record_payment(&name, &cash(dec!(78))).await.unwrap();

    let stored: Decimal = sqlx::query_scalar("SELECT paid_amount FROM invoices WHERE name = $1")
        .bind(name.as_str())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(stored, dec!(178));

    // Delete a payment row directly and the cache follows, without the repository being
    // involved at all.
    sqlx::query("DELETE FROM invoice_payments WHERE invoice = $1 AND idx = 2")
        .bind(name.as_str())
        .execute(db.pool())
        .await
        .unwrap();

    let after: Decimal = sqlx::query_scalar("SELECT paid_amount FROM invoices WHERE name = $1")
        .bind(name.as_str())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(after, dec!(100), "the trigger recomputes from the rows");
}

#[tokio::test]
async fn every_payment_amount_is_numeric_never_float() {
    let db = TestDb::new().await;

    let ty: String = sqlx::query_scalar(
        "SELECT data_type FROM information_schema.columns
          WHERE table_name = 'invoice_payments' AND column_name = 'amount'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    assert_eq!(
        ty, "numeric",
        "money must never touch a float type (peacock-core/src/money.rs)"
    );
}

// ===========================================================================
// 3. Overpayment
// ===========================================================================

#[tokio::test]
async fn overpayment_is_refused_by_the_repository() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;
    let name = create_unpaid(&repo).await;

    let err = repo
        .record_payment(&name, &cash(dec!(500)))
        .await
        .expect_err("₹500 on a ₹378 bill must be refused");

    assert!(
        err.to_string().contains("378"),
        "the error must name the bill it exceeds: {err}"
    );

    // Nothing was written, so the bill still owes the full amount.
    let invoice = repo.get(&name).await.unwrap();
    assert_eq!(invoice.paid_amount, Money::ZERO);
    assert!(invoice.payments.is_empty());
    assert_eq!(invoice.status, PosInvoiceStatus::Draft);
}

#[tokio::test]
async fn overpayment_is_refused_by_the_database_too() {
    // The application check reads then writes, and a concurrent pair of payments can both
    // pass it. The constraint trigger is what makes that unrepresentable, so it is tested
    // by going around the repository entirely.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;
    let name = create_unpaid(&repo).await;

    let err = sqlx::query(
        "INSERT INTO invoice_payments (invoice, idx, method, amount, paid_at)
         VALUES ($1, 1, 'Cash', 500, now())",
    )
    .bind(name.as_str())
    .execute(db.pool())
    .await
    .expect_err("the trigger must refuse this");

    let message = err.to_string();
    assert!(
        message.contains("exceeds"),
        "expected the overpayment guard, got: {message}"
    );
}

#[tokio::test]
async fn concurrent_payments_cannot_overpay_a_bill() {
    // Ten ₹100 tenders race at a ₹378 bill. At most three can land (₹300), and the rest
    // must be refused — never accepted into a total above ₹378.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;
    let name = create_unpaid(&repo).await;

    let mut handles = Vec::new();
    for _ in 0..10 {
        let repo = repo.clone();
        let name = name.clone();
        handles.push(tokio::spawn(async move {
            repo.record_payment(&name, &cash(dec!(100))).await
        }));
    }

    let mut accepted = 0;
    for handle in handles {
        if handle.await.expect("task must not panic").is_ok() {
            accepted += 1;
        }
    }

    let invoice = repo.get(&name).await.unwrap();
    assert_eq!(
        accepted, 3,
        "exactly three ₹100 tenders fit under a ₹378 bill"
    );
    assert_eq!(invoice.paid_amount, Money::new(dec!(300)));
    assert!(
        invoice.paid_amount <= invoice.totals.rounded_total,
        "the bill must never be overpaid"
    );
    assert_eq!(invoice.payments.len(), 3);

    // The idx values are a contiguous run: no tender claimed a position twice.
    let indexes: Vec<i32> = invoice.payments.iter().map(|p| p.idx).collect();
    assert_eq!(indexes, vec![1, 2, 3]);
}

#[tokio::test]
async fn a_zero_or_negative_payment_is_refused() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;
    let name = create_unpaid(&repo).await;

    for amount in [dec!(0), dec!(-10)] {
        let err = repo
            .record_payment(&name, &cash(amount))
            .await
            .expect_err("a non-positive tender is not a payment");
        assert!(matches!(err, StorageError::Domain(_)), "got {err:?}");
    }

    assert!(repo.get(&name).await.unwrap().payments.is_empty());
}

// ===========================================================================
// 4. Closed states refuse payment
// ===========================================================================

#[tokio::test]
async fn a_consolidated_invoice_refuses_further_payment() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;
    let name = create_unpaid(&repo).await;

    repo.record_payment(&name, &cash(dec!(378))).await.unwrap();
    repo.set_status(&name, PosInvoiceStatus::Consolidated)
        .await
        .unwrap();

    let err = repo
        .record_payment(&name, &cash(dec!(1)))
        .await
        .expect_err("a consolidated invoice is closed");
    assert!(
        err.to_string().contains("Consolidated"),
        "the error must say why: {err}"
    );
}

#[tokio::test]
async fn paying_an_unknown_invoice_reports_it_missing() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let err = repo
        .record_payment(&InvoiceName::from("POS-2627-999999"), &cash(dec!(10)))
        .await
        .expect_err("no such invoice");
    assert!(err.to_string().contains("no such invoice"), "got {err}");
}

// ===========================================================================
// 5. list_filtered — the GET /api/invoices query
// ===========================================================================

#[tokio::test]
async fn list_filtered_combines_business_day_status_and_table() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    // Two invoices on 2026-07-15, one settled; one on 2026-07-16, left in Draft.
    let settled = create_unpaid(&repo).await;
    repo.record_payment(&settled, &cash(dec!(378)))
        .await
        .unwrap();

    let _draft_same_day = create_unpaid(&repo).await;

    let mut next_day = unpaid_worked_example();
    next_day.business_day = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
    next_day.posted_at = Utc.with_ymd_and_hms(2026, 7, 16, 13, 30, 0).unwrap();
    repo.create_invoice_idempotent(Uuid::new_v4(), &next_day)
        .await
        .unwrap();

    // No filters: everything.
    assert_eq!(repo.list_filtered(None, None, None, None).await.unwrap().len(), 3);

    // A single business day, inclusive on both ends.
    let one_day = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
    assert_eq!(
        repo.list_filtered(Some(one_day), Some(one_day), None, None)
            .await
            .unwrap()
            .len(),
        2,
        "from = to = D means that one business day"
    );

    // Status alone.
    let paid = repo
        .list_filtered(None, None, Some(PosInvoiceStatus::Paid), None)
        .await
        .unwrap();
    assert_eq!(paid.len(), 1);
    assert_eq!(paid[0].name, settled);

    // Table alone, then a table nothing matches.
    assert_eq!(
        repo.list_filtered(None, None, None, Some(&TableName::from("T-01")))
            .await
            .unwrap()
            .len(),
        3
    );
    assert!(repo
        .list_filtered(None, None, None, Some(&TableName::from("T-99")))
        .await
        .unwrap()
        .is_empty());

    // Every filter at once.
    let all = repo
        .list_filtered(
            Some(one_day),
            Some(one_day),
            Some(PosInvoiceStatus::Paid),
            Some(&TableName::from("T-01")),
        )
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, settled);
}

#[tokio::test]
async fn list_filtered_reads_business_day_not_the_calendar_date() {
    // Bug 2: a 01:30 invoice belongs to the *previous* business day. Filtering on
    // `posted_at::date` would file it under the wrong day and double-count it at the
    // boundary.
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = repo(&db).await;

    let mut after_midnight = unpaid_worked_example();
    // 2026-07-16 01:30 IST → business day 2026-07-15, the day the shift opened.
    after_midnight.posted_at = Utc.with_ymd_and_hms(2026, 7, 15, 20, 0, 0).unwrap();
    after_midnight.business_day = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
    let name = repo
        .create_invoice_idempotent(Uuid::new_v4(), &after_midnight)
        .await
        .unwrap()
        .invoice
        .name;

    let day = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
    let found = repo.list_filtered(Some(day), Some(day), None, None).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, name);

    // And it does not appear under the calendar date of its posting instant + 1.
    let next = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
    assert!(repo
        .list_filtered(Some(next), Some(next), None, None)
        .await
        .unwrap()
        .is_empty());
}

// ===========================================================================
// 6. Station routing against real production units
// ===========================================================================

#[tokio::test]
async fn item_groups_come_back_batched_and_omit_unknown_codes() {
    let db = TestDb::new().await;
    seed(&db).await;
    let items = PgItemRepo::new(db.storage.clone());

    let got = items
        .item_groups_async(&[
            ItemCode::from("CHAI"),
            ItemCode::from("BIRYANI"),
            ItemCode::from("NOT-A-THING"),
        ])
        .await
        .unwrap();

    assert_eq!(got.len(), 2);
    assert_eq!(
        got.get(&ItemCode::from("CHAI")),
        Some(&ItemGroupName::from("Beverages"))
    );
    assert_eq!(
        got.get(&ItemCode::from("BIRYANI")),
        Some(&ItemGroupName::from("Main Course"))
    );
    // Absence, not an error: `unrouted_item_codes` reads it as "routes nowhere".
    assert!(!got.contains_key(&ItemCode::from("NOT-A-THING")));
}

#[tokio::test]
async fn a_disabled_item_routes_nowhere_rather_than_to_a_wrong_station() {
    let db = TestDb::new().await;
    seed(&db).await;
    sqlx::query("UPDATE items SET disabled = TRUE WHERE code = 'CHAI'")
        .execute(db.pool())
        .await
        .unwrap();

    let got = PgItemRepo::new(db.storage.clone())
        .item_groups_async(&[ItemCode::from("CHAI")])
        .await
        .unwrap();
    assert!(
        got.is_empty(),
        "a withdrawn item must not silently reach a kitchen"
    );
}

#[tokio::test]
async fn production_units_come_back_with_their_item_groups_in_order() {
    let db = TestDb::new().await;
    seed(&db).await;
    seed_production_units(&db).await;

    let units = PgProductionRepo::new(db.storage.clone())
        .list_for_branch_async(&BranchName::from(BRANCH))
        .await
        .unwrap();

    assert_eq!(units.len(), 2);
    let kitchen = units
        .iter()
        .find(|u| u.name.as_str() == "Hot Kitchen")
        .expect("Hot Kitchen");
    assert_eq!(
        kitchen.item_groups,
        vec![
            ItemGroupName::from("Main Course"),
            ItemGroupName::from("Breads"),
        ],
        "child rows must follow idx, so a station's ticket layout is deterministic"
    );

    // Another branch sees nothing.
    assert!(PgProductionRepo::new(db.storage.clone())
        .list_for_branch_async(&BranchName::from("Somewhere Else"))
        .await
        .unwrap()
        .is_empty());
}

fn order_line(code: &str, qty: Decimal) -> OrderLine {
    OrderLine {
        item_code: ItemCode::from(code),
        item_name: format!("{code} item"),
        qty,
        rate: Money::ZERO,
        comments: None,
        serve_priority: 0,
        indicate_course: false,
    }
}

fn ctx(invoice: &str) -> KotContext {
    let mut ctx = KotContext::new(
        invoice.to_owned(),
        BranchName::from(BRANCH),
        "KOT-".to_owned(),
        NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
    );
    ctx.room = Some(RoomName::from(ROOM));
    ctx.restaurant_table = Some(TableName::from("T-01"));
    ctx
}

#[tokio::test]
async fn routing_fans_an_order_out_to_the_real_stations() {
    let db = TestDb::new().await;
    seed(&db).await;
    seed_production_units(&db).await;

    let lines = vec![
        order_line("BIRYANI", dec!(2)),
        order_line("NAAN", dec!(4)),
        order_line("CHAI", dec!(2)),
    ];
    let codes = required_item_codes(&lines);

    let snapshot = RoutingSnapshot::load(
        &db.storage,
        "INV-1",
        &BranchName::from(BRANCH),
        Some(&RoomName::from(ROOM)),
        &codes,
    )
    .await
    .unwrap();

    let tickets = route_items_to_stations(&ctx("INV-1"), &lines, &snapshot.repos()).unwrap();

    assert_eq!(tickets.len(), 2, "two stations have work");

    let kitchen = tickets
        .iter()
        .find(|k| k.production.as_ref().map(|p| p.as_str()) == Some("Hot Kitchen"))
        .expect("Hot Kitchen ticket");
    let bar = tickets
        .iter()
        .find(|k| k.production.as_ref().map(|p| p.as_str()) == Some("Bar"))
        .expect("Bar ticket");

    // FIX BUG 1: each station's ticket carries only its own items. Upstream allocated the
    // item vector once outside the loop, so station B's ticket carried station A's items.
    let kitchen_items: Vec<&str> = kitchen.kot_items.iter().map(|i| i.item.as_str()).collect();
    assert_eq!(kitchen_items, vec!["BIRYANI", "NAAN"]);

    let bar_items: Vec<&str> = bar.kot_items.iter().map(|i| i.item.as_str()).collect();
    assert_eq!(bar_items, vec!["CHAI"]);

    // Nothing is written yet: routing returns unsaved tickets.
    assert!(tickets.iter().all(|k| k.name.is_none()));
    assert!(tickets.iter().all(|k| k.kot_type == KotType::NewOrder));
}

#[tokio::test]
async fn an_item_matching_no_station_is_reported_not_fatal() {
    let db = TestDb::new().await;
    seed(&db).await;
    seed_production_units(&db).await;

    // ORPHAN is in 'Stationery', which no station claims.
    let lines = vec![order_line("BIRYANI", dec!(1)), order_line("ORPHAN", dec!(1))];
    let codes = required_item_codes(&lines);

    let snapshot = RoutingSnapshot::load(
        &db.storage,
        "INV-2",
        &BranchName::from(BRANCH),
        Some(&RoomName::from(ROOM)),
        &codes,
    )
    .await
    .unwrap();

    let unrouted = unrouted_item_codes(&lines, snapshot.units(), snapshot.item_groups_map());
    assert_eq!(unrouted, vec![ItemCode::from("ORPHAN")]);

    // The rest of the order still routes: one mis-configured item must not stop a table
    // being fed.
    let tickets = route_items_to_stations(&ctx("INV-2"), &lines, &snapshot.repos()).unwrap();
    assert_eq!(tickets.len(), 1);
    let items: Vec<&str> = tickets[0].kot_items.iter().map(|i| i.item.as_str()).collect();
    assert_eq!(items, vec!["BIRYANI"]);
}

#[tokio::test]
async fn a_station_that_already_printed_gets_an_order_modified_ticket() {
    let db = TestDb::new().await;
    seed(&db).await;
    seed_production_units(&db).await;

    // A submitted ticket already exists for the Bar on INV-3.
    sqlx::query(
        "INSERT INTO kots (name, naming_series, invoice, date, kot_type, production, branch)
         VALUES ('KOT-00001', 'KOT-', 'INV-3', '2026-07-15', 'NewOrder', 'Bar', $1)",
    )
    .bind(BRANCH)
    .execute(db.pool())
    .await
    .unwrap();

    let lines = vec![order_line("BIRYANI", dec!(1)), order_line("CHAI", dec!(1))];
    let codes = required_item_codes(&lines);

    let snapshot = RoutingSnapshot::load(
        &db.storage,
        "INV-3",
        &BranchName::from(BRANCH),
        Some(&RoomName::from(ROOM)),
        &codes,
    )
    .await
    .unwrap();

    let tickets = route_items_to_stations(&ctx("INV-3"), &lines, &snapshot.repos()).unwrap();

    let kitchen = tickets
        .iter()
        .find(|k| k.production.as_ref().map(|p| p.as_str()) == Some("Hot Kitchen"))
        .unwrap();
    let bar = tickets
        .iter()
        .find(|k| k.production.as_ref().map(|p| p.as_str()) == Some("Bar"))
        .unwrap();

    // Deviation 2: probed per station, so one modified station cannot mislabel the others.
    assert_eq!(bar.kot_type, KotType::OrderModified);
    assert_eq!(kitchen.kot_type, KotType::NewOrder);
}

#[tokio::test]
async fn a_takeaway_order_routes_without_a_room_and_carries_no_course() {
    // Deviation 5: no room, no course lookup. The tickets still route.
    let db = TestDb::new().await;
    seed(&db).await;
    seed_production_units(&db).await;

    let lines = vec![order_line("BIRYANI", dec!(1))];
    let codes = required_item_codes(&lines);

    let snapshot = RoutingSnapshot::load(
        &db.storage,
        "INV-4",
        &BranchName::from(BRANCH),
        None,
        &codes,
    )
    .await
    .unwrap();

    let mut takeaway = ctx("INV-4");
    takeaway.room = None;
    takeaway.restaurant_table = None;
    takeaway.table_takeaway = true;

    let tickets = route_items_to_stations(&takeaway, &lines, &snapshot.repos()).unwrap();
    assert_eq!(tickets.len(), 1);
    assert!(tickets[0].kot_items.iter().all(|i| i.course.is_none()));
}

#[tokio::test]
async fn a_branch_with_no_production_units_yields_no_tickets() {
    // Deviation 3: upstream `frappe.throw`s here. Returning an empty list instead means a
    // mis-configured branch does not take the whole order down.
    let db = TestDb::new().await;
    seed(&db).await;

    let lines = vec![order_line("BIRYANI", dec!(1))];
    let snapshot = RoutingSnapshot::load(
        &db.storage,
        "INV-5",
        &BranchName::from(BRANCH),
        Some(&RoomName::from(ROOM)),
        &required_item_codes(&lines),
    )
    .await
    .unwrap();

    assert!(snapshot.units().is_empty());
    assert!(route_items_to_stations(&ctx("INV-5"), &lines, &snapshot.repos())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn the_snapshot_serves_routing_without_blocking_on_a_current_thread_runtime() {
    // The point of prefetching: routing runs with zero I/O, so it is safe from any runtime
    // flavour. `PgItemRepo`'s blocking port would panic here (blocking.rs documents why).
    let db = TestDb::new().await;
    seed(&db).await;
    seed_production_units(&db).await;

    let lines = vec![order_line("BIRYANI", dec!(1)), order_line("CHAI", dec!(1))];
    let snapshot = RoutingSnapshot::load(
        &db.storage,
        "INV-6",
        &BranchName::from(BRANCH),
        Some(&RoomName::from(ROOM)),
        &required_item_codes(&lines),
    )
    .await
    .unwrap();

    let tickets = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                route_items_to_stations(&ctx("INV-6"), &lines, &snapshot.repos()).unwrap()
            })
    })
    .join()
    .expect("routing must not panic on a current-thread runtime");

    assert_eq!(tickets.len(), 2);
}

// `multi_thread` is required, not incidental: the sync ports block via
// `block_in_place`, which needs a sibling worker to hand the reactor to. `blocking.rs`
// documents why a current-thread runtime panics here rather than deadlocking.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_blocking_item_port_agrees_with_the_async_one() {
    let db = TestDb::new().await;
    seed(&db).await;
    let repo = PgItemRepo::new(db.storage.clone());
    let codes = vec![ItemCode::from("CHAI"), ItemCode::from("BIRYANI")];

    let via_async = repo.item_groups_async(&codes).await.unwrap();
    let via_port = repo.item_groups(&codes).unwrap();

    assert_eq!(via_async, via_port);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_blocking_production_port_agrees_with_the_async_one() {
    let db = TestDb::new().await;
    seed(&db).await;
    seed_production_units(&db).await;
    let repo = PgProductionRepo::new(db.storage.clone());
    let branch = BranchName::from(BRANCH);

    let via_async = repo.list_for_branch_async(&branch).await.unwrap();
    let via_port = repo.list_for_branch(&branch).unwrap();

    assert_eq!(via_async, via_port);
}

// ===========================================================================
// 7. KOT lifecycle: pending → prepared
// ===========================================================================

#[tokio::test]
async fn a_prepared_kot_leaves_the_pending_list() {
    use peacock_storage::repos::PgKotRepo;

    let db = TestDb::new().await;
    seed(&db).await;
    seed_production_units(&db).await;
    let kots = PgKotRepo::new(db.storage.clone());

    let lines = vec![order_line("BIRYANI", dec!(2))];
    let snapshot = RoutingSnapshot::load(
        &db.storage,
        "INV-7",
        &BranchName::from(BRANCH),
        Some(&RoomName::from(ROOM)),
        &required_item_codes(&lines),
    )
    .await
    .unwrap();

    let routed = route_items_to_stations(&ctx("INV-7"), &lines, &snapshot.repos()).unwrap();
    let created = kots.create(routed[0].clone()).await.expect("persist ticket");
    let name = created.name.clone().expect("the sequence assigns a name");

    let unit = ProductionUnitName::from("Hot Kitchen");
    assert_eq!(
        kots.list_unprepared_for_production(&unit).await.unwrap().len(),
        1,
        "a fresh ticket is outstanding work"
    );

    let prepared = kots.mark_prepared(&name, None).await.expect("mark prepared");
    assert_eq!(prepared.order_status.as_deref(), Some("Prepared"));
    assert!(prepared.start_time_prep.is_some());

    assert!(
        kots.list_unprepared_for_production(&unit)
            .await
            .unwrap()
            .is_empty(),
        "a finished ticket must leave the display, or the queue only grows"
    );
}

#[tokio::test]
async fn marking_a_kot_prepared_twice_does_not_move_the_timestamp() {
    use peacock_storage::repos::PgKotRepo;

    let db = TestDb::new().await;
    seed(&db).await;
    seed_production_units(&db).await;
    let kots = PgKotRepo::new(db.storage.clone());

    let lines = vec![order_line("BIRYANI", dec!(1))];
    let snapshot = RoutingSnapshot::load(
        &db.storage,
        "INV-8",
        &BranchName::from(BRANCH),
        Some(&RoomName::from(ROOM)),
        &required_item_codes(&lines),
    )
    .await
    .unwrap();
    let routed = route_items_to_stations(&ctx("INV-8"), &lines, &snapshot.repos()).unwrap();
    let name = kots
        .create(routed[0].clone())
        .await
        .unwrap()
        .name
        .unwrap();

    let first = kots.mark_prepared(&name, None).await.unwrap();
    let second = kots.mark_prepared(&name, None).await.unwrap();

    assert_eq!(
        first.start_time_prep, second.start_time_prep,
        "a double-tapped display must not move the figure the service-time report reads"
    );
}

#[tokio::test]
async fn marking_an_unknown_kot_prepared_reports_it_missing() {
    use peacock_core::ids::KotName;
    use peacock_storage::repos::PgKotRepo;

    let db = TestDb::new().await;
    let err = PgKotRepo::new(db.storage.clone())
        .mark_prepared(&KotName::from("KOT-99999"), None)
        .await
        .expect_err("no such KOT");

    assert!(err.to_string().contains("not found"), "got {err}");
}
