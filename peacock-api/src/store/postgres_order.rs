//! The Postgres-backed [`OrderStore`] — Lane 4A-1.
//!
//! ## What this module is
//!
//! An adapter, not a second repository. Every query lives in
//! [`peacock_storage::repos::order::PgOrderRepo`]; this file translates between the
//! HTTP layer's vocabulary and the repository's, and does nothing else. The two
//! vocabularies genuinely differ, which is why the translation is not a no-op:
//!
//! | HTTP layer | storage layer |
//! |---|---|
//! | `id: String` (`"ORD-17"`) | `OrderId(i64)`, a `BIGSERIAL` |
//! | `version: u64` | `version: i64` |
//! | `OrderStatus` enum on the wire | derived from `cancelled_at` + `last_invoice` |
//! | `StoreError` | `StorageError` / `peacock_core::Error` |
//! | invoice = series + date | invoice = a whole [`NewInvoice`] with computed totals |
//!
//! ## The id format
//!
//! `orders.id` is a `BIGSERIAL` (007_order.sql explains why: upstream `URY Order` is a
//! Single doctype, so there is no docname to carry over). The API has always handed
//! clients an opaque string, so the number is wrapped as `ORD-<id>` rather than exposed
//! bare — that keeps the wire contract that Lane 3D's tests pin, and a client that stored
//! `"ORD-17"` yesterday still resolves today.
//!
//! Parsing is strict: anything that is not `ORD-<digits>` is a 404, not a 500. A caller
//! that invents an id gets the same answer as a caller naming a deleted order, which is
//! the honest one — neither exists.
//!
//! ## Where the money is computed
//!
//! Nowhere here. `total_of_domain` (the API's single definition of an order total) and
//! `peacock_core::tax::compute_totals` (the invoice arithmetic the parity harness
//! validates) are called; no sum, no rounding and no tax split is re-derived in this
//! file. That is deliberate: a second implementation of invoice arithmetic is a second
//! thing that can disagree with the oracle.

use chrono::{NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use peacock_core::ids::{BranchName, InvoiceName};
use peacock_core::invoicing::{fiscal_year_code, fiscal_year_for};
use peacock_core::model::UryOrderForm;
use peacock_core::money::Money;
use peacock_core::tax::{compute_totals, DiscountBasis, InvoiceLine, SupplyType};
use peacock_storage::repos::invoice::{NewInvoice, NewInvoiceLine};
use peacock_storage::repos::order::{
    CreateOutcome, OrderId, OrderLifecycle, PgOrderRepo, StoredOrder,
};
use peacock_storage::{StorageError, Storage};

use crate::dto::order::{total_of_domain, OrderStatus};
use crate::store::order::{InvoiceRecord, Mutation, OrderRecord, StoreError, StoreResult};

/// Prefix on the wire form of an order id.
const ID_PREFIX: &str = "ORD-";

/// Configuration the invoice write needs and an order form does not carry.
///
/// An order knows its table, its cart and its customer. It does not know the branch's GST
/// rate or whether the supply is intra- or interstate, and inventing a default inside the
/// repository would mean silently taxing at 0%. So these are set once at wiring time,
/// where the operator's configuration is known.
#[derive(Debug, Clone)]
pub struct InvoiceDefaults {
    /// Branch the invoice is booked against. Overridden per request by
    /// `CreateInvoiceRequest::branch`.
    pub branch: String,
    /// GST rate as a fraction: `0.05` for 5%.
    pub tax_rate: Decimal,
    pub supply_type: SupplyType,
    pub discount_basis: DiscountBasis,
    /// `restaurants.name`, when the deployment has one. `None` leaves the FK null.
    pub restaurant: Option<String>,
    /// `rooms.name` for the table's room.
    pub restaurant_room: Option<String>,
}

impl Default for InvoiceDefaults {
    fn default() -> Self {
        InvoiceDefaults {
            branch: "Peacock - Main".to_owned(),
            // Zero, not 5%: a wrong *default* tax rate is a compliance problem that looks
            // like a working system. Zero is visibly wrong the first time anyone reads a
            // bill, which is the failure mode that gets fixed.
            tax_rate: Decimal::ZERO,
            supply_type: SupplyType::Intrastate,
            discount_basis: DiscountBasis::NetTotal,
            restaurant: None,
            restaurant_room: None,
        }
    }
}

/// [`OrderStore`] over a real database.
#[derive(Clone)]
pub struct PostgresOrderStore {
    repo: PgOrderRepo,
    defaults: InvoiceDefaults,
}

impl PostgresOrderStore {
    pub fn new(storage: Storage) -> Self {
        PostgresOrderStore {
            repo: storage.order_repo(),
            defaults: InvoiceDefaults::default(),
        }
    }

    /// Same, with the tax and branch configuration the invoice write needs.
    pub fn with_defaults(storage: Storage, defaults: InvoiceDefaults) -> Self {
        PostgresOrderStore {
            repo: storage.order_repo(),
            defaults,
        }
    }

    pub fn repo(&self) -> &PgOrderRepo {
        &self.repo
    }

    /// Load an order, mapping "no such row" onto [`StoreError::NotFound`].
    async fn load(&self, id: &str) -> StoreResult<StoredOrder> {
        let order_id = parse_id(id)?;
        self.repo
            .get(order_id)
            .await
            .map_err(|e| map_storage_error(e, id))?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))
    }
}

