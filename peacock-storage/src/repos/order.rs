//! Order repository — Lane 2H.
//!
//! # Two different things called "order"
//!
//! `URY Order` is a Frappe **UI form** (`"issingle": 1`, no status field, no tax or
//! payment fields — see [`peacock_core::model::UryOrderForm`]). It holds what the waiter
//! currently has on screen: the table binding, the cart, the pax count. It is transient
//! session state and it is deleted when the bill is settled.
//!
//! The **order of record** is the POS Invoice (Lane 2F, `repos/invoice.rs`). That is
//! where money, tax and status live, and it is what `last_invoice` points at.
//!
//! This module owns the form. It deliberately does *not* own anything that decides
//! revenue.
//!
//! # Why `OrderRepo::count_separate_active` queries `invoices` and not `orders`
//!
//! The trait's one method is the merge guard from `merge.rs`
//! (`Error::MultipleActiveOrders`). Upstream implements it as
//!
//! ```python
//! # ury_order.py:223-233, _table_has_active_order
//! frappe.db.exists("POS Invoice", {
//!     "docstatus": 0,           # Draft
//!     "restaurant_table": table_name,
//!     "invoice_printed": 0,     # not yet printed
//! })
//! ```
//!
//! — a probe against **POS Invoice**, one per member table
//! (`_count_separate_active_orders`, ury_order.py:236). So "active order" means an
//! unprinted draft invoice, not a row in this table. Counting `orders` rows instead
//! would be a different question with a different answer: a waiter can open a form,
//! browse the menu and walk away without ever raising an invoice, and that must not
//! block a merge.
//!
//! The port pushes the whole member set into one call
//! (`ports.rs`: "the merge BFS must not re-query per hop"), so the N probes collapse
//! into one `WHERE restaurant_table = ANY($1)` — served by the
//! `invoices_table_open_idx` partial index from 005_invoice.sql.
//!
//! # Row-level locking
//!
//! Two waiters on two tablets can open the same table's form at the same moment. Every
//! mutating path here takes `SELECT ... FOR UPDATE` on the `orders` row and holds it for
//! the whole read-modify-write, so the second waiter **blocks** on the first and then
//! reads what the first actually wrote. No lost update, no interleaved cart.
//!
//! `version` is the audit half of the same story: it advances on every write, so a
//! caller that read version N and wants to be told when it has been overtaken can use
//! [`PgOrderRepo::update_if_version`] and get [`peacock_core::error::Error::Conflict`]
//! instead of silently winning a race it should have lost.
//!
//! Deadlock is not possible between these paths: each transaction locks exactly one
//! `orders` row, and the child `order_items` rows are only ever reached through their
//! parent, so there is no second lock to acquire in a conflicting order.

use std::collections::HashMap;

use peacock_core::error::{Error as DomainError, Result as DomainResult};
use peacock_core::ids::{
    CustomerName, InvoiceName, ItemCode, PosProfileName, TableName, UserName,
};
use peacock_core::model::{OrderItem, UryOrderForm};
use peacock_core::money::Money;
use peacock_core::ports::OrderRepo;
use rust_decimal::Decimal;
use sqlx::types::chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

use crate::error::{StorageError, StorageResult};
use crate::repos::blocking::block_on;
use crate::repos::to_domain_error;
use crate::Storage;

/// Retries for the one race the idempotent paths can lose: two concurrent requests
/// carrying the *same* key, where both miss the lookup and one loses the insert. One retry
/// is enough — the winner has committed by then, so the retry's lookup hits. Same constant
/// and same reasoning as `repos/invoice.rs`.
const IDEMPOTENCY_REPLAY_ATTEMPTS: u32 = 2;

/// The surrogate key of an `orders` row.
///
/// Upstream `URY Order` is a Single doctype, so there is no Frappe docname to carry
/// over — unlike every Lane 2A entity, whose TEXT primary key *is* the user-visible
/// name. A newtype rather than a bare `i64` so an order id cannot be passed where an
/// invoice line id belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrderId(pub i64);

impl OrderId {
    pub fn get(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for OrderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An `orders` row plus the identity and concurrency metadata the domain model does not
/// carry.
///
/// [`UryOrderForm`] is the UI payload and has no id and no version, by design. Callers
/// that need to write need both, so reads hand back this wrapper rather than forcing a
/// second lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredOrder {
    pub id: OrderId,
    /// Advances on every committed write. Pass it to
    /// [`PgOrderRepo::update_if_version`] to make a lost update an error.
    pub version: i64,
    pub form: UryOrderForm,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the order was soft-cancelled (008_order_lifecycle.sql). `None` for a live
    /// order. Together with `form.last_invoice` this decides [`StoredOrder::status`].
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
}

impl StoredOrder {
    /// Where the order is in its life.
    ///
    /// Derived rather than stored: the two facts that decide it — the invoice pointer and
    /// the cancellation timestamp — are already columns with their own constraints, and a
    /// third column repeating their conclusion is a third thing that can disagree with
    /// them. Cancellation wins over invoiced because a cancelled order that had raised an
    /// invoice is voided, not billable; the invoice itself keeps its own status.
    pub fn status(&self) -> OrderLifecycle {
        if self.cancelled_at.is_some() {
            OrderLifecycle::Cancelled
        } else if self.form.last_invoice.is_some() {
            OrderLifecycle::Invoiced
        } else {
            OrderLifecycle::Open
        }
    }

    /// Whether the cart may still be changed.
    pub fn is_modifiable(&self) -> bool {
        self.status() == OrderLifecycle::Open
    }
}

/// The lifecycle of an order form, as storage sees it.
///
/// The API has its own `OrderStatus` for the wire (`peacock-api/src/dto/order.rs`), and
/// the domain deliberately has neither — upstream `URY Order` is a UI form with no status
/// field. This is the storage-side spelling, derived from the columns, and the API maps
/// it across the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderLifecycle {
    Open,
    Invoiced,
    Cancelled,
}

