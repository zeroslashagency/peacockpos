//! Invoice repository — Lane 2F. **Money lane.**
//!
//! Implements the `SeriesAllocator` and `IdempotencyStore` ports from
//! [`peacock_core::invoicing`] against Postgres, plus the invoice + line CRUD the rest
//! of Phase 2 needs.
//!
//! # Gapless numbering: why a row lock and not a sequence
//!
//! `nextval()` is exempt from rollback by design. Every failed invoice insert would
//! burn a number, and CGST Rule 46(b) forbids the gap. So the counter is a row in
//! `invoice_naming_series` and allocation is the single statement
//! [`peacock_core::invoicing::SeriesAllocator`] specifies:
//!
//! ```sql
//! UPDATE invoice_naming_series
//!    SET next_number = next_number + 1
//!  WHERE series = $1 AND fiscal_year = $2
//! RETURNING next_number - 1
//! ```
//!
//! The `UPDATE` takes a row lock held to commit, so concurrent allocations queue and
//! each sees the previous one's value. The increment rolls back with the transaction,
//! which is what `invoicing.rs::rolled_back_allocation_does_not_burn_number` pins.
//! 005_invoice.sql carries the full argument.
//!
//! # Isolation: READ COMMITTED, deliberately
//!
//! [`Storage::with_serializable_retry`] exists for this lane and this lane does not
//! use it. Correctness here comes from the row lock, not the isolation level:
//!
//! * At READ COMMITTED an `UPDATE` that blocks on a locked row re-reads the committed
//!   value when the lock clears and computes from that. Sequential numbers, no
//!   aborts, at any concurrency.
//! * At SERIALIZABLE the same contention surfaces as 40001 aborts to retry. Identical
//!   results, strictly more work, and under the 100-way concurrent load this lane must
//!   survive the retry storm is the failure mode.
//!
//! `with_serializable_retry` remains right for a read-then-write pattern where the
//! read is not itself a lock (Lane 2E's sequence + insert). Ours is a locking write.
//! `create_invoice_idempotent` retries once on the narrow race described there.
//!
//! # Money
//!
//! Every column is `NUMERIC(18,6)` and crosses this boundary as `rust_decimal::Decimal`.
//! No `f64` appears anywhere in this file. The parity harness proves the arithmetic in
//! `peacock-core`; the CHECK constraints in 005_invoice.sql prove storage cannot then
//! contradict it.

use std::collections::HashMap;

use peacock_core::error::{Error as DomainError, Result as DomainResult};
use peacock_core::ids::{BranchName, InvoiceName, ItemCode, MenuCourseName, TableName};
use peacock_core::invoicing::{IdempotencyStore, SeriesAllocator};
use peacock_core::model::PosInvoiceStatus;
use peacock_core::money::Money;
use peacock_core::tax::{DiscountBasis, InvoiceTotals, SupplyType, TaxBreakdown};
use rust_decimal::Decimal;
use sqlx::types::chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{on_missing, StorageError, StorageResult};
use crate::Storage;

/// Retries for the one race `create_invoice_idempotent` can lose: two concurrent
/// requests carrying the *same* key, where both miss the lookup and one loses the
/// insert. One retry is enough — the winner has committed by then, so the retry's
/// lookup hits.
const IDEMPOTENCY_REPLAY_ATTEMPTS: u32 = 2;

// ---------------------------------------------------------------------------
// Status mapping
// ---------------------------------------------------------------------------

/// `PosInvoiceStatus` as the `invoice_status` enum labels. Exhaustive on purpose: a new
/// variant in the domain must fail to compile here rather than fall through to a
/// default that silently mis-files revenue (bug 4's shape).
fn status_to_str(status: PosInvoiceStatus) -> &'static str {
    match status {
        PosInvoiceStatus::Draft => "Draft",
        PosInvoiceStatus::Paid => "Paid",
        PosInvoiceStatus::Consolidated => "Consolidated",
        PosInvoiceStatus::Return => "Return",
    }
}

/// Inverse of [`status_to_str`].
///
/// Returns an error rather than defaulting: an unrecognised label means the enum and
/// this code have diverged, and guessing "Draft" would drop a paid invoice out of
/// revenue. The `invoice_status` enum type makes this unreachable through normal
/// writes; it is reachable through a migration that adds a label without updating here.
fn status_from_str(raw: &str) -> StorageResult<PosInvoiceStatus> {
    match raw {
        "Draft" => Ok(PosInvoiceStatus::Draft),
        "Paid" => Ok(PosInvoiceStatus::Paid),
        "Consolidated" => Ok(PosInvoiceStatus::Consolidated),
        "Return" => Ok(PosInvoiceStatus::Return),
        other => Err(StorageError::Constraint {
            table: "invoices".to_owned(),
            constraint: "invoice_status".to_owned(),
            message: format!("unknown invoice status {other:?}"),
        }),
    }
}

fn supply_type_to_str(s: SupplyType) -> &'static str {
    match s {
        SupplyType::Intrastate => "Intrastate",
        SupplyType::Interstate => "Interstate",
    }
}

fn supply_type_from_str(raw: &str) -> StorageResult<SupplyType> {
    match raw {
        "Intrastate" => Ok(SupplyType::Intrastate),
        "Interstate" => Ok(SupplyType::Interstate),
        other => Err(StorageError::Constraint {
            table: "invoices".to_owned(),
            constraint: "invoices_supply_type_check".to_owned(),
            message: format!("unknown supply type {other:?}"),
        }),
    }
}

fn discount_basis_to_str(b: DiscountBasis) -> &'static str {
    match b {
        DiscountBasis::NetTotal => "NetTotal",
        DiscountBasis::GrandTotal => "GrandTotal",
    }
}

fn discount_basis_from_str(raw: &str) -> StorageResult<DiscountBasis> {
    match raw {
        "NetTotal" => Ok(DiscountBasis::NetTotal),
        "GrandTotal" => Ok(DiscountBasis::GrandTotal),
        other => Err(StorageError::Constraint {
            table: "invoices".to_owned(),
            constraint: "invoices_discount_basis_check".to_owned(),
            message: format!("unknown discount basis {other:?}"),
        }),
    }
}

/// True when the transition is one the schema trigger will accept.
///
/// Mirrors `invoice_status_transition_allowed` in 005_invoice.sql. Duplicated so the
/// repository can reject early with a typed domain error instead of round-tripping to
/// the database for a 23514; the trigger stays the authority, because it also covers
/// writes that never pass through this code.
pub fn transition_allowed(from: PosInvoiceStatus, to: PosInvoiceStatus) -> bool {
    use PosInvoiceStatus::*;
    from == to
        || matches!(
            (from, to),
            (Draft, Paid) | (Paid, Consolidated) | (Paid, Return)
        )
}

// ---------------------------------------------------------------------------
// Input / output shapes
// ---------------------------------------------------------------------------

/// One line as it goes into `invoice_lines`.
///
/// `amount` is not carried: it is `qty * rate`, computed on insert so the stored value
/// and the CHECK constraint cannot disagree.
#[derive(Debug, Clone, PartialEq)]
pub struct NewInvoiceLine {
    pub item_code: ItemCode,
    pub item_name: String,
    pub qty: Decimal,
    pub rate: Money,
    /// `None` until the menu HSN backfill lands. See `tax.rs`.
    pub hsn_sac: Option<String>,
    pub course: Option<MenuCourseName>,
    pub comments: Option<String>,
    pub serve_priority: i32,
    pub indicate_course: bool,
}

impl NewInvoiceLine {
    pub fn amount(&self) -> Money {
        self.rate * self.qty
    }
}

/// Everything needed to write an invoice, minus the number (this repo allocates it).
///
/// `totals` is the [`InvoiceTotals`] the domain already computed. It is stored, never
/// recomputed here: `peacock_core::tax::compute_totals` is the single arithmetic the
/// parity harness validates, and a second implementation in SQL would be a second
/// thing to keep in step.
#[derive(Debug, Clone, PartialEq)]
pub struct NewInvoice {
    pub naming_series: String,
    pub fiscal_year: String,
    pub restaurant: Option<String>,
    pub restaurant_table: Option<TableName>,
    pub restaurant_room: Option<String>,
    pub branch: BranchName,
    pub pos_profile: Option<String>,
    pub customer: String,
    pub waiter: Option<String>,
    pub cashier: Option<String>,
    pub no_of_pax: i32,
    pub order_type: Option<String>,
    pub posted_at: DateTime<Utc>,
    pub business_day: NaiveDate,
    pub supply_type: SupplyType,
    pub discount_basis: DiscountBasis,
    pub tax_rate: Decimal,
    pub totals: InvoiceTotals,
    pub paid_amount: Money,
    pub change_amount: Money,
    pub comments: Option<String>,
    pub lines: Vec<NewInvoiceLine>,
}