/// The operations the routes call.
///
/// Inherent methods, not a trait impl. These were `impl OrderStore for PostgresOrderStore`
/// until Lane W1-A deleted the trait: with the in-memory store gone there was exactly one
/// implementor, so the trait bought no substitutability and cost an `#[async_trait]` boxed
/// future per call. The signatures are unchanged, which is why no handler moved.
impl PostgresOrderStore {
    /// Insert a new order.
    ///
    /// With `Some(key)`, a replay returns the original record unchanged and creates
    /// nothing. Callers tell the two apart by the returned `created` flag.
    pub async fn create(
        &self,
        form: UryOrderForm,
        idempotency_key: Option<Uuid>,
    ) -> StoreResult<(OrderRecord, bool)> {
        let created = self
            .repo
            .create_idempotent(idempotency_key, &form)
            .await
            .map_err(|e| map_storage_error(e, "<new>"))?;

        let is_new = created.outcome == CreateOutcome::Created;
        Ok((to_record(created.order), is_new))
    }

    pub async fn get(&self, id: &str) -> StoreResult<OrderRecord> {
        Ok(to_record(self.load(id).await?))
    }

    /// Read-modify-write under `SELECT … FOR UPDATE`.
    ///
    /// `mutate` runs while the row lock is held, so it observes the current record and
    /// nothing can interleave between its read and the write. Returning `Err` from
    /// `mutate` leaves the row untouched.
    pub async fn modify(
        &self,
        id: &str,
        expected_version: Option<u64>,
        mutate: Mutation,
    ) -> StoreResult<OrderRecord> {
        let order_id = parse_id(id)?;
        let expected = expected_version.map(|v| v as i64);

        let updated = self
            .repo
            .modify(order_id, expected, move |form| {
                // The HTTP layer's mutation speaks `StoreError`; the repository speaks
                // `StorageError`. Wrapped as a domain conflict so it survives the
                // boundary with its message, and unwrapped again below.
                mutate(form).map_err(|e| {
                    StorageError::Domain(peacock_core::error::Error::Conflict {
                        expected: MUTATION_REFUSED.to_owned(),
                        actual: e.to_string(),
                    })
                })?;
                // The total is recomputed from the lines the mutation left behind, so a
                // client cannot patch a cart and keep a stale total.
                form.grand_total = total_of_domain(&form.items);
                form.modified_time = Some(Utc::now());
                Ok(())
            })
            .await
            .map_err(|e| map_storage_error(e, id))?;

        Ok(to_record(updated))
    }

    /// Allocate a gapless invoice number for the order and mark it invoiced.
    ///
    /// Idempotent on `idempotency_key`: a replay returns the original invoice and burns
    /// no second number. Also idempotent on the order's own state — an order that already
    /// carries `last_invoice` returns that invoice rather than allocating another, because
    /// a second number would gap the series.
    pub async fn create_invoice(
        &self,
        id: &str,
        series: &str,
        date: NaiveDate,
        idempotency_key: Option<Uuid>,
    ) -> StoreResult<(InvoiceRecord, bool)> {
        let order_id = parse_id(id)?;
        let order = self.load(id).await?;

        if order.status() == OrderLifecycle::Cancelled {
            return Err(StoreError::NotModifiable {
                id: id.to_owned(),
                status: OrderStatus::Cancelled.as_str(),
            });
        }

        // Checked before the counter can move. An empty invoice is not a document anyone
        // wants, and rejecting it after allocation would burn a number.
        if order.form.items.is_empty() && order.form.last_invoice.is_none() {
            return Err(StoreError::Invalid(
                "cannot invoice an order with no items".into(),
            ));
        }

        let new_invoice = self.build_new_invoice(&order, series, date)?;

        let (stored, created) = self
            .repo
            .create_invoice(order_id, idempotency_key, &new_invoice)
            .await
            .map_err(|e| map_storage_error(e, id))?;

        Ok((
            InvoiceRecord {
                name: stored.name.clone(),
                order_id: id.to_owned(),
                grand_total: stored.totals.grand_total,
                rounded_total: stored.totals.rounded_total,
                round_off: stored.totals.round_off,
                fiscal_year: fiscal_year_for(stored.business_day),
                status: format!("{:?}", stored.status),
                date: stored.business_day,
            },
            created,
        ))
    }