impl OrderLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            OrderLifecycle::Open => "open",
            OrderLifecycle::Invoiced => "invoiced",
            OrderLifecycle::Cancelled => "cancelled",
        }
    }
}

/// What [`PgOrderRepo::create_idempotent`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateOutcome {
    /// The key was new (or absent): a row was inserted.
    Created,
    /// The key had been seen: the original order came back and nothing was inserted.
    Replayed,
}

/// An order plus whether this call created it.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatedOrder {
    pub order: StoredOrder,
    pub outcome: CreateOutcome,
}

/// PostgreSQL-backed order form repository.
#[derive(Clone)]
pub struct PgOrderRepo {
    storage: Storage,
}

impl PgOrderRepo {
    pub fn new(storage: Storage) -> Self {
        PgOrderRepo { storage }
    }

    // -----------------------------------------------------------------------
    // Create
    // -----------------------------------------------------------------------

    /// Insert a form and its cart lines in one transaction.
    ///
    /// The unique partial index `orders_one_live_form_per_table_idx` means a second
    /// concurrent create for the same table fails with a unique violation rather than
    /// producing two rival carts. Callers that want "open or reuse" should use
    /// [`PgOrderRepo::get_or_create_for_table`].
    pub async fn create(&self, form: &UryOrderForm) -> StorageResult<StoredOrder> {
        let mut tx = self.storage.begin().await?;
        let stored = insert_form(&mut tx, form).await?;
        tx.commit().await?;
        Ok(stored)
    }

    /// Insert a form, or replay a previous insert that carried the same key.
    ///
    /// `None` for the key means "no replay protection", which is what a client that did
    /// not send `Idempotency-Key` asked for: every call inserts.
    ///
    /// ## The guarantee
    ///
    /// With `Some(key)`, the key row and the order row are written in one transaction, so
    /// a rolled-back insert leaves no key behind pointing at an order that does not
    /// exist. A replay reads the original row and inserts nothing —
    /// [`CreateOutcome::Replayed`] tells the caller so it can answer 200 rather than 201.
    ///
    /// ## The race, and the retry
    ///
    /// Two concurrent requests with the same key can both miss the lookup. Both insert,
    /// one loses `order_idempotency_keys_pkey` and its whole transaction rolls back
    /// (order row included), and its retry's lookup then finds the winner. Net effect:
    /// one key, one order. Same shape as the invoice path in `repos/invoice.rs`, and the
    /// reason one retry suffices is the same — the winner has committed by then.
    pub async fn create_idempotent(
        &self,
        idempotency_key: Option<uuid::Uuid>,
        form: &UryOrderForm,
    ) -> StorageResult<CreatedOrder> {
        let Some(key) = idempotency_key else {
            return Ok(CreatedOrder {
                order: self.create(form).await?,
                outcome: CreateOutcome::Created,
            });
        };

        let mut last_conflict: Option<StorageError> = None;

        for attempt in 1..=IDEMPOTENCY_REPLAY_ATTEMPTS {
            let mut tx = self.storage.begin().await?;

            if let Some(existing) = lookup_order_key(&mut tx, key).await? {
                let order = load_locked(&mut tx, existing).await?;
                tx.commit().await?;
                return Ok(CreatedOrder {
                    order,
                    outcome: CreateOutcome::Replayed,
                });
            }

            let inserted = async {
                let stored = insert_form(&mut tx, form).await?;
                sqlx::query(
                    "INSERT INTO order_idempotency_keys (key, order_id) VALUES ($1, $2)",
                )
                .bind(key)
                .bind(stored.id.get())
                .execute(&mut *tx)
                .await?;
                Ok::<_, StorageError>(stored)
            }
            .await;

            match inserted {
                Ok(order) => {
                    tx.commit().await?;
                    return Ok(CreatedOrder {
                        order,
                        outcome: CreateOutcome::Created,
                    });
                }
                Err(err) => {
                    let _ = tx.rollback().await;

                    let is_key_race = crate::error::is_unique_violation(
                        &err,
                        "order_idempotency_keys_pkey",
                    );
                    if !is_key_race || attempt == IDEMPOTENCY_REPLAY_ATTEMPTS {
                        return Err(err);
                    }

                    tracing::warn!(
                        target: "peacock_storage",
                        attempt,
                        key = %key,
                        "concurrent request replayed the same order idempotency key, re-reading"
                    );
                    last_conflict = Some(err);
                }
            }
        }

        Err(last_conflict.unwrap_or(StorageError::Constraint {
            table: "order_idempotency_keys".to_owned(),
            constraint: "order_idempotency_keys_pkey".to_owned(),
            message: "exhausted idempotency replay attempts".to_owned(),
        }))
    }

    /// Open the form for a table, or return the one already there.
    ///
    /// Serialised through the table's row lock: the loser of the race blocks, then sees
    /// the winner's row and returns it instead of colliding on the unique index.
    pub async fn get_or_create_for_table(
        &self,
        table: &TableName,
        form: &UryOrderForm,
    ) -> StorageResult<StoredOrder> {
        let mut tx = self.storage.begin().await?;

        // FOR UPDATE on the table row: two concurrent openers contend here, not on the
        // unique index, so the second one waits and then finds the first one's form.
        let existing = lock_by_table(&mut tx, table).await?;
        let stored = match existing {
            Some(id) => load_locked(&mut tx, id).await?,
            None => {
                let mut to_insert = form.clone();
                to_insert.restaurant_table = Some(table.clone());
                insert_form(&mut tx, &to_insert).await?
            }
        };

        tx.commit().await?;
        Ok(stored)
    }