/// Payment instrument, mirroring the `payment_method` enum in 008_invoice_payments.sql.
///
/// A closed enum rather than a string because the Z-report branches on `Cash` for the
/// drawer total and the CGST Rule 56 ₹10k threshold. A typo'd label would drop a note
/// out of both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaymentMethod {
    Cash,
    Card,
    Upi,
    Wallet,
    Credit,
}

impl PaymentMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            PaymentMethod::Cash => "Cash",
            PaymentMethod::Card => "Card",
            PaymentMethod::Upi => "Upi",
            PaymentMethod::Wallet => "Wallet",
            PaymentMethod::Credit => "Credit",
        }
    }

    /// Counts toward the CGST Rule 56 cash drawer total.
    pub fn is_cash(self) -> bool {
        matches!(self, PaymentMethod::Cash)
    }

    /// Inverse of [`PaymentMethod::as_str`].
    ///
    /// Errors rather than defaulting: an unrecognised label means the enum type and
    /// this code have diverged, and guessing `Cash` would invent drawer cash that is
    /// not there.
    ///
    /// Not `std::str::FromStr`: that trait's `Err` would have to be `StorageError`, which
    /// would let `"Cash".parse()` succeed anywhere in the workspace and quietly widen a
    /// storage-layer mapping into a public conversion. The label set is a column
    /// definition, not a general parse.
    pub fn parse_label(raw: &str) -> StorageResult<Self> {
        match raw {
            "Cash" => Ok(PaymentMethod::Cash),
            "Card" => Ok(PaymentMethod::Card),
            "Upi" => Ok(PaymentMethod::Upi),
            "Wallet" => Ok(PaymentMethod::Wallet),
            "Credit" => Ok(PaymentMethod::Credit),
            other => Err(StorageError::Constraint {
                table: "invoice_payments".to_owned(),
                constraint: "payment_method".to_owned(),
                message: format!("unknown payment method {other:?}"),
            }),
        }
    }
}

/// A payment to record against an invoice.
#[derive(Debug, Clone, PartialEq)]
pub struct NewPayment {
    pub method: PaymentMethod,
    pub amount: Money,
    pub reference: Option<String>,
    pub paid_at: DateTime<Utc>,
}

/// A payment as stored, with the `idx` the database assigned.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredPayment {
    pub idx: i32,
    pub method: PaymentMethod,
    pub amount: Money,
    pub reference: Option<String>,
    pub paid_at: DateTime<Utc>,
}

/// A stored invoice, read back.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredInvoice {
    pub name: InvoiceName,
    pub naming_series: String,
    pub fiscal_year: String,
    pub series_number: u64,
    pub status: PosInvoiceStatus,
    pub restaurant: Option<String>,
    pub restaurant_table: Option<TableName>,
    pub restaurant_room: Option<String>,
    pub branch: BranchName,
    pub pos_profile: Option<String>,
    pub customer: String,
    pub waiter: Option<String>,
    pub cashier: Option<String>,
    pub no_of_pax: i32,
    pub order_type: Option<String>,
    pub posted_at: DateTime<Utc>,
    pub business_day: NaiveDate,
    pub supply_type: SupplyType,
    pub discount_basis: DiscountBasis,
    pub tax_rate: Decimal,
    pub totals: InvoiceTotals,
    pub paid_amount: Money,
    pub change_amount: Money,
    pub invoice_printed: bool,
    pub cancel_reason: Option<String>,
    pub comments: Option<String>,
    pub lines: Vec<StoredInvoiceLine>,
    /// Recorded tenders, ordered by `idx`. `paid_amount` above is their sum, kept by
    /// the trigger in 008_invoice_payments.sql — this is the evidence for it.
    pub payments: Vec<StoredPayment>,
}

impl StoredInvoice {
    /// What is still owed against `rounded_total` — the figure the customer pays.
    ///
    /// `rounded_total`, not `grand_total`: settling against the unrounded figure is
    /// `businessday.rs` bug 3 and leaves a sub-rupee residue on every cash bill.
    pub fn outstanding_amount(&self) -> Money {
        self.totals.rounded_total - self.paid_amount
    }

    /// True once the tenders cover the bill exactly.
    pub fn is_settled(&self) -> bool {
        self.paid_amount == self.totals.rounded_total
    }

    /// Drawer cash on this invoice — the CGST Rule 56 input.
    pub fn cash_total(&self) -> Money {
        self.payments
            .iter()
            .filter(|p| p.method.is_cash())
            .map(|p| p.amount)
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredInvoiceLine {
    pub idx: i32,
    pub item_code: ItemCode,
    pub item_name: String,
    pub qty: Decimal,
    pub rate: Money,
    pub amount: Money,
    pub hsn_sac: Option<String>,
    pub course: Option<MenuCourseName>,
    pub comments: Option<String>,
    pub serve_priority: i32,
    pub indicate_course: bool,
}

/// What [`PgInvoiceRepo::create_invoice_idempotent`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateOutcome {
    /// The key was new: a number was allocated and the invoice written.
    Created,
    /// The key had been seen: the existing invoice came back and no number was burned.
    Replayed,
}

/// An invoice plus whether it was freshly created or replayed.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatedInvoice {
    pub invoice: StoredInvoice,
    pub outcome: CreateOutcome,
}

// ---------------------------------------------------------------------------
// Repository
// ---------------------------------------------------------------------------

/// Postgres-backed invoice repository.
#[derive(Clone)]
pub struct PgInvoiceRepo {
    storage: Storage,
}

impl PgInvoiceRepo {
    pub fn new(storage: Storage) -> Self {
        PgInvoiceRepo { storage }
    }

    fn pool(&self) -> &PgPool {
        self.storage.pool()
    }

    // -----------------------------------------------------------------------
    // Naming series
    // -----------------------------------------------------------------------