    /// Cancel an order. Idempotent: cancelling a cancelled order succeeds.
    pub async fn cancel(&self, id: &str) -> StoreResult<OrderRecord> {
        let order_id = parse_id(id)?;
        let cancelled = self
            .repo
            .cancel(order_id, None)
            .await
            .map_err(|e| map_storage_error(e, id))?;
        Ok(to_record(cancelled))
    }

    /// Assemble the invoice write from the order plus the configured defaults.
    ///
    /// The arithmetic is `peacock_core::tax::compute_totals` — the one implementation the
    /// parity harness diffs against the Python oracle. Nothing is summed here.
    fn build_new_invoice(
        &self,
        order: &StoredOrder,
        series: &str,
        date: NaiveDate,
    ) -> StoreResult<NewInvoice> {
        let tax_lines: Vec<InvoiceLine> = order
            .form
            .items
            .iter()
            .map(|item| InvoiceLine {
                item_name: item.item_name.clone(),
                quantity: Decimal::from(item.qty),
                rate: item.rate,
                hsn_sac: None,
            })
            .collect();

        let totals = compute_totals(
            &tax_lines,
            Money::ZERO,
            self.defaults.tax_rate,
            self.defaults.supply_type,
            self.defaults.discount_basis,
        )
        .map_err(|e| StoreError::Invalid(e.to_string()))?;

        let lines: Vec<NewInvoiceLine> = order
            .form
            .items
            .iter()
            .map(|item| NewInvoiceLine {
                item_code: item.item.clone(),
                item_name: item.item_name.clone(),
                qty: Decimal::from(item.qty),
                rate: item.rate,
                hsn_sac: None,
                course: None,
                comments: item.comments.clone(),
                serve_priority: 0,
                indicate_course: false,
            })
            .collect();

        // `posted_at` is midday on the business date rather than `now()`: the caller chose
        // the date, and stamping the current instant would put an invoice dated yesterday
        // at a timestamp inside today. Midday is far enough from either midnight that no
        // timezone conversion moves it across a day boundary.
        let posted_at = date
            .and_hms_opt(12, 0, 0)
            .map(|naive| Utc.from_utc_datetime(&naive))
            .unwrap_or_else(Utc::now);

        Ok(NewInvoice {
            naming_series: series.to_owned(),
            // The compact 4-digit code, not the 7-character display form: the schema's
            // `invoices_fy_is_four_digits` CHECK requires it, and it is what keeps the
            // whole name inside Rule 46(b)'s 16 characters.
            fiscal_year: fiscal_year_code(date),
            restaurant: self.defaults.restaurant.clone(),
            restaurant_table: order.form.restaurant_table.clone(),
            restaurant_room: self.defaults.restaurant_room.clone(),
            branch: BranchName::from(self.defaults.branch.as_str()),
            pos_profile: order
                .form
                .pos_profile
                .as_ref()
                .map(|p| p.as_str().to_owned()),
            customer: order.form.customer_name.as_str().to_owned(),
            waiter: order.form.waiter.as_ref().map(|w| w.as_str().to_owned()),
            cashier: order.form.cashier.as_ref().map(|c| c.as_str().to_owned()),
            // The schema requires `no_of_pax >= 0`; an order carrying 0 pax is a takeaway.
            no_of_pax: order.form.no_of_pax.max(0),
            order_type: Some(if order.form.take_away {
                "Take Away".to_owned()
            } else {
                "Dine In".to_owned()
            }),
            posted_at,
            business_day: date,
            supply_type: self.defaults.supply_type,
            discount_basis: self.defaults.discount_basis,
            tax_rate: self.defaults.tax_rate,
            totals,
            paid_amount: Money::ZERO,
            change_amount: Money::ZERO,
            comments: order.form.comments.clone(),
            lines,
        })
    }
}