    // -----------------------------------------------------------------------
    // Read
    // -----------------------------------------------------------------------

    /// Read a form by id. No lock — this is the screen-refresh path.
    pub async fn get(&self, id: OrderId) -> StorageResult<Option<StoredOrder>> {
        let Some(row) = sqlx::query_as::<_, OrderRow>(&format!("{SELECT_ORDER} WHERE id = $1"))
            .bind(id.get())
            .fetch_optional(self.storage.pool())
            .await?
        else {
            return Ok(None);
        };

        let items = load_items(self.storage.pool(), row.id).await?;
        Ok(Some(row.into_stored(items)))
    }

    /// The live form for a table, if any.
    pub async fn find_by_table(&self, table: &TableName) -> StorageResult<Option<StoredOrder>> {
        let Some(row) =
            sqlx::query_as::<_, OrderRow>(&format!("{SELECT_ORDER} WHERE restaurant_table = $1"))
                .bind(table.as_str())
                .fetch_optional(self.storage.pool())
                .await?
        else {
            return Ok(None);
        };

        let items = load_items(self.storage.pool(), row.id).await?;
        Ok(Some(row.into_stored(items)))
    }

    /// Every open take-away form, oldest first — the take-away queue.
    pub async fn list_take_away(&self) -> StorageResult<Vec<StoredOrder>> {
        let rows = sqlx::query_as::<_, OrderRow>(&format!(
            "{SELECT_ORDER} WHERE take_away ORDER BY created_at, id"
        ))
        .fetch_all(self.storage.pool())
        .await?;

        self.attach_items(rows).await
    }

    /// Every open form for a waiter — the POS home screen.
    pub async fn list_for_waiter(&self, waiter: &UserName) -> StorageResult<Vec<StoredOrder>> {
        let rows = sqlx::query_as::<_, OrderRow>(&format!(
            "{SELECT_ORDER} WHERE waiter = $1 ORDER BY created_at, id"
        ))
        .bind(waiter.as_str())
        .fetch_all(self.storage.pool())
        .await?;

        self.attach_items(rows).await
    }

    /// Batch the child-table read so a list of N forms costs 2 queries, not N+1 — the
    /// same rule Lane 2E applies to `kot_items`.
    async fn attach_items(&self, rows: Vec<OrderRow>) -> StorageResult<Vec<StoredOrder>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        let item_rows = sqlx::query_as::<_, OrderItemRow>(
            r#"
            SELECT order_id, item, item_name, qty, rate, comments
            FROM order_items
            WHERE order_id = ANY($1)
            ORDER BY order_id, idx
            "#,
        )
        .bind(&ids)
        .fetch_all(self.storage.pool())
        .await?;

        let mut by_order: HashMap<i64, Vec<OrderItem>> = HashMap::new();
        for item in item_rows {
            by_order
                .entry(item.order_id)
                .or_default()
                .push(item.into_model());
        }

        Ok(rows
            .into_iter()
            .map(|row| {
                let items = by_order.get(&row.id).cloned().unwrap_or_default();
                row.into_stored(items)
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Update — always under the row lock
    // -----------------------------------------------------------------------

    /// Replace a form's contents, including its cart.
    ///
    /// Takes `SELECT ... FOR UPDATE` first and holds it to commit, so a concurrent
    /// waiter doing the same thing blocks here rather than interleaving writes. Both
    /// calls succeed; the second one's `version` is the first's plus one.
    ///
    /// # This is last-writer-wins, and the lock does not change that
    ///
    /// `form` replaces the row wholesale, so a field the caller's snapshot does not carry
    /// is cleared even if someone else set it a moment ago. The lock buys serialisation
    /// and an intact row — never a torn one, never a half-applied cart — but it cannot
    /// merge two payloads, because the second payload does not contain the first one's
    /// edits. Callers for whom a stale overwrite is worse than a retry want
    /// [`PgOrderRepo::update_if_version`].
    pub async fn update(&self, id: OrderId, form: &UryOrderForm) -> StorageResult<StoredOrder> {
        self.update_locked(id, None, form).await
    }

    /// Same, but fails with [`DomainError::Conflict`] when the row has moved on since
    /// the caller read version `expected_version`.
    ///
    /// Use this where a stale overwrite is worse than a retry: a waiter whose tablet
    /// went to sleep mid-order should be told the cart changed, not silently win.
    pub async fn update_if_version(
        &self,
        id: OrderId,
        expected_version: i64,
        form: &UryOrderForm,
    ) -> StorageResult<StoredOrder> {
        self.update_locked(id, Some(expected_version), form).await
    }

    async fn update_locked(
        &self,
        id: OrderId,
        expected_version: Option<i64>,
        form: &UryOrderForm,
    ) -> StorageResult<StoredOrder> {
        let mut tx = self.storage.begin().await?;

        // The lock. Everything below runs with the row held, and the lock is released
        // by the commit, not before.
        let current: Option<i64> =
            sqlx::query_scalar("SELECT version FROM orders WHERE id = $1 FOR UPDATE")
                .bind(id.get())
                .fetch_optional(&mut *tx)
                .await?;

        let Some(current) = current else {
            return Err(order_not_found(id));
        };

        if let Some(expected) = expected_version {
            if expected != current {
                return Err(StorageError::Domain(DomainError::Conflict {
                    expected: expected.to_string(),
                    actual: current.to_string(),
                }));
            }
        }

        write_form(&mut tx, id, form).await?;

        let stored = load_locked(&mut tx, id).await?;
        tx.commit().await?;
        Ok(stored)
    }

    /// Read-modify-write under the row lock, with the mutation supplied by the caller.
    ///
    /// This is what [`PgOrderRepo::update`] cannot be: `update` takes a whole form and is
    /// therefore last-writer-wins, which loses a concurrent waiter's edit. Here the
    /// closure runs *inside* the lock, so it observes the row as it currently is and two
    /// waiters appending to the same cart both keep their lines.
    ///
    /// A closure that returns `Err` leaves the row untouched — no partial write, no
    /// version bump. Refuses an invoiced or cancelled order: at that point the invoice is
    /// the record and a silent line edit would diverge from the printed ticket.
    pub async fn modify<F>(
        &self,
        id: OrderId,
        expected_version: Option<i64>,
        mutate: F,
    ) -> StorageResult<StoredOrder>
    where
        F: FnOnce(&mut UryOrderForm) -> StorageResult<()> + Send,
    {
        let mut tx = self.storage.begin().await?;

        // The lock, held to commit. A second modify on this order waits here.
        let locked: Option<i64> =
            sqlx::query_scalar("SELECT version FROM orders WHERE id = $1 FOR UPDATE")
                .bind(id.get())
                .fetch_optional(&mut *tx)
                .await?;

        if locked.is_none() {
            let _ = tx.rollback().await;
            return Err(order_not_found(id));
        }

        let current = load_locked(&mut tx, id).await?;

        if !current.is_modifiable() {
            let _ = tx.rollback().await;
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: format!("order {id} to be open"),
                actual: format!("the order is {}", current.status().as_str()),
            }));
        }

        if let Some(expected) = expected_version {
            if expected != current.version {
                let _ = tx.rollback().await;
                return Err(StorageError::Domain(DomainError::Conflict {
                    expected: expected.to_string(),
                    actual: current.version.to_string(),
                }));
            }
        }

        let mut form = current.form;
        if let Err(err) = mutate(&mut form) {
            // A refused mutation must not leave a half-applied cart behind.
            let _ = tx.rollback().await;
            return Err(err);
        }

        write_form(&mut tx, id, &form).await?;

        let stored = load_locked(&mut tx, id).await?;
        tx.commit().await?;
        Ok(stored)
    }