    /// Register a series for a fiscal year, starting at `start`.
    ///
    /// Idempotent: re-registering an existing series leaves its counter alone rather
    /// than rewinding it onto already-issued numbers.
    pub async fn register_series(
        &self,
        series: &str,
        fiscal_year: &str,
        start: u64,
    ) -> StorageResult<()> {
        sqlx::query(
            "INSERT INTO invoice_naming_series (series, fiscal_year, next_number)
             VALUES ($1, $2, $3)
             ON CONFLICT (series, fiscal_year) DO NOTHING",
        )
        .bind(series)
        .bind(fiscal_year)
        .bind(u64_to_i64(start)?)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The number the next allocation will hand out, without consuming it.
    pub async fn peek_series(&self, series: &str, fiscal_year: &str) -> StorageResult<Option<u64>> {
        let next: Option<i64> = sqlx::query_scalar(
            "SELECT next_number FROM invoice_naming_series
             WHERE series = $1 AND fiscal_year = $2",
        )
        .bind(series)
        .bind(fiscal_year)
        .fetch_optional(self.pool())
        .await?;

        Ok(next.map(|n| n as u64))
    }

    // -----------------------------------------------------------------------
    // Create
    // -----------------------------------------------------------------------

    /// Allocate a number and write the invoice, or replay a previous result.
    ///
    /// One transaction covers the lookup, the counter increment, the invoice, the lines
    /// and the key row. Every failure path therefore rolls the counter back and no
    /// number is burned.
    ///
    /// ## Ordering, and why the lookup comes first
    ///
    /// The replay check runs before the `UPDATE`, so a replay never touches the
    /// counter. That is the invariant `invoicing.rs` calls critical: incrementing first
    /// and discarding the number on a replay would gap the series.
    ///
    /// ## The race, and the retry
    ///
    /// Two concurrent requests with the same key can both miss the lookup. Both
    /// allocate, one loses the unique insert on `idempotency_keys.key` and its whole
    /// transaction — including its counter increment — rolls back. The retry's lookup
    /// then finds the winner's invoice. Net effect: one number, one invoice, no gap.
    pub async fn create_invoice_idempotent(
        &self,
        idempotency_key: Uuid,
        new_invoice: &NewInvoice,
    ) -> StorageResult<CreatedInvoice> {
        let mut last_conflict: Option<StorageError> = None;

        for attempt in 1..=IDEMPOTENCY_REPLAY_ATTEMPTS {
            let mut tx = self.storage.begin().await?;

            // Replay first: a seen key must not reach the counter.
            if let Some(existing) = lookup_key(&mut tx, idempotency_key).await? {
                let invoice = load_invoice(&mut tx, &existing).await?;
                tx.commit().await?;
                return Ok(CreatedInvoice {
                    invoice,
                    outcome: CreateOutcome::Replayed,
                });
            }

            match self
                .insert_new_invoice(&mut tx, idempotency_key, new_invoice)
                .await
            {
                Ok(invoice) => {
                    tx.commit().await?;
                    return Ok(CreatedInvoice {
                        invoice,
                        outcome: CreateOutcome::Created,
                    });
                }
                Err(err) => {
                    // Rollback restores the counter: this is the no-burn guarantee.
                    let _ = tx.rollback().await;

                    let is_key_race = crate::error::is_unique_violation(
                        &err,
                        "idempotency_keys_pkey",
                    );
                    if !is_key_race || attempt == IDEMPOTENCY_REPLAY_ATTEMPTS {
                        return Err(err);
                    }

                    tracing::warn!(
                        target: "peacock_storage",
                        attempt,
                        key = %idempotency_key,
                        "concurrent request replayed the same idempotency key, re-reading"
                    );
                    last_conflict = Some(err);
                }
            }
        }

        Err(last_conflict.unwrap_or(StorageError::Constraint {
            table: "idempotency_keys".to_owned(),
            constraint: "idempotency_keys_pkey".to_owned(),
            message: "exhausted idempotency replay attempts".to_owned(),
        }))
    }

    /// Allocate + insert, inside a caller-owned transaction.
    ///
    /// `pub(crate)` because `repos::order` needs the *same* transaction to carry the
    /// invoice insert and the `orders.last_invoice` update: an order that points at an
    /// invoice which was rolled back, or an invoice whose order never learned about it,
    /// are both states no later read can repair. Re-implementing the insert there would
    /// be a second copy of the Rule 46(b) allocation to keep in step.
    pub(crate) async fn insert_new_invoice(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        idempotency_key: Uuid,
        new_invoice: &NewInvoice,
    ) -> StorageResult<StoredInvoice> {
        let series_number =
            allocate_number(tx, &new_invoice.naming_series, &new_invoice.fiscal_year).await?;

        // Formatting and the 16-character cap belong to the domain
        // (`Error::InvoiceNameTooLong`). Called after the counter moved but inside the
        // transaction, so a rejection rolls the increment back and burns nothing —
        // exactly what `over_long_series_does_not_burn_a_number` asserts.
        let name = format_invoice_name(
            &new_invoice.naming_series,
            &new_invoice.fiscal_year,
            series_number,
        )?;

        let totals = &new_invoice.totals;

        sqlx::query(
            "INSERT INTO invoices (
                 name, naming_series, fiscal_year, series_number, status,
                 restaurant, restaurant_table, restaurant_room, branch, pos_profile,
                 customer, waiter, cashier, no_of_pax, order_type,
                 posted_at, business_day,
                 supply_type, discount_basis, tax_rate,
                 net_total, discount, taxable_value,
                 cgst, sgst, igst, total_tax,
                 grand_total, rounded_total, round_off,
                 paid_amount, change_amount, comments
             ) VALUES (
                 $1, $2, $3, $4, 'Draft'::invoice_status,
                 $5, $6, $7, $8, $9,
                 $10, $11, $12, $13, $14,
                 $15, $16,
                 $17, $18, $19,
                 $20, $21, $22,
                 $23, $24, $25, $26,
                 $27, $28, $29,
                 $30, $31, $32
             )",
        )
        .bind(name.as_str())
        .bind(&new_invoice.naming_series)
        .bind(&new_invoice.fiscal_year)
        .bind(u64_to_i64(series_number)?)
        .bind(new_invoice.restaurant.as_deref())
        .bind(new_invoice.restaurant_table.as_ref().map(|t| t.as_str()))
        .bind(new_invoice.restaurant_room.as_deref())
        .bind(new_invoice.branch.as_str())
        .bind(new_invoice.pos_profile.as_deref())
        .bind(&new_invoice.customer)
        .bind(new_invoice.waiter.as_deref())
        .bind(new_invoice.cashier.as_deref())
        .bind(new_invoice.no_of_pax)
        .bind(new_invoice.order_type.as_deref())
        .bind(new_invoice.posted_at)
        .bind(new_invoice.business_day)
        .bind(supply_type_to_str(new_invoice.supply_type))
        .bind(discount_basis_to_str(new_invoice.discount_basis))
        .bind(new_invoice.tax_rate)
        .bind(totals.net_total.inner())
        .bind(totals.discount.inner())
        .bind(totals.taxable_value.inner())
        .bind(totals.tax.cgst.inner())
        .bind(totals.tax.sgst.inner())
        .bind(totals.tax.igst.inner())
        .bind(totals.tax.total_tax.inner())
        .bind(totals.grand_total.inner())
        .bind(totals.rounded_total.inner())
        .bind(totals.round_off.inner())
        .bind(new_invoice.paid_amount.inner())
        .bind(new_invoice.change_amount.inner())
        .bind(new_invoice.comments.as_deref())
        .execute(&mut **tx)
        .await?;

        insert_lines(tx, &name, &new_invoice.lines).await?;

        // The critical link: key and number committed together (invoicing.rs).
        sqlx::query("INSERT INTO idempotency_keys (key, invoice) VALUES ($1, $2)")
            .bind(idempotency_key)
            .bind(name.as_str())
            .execute(&mut **tx)
            .await?;

        load_invoice(tx, &name).await
    }

    // -----------------------------------------------------------------------
    // Read
    // -----------------------------------------------------------------------

    /// Fetch an invoice with its lines, ordered by `idx`.
    pub async fn get(&self, name: &InvoiceName) -> StorageResult<StoredInvoice> {
        let mut tx = self.storage.begin().await?;
        let invoice = load_invoice(&mut tx, name).await?;
        tx.commit().await?;
        Ok(invoice)
    }

    pub async fn find(&self, name: &InvoiceName) -> StorageResult<Option<StoredInvoice>> {
        match self.get(name).await {
            Ok(inv) => Ok(Some(inv)),
            Err(StorageError::Domain(DomainError::Conflict { .. })) => Ok(None),
            Err(e) if is_missing_invoice(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Invoices posted in `[start, end)`.
    ///
    /// Half-open on `posted_at`, which is the bug 2 fix (`businessday.rs`): upstream
    /// filtered a DATE column against datetime bounds and counted a 01:30 order in two
    /// shifts. An instant equal to `end` belongs to the next window, never both.
    pub async fn list_by_posted_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> StorageResult<Vec<StoredInvoice>> {
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM invoices
             WHERE posted_at >= $1 AND posted_at < $2
             ORDER BY posted_at, name",
        )
        .bind(start)
        .bind(end)
        .fetch_all(self.pool())
        .await?;

        self.load_many(&names).await
    }

    pub async fn list_by_status(
        &self,
        status: PosInvoiceStatus,
    ) -> StorageResult<Vec<StoredInvoice>> {
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM invoices
             WHERE status = $1::invoice_status
             ORDER BY posted_at, name",
        )
        .bind(status_to_str(status))
        .fetch_all(self.pool())
        .await?;

        self.load_many(&names).await
    }

    pub async fn list_by_table(&self, table: &TableName) -> StorageResult<Vec<StoredInvoice>> {
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM invoices
             WHERE restaurant_table = $1
             ORDER BY posted_at, name",
        )
        .bind(table.as_str())
        .fetch_all(self.pool())
        .await?;

        self.load_many(&names).await
    }