// ---------------------------------------------------------------------------
// Translation
// ---------------------------------------------------------------------------

/// Marker in the `expected` field of the conflict a refused mutation is wrapped in.
///
/// The closure the HTTP layer supplies returns `StoreError`, the repository's closure
/// returns `StorageError`, and there is no variant in the latter that means "the caller's
/// own validation refused". Rather than flatten it to a 500, it travels as a `Conflict`
/// tagged with this string and [`map_storage_error`] unwraps it back to the original
/// message on the way out.
const MUTATION_REFUSED: &str = "peacock-api: mutation refused";

/// Wire id → surrogate key.
///
/// A malformed id is [`StoreError::NotFound`] rather than an invalid-input error: from the
/// caller's side "there is no order ORD-abc" is true and actionable, and a 400 would imply
/// a correctly-formed id would have worked.
fn parse_id(id: &str) -> StoreResult<OrderId> {
    id.strip_prefix(ID_PREFIX)
        .and_then(|digits| digits.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .map(OrderId)
        .ok_or_else(|| StoreError::NotFound(id.to_owned()))
}

/// Surrogate key → wire id.
fn format_id(id: OrderId) -> String {
    format!("{ID_PREFIX}{}", id.get())
}

/// Storage lifecycle → the API's wire status.
fn to_status(lifecycle: OrderLifecycle) -> OrderStatus {
    match lifecycle {
        OrderLifecycle::Open => OrderStatus::Open,
        OrderLifecycle::Invoiced => OrderStatus::Invoiced,
        OrderLifecycle::Cancelled => OrderStatus::Cancelled,
    }
}

fn to_record(order: StoredOrder) -> OrderRecord {
    OrderRecord {
        id: format_id(order.id),
        status: to_status(order.status()),
        // `version` is `BIGINT NOT NULL DEFAULT 1` with a `> 0` CHECK, so the cast cannot
        // wrap in practice; `max(0)` keeps it total rather than relying on that.
        version: order.version.max(0) as u64,
        form: order.form,
        created_at: order.created_at,
        modified_at: order.updated_at,
    }
}

/// Storage failures → the HTTP layer's vocabulary.
///
/// The interesting case is `Conflict`: the repository uses it for three different things
/// — a stale version, an order that is no longer modifiable, and a mutation the caller's
/// own closure refused — and they map to three different HTTP responses. They are told
/// apart by the `expected` field the repository set, which is why those strings are
/// constructed in one place each.
fn map_storage_error(err: StorageError, id: &str) -> StoreError {
    use peacock_core::error::Error as DomainError;

    match &err {
        StorageError::Domain(DomainError::Conflict { expected, actual }) => {
            if expected == MUTATION_REFUSED {
                // Round-trip complete: hand back what the caller's closure actually said.
                return StoreError::Invalid(actual.clone());
            }
            if let (Ok(expected_v), Ok(actual_v)) =
                (expected.parse::<u64>(), actual.parse::<u64>())
            {
                return StoreError::VersionConflict {
                    id: id.to_owned(),
                    expected: expected_v,
                    actual: actual_v,
                };
            }
            if actual.contains("cancelled") {
                return StoreError::NotModifiable {
                    id: id.to_owned(),
                    status: OrderStatus::Cancelled.as_str(),
                };
            }
            if actual.contains("invoiced") {
                return StoreError::NotModifiable {
                    id: id.to_owned(),
                    status: OrderStatus::Invoiced.as_str(),
                };
            }
            if let Some(invoice) = already_invoiced_name(actual) {
                return StoreError::AlreadyInvoiced {
                    id: id.to_owned(),
                    invoice,
                };
            }
            StoreError::Invalid(format!("{expected}; got {actual}"))
        }

        StorageError::Domain(DomainError::InvoiceNameTooLong { name, limit }) => {
            StoreError::InvoiceNameTooLong {
                name: name.clone(),
                limit: *limit,
            }
        }

        // The series has no counter row for this fiscal year. An operator problem, and
        // one the caller cannot fix by changing the request, so it must not read as a 400.
        StorageError::Domain(DomainError::SeriesNotConfigured(series, fy)) => {
            StoreError::Invalid(format!(
                "invoice series {series:?} is not configured for fiscal year {fy}; \
                 register it before invoicing"
            ))
        }

        StorageError::Constraint {
            constraint,
            message,
            ..
        } => match constraint.as_str() {
            "not_found" => StoreError::NotFound(id.to_owned()),
            // The FKs `orders` carries: a table, an item or an invoice the caller named
            // that does not exist. The caller *can* fix these, so they are 400s.
            "orders_restaurant_table_fkey" => StoreError::Invalid(
                "restaurant_table does not exist; create the table first".to_owned(),
            ),
            "order_items_item_fkey" => {
                StoreError::Invalid("one or more item codes do not exist".to_owned())
            }
            "orders_one_live_form_per_table_idx" => StoreError::AlreadyInvoiced {
                id: id.to_owned(),
                invoice: "another open order already holds this table".to_owned(),
            },
            "orders_no_of_pax_positive" => {
                StoreError::Invalid("no_of_pax must be at least 1".to_owned())
            }
            "orders_has_a_binding" => StoreError::Invalid(
                "either restaurant_table or take_away must be set".to_owned(),
            ),
            other => StoreError::Invalid(format!("{other}: {message}")),
        },

        // A lost race the caller may retry unchanged. Surfaced as a conflict so the client
        // retries rather than treating it as a server fault.
        StorageError::Retryable { message, .. } => StoreError::VersionConflict {
            id: id.to_owned(),
            expected: 0,
            actual: 0,
        }
        .with_context(message),

        // Everything else is infrastructure: an unreachable database, a timeout, a bug in
        // a query. `Invalid` would be a lie, so these stay a 500 through the
        // `InvoiceNameTooLong`-style internal mapping in `routes::orders`.
        other => StoreError::Invalid(format!("storage error: {other}")),
    }
}

/// Pull the invoice name out of the repository's "already raised" conflict message.
fn already_invoiced_name(actual: &str) -> Option<String> {
    let rest = actual.strip_prefix("invoice ")?;
    let name = rest.strip_suffix(" was already raised")?;
    Some(name.to_owned())
}

impl StoreError {
    /// Attach a retry hint without inventing a new variant.
    ///
    /// `VersionConflict` already means "someone else got there first, re-read and retry",
    /// which is exactly what a serialization failure means to a client.
    fn with_context(self, message: &str) -> Self {
        match self {
            StoreError::VersionConflict { id, .. } => StoreError::NotModifiable {
                id: format!("{id}: {message}"),
                status: "contended",
            },
            other => other,
        }
    }
}

/// Silence the unused-import warning for the doc-referenced type.
#[allow(dead_code)]
fn _invoice_name_is_used(n: InvoiceName) -> InvoiceName {
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_ids_round_trip_through_the_surrogate_key() {
        assert_eq!(format_id(OrderId(17)), "ORD-17");
        assert_eq!(parse_id("ORD-17").unwrap(), OrderId(17));
    }

    #[test]
    fn a_malformed_id_is_not_found_rather_than_a_five_hundred() {
        // Every one of these is a client typo or a stale bookmark, not a server fault.
        for bad in [
            "ORD-abc",
            "17",
            "",
            "ORD-",
            "ORD--1",
            "ORD-0",
            "ord-17",
            "ORD-9999999999999999999999",
        ] {
            assert!(
                matches!(parse_id(bad), Err(StoreError::NotFound(_))),
                "{bad:?} must be NotFound"
            );
        }
    }

    #[test]
    fn the_lifecycle_maps_onto_the_wire_status() {
        assert_eq!(to_status(OrderLifecycle::Open), OrderStatus::Open);
        assert_eq!(to_status(OrderLifecycle::Invoiced), OrderStatus::Invoiced);
        assert_eq!(to_status(OrderLifecycle::Cancelled), OrderStatus::Cancelled);
    }

    #[test]
    fn a_version_conflict_survives_the_translation() {
        let err = map_storage_error(
            StorageError::Domain(peacock_core::error::Error::Conflict {
                expected: "3".to_owned(),
                actual: "4".to_owned(),
            }),
            "ORD-1",
        );
        assert_eq!(
            err,
            StoreError::VersionConflict {
                id: "ORD-1".to_owned(),
                expected: 3,
                actual: 4,
            }
        );
    }

    #[test]
    fn a_refused_mutation_comes_back_as_the_callers_own_message() {
        // The round trip that keeps a 400 a 400: the closure's message must not be
        // flattened into a generic conflict.
        let err = map_storage_error(
            StorageError::Domain(peacock_core::error::Error::Conflict {
                expected: MUTATION_REFUSED.to_owned(),
                actual: "append_items cannot be empty".to_owned(),
            }),
            "ORD-1",
        );
        assert_eq!(
            err,
            StoreError::Invalid("append_items cannot be empty".to_owned())
        );
    }

    #[test]
    fn an_unmodifiable_order_is_told_apart_by_its_status() {
        let cancelled = map_storage_error(
            StorageError::Domain(peacock_core::error::Error::Conflict {
                expected: "order 1 to be open".to_owned(),
                actual: "the order is cancelled".to_owned(),
            }),
            "ORD-1",
        );
        assert_eq!(
            cancelled,
            StoreError::NotModifiable {
                id: "ORD-1".to_owned(),
                status: "cancelled",
            }
        );

        let invoiced = map_storage_error(
            StorageError::Domain(peacock_core::error::Error::Conflict {
                expected: "order 1 to be open".to_owned(),
                actual: "the order is invoiced".to_owned(),
            }),
            "ORD-1",
        );
        assert_eq!(
            invoiced,
            StoreError::NotModifiable {
                id: "ORD-1".to_owned(),
                status: "invoiced",
            }
        );
    }

    #[test]
    fn an_already_invoiced_order_reports_the_invoice_it_has() {
        let err = map_storage_error(
            StorageError::Domain(peacock_core::error::Error::Conflict {
                expected: "order 1 to have no invoice before it is cancelled".to_owned(),
                actual: "invoice PCK-2627-000001 was already raised".to_owned(),
            }),
            "ORD-1",
        );
        assert_eq!(
            err,
            StoreError::AlreadyInvoiced {
                id: "ORD-1".to_owned(),
                invoice: "PCK-2627-000001".to_owned(),
            }
        );
    }

    #[test]
    fn a_missing_row_is_not_found() {
        let err = map_storage_error(
            StorageError::Constraint {
                table: "orders".to_owned(),
                constraint: "not_found".to_owned(),
                message: "order 9 not found".to_owned(),
            },
            "ORD-9",
        );
        assert_eq!(err, StoreError::NotFound("ORD-9".to_owned()));
    }

    #[test]
    fn a_bad_foreign_key_is_the_callers_problem_not_a_five_hundred() {
        for (constraint, needle) in [
            ("orders_restaurant_table_fkey", "restaurant_table"),
            ("order_items_item_fkey", "item codes"),
            ("orders_no_of_pax_positive", "no_of_pax"),
            ("orders_has_a_binding", "take_away"),
        ] {
            let err = map_storage_error(
                StorageError::Constraint {
                    table: "orders".to_owned(),
                    constraint: constraint.to_owned(),
                    message: "violated".to_owned(),
                },
                "ORD-1",
            );
            match err {
                StoreError::Invalid(detail) => assert!(
                    detail.contains(needle),
                    "{constraint} should mention {needle}, said {detail:?}"
                ),
                other => panic!("{constraint} should be Invalid, got {other:?}"),
            }
        }
    }

    #[test]
    fn an_over_long_invoice_name_keeps_its_limit() {
        let err = map_storage_error(
            StorageError::Domain(peacock_core::error::Error::InvoiceNameTooLong {
                name: "TOO-LONG-SERIES-2627-000001".to_owned(),
                limit: 16,
            }),
            "ORD-1",
        );
        assert!(matches!(
            err,
            StoreError::InvoiceNameTooLong { limit: 16, .. }
        ));
    }

    #[test]
    fn an_unconfigured_series_names_the_series_and_the_year() {
        let err = map_storage_error(
            StorageError::Domain(peacock_core::error::Error::SeriesNotConfigured(
                "PCK".to_owned(),
                "2627".to_owned(),
            )),
            "ORD-1",
        );
        match err {
            StoreError::Invalid(detail) => {
                assert!(detail.contains("PCK"), "{detail}");
                assert!(detail.contains("2627"), "{detail}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn the_default_tax_rate_is_zero_not_a_guess() {
        // A wrong default rate is a compliance bug that looks like a working system.
        assert_eq!(InvoiceDefaults::default().tax_rate, Decimal::ZERO);
        assert_eq!(
            InvoiceDefaults::default().supply_type,
            SupplyType::Intrastate
        );
    }
}