    /// Point a form at the invoice it raised.
    ///
    /// The FK to `invoices(name)` means an unknown invoice is rejected by the database
    /// (SQLSTATE 23503), not by a check the repository could forget.
    pub async fn set_last_invoice(
        &self,
        id: OrderId,
        invoice: Option<&InvoiceName>,
    ) -> StorageResult<StoredOrder> {
        let mut tx = self.storage.begin().await?;

        let exists: Option<i64> =
            sqlx::query_scalar("SELECT id FROM orders WHERE id = $1 FOR UPDATE")
                .bind(id.get())
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(order_not_found(id));
        }

        sqlx::query(
            "UPDATE orders SET last_invoice = $2, version = version + 1 WHERE id = $1",
        )
        .bind(id.get())
        .bind(invoice.map(|i| i.as_str()))
        .execute(&mut *tx)
        .await?;

        let stored = load_locked(&mut tx, id).await?;
        tx.commit().await?;
        Ok(stored)
    }

    // -----------------------------------------------------------------------
    // Cancel — soft, under the row lock
    // -----------------------------------------------------------------------

    /// Soft-cancel a form: stamp `cancelled_at` and leave the row for the audit trail.
    ///
    /// Idempotent. A second cancel returns the same row without moving `cancelled_at` or
    /// the version — a retried DELETE is not an error, and re-stamping the timestamp would
    /// lose the time it was actually voided.
    ///
    /// An invoiced order is refused with [`DomainError::Conflict`]: the invoice is the
    /// order of record and voiding it is `PgInvoiceRepo`'s business, with its own
    /// `cancel_reason` and its own Rule 46(b) audit trail. Cancelling the form alone would
    /// leave a billable invoice attached to an order the UI reports as void.
    pub async fn cancel(
        &self,
        id: OrderId,
        reason: Option<&str>,
    ) -> StorageResult<StoredOrder> {
        let mut tx = self.storage.begin().await?;

        let current: Option<(Option<DateTime<Utc>>, Option<String>)> = sqlx::query_as(
            "SELECT cancelled_at, last_invoice FROM orders WHERE id = $1 FOR UPDATE",
        )
        .bind(id.get())
        .fetch_optional(&mut *tx)
        .await?;

        let Some((cancelled_at, last_invoice)) = current else {
            let _ = tx.rollback().await;
            return Err(order_not_found(id));
        };

        // Already cancelled: no write, no bump, no error.
        if cancelled_at.is_some() {
            let stored = load_locked(&mut tx, id).await?;
            tx.commit().await?;
            return Ok(stored);
        }

        if let Some(invoice) = last_invoice {
            let _ = tx.rollback().await;
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: format!("order {id} to have no invoice before it is cancelled"),
                actual: format!("invoice {invoice} was already raised"),
            }));
        }

        let trimmed = reason.map(str::trim).filter(|r| !r.is_empty());

        sqlx::query(
            "UPDATE orders
                SET cancelled_at = now(), cancel_reason = $2, version = version + 1
              WHERE id = $1",
        )
        .bind(id.get())
        .bind(trimmed)
        .execute(&mut *tx)
        .await?;

        let stored = load_locked(&mut tx, id).await?;
        tx.commit().await?;
        Ok(stored)
    }

    // -----------------------------------------------------------------------
    // Order → invoice, in one transaction
    // -----------------------------------------------------------------------

    /// Raise the invoice for an order and point the form at it, atomically.
    ///
    /// ## Why this lives here and not in a handler
    ///
    /// Three writes have to agree: the number is allocated, the invoice and its lines are
    /// written, and `orders.last_invoice` is set. Split across two repository calls, a
    /// failure between them leaves either an invoice no order knows about or — worse — an
    /// order whose `last_invoice` names a rolled-back invoice. One transaction makes both
    /// impossible, and it is also what makes the *order's* replay check meaningful: the
    /// `FOR UPDATE` taken here is held while the invoice is allocated, so a concurrent
    /// second call to the same order blocks and then sees `last_invoice` already set
    /// rather than burning a second number.
    ///
    /// ## Idempotency, twice over
    ///
    /// * By key — delegated to `idempotency_keys`, the same table and the same reasoning
    ///   as [`PgInvoiceRepo::create_invoice_idempotent`].
    /// * By state — an order that already carries `last_invoice` returns that invoice.
    ///   This is the one that matters for Rule 46(b): without it a client that lost the
    ///   response and retried without a key would gap the series.
    pub async fn create_invoice(
        &self,
        id: OrderId,
        idempotency_key: Option<uuid::Uuid>,
        new_invoice: &crate::repos::invoice::NewInvoice,
    ) -> StorageResult<(crate::repos::invoice::StoredInvoice, bool)> {
        use crate::repos::invoice::CreateOutcome as InvoiceOutcome;

        let invoices = self.storage.invoice_repo();

        for attempt in 1..=IDEMPOTENCY_REPLAY_ATTEMPTS {
            let mut tx = self.storage.begin().await?;

            // The order lock comes first and is held for everything below, so a
            // concurrent invoice attempt on this order waits here rather than allocating.
            let current: Option<Option<String>> = sqlx::query_scalar(
                "SELECT last_invoice FROM orders WHERE id = $1 FOR UPDATE",
            )
            .bind(id.get())
            .fetch_optional(&mut *tx)
            .await?;

            let Some(last_invoice) = current else {
                let _ = tx.rollback().await;
                return Err(order_not_found(id));
            };

            // Replay by state. Checked under the lock, before the counter is touched.
            if let Some(existing) = last_invoice {
                let name = InvoiceName::new(existing);
                let invoice = crate::repos::invoice::load_invoice(&mut tx, &name).await?;
                // Record the key against the invoice the order already has, so a later
                // replay of *this* key short-circuits without re-taking the lock.
                if let Some(key) = idempotency_key {
                    sqlx::query(
                        "INSERT INTO idempotency_keys (key, invoice) VALUES ($1, $2)
                         ON CONFLICT (key) DO NOTHING",
                    )
                    .bind(key)
                    .bind(name.as_str())
                    .execute(&mut *tx)
                    .await?;
                }
                tx.commit().await?;
                return Ok((invoice, false));
            }

            let cancelled: Option<DateTime<Utc>> =
                sqlx::query_scalar("SELECT cancelled_at FROM orders WHERE id = $1")
                    .bind(id.get())
                    .fetch_one(&mut *tx)
                    .await?;
            if cancelled.is_some() {
                let _ = tx.rollback().await;
                return Err(StorageError::Domain(DomainError::Conflict {
                    expected: format!("order {id} to be open before it is invoiced"),
                    actual: "the order was cancelled".to_owned(),
                }));
            }

            // Replay by key.
            if let Some(key) = idempotency_key {
                if let Some(existing) = lookup_invoice_key(&mut tx, key).await? {
                    let invoice =
                        crate::repos::invoice::load_invoice(&mut tx, &existing).await?;
                    // The order does not point at it yet — it lost the response, not the
                    // write — so link it now, still inside this transaction.
                    link_invoice(&mut tx, id, Some(&existing)).await?;
                    tx.commit().await?;
                    return Ok((invoice, false));
                }
            }

            // No key means no replay row to write, but the invoice path needs one: it is
            // what makes the allocation and the dedup record commit together. A key
            // generated here is never handed out, so it can only ever be matched by this
            // one insert — which is exactly the "no replay protection" the caller asked
            // for, with the series still gapless.
            let key = idempotency_key.unwrap_or_else(uuid::Uuid::new_v4);

            let written = invoices
                .insert_new_invoice(&mut tx, key, new_invoice)
                .await;

            match written {
                Ok(invoice) => {
                    link_invoice(&mut tx, id, Some(&invoice.name)).await?;
                    tx.commit().await?;
                    let _ = InvoiceOutcome::Created;
                    return Ok((invoice, true));
                }
                Err(err) => {
                    // Rollback restores the counter: nothing is burned, and the order's
                    // `last_invoice` is untouched.
                    let _ = tx.rollback().await;

                    let is_key_race =
                        crate::error::is_unique_violation(&err, "idempotency_keys_pkey");
                    if !is_key_race || attempt == IDEMPOTENCY_REPLAY_ATTEMPTS {
                        return Err(err);
                    }

                    tracing::warn!(
                        target: "peacock_storage",
                        attempt,
                        order = %id,
                        "concurrent invoice for the same order, re-reading"
                    );
                }
            }
        }

        Err(StorageError::Constraint {
            table: "idempotency_keys".to_owned(),
            constraint: "idempotency_keys_pkey".to_owned(),
            message: "exhausted idempotency replay attempts".to_owned(),
        })
    }

    // -----------------------------------------------------------------------
    // Delete
    // -----------------------------------------------------------------------

    /// Delete a form. Cart lines go with it (`ON DELETE CASCADE`).
    ///
    /// This is the settle path: once the invoice is paid the form has served its
    /// purpose. Nothing is lost — the invoice is the record.
    pub async fn delete(&self, id: OrderId) -> StorageResult<bool> {
        let affected = sqlx::query("DELETE FROM orders WHERE id = $1")
            .bind(id.get())
            .execute(self.storage.pool())
            .await?
            .rows_affected();
        Ok(affected > 0)
    }

    // -----------------------------------------------------------------------
    // The port method, async form
    // -----------------------------------------------------------------------

    /// Async body of [`OrderRepo::count_separate_active`].
    ///
    /// One query for the whole member set. "Active" is upstream's definition: a Draft
    /// invoice that has not been printed (`ury_order.py:223-233`). `DISTINCT
    /// restaurant_table` is what makes it *separate* orders — two unprinted drafts on
    /// one table are one active table, and the merge guard counts tables.
    pub async fn count_separate_active_async(&self, tables: &[TableName]) -> StorageResult<usize> {
        if tables.is_empty() {
            return Ok(0);
        }

        let names: Vec<&str> = tables.iter().map(|t| t.as_str()).collect();
        let count: i64 = sqlx::query_scalar(
            r#"
            SELECT count(DISTINCT restaurant_table)
            FROM invoices
            WHERE restaurant_table = ANY($1)
              AND status = 'Draft'
              AND NOT invoice_printed
            "#,
        )
        .bind(&names)
        .fetch_one(self.storage.pool())
        .await?;

        Ok(count.max(0) as usize)
    }

    /// Transfer the live order (and any draft invoices) from one table to another.
    ///
    /// Validates that source has an order and destination is empty. Moves the order
    /// row and any unprinted draft invoices in one transaction so a concurrent
    /// transfer or merge sees a consistent state.
    pub async fn transfer_table(
        &self,
        from: &TableName,
        to: &TableName,
    ) -> StorageResult<StoredOrder> {
        let mut tx = self.storage.begin().await?;

        // Lock and locate the source order.
        let src_id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM orders WHERE restaurant_table = $1 FOR UPDATE")
                .bind(from.as_str())
                .fetch_optional(&mut *tx)
                .await?;

        let Some(src_id) = src_id else {
            let _ = tx.rollback().await;
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: format!("an active order on table {}", from.as_str()),
                actual: "no order found on source table".to_owned(),
            }));
        };

        // Destination must be empty (no order form).
        let dst_exists: Option<i64> =
            sqlx::query_scalar("SELECT id FROM orders WHERE restaurant_table = $1 FOR UPDATE")
                .bind(to.as_str())
                .fetch_optional(&mut *tx)
                .await?;

        if dst_exists.is_some() {
            let _ = tx.rollback().await;
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: format!("destination table {} to be empty", to.as_str()),
                actual: "destination already has an order".to_owned(),
            }));
        }

        // Also ensure destination has no draft invoice (merge guard counts invoices).
        let dst_invoice: Option<String> = sqlx::query_scalar(
            "SELECT name FROM invoices WHERE restaurant_table = $1 AND status = 'Draft' AND NOT invoice_printed LIMIT 1",
        )
        .bind(to.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        if dst_invoice.is_some() {
            let _ = tx.rollback().await;
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: format!(
                    "destination table {} to have no draft invoice",
                    to.as_str()
                ),
                actual: "destination has a draft invoice".to_owned(),
            }));
        }

        sqlx::query(
            "UPDATE orders SET restaurant_table = $2, version = version + 1 WHERE id = $1",
        )
        .bind(src_id)
        .bind(to.as_str())
        .execute(&mut *tx)
        .await?;

        // Move any draft invoices as well so the order and its bill stay together.
        sqlx::query(
            "UPDATE invoices SET restaurant_table = $2 WHERE restaurant_table = $1 AND status = 'Draft' AND NOT invoice_printed",
        )
        .bind(from.as_str())
        .bind(to.as_str())
        .execute(&mut *tx)
        .await?;

        let stored = load_locked(&mut tx, OrderId(src_id)).await?;
        tx.commit().await?;
        Ok(stored)
    }
}