    /// Revenue-counting invoices for a branch on a business day.
    ///
    /// Filtered by [`PosInvoiceStatus::REVENUE`] — the single definition shift close
    /// and the P&L both use, which is the bug 4 fix. Encoding either status list
    /// literally here would recreate the disagreement.
    pub async fn list_revenue_for_business_day(
        &self,
        branch: &BranchName,
        business_day: NaiveDate,
    ) -> StorageResult<Vec<StoredInvoice>> {
        let statuses: Vec<&str> = PosInvoiceStatus::REVENUE
            .iter()
            .copied()
            .map(status_to_str)
            .collect();

        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM invoices
             WHERE branch = $1
               AND business_day = $2
               AND status::TEXT = ANY($3)
             ORDER BY posted_at, name",
        )
        .bind(branch.as_str())
        .bind(business_day)
        .bind(&statuses)
        .fetch_all(self.pool())
        .await?;

        self.load_many(&names).await
    }

    /// The `GET /api/invoices` query: any combination of business-day range, status and
    /// table, all optional.
    ///
    /// Filtered on **business day**, not on `posted_at`'s calendar date. A 01:30 invoice
    /// belongs to the previous business day, and filtering it by calendar date is upstream
    /// bug 2 (`sub_pos_closing.py:42`). The range is inclusive on both ends because a
    /// caller asking for `from = to = D` means that one business day — unlike
    /// [`PgInvoiceRepo::list_by_posted_range`], which is half-open over instants.
    ///
    /// Every predicate is `($n IS NULL OR …)` so one prepared statement serves all
    /// sixteen filter combinations. The planner still uses `invoices_business_day_idx` /
    /// `invoices_status_idx` for the arms that are supplied.
    pub async fn list_filtered(
        &self,
        from: Option<NaiveDate>,
        to: Option<NaiveDate>,
        status: Option<PosInvoiceStatus>,
        table: Option<&TableName>,
    ) -> StorageResult<Vec<StoredInvoice>> {
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM invoices
              WHERE ($1::DATE IS NULL OR business_day >= $1)
                AND ($2::DATE IS NULL OR business_day <= $2)
                AND ($3::TEXT IS NULL OR status::TEXT = $3)
                AND ($4::TEXT IS NULL OR restaurant_table = $4)
              ORDER BY posted_at, name",
        )
        .bind(from)
        .bind(to)
        .bind(status.map(status_to_str))
        .bind(table.map(|t| t.as_str()))
        .fetch_all(self.pool())
        .await?;

        self.load_many(&names).await
    }

    async fn load_many(&self, names: &[String]) -> StorageResult<Vec<StoredInvoice>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }

        let mut tx = self.storage.begin().await?;
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            out.push(load_invoice(&mut tx, &InvoiceName::from(name.as_str())).await?);
        }
        tx.commit().await?;
        Ok(out)
    }

    /// Invoice summaries for a posted_at range (Lane W1-C reports).
    ///
    /// Returns minimal invoice data for revenue calculation. Half-open `[start, end)`,
    /// matching [`PgInvoiceRepo::list_by_posted_range`].
    pub async fn summaries_between(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> StorageResult<Vec<peacock_core::businessday::InvoiceSummary>> {
        #[derive(sqlx::FromRow)]
        struct SummaryRow {
            name: String,
            posted_at: DateTime<Utc>,
            status: String,
            totals_grand_total: Decimal,
            totals_rounded_total: Decimal,
        }

        let rows = sqlx::query_as::<_, SummaryRow>(
            "SELECT name, posted_at, status::TEXT as status,
                    grand_total as totals_grand_total,
                    rounded_total as totals_rounded_total
             FROM invoices
             WHERE posted_at >= $1 AND posted_at < $2
             ORDER BY posted_at, name"
        )
        .bind(start)
        .bind(end)
        .fetch_all(self.pool())
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let status = status_from_str(&row.status)?;
            let grand_total = Money::new(row.totals_grand_total);
            let rounded_total = Money::new(row.totals_rounded_total);
            let round_off = rounded_total - grand_total;

            out.push(peacock_core::businessday::InvoiceSummary {
                name: row.name,
                posted_at: row.posted_at,
                status,
                grand_total,
                rounded_total,
                round_off,
            });
        }

        Ok(out)
    }

    /// Revenue-counting invoice lines for a posted_at range (Lane W1-C COGS).
    ///
    /// Filters invoices by `PosInvoiceStatus::REVENUE`, then flattens their lines.
    /// Half-open `[start, end)`.
    pub async fn revenue_lines_between(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> StorageResult<Vec<peacock_core::model::OrderLine>> {
        let statuses: Vec<&str> = PosInvoiceStatus::REVENUE
            .iter()
            .copied()
            .map(status_to_str)
            .collect();

        #[derive(sqlx::FromRow)]
        struct LineRow {
            item_code: String,
            item_name: String,
            qty: Decimal,
            rate: Decimal,
            comments: Option<String>,
            serve_priority: i32,
            indicate_course: bool,
        }

        let rows = sqlx::query_as::<_, LineRow>(
            "SELECT l.item_code, l.item_name, l.qty, l.rate,
                    l.comments, l.serve_priority, l.indicate_course
             FROM invoice_lines l
             JOIN invoices i ON l.invoice = i.name
             WHERE i.posted_at >= $1 AND i.posted_at < $2
               AND i.status::TEXT = ANY($3)
             ORDER BY i.posted_at, i.name, l.idx"
        )
        .bind(start)
        .bind(end)
        .bind(&statuses)
        .fetch_all(self.pool())
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(peacock_core::model::OrderLine {
                item_code: peacock_core::ids::ItemCode::from(row.item_code.as_str()),
                item_name: row.item_name,
                qty: row.qty,
                rate: Money::new(row.rate),
                comments: row.comments,
                serve_priority: row.serve_priority,
                indicate_course: row.indicate_course,
            });
        }

        Ok(out)
    }

    // -----------------------------------------------------------------------
    // Status transitions
    // -----------------------------------------------------------------------

    /// Move an invoice to `to`, refusing anything but a legal edge.
    ///
    /// Draft → Paid → Consolidated, plus Paid → Return, plus the same-status no-op so a
    /// retried "mark paid" is not an error. Rejected early with a typed
    /// [`DomainError::Conflict`]; the trigger in 005_invoice.sql is the backstop for
    /// writes that never come through here.
    pub async fn set_status(
        &self,
        name: &InvoiceName,
        to: PosInvoiceStatus,
    ) -> StorageResult<StoredInvoice> {
        let mut tx = self.storage.begin().await?;

        // FOR UPDATE: read and write the status under one lock so two concurrent
        // transitions cannot both pass the check from the same starting value.
        let current: Option<String> = sqlx::query_scalar(
            "SELECT status::TEXT FROM invoices WHERE name = $1 FOR UPDATE",
        )
        .bind(name.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        let Some(current) = current else {
            let _ = tx.rollback().await;
            return Err(missing_invoice(name));
        };
        let from = status_from_str(&current)?;

        if !transition_allowed(from, to) {
            let _ = tx.rollback().await;
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: format!(
                    "a legal transition out of {from:?} (Draft->Paid, Paid->Consolidated, Paid->Return)"
                ),
                actual: format!("{from:?} -> {to:?}"),
            }));
        }

        if from != to {
            sqlx::query("UPDATE invoices SET status = $2::invoice_status WHERE name = $1")
                .bind(name.as_str())
                .bind(status_to_str(to))
                .execute(&mut *tx)
                .await?;
        }

        let invoice = load_invoice(&mut tx, name).await?;
        tx.commit().await?;
        Ok(invoice)
    }

    /// Record a cancellation reason — the audit trail for the one legal gap in the
    /// series (`invoicing.rs`: "A gap only appears for a deliberately cancelled
    /// invoice, which must carry a logged void reason").
    ///
    /// The row stays. Deleting it would remove the evidence that explains the hole.
    pub async fn record_cancel_reason(
        &self,
        name: &InvoiceName,
        reason: &str,
    ) -> StorageResult<()> {
        if reason.trim().is_empty() {
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: "a non-empty cancellation reason for the audit trail".to_owned(),
                actual: "blank".to_owned(),
            }));
        }

        let affected = sqlx::query("UPDATE invoices SET cancel_reason = $2 WHERE name = $1")
            .bind(name.as_str())
            .bind(reason)
            .execute(self.pool())
            .await?
            .rows_affected();

        if affected == 0 {
            return Err(missing_invoice(name));
        }
        Ok(())
    }

    pub async fn mark_printed(&self, name: &InvoiceName) -> StorageResult<()> {
        let affected = sqlx::query("UPDATE invoices SET invoice_printed = TRUE WHERE name = $1")
            .bind(name.as_str())
            .execute(self.pool())
            .await?
            .rows_affected();

        if affected == 0 {
            return Err(missing_invoice(name));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Payments
    // -----------------------------------------------------------------------

    /// Record a tender against an invoice, settling it when the payments cover the bill.
    ///
    /// One transaction covers the row lock, the overpayment check, the insert and the
    /// status move, so a bill cannot be marked `Paid` by a payment that then fails to
    /// commit — nor accumulate a payment that leaves the status stale.
    ///
    /// ## Concurrency
    ///
    /// `SELECT … FOR UPDATE` on the invoice serialises concurrent payments against the
    /// same bill. Without it two ₹300 tenders on a ₹378 bill both read ₹0 settled, both
    /// pass the check, and the invoice ends up ₹222 over. The constraint trigger in
    /// 008_invoice_payments.sql is the backstop for writes that never come through here.
    ///
    /// ## Status
    ///
    /// Draft → Paid fires only on exact settlement. A short payment is accepted and
    /// leaves the invoice `Draft` with an outstanding balance, which is how a split
    /// tender bill accumulates. A `Consolidated` or `Return` invoice refuses payment:
    /// those are closed states.
    pub async fn record_payment(
        &self,
        name: &InvoiceName,
        payment: &NewPayment,
    ) -> StorageResult<StoredInvoice> {
        if payment.amount.inner() <= Decimal::ZERO {
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: "a positive payment amount".to_owned(),
                actual: payment.amount.inner().to_string(),
            }));
        }

        let mut tx = self.storage.begin().await?;

        // FOR UPDATE: the read that the overpayment check below depends on must not be
        // able to go stale before the insert lands.
        let row: Option<(String, Decimal, Decimal)> = sqlx::query_as(
            "SELECT status::TEXT, rounded_total, paid_amount
               FROM invoices WHERE name = $1 FOR UPDATE",
        )
        .bind(name.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        let Some((status_label, rounded_total, paid_amount)) = row else {
            let _ = tx.rollback().await;
            return Err(missing_invoice(name));
        };
        let status = status_from_str(&status_label)?;

        match status {
            PosInvoiceStatus::Draft | PosInvoiceStatus::Paid => {}
            PosInvoiceStatus::Consolidated | PosInvoiceStatus::Return => {
                let _ = tx.rollback().await;
                return Err(StorageError::Domain(DomainError::Conflict {
                    expected: "a Draft or Paid invoice".to_owned(),
                    actual: format!(
                        "invoice {} is {status_label}; payments are closed",
                        name.as_str()
                    ),
                }));
            }
        }

        let new_paid = paid_amount + payment.amount.inner();
        if new_paid > rounded_total {
            let _ = tx.rollback().await;
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: format!(
                    "a payment of at most {}",
                    rounded_total - paid_amount
                ),
                actual: format!(
                    "{}, which would take invoice {} to {new_paid} against a bill of {rounded_total}",
                    payment.amount.inner(),
                    name.as_str()
                ),
            }));
        }

        // idx is derived under the same lock, so two concurrent tenders cannot claim
        // the same position. The unique index on (invoice, idx) is the backstop.
        let next_idx: i32 = sqlx::query_scalar(
            "SELECT COALESCE(max(idx), 0) + 1 FROM invoice_payments WHERE invoice = $1",
        )
        .bind(name.as_str())
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO invoice_payments (invoice, idx, method, amount, reference, paid_at)
             VALUES ($1, $2, $3::payment_method, $4, $5, $6)",
        )
        .bind(name.as_str())
        .bind(next_idx)
        .bind(payment.method.as_str())
        .bind(payment.amount.inner())
        .bind(payment.reference.as_deref())
        .bind(payment.paid_at)
        .execute(&mut *tx)
        .await?;

        // Compared against rounded_total, the figure the customer actually pays
        // (businessday.rs bug 3).
        if new_paid == rounded_total && status == PosInvoiceStatus::Draft {
            sqlx::query("UPDATE invoices SET status = 'Paid'::invoice_status WHERE name = $1")
                .bind(name.as_str())
                .execute(&mut *tx)
                .await?;
        }

        let invoice = load_invoice(&mut tx, name).await?;
        tx.commit().await?;
        Ok(invoice)
    }

    /// Every tender on an invoice, ordered by `idx`.
    pub async fn list_payments(&self, name: &InvoiceName) -> StorageResult<Vec<StoredPayment>> {
        let mut tx = self.storage.begin().await?;
        let payments = load_payments(&mut tx, name).await?;
        tx.commit().await?;
        Ok(payments)
    }

    // -----------------------------------------------------------------------
    // Gaplessness audit
    // -----------------------------------------------------------------------

    /// Rule 46(b) self-audit: the issued numbers for a series form an unbroken run.
    ///
    /// Returns the missing numbers. Non-empty means either a rolled-back allocation
    /// burned a number — which the row-lock design makes impossible — or an invoice was
    /// hard-deleted. Every entry must have a logged `cancel_reason` somewhere or the
    /// series is not defensible.
    pub async fn find_series_gaps(
        &self,
        series: &str,
        fiscal_year: &str,
    ) -> StorageResult<Vec<u64>> {
        let gaps: Vec<i64> = sqlx::query_scalar(
            "WITH bounds AS (
                 SELECT min(series_number) AS lo, max(series_number) AS hi
                 FROM invoices
                 WHERE naming_series = $1 AND fiscal_year = $2
             )
             SELECT g
             FROM bounds, generate_series(bounds.lo, bounds.hi) AS g
             WHERE bounds.lo IS NOT NULL
               AND NOT EXISTS (
                   SELECT 1 FROM invoices i
                   WHERE i.naming_series = $1
                     AND i.fiscal_year = $2
                     AND i.series_number = g
               )
             ORDER BY g",
        )
        .bind(series)
        .bind(fiscal_year)
        .fetch_all(self.pool())
        .await?;

        Ok(gaps.into_iter().map(|g| g as u64).collect())
    }

    /// Issued numbers for a series, ascending. The concurrency proof reads this.
    pub async fn issued_numbers(
        &self,
        series: &str,
        fiscal_year: &str,
    ) -> StorageResult<Vec<u64>> {
        let nums: Vec<i64> = sqlx::query_scalar(
            "SELECT series_number FROM invoices
             WHERE naming_series = $1 AND fiscal_year = $2
             ORDER BY series_number",
        )
        .bind(series)
        .bind(fiscal_year)
        .fetch_all(self.pool())
        .await?;

        Ok(nums.into_iter().map(|n| n as u64).collect())
    }

    // -----------------------------------------------------------------------
    // Idempotency keys
    // -----------------------------------------------------------------------

    pub async fn lookup_idempotency_key(
        &self,
        key: Uuid,
    ) -> StorageResult<Option<InvoiceName>> {
        let found: Option<String> =
            sqlx::query_scalar("SELECT invoice FROM idempotency_keys WHERE key = $1")
                .bind(key)
                .fetch_optional(self.pool())
                .await?;

        Ok(found.map(|n| InvoiceName::from(n.as_str())))
    }

    /// Delete keys past `expires_at`, returning how many went.
    ///
    /// Nothing calls this on a timer; expiry is advisory (see 005_invoice.sql). Safe by
    /// construction: purging can only cost a future replay a new invoice number, never
    /// renumber an existing invoice, so Rule 46(b) holds either way.
    pub async fn purge_expired_idempotency_keys(&self) -> StorageResult<u64> {
        let deleted = sqlx::query("DELETE FROM idempotency_keys WHERE expires_at <= now()")
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(deleted)
    }
}

// ---------------------------------------------------------------------------
// Free functions over a transaction
// ---------------------------------------------------------------------------

/// The gapless allocation. One statement, one row lock, rolls back with the caller.
async fn allocate_number(
    tx: &mut Transaction<'static, Postgres>,
    series: &str,
    fiscal_year: &str,
) -> StorageResult<u64> {
    let allocated: Option<i64> = sqlx::query_scalar(
        "UPDATE invoice_naming_series
            SET next_number = next_number + 1
          WHERE series = $1 AND fiscal_year = $2
         RETURNING next_number - 1",
    )
    .bind(series)
    .bind(fiscal_year)
    .fetch_optional(&mut **tx)
    .await?;

    match allocated {
        Some(n) => Ok(n as u64),
        // The domain has a word for this; do not let it surface as a 500.
        None => Err(StorageError::Domain(DomainError::SeriesNotConfigured(
            series.to_owned(),
            fiscal_year.to_owned(),
        ))),
    }
}

/// Format and length-check, using the domain's own rule.
///
/// `{series}-{fy}-{counter:06}`, capped at 16 characters by CGST Rule 46(b). Kept
/// identical to `invoicing::allocate_invoice_number` so a name built here and a name
/// built there cannot diverge.
fn format_invoice_name(
    series: &str,
    fiscal_year: &str,
    number: u64,
) -> StorageResult<InvoiceName> {
    let formatted = format!("{series}-{fiscal_year}-{number:06}");
    if formatted.chars().count() > peacock_core::invoicing::MAX_INVOICE_NAME_LEN {
        return Err(StorageError::Domain(DomainError::InvoiceNameTooLong {
            name: formatted,
            limit: peacock_core::invoicing::MAX_INVOICE_NAME_LEN,
        }));
    }
    Ok(InvoiceName::new(formatted))
}