impl OrderRepo for PgOrderRepo {
    fn count_separate_active(&self, tables: &[TableName]) -> DomainResult<usize> {
        // The port traits are synchronous on purpose (`ports.rs`), so the async boundary
        // is crossed here. `repos::blocking::block_on` is the one place that knows how:
        // it parks the worker with `block_in_place` so a sibling keeps driving the
        // reactor the pool connection depends on, and it returns an error on a
        // current-thread runtime rather than deadlocking.
        block_on(self.count_separate_active_async(tables))
            .map_err(to_domain_error)?
            .map_err(to_domain_error)
    }
}

// ---------------------------------------------------------------------------
// SQL fragments and row types
// ---------------------------------------------------------------------------

const SELECT_ORDER: &str = r#"
    SELECT id, version, take_away, restaurant_table, customer_name, no_of_pax,
           grand_total, last_invoice, waiter, pos_profile, cashier, comments,
           modified_time, created_at, updated_at, cancelled_at, cancel_reason
    FROM orders
"#;

#[derive(sqlx::FromRow)]
struct OrderRow {
    id: i64,
    version: i64,
    take_away: bool,
    restaurant_table: Option<String>,
    customer_name: String,
    no_of_pax: i32,
    grand_total: Decimal,
    last_invoice: Option<String>,
    waiter: Option<String>,
    pos_profile: Option<String>,
    cashier: Option<String>,
    comments: Option<String>,
    modified_time: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    cancelled_at: Option<DateTime<Utc>>,
    cancel_reason: Option<String>,
}