async fn lookup_key(
    tx: &mut Transaction<'static, Postgres>,
    key: Uuid,
) -> StorageResult<Option<InvoiceName>> {
    let found: Option<String> =
        sqlx::query_scalar("SELECT invoice FROM idempotency_keys WHERE key = $1")
            .bind(key)
            .fetch_optional(&mut **tx)
            .await?;

    Ok(found.map(|n| InvoiceName::from(n.as_str())))
}

async fn insert_lines(
    tx: &mut Transaction<'static, Postgres>,
    invoice: &InvoiceName,
    lines: &[NewInvoiceLine],
) -> StorageResult<()> {
    for (position, line) in lines.iter().enumerate() {
        sqlx::query(
            "INSERT INTO invoice_lines (
                 invoice, idx, item_code, item_name, qty, rate, amount,
                 hsn_sac, course, comments, serve_priority, indicate_course
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(invoice.as_str())
        // 1-based, matching Frappe's child-table `idx`.
        .bind(position as i32 + 1)
        .bind(line.item_code.as_str())
        .bind(&line.item_name)
        .bind(line.qty)
        .bind(line.rate.inner())
        .bind(line.amount().inner())
        .bind(line.hsn_sac.as_deref())
        .bind(line.course.as_ref().map(|c| c.as_str()))
        .bind(line.comments.as_deref())
        .bind(line.serve_priority)
        .bind(line.indicate_course)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Load an invoice with its lines and payments, inside a caller-owned transaction.
///
/// `pub(crate)` because `order.rs` reads an invoice back inside the transaction that
/// allocated it (`POST /api/orders/:id/invoice`), and going through
/// [`PgInvoiceRepo::get`] there would open a second transaction that cannot see the
/// uncommitted row.
pub(crate) async fn load_invoice(
    tx: &mut Transaction<'static, Postgres>,
    name: &InvoiceName,
) -> StorageResult<StoredInvoice> {
    let row = on_missing(
        sqlx::query_as::<_, InvoiceRow>(
            "SELECT
                 name, naming_series, fiscal_year, series_number, status::TEXT AS status,
                 restaurant, restaurant_table, restaurant_room, branch, pos_profile,
                 customer, waiter, cashier, no_of_pax, order_type,
                 posted_at, business_day,
                 supply_type, discount_basis, tax_rate,
                 net_total, discount, taxable_value,
                 cgst, sgst, igst, total_tax,
                 grand_total, rounded_total, round_off,
                 paid_amount, change_amount,
                 invoice_printed, cancel_reason, comments
             FROM invoices WHERE name = $1",
        )
        .bind(name.as_str())
        .fetch_optional(&mut **tx)
        .await,
        || missing_invoice_domain(name),
    )?;

    let line_rows = sqlx::query_as::<_, InvoiceLineRow>(
        "SELECT idx, item_code, item_name, qty, rate, amount,
                hsn_sac, course, comments, serve_priority, indicate_course
         FROM invoice_lines WHERE invoice = $1 ORDER BY idx",
    )
    .bind(name.as_str())
    .fetch_all(&mut **tx)
    .await?;

    let payments = load_payments(tx, name).await?;

    row_to_stored(row, line_rows, payments)
}

/// Tenders on an invoice, ordered by `idx`. Read inside the caller's transaction so a
/// payment and the `paid_amount` it produced are always read as one consistent pair.
async fn load_payments(
    tx: &mut Transaction<'static, Postgres>,
    name: &InvoiceName,
) -> StorageResult<Vec<StoredPayment>> {
    let rows = sqlx::query_as::<_, InvoicePaymentRow>(
        "SELECT idx, method::TEXT AS method, amount, reference, paid_at
           FROM invoice_payments WHERE invoice = $1 ORDER BY idx",
    )
    .bind(name.as_str())
    .fetch_all(&mut **tx)
    .await?;

    rows.into_iter()
        .map(|r| {
            Ok(StoredPayment {
                idx: r.idx,
                method: PaymentMethod::parse_label(&r.method)?,
                amount: unpad(r.amount),
                reference: r.reference,
                paid_at: r.paid_at,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct InvoiceRow {
    name: String,
    naming_series: String,
    fiscal_year: String,
    series_number: i64,
    status: String,
    restaurant: Option<String>,
    restaurant_table: Option<String>,
    restaurant_room: Option<String>,
    branch: String,
    pos_profile: Option<String>,
    customer: String,
    waiter: Option<String>,
    cashier: Option<String>,
    no_of_pax: i32,
    order_type: Option<String>,
    posted_at: DateTime<Utc>,
    business_day: NaiveDate,
    supply_type: String,
    discount_basis: String,
    tax_rate: Decimal,
    net_total: Decimal,
    discount: Decimal,
    taxable_value: Decimal,
    cgst: Decimal,
    sgst: Decimal,
    igst: Decimal,
    total_tax: Decimal,
    grand_total: Decimal,
    rounded_total: Decimal,
    round_off: Decimal,
    paid_amount: Decimal,
    change_amount: Decimal,
    invoice_printed: bool,
    cancel_reason: Option<String>,
    comments: Option<String>,
}

#[derive(sqlx::FromRow)]
struct InvoiceLineRow {
    idx: i32,
    item_code: String,
    item_name: String,
    qty: Decimal,
    rate: Decimal,
    amount: Decimal,
    hsn_sac: Option<String>,
    course: Option<String>,
    comments: Option<String>,
    serve_priority: i32,
    indicate_course: bool,
}

#[derive(sqlx::FromRow)]
struct InvoicePaymentRow {
    idx: i32,
    method: String,
    amount: Decimal,
    reference: Option<String>,
    paid_at: DateTime<Utc>,
}

/// Strip the padding a fixed-scale `NUMERIC(18,6)` column adds on the way out.
///
/// Postgres returns `100` as `100.000000`. That is the same value but a different string,
/// and `Money` serialises its `Decimal` verbatim (`money.rs`), so the padding would reach
/// every JSON client.
///
/// Used for the figures whose scale is **not** recoverable: a line's `rate` and `amount`,
/// and `paid_amount`. Their scale came from client input rather than from
/// [`compute_totals`], and the column does not record what it was — a rate submitted as
/// `100.00` and one submitted as `100` are the same six-decimal row. `unpad` gives the
/// shortest exact form, which is stable and round-trips, rather than guessing.
///
/// The invoice *totals* do not go through this: their scale is recoverable, so
/// [`domain_scaled_totals`] recovers it exactly instead of approximating.
fn unpad(d: Decimal) -> Money {
    Money::new(d.normalize())
}

/// The money columns as stored, wrapped without reinterpretation.
fn stored_totals(row: &InvoiceRow) -> InvoiceTotals {
    InvoiceTotals {
        net_total: Money::new(row.net_total),
        discount: Money::new(row.discount),
        taxable_value: Money::new(row.taxable_value),
        tax: TaxBreakdown {
            cgst: Money::new(row.cgst),
            sgst: Money::new(row.sgst),
            igst: Money::new(row.igst),
            total_tax: Money::new(row.total_tax),
        },
        grand_total: Money::new(row.grand_total),
        rounded_total: Money::new(row.rounded_total),
        round_off: Money::new(row.round_off),
    }
}

/// Recover the totals at the **scale the domain produced**, and prove they still match
/// what is stored.
///
/// # The problem this solves
///
/// `NUMERIC(18,6)` is a fixed-scale type: a stored `400` comes back as `400.000000`, and a
/// stored `18.00` comes back as `18.000000`. `Money` serialises its `Decimal` verbatim
/// (`money.rs`: `#[serde(with = "rust_decimal::serde::str")]`), so the same invoice would
/// appear on the wire as `"400.000000"` out of Postgres and `"400"` out of the in-memory
/// backend. Every JSON client sees that difference, and a client that string-compares
/// amounts — or displays them — treats it as a changed figure.
///
/// Rounding cannot fix it. The scale is not uniform: for the §5 worked example
/// `compute_totals` yields `net_total = 400`, `total_tax = 18.00` and `rounded_total = 378`
/// in one call. There is no single decimal place to normalise to, so any `round_dp` or
/// `normalize` gets some field wrong.
///
/// # Why recomputing is safe, and not a second implementation
///
/// [`compute_totals`] is *the* implementation — the one the parity harness validates
/// against the Python oracle. Everything needed to call it is stored: the lines, the
/// discount, the rate, the supply type and the discount basis. Calling it is therefore not
/// a second source of truth; it is the same source, re-derived at full fidelity.
///
/// The stored columns are then the **check**. They were written by that same function at
/// creation time and 005_invoice.sql's CHECK constraints have held every tax invariant
/// since, so a disagreement here means stored money has been altered behind the
/// repository's back — which is exactly the condition a money lane must refuse to serve
/// rather than paper over. Hence the error rather than a preference for one side.
///
/// A value equality is used, not a string one: `400.000000 == 400` as `Decimal`, so the
/// check passes on scale and fails only on substance.
fn domain_scaled_totals(row: &InvoiceRow, lines: &[InvoiceLineRow]) -> StorageResult<InvoiceTotals> {
    let stored = stored_totals(row);

    let supply_type = supply_type_from_str(&row.supply_type)?;
    let discount_basis = discount_basis_from_str(&row.discount_basis)?;

    // Unpadded before they go in. `compute_totals` multiplies `rate * quantity`, and two
    // six-decimal columns multiply to a twelve-decimal product — so feeding the padded
    // values in would reintroduce, and double, the very scale artefact this function
    // exists to remove.
    let tax_lines: Vec<peacock_core::tax::InvoiceLine> = lines
        .iter()
        .map(|l| peacock_core::tax::InvoiceLine {
            item_name: l.item_name.clone(),
            quantity: l.qty.normalize(),
            rate: unpad(l.rate),
            hsn_sac: l.hsn_sac.clone(),
        })
        .collect();

    let recomputed = match peacock_core::tax::compute_totals(
        &tax_lines,
        unpad(stored.discount.inner()),
        row.tax_rate.normalize(),
        supply_type,
        discount_basis,
    ) {
        Ok(totals) => totals,
        // The domain refused its own stored inputs. Serve what is stored rather than
        // failing the read: the row is what the customer was charged, and a reporting
        // query must still be able to see it.
        Err(_) => return Ok(stored),
    };

    // Compared field by field so the error names the one that moved.
    for (field, want, got) in [
        ("net_total", stored.net_total, recomputed.net_total),
        ("discount", stored.discount, recomputed.discount),
        ("taxable_value", stored.taxable_value, recomputed.taxable_value),
        ("cgst", stored.tax.cgst, recomputed.tax.cgst),
        ("sgst", stored.tax.sgst, recomputed.tax.sgst),
        ("igst", stored.tax.igst, recomputed.tax.igst),
        ("total_tax", stored.tax.total_tax, recomputed.tax.total_tax),
        ("grand_total", stored.grand_total, recomputed.grand_total),
        ("rounded_total", stored.rounded_total, recomputed.rounded_total),
        ("round_off", stored.round_off, recomputed.round_off),
    ] {
        if want.inner() != got.inner() {
            return Err(StorageError::Constraint {
                table: "invoices".to_owned(),
                constraint: "invoices_totals_match_the_domain".to_owned(),
                message: format!(
                    "invoice {}: stored {field} is {want} but the domain computes {got} \
                     from the stored lines; stored money has been altered outside the repository",
                    row.name
                ),
            });
        }
    }

    Ok(recomputed)
}

fn row_to_stored(
    row: InvoiceRow,
    lines: Vec<InvoiceLineRow>,
    payments: Vec<StoredPayment>,
) -> StorageResult<StoredInvoice> {
    let totals = domain_scaled_totals(&row, &lines)?;

    Ok(StoredInvoice {
        name: InvoiceName::from(row.name.as_str()),
        naming_series: row.naming_series,
        fiscal_year: row.fiscal_year,
        series_number: row.series_number as u64,
        status: status_from_str(&row.status)?,
        restaurant: row.restaurant,
        restaurant_table: row.restaurant_table.map(|t| TableName::from(t.as_str())),
        restaurant_room: row.restaurant_room,
        branch: BranchName::from(row.branch.as_str()),
        pos_profile: row.pos_profile,
        customer: row.customer,
        waiter: row.waiter,
        cashier: row.cashier,
        no_of_pax: row.no_of_pax,
        order_type: row.order_type,
        posted_at: row.posted_at,
        business_day: row.business_day,
        supply_type: supply_type_from_str(&row.supply_type)?,
        discount_basis: discount_basis_from_str(&row.discount_basis)?,
        tax_rate: row.tax_rate,
        totals,
        // Trigger-maintained sum of the payment rows (010_invoice_payments.sql), unpadded
        // for the same reason the line figures below are.
        paid_amount: unpad(row.paid_amount),
        change_amount: unpad(row.change_amount),
        invoice_printed: row.invoice_printed,
        cancel_reason: row.cancel_reason,
        comments: row.comments,
        lines: lines
            .into_iter()
            .map(|l| StoredInvoiceLine {
                idx: l.idx,
                item_code: ItemCode::from(l.item_code.as_str()),
                item_name: l.item_name,
                qty: l.qty.normalize(),
                rate: unpad(l.rate),
                amount: unpad(l.amount),
                hsn_sac: l.hsn_sac,
                course: l.course.map(|c| MenuCourseName::from(c.as_str())),
                comments: l.comments,
                serve_priority: l.serve_priority,
                indicate_course: l.indicate_course,
            })
            .collect(),
        payments,
    })
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// `peacock_core::Error` has no generic `NotFound` and no `InvoiceNotFound`
/// (error.rs gives one variant per entity, on purpose). `Conflict` is the closest
/// honest fit: the caller referenced a name the database does not have.
fn missing_invoice_domain(name: &InvoiceName) -> DomainError {
    DomainError::Conflict {
        expected: format!("invoice {}", name.as_str()),
        actual: "no such invoice".to_owned(),
    }
}

fn missing_invoice(name: &InvoiceName) -> StorageError {
    StorageError::Domain(missing_invoice_domain(name))
}

fn is_missing_invoice(err: &StorageError) -> bool {
    matches!(
        err,
        StorageError::Domain(DomainError::Conflict { actual, .. }) if actual == "no such invoice"
    )
}

/// `series_number` and `next_number` are BIGINT; Postgres has no unsigned integer, so a
/// `u64` past `i64::MAX` has nowhere to go. Unreachable in practice (Rule 46(b) caps the
/// counter at 999_999) but silently wrapping a money-lane identifier is not acceptable.
fn u64_to_i64(v: u64) -> StorageResult<i64> {
    i64::try_from(v).map_err(|_| StorageError::Constraint {
        table: "invoice_naming_series".to_owned(),
        constraint: "invoice_naming_series_next_number_positive".to_owned(),
        message: format!("series number {v} exceeds the BIGINT range"),
    })
}

// ---------------------------------------------------------------------------
// Domain port adapters
// ---------------------------------------------------------------------------

/// Blocking adapter for the two synchronous ports in [`peacock_core::invoicing`].
///
/// The ports are `&mut self` and sync, and both must run inside the caller's
/// transaction — a `SeriesAllocator` that committed on its own would defeat the
/// no-burn guarantee. So this borrows the transaction and bridges with
/// `block_in_place`, which needs a multi-threaded runtime (`#[tokio::main]`, or
/// `#[tokio::test(flavor = "multi_thread")]`).
///
/// Prefer [`PgInvoiceRepo::create_invoice_idempotent`] in async code. This exists for
/// call sites that want to drive `invoicing::allocate_invoice_number` itself and get
/// its exact semantics, including the 16-character guard firing before the counter
/// moves.
pub struct TxSeriesAllocator<'t> {
    tx: &'t mut Transaction<'static, Postgres>,
}

impl<'t> TxSeriesAllocator<'t> {
    pub fn new(tx: &'t mut Transaction<'static, Postgres>) -> Self {
        TxSeriesAllocator { tx }
    }
}

impl SeriesAllocator for TxSeriesAllocator<'_> {
    fn allocate(&mut self, series: &str, fiscal_year: &str) -> DomainResult<u64> {
        let tx = &mut *self.tx;
        block_on_current(async move {
            allocate_number(tx, series, fiscal_year)
                .await
                .map_err(storage_to_domain)
        })
    }
}

/// Transaction-scoped [`IdempotencyStore`].
///
/// `record` writes the key row; the FK to `invoices` means the invoice must already
/// exist in this transaction. Callers driving `allocate_invoice_number` directly must
/// therefore insert the invoice before the store records the key — the same ordering
/// `create_invoice_idempotent` uses internally.
pub struct TxIdempotencyStore<'t> {
    tx: &'t mut Transaction<'static, Postgres>,
    /// Written-through cache so `get` after `record` is consistent without a query.
    seen: HashMap<Uuid, InvoiceName>,
}

impl<'t> TxIdempotencyStore<'t> {
    pub fn new(tx: &'t mut Transaction<'static, Postgres>) -> Self {
        TxIdempotencyStore {
            tx,
            seen: HashMap::new(),
        }
    }
}

impl IdempotencyStore for TxIdempotencyStore<'_> {
    fn get(&self, key: Uuid) -> Option<InvoiceName> {
        if let Some(hit) = self.seen.get(&key) {
            return Some(hit.clone());
        }

        // `get` is `&self`, so the transaction cannot be borrowed mutably here. A
        // fresh pool connection would sit outside the transaction and miss its
        // uncommitted rows, which is worse than a cache miss: it would report "new
        // key" for a key this very transaction just recorded. The cache above covers
        // the in-transaction case; cross-transaction replays are the async path's job
        // (`create_invoice_idempotent` queries inside its own transaction).
        None
    }

    fn record(&mut self, key: Uuid, invoice_name: InvoiceName) -> DomainResult<()> {
        let tx = &mut *self.tx;
        let name = invoice_name.clone();
        block_on_current(async move {
            sqlx::query("INSERT INTO idempotency_keys (key, invoice) VALUES ($1, $2)")
                .bind(key)
                .bind(name.as_str())
                .execute(&mut **tx)
                .await
                .map_err(|e| storage_to_domain(StorageError::from(e)))?;
            Ok::<(), DomainError>(())
        })?;
        self.seen.insert(key, invoice_name);
        Ok(())
    }
}

/// Run an async block from sync code on the ambient runtime.
fn block_on_current<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

/// Collapse a `StorageError` into the domain vocabulary the sync ports return.
fn storage_to_domain(err: StorageError) -> DomainError {
    match err {
        StorageError::Domain(d) => d,
        StorageError::Retryable { sqlstate, message } => DomainError::Conflict {
            expected: "a committed write".to_owned(),
            actual: format!("serialization conflict {sqlstate}: {message}"),
        },
        other => DomainError::Conflict {
            expected: "a successful database write".to_owned(),
            actual: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn status_labels_round_trip_through_the_enum() {
        for status in [
            PosInvoiceStatus::Draft,
            PosInvoiceStatus::Paid,
            PosInvoiceStatus::Consolidated,
            PosInvoiceStatus::Return,
        ] {
            assert_eq!(status_from_str(status_to_str(status)).unwrap(), status);
        }
    }

    #[test]
    fn unknown_status_is_an_error_not_a_default() {
        // Defaulting to Draft would drop a paid invoice out of revenue — bug 4's shape.
        assert!(status_from_str("Cancelled").is_err());
        assert!(status_from_str("").is_err());
    }

    #[test]
    fn supply_type_and_discount_basis_round_trip() {
        for s in [SupplyType::Intrastate, SupplyType::Interstate] {
            assert_eq!(supply_type_from_str(supply_type_to_str(s)).unwrap(), s);
        }
        for b in [DiscountBasis::NetTotal, DiscountBasis::GrandTotal] {
            assert_eq!(discount_basis_from_str(discount_basis_to_str(b)).unwrap(), b);
        }
        assert!(supply_type_from_str("Export").is_err());
        assert!(discount_basis_from_str("LineTotal").is_err());
    }

    #[test]
    fn transitions_match_the_documented_lifecycle() {
        use PosInvoiceStatus::*;

        assert!(transition_allowed(Draft, Paid));
        assert!(transition_allowed(Paid, Consolidated));
        assert!(transition_allowed(Paid, Return));

        // No-op: a retried "mark paid" must not be an error.
        for s in [Draft, Paid, Consolidated, Return] {
            assert!(transition_allowed(s, s), "{s:?} -> {s:?} should be a no-op");
        }

        // Skipping Paid would let unpaid revenue into the P&L.
        assert!(!transition_allowed(Draft, Consolidated));
        assert!(!transition_allowed(Draft, Return));
        // Terminal states.
        assert!(!transition_allowed(Consolidated, Paid));
        assert!(!transition_allowed(Consolidated, Draft));
        assert!(!transition_allowed(Return, Paid));
        // No un-paying.
        assert!(!transition_allowed(Paid, Draft));
    }

    #[test]
    fn invoice_name_format_matches_the_domain() {
        let name = format_invoice_name("POS", "2627", 1).unwrap();
        assert_eq!(name.as_str(), "POS-2627-000001");
        assert_eq!(name.as_str().len(), 15);

        // 4 + 1 + 4 + 1 + 6 = 16, the widest the budget allows.
        let widest = format_invoice_name("PCOS", "2627", 1).unwrap();
        assert_eq!(widest.as_str(), "PCOS-2627-000001");
        assert_eq!(
            widest.as_str().len(),
            peacock_core::invoicing::MAX_INVOICE_NAME_LEN
        );
    }

    #[test]
    fn over_long_name_is_rejected_with_the_domain_error() {
        let err = format_invoice_name("TOOLONG", "2627", 1).unwrap_err();
        match err {
            StorageError::Domain(DomainError::InvoiceNameTooLong { name, limit }) => {
                assert_eq!(name, "TOOLONG-2627-000001");
                assert_eq!(limit, 16);
            }
            other => panic!("expected InvoiceNameTooLong, got {other:?}"),
        }
    }

    #[test]
    fn counter_past_six_digits_widens_the_name_and_is_rejected() {
        // The 6-digit assumption is not free: a 7-digit counter breaks the cap.
        assert!(format_invoice_name("PCOS", "2627", 999_999).is_ok());
        assert!(format_invoice_name("PCOS", "2627", 1_000_000).is_err());
    }

    #[test]
    fn line_amount_is_qty_times_rate_in_decimal() {
        let line = NewInvoiceLine {
            item_code: ItemCode::from("CHAI"),
            item_name: "Masala Chai".to_owned(),
            qty: dec!(3),
            rate: Money::new(dec!(40.50)),
            hsn_sac: None,
            course: None,
            comments: None,
            serve_priority: 0,
            indicate_course: false,
        };
        assert_eq!(line.amount(), Money::new(dec!(121.50)));
    }

    #[test]
    fn u64_to_i64_refuses_to_wrap() {
        assert_eq!(u64_to_i64(42).unwrap(), 42);
        assert!(u64_to_i64(u64::MAX).is_err());
    }

    #[test]
    fn revenue_statuses_come_from_the_single_domain_definition() {
        // The bug 4 guard: this repo must not hardcode its own status list.
        let labels: Vec<&str> = PosInvoiceStatus::REVENUE
            .iter()
            .copied()
            .map(status_to_str)
            .collect();
        assert_eq!(labels, vec!["Paid", "Consolidated"]);
    }
}