impl OrderRow {
    fn into_stored(self, items: Vec<OrderItem>) -> StoredOrder {
        StoredOrder {
            id: OrderId(self.id),
            version: self.version,
            created_at: self.created_at,
            updated_at: self.updated_at,
            cancelled_at: self.cancelled_at,
            cancel_reason: self.cancel_reason,
            form: UryOrderForm {
                take_away: self.take_away,
                restaurant_table: self.restaurant_table.map(TableName::new),
                customer_name: CustomerName::new(self.customer_name),
                no_of_pax: self.no_of_pax,
                grand_total: Money::new(self.grand_total),
                last_invoice: self.last_invoice.map(InvoiceName::new),
                items,
                waiter: self.waiter.map(UserName::new),
                pos_profile: self.pos_profile.map(PosProfileName::new),
                cashier: self.cashier.map(UserName::new),
                comments: self.comments,
                modified_time: self.modified_time,
            },
        }
    }
}

#[derive(sqlx::FromRow)]
struct OrderItemRow {
    order_id: i64,
    item: String,
    item_name: String,
    qty: i32,
    rate: Decimal,
    comments: Option<String>,
}

impl OrderItemRow {
    fn into_model(self) -> OrderItem {
        OrderItem {
            item: ItemCode::new(self.item),
            item_name: self.item_name,
            qty: self.qty,
            rate: Money::new(self.rate),
            comments: self.comments,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared query helpers
// ---------------------------------------------------------------------------

/// `INSERT` a form and its lines on an existing transaction.
async fn insert_form(
    tx: &mut Transaction<'_, Postgres>,
    form: &UryOrderForm,
) -> StorageResult<StoredOrder> {
    let id: i64 = sqlx::query_scalar(
        r#"
        INSERT INTO orders (
            take_away, restaurant_table, customer_name, no_of_pax, grand_total,
            last_invoice, waiter, pos_profile, cashier, comments, modified_time
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        RETURNING id
        "#,
    )
    .bind(form.take_away)
    .bind(form.restaurant_table.as_ref().map(|t| t.as_str()))
    .bind(form.customer_name.as_str())
    .bind(form.no_of_pax)
    .bind(form.grand_total.inner())
    .bind(form.last_invoice.as_ref().map(|i| i.as_str()))
    .bind(form.waiter.as_ref().map(|w| w.as_str()))
    .bind(form.pos_profile.as_ref().map(|p| p.as_str()))
    .bind(form.cashier.as_ref().map(|c| c.as_str()))
    .bind(form.comments.as_deref())
    .bind(form.modified_time)
    .fetch_one(&mut **tx)
    .await?;

    let id = OrderId(id);
    insert_items(tx, id, &form.items).await?;
    load_locked(tx, id).await
}

/// `UPDATE` a form and replace its cart, on a transaction that already holds the row lock.
///
/// The single write path: [`PgOrderRepo::update_locked`] and [`PgOrderRepo::modify`] differ
/// only in where the form came from, and duplicating the column list would be a second
/// place for a new field to be forgotten.
async fn write_form(
    tx: &mut Transaction<'_, Postgres>,
    id: OrderId,
    form: &UryOrderForm,
) -> StorageResult<()> {
    sqlx::query(
        r#"
        UPDATE orders SET
            take_away        = $2,
            restaurant_table = $3,
            customer_name    = $4,
            no_of_pax        = $5,
            grand_total      = $6,
            last_invoice     = $7,
            waiter           = $8,
            pos_profile      = $9,
            cashier          = $10,
            comments         = $11,
            modified_time    = $12,
            version          = version + 1
        WHERE id = $1
        "#,
    )
    .bind(id.get())
    .bind(form.take_away)
    .bind(form.restaurant_table.as_ref().map(|t| t.as_str()))
    .bind(form.customer_name.as_str())
    .bind(form.no_of_pax)
    .bind(form.grand_total.inner())
    .bind(form.last_invoice.as_ref().map(|i| i.as_str()))
    .bind(form.waiter.as_ref().map(|w| w.as_str()))
    .bind(form.pos_profile.as_ref().map(|p| p.as_str()))
    .bind(form.cashier.as_ref().map(|c| c.as_str()))
    .bind(form.comments.as_deref())
    .bind(form.modified_time)
    .execute(&mut **tx)
    .await?;

    // The cart is replaced wholesale: the client sends the whole array, and diffing it
    // would only add a way for `idx` to drift from cart order.
    sqlx::query("DELETE FROM order_items WHERE order_id = $1")
        .bind(id.get())
        .execute(&mut **tx)
        .await?;
    insert_items(tx, id, &form.items).await?;

    Ok(())
}

/// Write the cart. `idx` is 1-based to match Frappe child tables.
async fn insert_items(
    tx: &mut Transaction<'_, Postgres>,
    id: OrderId,
    items: &[OrderItem],
) -> StorageResult<()> {
    for (position, item) in items.iter().enumerate() {
        sqlx::query(
            r#"
            INSERT INTO order_items (order_id, idx, item, item_name, qty, rate, comments)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(id.get())
        .bind(position as i32 + 1)
        .bind(item.item.as_str())
        .bind(&item.item_name)
        .bind(item.qty)
        .bind(item.rate.inner())
        .bind(item.comments.as_deref())
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Look up the order an idempotency key already produced.
async fn lookup_order_key(
    tx: &mut Transaction<'_, Postgres>,
    key: uuid::Uuid,
) -> StorageResult<Option<OrderId>> {
    let found: Option<i64> =
        sqlx::query_scalar("SELECT order_id FROM order_idempotency_keys WHERE key = $1")
            .bind(key)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(found.map(OrderId))
}

/// Look up the invoice an idempotency key already produced.
async fn lookup_invoice_key(
    tx: &mut Transaction<'_, Postgres>,
    key: uuid::Uuid,
) -> StorageResult<Option<InvoiceName>> {
    let found: Option<String> =
        sqlx::query_scalar("SELECT invoice FROM idempotency_keys WHERE key = $1")
            .bind(key)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(found.map(InvoiceName::new))
}

/// Point a form at an invoice on a caller-owned transaction.
///
/// The row-lock version of [`PgOrderRepo::set_last_invoice`], for the paths that already
/// hold the lock and must not commit before the invoice is written.
async fn link_invoice(
    tx: &mut Transaction<'_, Postgres>,
    id: OrderId,
    invoice: Option<&InvoiceName>,
) -> StorageResult<()> {
    sqlx::query("UPDATE orders SET last_invoice = $2, version = version + 1 WHERE id = $1")
        .bind(id.get())
        .bind(invoice.map(|i| i.as_str()))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Take the row lock for a table's form and return its id, if the form exists.
///
/// `FOR UPDATE` on a `SELECT` that matches no row locks nothing, which is why
/// [`PgOrderRepo::get_or_create_for_table`] still relies on the unique index as the
/// backstop for two simultaneous first-opens.
async fn lock_by_table(
    tx: &mut Transaction<'_, Postgres>,
    table: &TableName,
) -> StorageResult<Option<OrderId>> {
    let id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM orders WHERE restaurant_table = $1 FOR UPDATE")
            .bind(table.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    Ok(id.map(OrderId))
}

/// Read a form back inside the transaction that just wrote it.
async fn load_locked(
    tx: &mut Transaction<'_, Postgres>,
    id: OrderId,
) -> StorageResult<StoredOrder> {
    let row = sqlx::query_as::<_, OrderRow>(&format!("{SELECT_ORDER} WHERE id = $1"))
        .bind(id.get())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| order_not_found(id))?;

    let items = sqlx::query_as::<_, OrderItemRow>(
        r#"
        SELECT order_id, item, item_name, qty, rate, comments
        FROM order_items
        WHERE order_id = $1
        ORDER BY idx
        "#,
    )
    .bind(id.get())
    .fetch_all(&mut **tx)
    .await?;

    Ok(row.into_stored(items.into_iter().map(OrderItemRow::into_model).collect()))
}

async fn load_items(pool: &sqlx::PgPool, id: i64) -> StorageResult<Vec<OrderItem>> {
    let rows = sqlx::query_as::<_, OrderItemRow>(
        r#"
        SELECT order_id, item, item_name, qty, rate, comments
        FROM order_items
        WHERE order_id = $1
        ORDER BY idx
        "#,
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(OrderItemRow::into_model).collect())
}

/// `peacock_core::Error` has no generic `NotFound` variant and adding one is not this
/// lane's call (`error.rs::on_missing` documents the reasoning), so a missing form is a
/// storage-level `Constraint` rather than a domain error.
fn order_not_found(id: OrderId) -> StorageError {
    StorageError::Constraint {
        table: "orders".to_owned(),
        constraint: "not_found".to_owned(),
        message: format!("order {id} not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn form() -> UryOrderForm {
        UryOrderForm {
            take_away: false,
            restaurant_table: Some(TableName::from("T-01")),
            customer_name: CustomerName::from("Walk-in"),
            no_of_pax: 2,
            grand_total: Money::new(dec!(0)),
            last_invoice: None,
            items: vec![],
            waiter: None,
            pos_profile: None,
            cashier: None,
            comments: None,
            modified_time: None,
        }
    }

    #[test]
    fn order_id_displays_as_its_number() {
        assert_eq!(OrderId(42).to_string(), "42");
        assert_eq!(OrderId(42).get(), 42);
    }

    #[test]
    fn missing_order_is_a_storage_constraint_not_a_domain_error() {
        let err = order_not_found(OrderId(7));
        match &err {
            StorageError::Constraint {
                table, constraint, ..
            } => {
                assert_eq!(table, "orders");
                assert_eq!(constraint, "not_found");
            }
            other => panic!("expected Constraint, got {other:?}"),
        }
        assert!(!err.is_retryable());
    }

    /// A stale-version rejection must reach the caller as `Conflict`, not be flattened
    /// into an infrastructure error: the HTTP layer turns it into a 409 and the client
    /// retries, whereas an infrastructure error is a 500 and the cart is lost.
    #[test]
    fn a_version_conflict_survives_the_sync_port_boundary() {
        // `peacock_core::Error` is not Clone, so build the expectation separately.
        let mapped = to_domain_error(StorageError::Domain(DomainError::Conflict {
            expected: "3".to_owned(),
            actual: "4".to_owned(),
        }));
        assert_eq!(
            mapped,
            DomainError::Conflict {
                expected: "3".to_owned(),
                actual: "4".to_owned(),
            }
        );
    }

    #[test]
    fn a_row_round_trips_into_the_domain_form() {
        let row = OrderRow {
            id: 3,
            version: 5,
            take_away: true,
            restaurant_table: None,
            customer_name: "Walk-in".to_owned(),
            no_of_pax: 4,
            grand_total: dec!(123.450000),
            last_invoice: Some("POS-2627-000001".to_owned()),
            waiter: Some("waiter@peacock.test".to_owned()),
            pos_profile: Some("Peacock POS".to_owned()),
            cashier: None,
            comments: Some("no onions".to_owned()),
            modified_time: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            cancelled_at: None,
            cancel_reason: None,
        };

        let stored = row.into_stored(vec![OrderItem {
            item: ItemCode::from("CURRY"),
            item_name: "Chicken Curry".to_owned(),
            qty: 2,
            rate: Money::new(dec!(61.725)),
            comments: None,
        }]);

        assert_eq!(stored.id, OrderId(3));
        assert_eq!(stored.version, 5);
        assert!(stored.form.take_away);
        assert!(stored.form.restaurant_table.is_none());
        assert_eq!(stored.form.grand_total, Money::new(dec!(123.45)));
        assert_eq!(
            stored.form.last_invoice.as_ref().map(|i| i.as_str()),
            Some("POS-2627-000001")
        );
        assert_eq!(stored.form.items.len(), 1);
        assert_eq!(stored.form.items[0].qty, 2);
    }

    #[test]
    fn a_table_bound_form_carries_its_table() {
        let f = form();
        assert!(!f.take_away);
        assert_eq!(
            f.restaurant_table.as_ref().map(|t| t.as_str()),
            Some("T-01")
        );
    }
}
