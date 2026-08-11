//! Invoice and payment endpoints.
//!
//! | method | path | purpose |
//! |---|---|---|
//! | POST | `/api/invoices` | create from an order (honours `Idempotency-Key`) |
//! | GET | `/api/invoices/:id` | fetch one |
//! | POST | `/api/invoices/:id/pay` | record a payment |
//! | GET | `/api/invoices` | list, filtered by business day / status / table |
//! | POST | `/api/invoices/:id/consolidate` | Paid → Consolidated |
//!
//! # Where the money comes from
//!
//! Every figure on the response is produced by one call to
//! [`peacock_core::tax::compute_totals`] and then **copied**. This module performs no
//! money arithmetic, so the parity harness's guarantees hold through the HTTP layer
//! unchanged.
//!
//! # Gapless numbering
//!
//! Numbers come from [`PgInvoiceRepo`], which allocates them with
//!
//! ```sql
//! UPDATE invoice_naming_series SET next_number = next_number + 1
//!  WHERE series = $1 AND fiscal_year = $2 RETURNING next_number - 1
//! ```
//!
//! inside the same transaction as the invoice insert *and* the `idempotency_keys` row.
//! The critical invariant (`invoicing.rs:5`) is that the key is recorded **with** the
//! allocated number, so a retried submit returns the original name instead of burning a
//! second one. This module does not re-implement any of it.
//!
//! # One backend (Lane W1-A)
//!
//! [`PgInvoiceRepo`] is the only store. The previous shape had an `InvoiceBackend` enum
//! whose `Memory` arm served an in-memory `InvoiceStore` whenever no `DATABASE_URL` was
//! configured, and every handler carried a `match` over the two. That is deleted.
//!
//! It had to go because of what it did on the money path when it was live. The in-memory
//! counter is a `HashMap` behind a `Mutex`: correct within one process, and worthless
//! across two of them or across a restart. A deployment that came up without a reachable
//! database therefore issued invoice `POS-2627-000001` — a plausible, Rule 46(b)-shaped
//! number — from an empty counter, served a full breakdown for it, accepted payment
//! against it, and lost the lot at the next restart. The number would then be issued
//! again. A handler that returns `503` is an outage; a handler that returns a *credible
//! invoice number for a bill nobody will ever be able to reconcile* is a compliance
//! incident, and it is invisible to any test that only reads the response body.
//!
//! So gaplessness is now the row lock's job in every configuration — see
//! `peacock-storage/src/repos/invoice.rs` and `005_invoice.sql` for why a sequence cannot
//! do it (`nextval` is exempt from rollback, so every failed insert would burn a number
//! and gap the series Rule 46(b) requires to be unbroken).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use chrono_tz::Tz;
use uuid::Uuid;

use peacock_core::businessday::BusinessDay;
use peacock_core::ids::InvoiceName;
use peacock_core::invoicing::{fiscal_year_code, fiscal_year_for};
use peacock_core::model::PosInvoiceStatus;
use peacock_core::money::Money;
use peacock_core::tax::{compute_totals, InvoiceLine};
use peacock_storage::repos::PgInvoiceRepo;

use crate::dto::invoice::{
    CreateInvoiceRequest, InvoiceLineResponse, InvoiceListQuery, InvoiceListResponse,
    InvoiceResponse, InvoiceStatusDto, InvoiceSummaryResponse, InvoiceTotalsResponse,
    PaymentResponse, RecordPaymentRequest,
};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Header carrying the idempotency key for `POST /api/invoices`.
pub const IDEMPOTENCY_KEY: &str = "idempotency-key";

/// Restaurant timezone. URY is single-region; IST is not negotiable and the 30-minute
/// offset matters (see `businessday.rs`).
const RESTAURANT_TZ: Tz = chrono_tz::Asia::Kolkata;

/// Business-day rollover hour, IST. A 02:00 invoice belongs to the previous day.
///
/// Hard-coded until `URY Report Settings.hours` is readable through storage; the same
/// value must then feed shift close, or bug 2 comes back as a mismatch between the two.
const BUSINESS_DAY_CUTOFF_HOUR: u32 = 3;

/// Branch every invoice is filed under until the request carries one.
///
/// `invoices.branch` is `NOT NULL` (005_invoice.sql) because the shift-close and P&L
/// queries are branch-scoped — bug 4's single revenue definition is
/// `(branch, business_day, status ∈ REVENUE)`. [`CreateInvoiceRequest`] has no branch
/// field, so single-site deployments file everything here. A multi-branch deployment
/// must add the field before its reports mean anything, and that is a DTO change rather
/// than a handler one.
const DEFAULT_BRANCH: &str = "Peacock - Main";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/invoices", post(create_invoice).get(list_invoices))
        .route("/api/invoices/:id", get(get_invoice))
        .route("/api/invoices/:id/pay", post(record_payment))
        .route("/api/invoices/:id/consolidate", post(consolidate_invoice))
}

// ---------------------------------------------------------------------------
// Projection: storage row → wire shape
// ---------------------------------------------------------------------------

/// The storage layer's row, in the shape the API returns.
///
/// [`peacock_storage::repos::StoredInvoice`] carries columns the API does not expose
/// (`series_number`, `no_of_pax`, `change_amount`) and lacks two the API does
/// (`order_id`, the owning idempotency key). This is the projection, kept as one function
/// so no two handlers can drift in what they publish.
///
/// `order_id` maps to the repository's `order_type` column: 007_order.sql owns orders and
/// the invoice table has no FK to it, so the order reference rides in the field upstream
/// uses for it. `idempotency_key` is not read back — the key is the request's, and the
/// handler already has it.
fn to_response(
    invoice: &peacock_storage::repos::StoredInvoice,
    idempotency_key: Option<Uuid>,
) -> InvoiceResponse {
    InvoiceResponse {
        invoice_id: invoice.name.as_str().to_owned(),
        order_id: invoice.order_type.clone().unwrap_or_default(),
        table: invoice
            .restaurant_table
            .as_ref()
            .map(|t| t.as_str().to_owned()),
        customer_name: invoice.customer.clone(),
        status: invoice.status.into(),
        posted_at: invoice.posted_at,
        business_day: invoice.business_day,
        fiscal_year: fiscal_year_for(invoice.business_day),
        lines: invoice
            .lines
            .iter()
            .map(|l| InvoiceLineResponse {
                item_code: l.item_code.as_str().to_owned(),
                item_name: l.item_name.clone(),
                quantity: l.qty,
                rate: l.rate,
                // The stored value, not a re-multiplication: 005_invoice.sql's
                // `invoice_lines_amount_is_qty_times_rate` CHECK already proved it.
                amount: l.amount,
                hsn_sac: l.hsn_sac.clone(),
            })
            .collect(),
        totals: InvoiceTotalsResponse::from(&invoice.totals),
        payments: invoice
            .payments
            .iter()
            .map(|p| PaymentResponse {
                method: pg_method_to_dto(p.method),
                amount: p.amount,
                reference: p.reference.clone(),
                paid_at: p.paid_at,
            })
            .collect(),
        // The trigger-maintained sum of the payment rows above (010_invoice_payments.sql),
        // never a figure this layer adds up itself.
        paid_amount: invoice.paid_amount,
        outstanding_amount: invoice.outstanding_amount(),
        idempotency_key,
    }
}

fn to_summary(invoice: &peacock_storage::repos::StoredInvoice) -> InvoiceSummaryResponse {
    InvoiceSummaryResponse {
        invoice_id: invoice.name.as_str().to_owned(),
        order_id: invoice.order_type.clone().unwrap_or_default(),
        table: invoice
            .restaurant_table
            .as_ref()
            .map(|t| t.as_str().to_owned()),
        customer_name: invoice.customer.clone(),
        status: invoice.status.into(),
        posted_at: invoice.posted_at,
        business_day: invoice.business_day,
        grand_total: invoice.totals.grand_total,
        rounded_total: invoice.totals.rounded_total,
        round_off: invoice.totals.round_off,
        paid_amount: invoice.paid_amount,
        outstanding_amount: invoice.outstanding_amount(),
    }
}

/// Wire method → storage method. Exhaustive both ways so a new instrument cannot be
/// silently dropped out of the cash drawer total.
fn dto_method_to_pg(
    method: crate::dto::invoice::PaymentMethodDto,
) -> peacock_storage::repos::PaymentMethod {
    use crate::dto::invoice::PaymentMethodDto as Dto;
    use peacock_storage::repos::PaymentMethod as Pg;
    match method {
        Dto::Cash => Pg::Cash,
        Dto::Card => Pg::Card,
        Dto::Upi => Pg::Upi,
        Dto::Wallet => Pg::Wallet,
        Dto::Credit => Pg::Credit,
    }
}

fn pg_method_to_dto(
    method: peacock_storage::repos::PaymentMethod,
) -> crate::dto::invoice::PaymentMethodDto {
    use crate::dto::invoice::PaymentMethodDto as Dto;
    use peacock_storage::repos::PaymentMethod as Pg;
    match method {
        Pg::Cash => Dto::Cash,
        Pg::Card => Dto::Card,
        Pg::Upi => Dto::Upi,
        Pg::Wallet => Dto::Wallet,
        Pg::Credit => Dto::Credit,
    }
}

/// Map a storage failure onto the HTTP vocabulary.
///
/// A `StorageError::Domain` carries a `peacock_core::Error` the existing
/// `From<DomainError>` impl already classifies, so those keep their statuses — a
/// `Conflict` stays 409, a `SeriesNotConfigured` stays 500. Everything else is an
/// infrastructure fault and becomes 500.
///
/// Two special cases:
///
/// * A missing invoice. `peacock_core::Error` has no generic `NotFound` variant (one per
///   entity, on purpose), so `PgInvoiceRepo` reports it as
///   `Conflict { actual: "no such invoice" }`. Left alone that would surface as 409 where
///   the API contract — and every test — expects 404.
/// * An overpayment. The repository refuses it as a `Conflict`, but from the caller's side
///   the *request* is wrong rather than the world: they tendered more than the bill. 400
///   is the answer that tells them to change the amount, and it is what the endpoint has
///   always returned.
fn storage_error(err: peacock_storage::StorageError) -> ApiError {
    use peacock_core::Error as DomainError;
    use peacock_storage::StorageError as SE;

    match err {
        SE::Domain(DomainError::Conflict { expected, actual }) if actual == "no such invoice" => {
            ApiError::not_found(format!("{expected} not found"))
        }
        // `record_payment` phrases the ceiling as "a payment of at most {outstanding}".
        SE::Domain(DomainError::Conflict { expected, actual })
            if expected.starts_with("a payment of at most") =>
        {
            ApiError::invalid_input(format!("{actual} exceeds {expected}"))
        }
        SE::Domain(domain) => ApiError::from(domain),
        // A lost race is the caller's to retry, not a server fault. 409 tells them so.
        SE::Retryable { sqlstate, message } => ApiError::conflict(format!(
            "the write lost a race ({sqlstate}) and can be retried: {message}"
        )),
        other => ApiError::internal(other.to_string()),
    }
}
// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Reads and validates the `Idempotency-Key` header.
///
/// A malformed key is rejected rather than replaced with a fresh UUID: silently
/// generating one would turn a client's retry into a duplicate invoice.
fn idempotency_key(headers: &HeaderMap) -> ApiResult<Uuid> {
    let raw = headers
        .get(IDEMPOTENCY_KEY)
        .ok_or_else(|| {
            ApiError::invalid_input(
                "Idempotency-Key header is required for invoice creation (gapless numbering)",
            )
        })?
        .to_str()
        .map_err(|_| ApiError::invalid_input("Idempotency-Key must be ASCII"))?;

    Uuid::parse_str(raw.trim())
        .map_err(|_| ApiError::invalid_input(format!("Idempotency-Key {raw:?} is not a UUID")))
}

/// `POST /api/invoices` — create an invoice from an order.
///
/// Returns 201 on first sight and 200 on replay, so a client can tell the two apart
/// while both carry the identical body.
async fn create_invoice(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateInvoiceRequest>,
) -> ApiResult<(StatusCode, Json<InvoiceResponse>)> {
    let key = idempotency_key(&headers)?;
    validate_create(&req)?;

    let posted_at = req.posted_at.unwrap_or_else(Utc::now);
    let business_day =
        BusinessDay::for_instant(posted_at, BUSINESS_DAY_CUTOFF_HOUR, RESTAURANT_TZ).label;

    // The name carries the compact 4-character code.
    let fy_code = fiscal_year_code(business_day);

    let lines: Vec<InvoiceLine> = req
        .lines
        .iter()
        .map(|l| InvoiceLine {
            item_name: l.item_name.clone(),
            quantity: l.quantity,
            rate: l.rate,
            hsn_sac: l.hsn_sac.clone(),
        })
        .collect();

    // The single money computation. Everything downstream copies from this.
    let totals = compute_totals(
        &lines,
        req.discount,
        req.tax_rate,
        req.supply_type.into(),
        req.discount_basis.into(),
    )?;

    let repo: PgInvoiceRepo = state.invoice_repo();

    // The series must be registered for this fiscal year before a number can be
    // allocated. Doing it here rather than demanding an out-of-band setup step keeps the
    // first invoice of a new financial year from failing at 00:00 on 1 April;
    // `register_series` is `ON CONFLICT DO NOTHING`, so it never rewinds a counter onto
    // numbers that were already issued.
    repo.register_series(&req.series, &fy_code, 1)
        .await
        .map_err(storage_error)?;

    let new_invoice = peacock_storage::repos::NewInvoice {
        naming_series: req.series.clone(),
        fiscal_year: fy_code,
        restaurant: None,
        restaurant_table: req
            .table
            .as_deref()
            .map(peacock_core::ids::TableName::from),
        restaurant_room: None,
        branch: peacock_core::ids::BranchName::from(DEFAULT_BRANCH),
        pos_profile: None,
        customer: req.customer_name.clone(),
        waiter: None,
        cashier: None,
        no_of_pax: 0,
        // The order this invoice bills. See `to_response`.
        order_type: Some(req.order_id.clone()),
        posted_at,
        business_day,
        supply_type: req.supply_type.into(),
        discount_basis: req.discount_basis.into(),
        tax_rate: req.tax_rate,
        // The totals the single `compute_totals` call above produced, stored verbatim.
        // 005_invoice.sql's CHECK constraints re-assert every tax invariant on the way in,
        // so storage cannot contradict the arithmetic the parity harness proved.
        totals,
        paid_amount: Money::ZERO,
        change_amount: Money::ZERO,
        comments: None,
        lines: req
            .lines
            .iter()
            .map(|l| peacock_storage::repos::NewInvoiceLine {
                item_code: peacock_core::ids::ItemCode::from(l.item_code.as_str()),
                item_name: l.item_name.clone(),
                qty: l.quantity,
                rate: l.rate,
                hsn_sac: l.hsn_sac.clone(),
                course: None,
                comments: None,
                serve_priority: 0,
                indicate_course: false,
            })
            .collect(),
    };

    let created = repo
        .create_invoice_idempotent(key, &new_invoice)
        .await
        .map_err(storage_error)?;

    let body = Json(to_response(&created.invoice, Some(key)));
    Ok(match created.outcome {
        peacock_storage::repos::CreateOutcome::Created => (StatusCode::CREATED, body),
        peacock_storage::repos::CreateOutcome::Replayed => (StatusCode::OK, body),
    })
}

/// Rejects requests the domain would otherwise accept as nonsense.
///
/// Runs **before** any number is allocated. A rejection after allocation would consume
/// a number and gap a series Rule 46(b) requires to be gapless.
fn validate_create(req: &CreateInvoiceRequest) -> ApiResult<()> {
    if req.order_id.trim().is_empty() {
        return Err(ApiError::invalid_input("order_id is required"));
    }
    if req.series.trim().is_empty() {
        return Err(ApiError::invalid_input("series is required"));
    }
    if req.customer_name.trim().is_empty() {
        return Err(ApiError::invalid_input("customer_name is required"));
    }
    if req.lines.is_empty() {
        return Err(ApiError::invalid_input(
            "an invoice must have at least one line",
        ));
    }
    if req.tax_rate.is_sign_negative() {
        return Err(ApiError::invalid_input("tax_rate cannot be negative"));
    }
    if req.discount.inner().is_sign_negative() {
        return Err(ApiError::invalid_input("discount cannot be negative"));
    }
    for line in &req.lines {
        if line.item_code.trim().is_empty() {
            return Err(ApiError::invalid_input("every line needs an item_code"));
        }
        if line.quantity <= rust_decimal::Decimal::ZERO {
            return Err(ApiError::invalid_input(format!(
                "line {} has non-positive quantity {}",
                line.item_code, line.quantity
            )));
        }
        if line.rate.inner().is_sign_negative() {
            return Err(ApiError::invalid_input(format!(
                "line {} has a negative rate",
                line.item_code
            )));
        }
    }
    Ok(())
}

/// `GET /api/invoices/:id`
async fn get_invoice(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<InvoiceResponse>> {
    let name = InvoiceName::from(id.as_str());
    let invoice = state
        .invoice_repo()
        .find(&name)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| ApiError::not_found(format!("invoice {id} not found")))?;
    // The owning key is not read back: it is the requester's, and a GET has none.
    Ok(Json(to_response(&invoice, None)))
}

/// `POST /api/invoices/:id/pay` — record a payment.
///
/// Settling the full `rounded_total` moves the invoice to `Paid`. A short payment is
/// accepted and leaves the invoice `Draft` with an outstanding balance, which is how a
/// split-tender bill accumulates.
///
/// The overpayment check, the insert and the status move all happen inside the
/// repository's transaction, which holds the invoice row under `FOR UPDATE` throughout.
/// Doing any of it here would need a lock this layer does not have: two concurrent ₹300
/// tenders on a ₹378 bill would both read ₹0 settled, both pass, and leave the invoice
/// ₹222 over.
async fn record_payment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RecordPaymentRequest>,
) -> ApiResult<Json<InvoiceResponse>> {
    if req.amount.inner().is_sign_negative() || req.amount.is_zero() {
        return Err(ApiError::invalid_input("payment amount must be positive"));
    }
    let paid_at = req.paid_at.unwrap_or_else(Utc::now);

    let name = InvoiceName::from(id.as_str());
    let invoice = state
        .invoice_repo()
        .record_payment(
            &name,
            &peacock_storage::repos::NewPayment {
                method: dto_method_to_pg(req.method),
                amount: req.amount,
                reference: req.reference.clone(),
                paid_at,
            },
        )
        .await
        .map_err(storage_error)?;

    Ok(Json(to_response(&invoice, None)))
}

/// `POST /api/invoices/:id/consolidate` — Paid → Consolidated.
///
/// Consolidation is the end-of-day merge into a Sales Invoice, so only a settled
/// invoice can make the move. Both statuses count as revenue
/// (`PosInvoiceStatus::REVENUE`), so the transition does not change any total.
///
/// `set_status` reads and writes under one `FOR UPDATE`, refuses anything but a legal
/// edge, and treats Consolidated → Consolidated as a no-op — so a retried end-of-day job
/// is idempotent rather than failing half way through. The trigger in 005_invoice.sql is
/// the backstop.
async fn consolidate_invoice(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<InvoiceResponse>> {
    let name = InvoiceName::from(id.as_str());
    let invoice = state
        .invoice_repo()
        .set_status(&name, PosInvoiceStatus::Consolidated)
        .await
        .map_err(storage_error)?;
    Ok(Json(to_response(&invoice, None)))
}

/// `GET /api/invoices` — list with filters.
///
/// `from`/`to` filter on **business day**, not on `posted_at`'s calendar date. A
/// 01:30 invoice belongs to the previous business day, and filtering it by calendar
/// date is upstream bug 2 (`sub_pos_closing.py:42`).
async fn list_invoices(
    State(state): State<AppState>,
    Query(query): Query<InvoiceListQuery>,
) -> ApiResult<Json<InvoiceListResponse>> {
    let status_filter = match query.status.as_deref() {
        Some(raw) => Some(InvoiceStatusDto::parse_filter(raw).ok_or_else(|| {
            ApiError::invalid_input(format!(
                "unknown status {raw:?}; expected Draft, Paid, Consolidated or Return"
            ))
        })?),
        None => None,
    };

    if let (Some(from), Some(to)) = (query.from, query.to) {
        if from > to {
            return Err(ApiError::invalid_input(format!(
                "from {from} is after to {to}"
            )));
        }
    }

    // Filtered in SQL on the columns that have indexes for it
    // (`invoices_business_day_idx`, `invoices_status_idx`,
    // `invoices_restaurant_table_idx`) rather than by reading the table and discarding
    // rows in Rust. Business day, not `posted_at::date`: a 01:30 invoice belongs to the
    // previous business day, and filtering it by calendar date is upstream bug 2.
    let mut matching = state
        .invoice_repo()
        .list_filtered(
            query.from,
            query.to,
            status_filter.map(PosInvoiceStatus::from),
            query
                .table
                .as_deref()
                .map(peacock_core::ids::TableName::from)
                .as_ref(),
        )
        .await
        .map_err(storage_error)?;

    // Newest first — what a cashier looking for the last bill wants.
    matching.sort_by_key(|b| std::cmp::Reverse(b.posted_at));

    // Revenue uses rounded_total over PosInvoiceStatus::REVENUE, the one definition
    // shift close and the P&L share (businessday.rs bugs 3 and 4).
    let total_revenue: Money = matching
        .iter()
        .filter(|inv| inv.status.counts_as_revenue())
        .map(|inv| inv.totals.rounded_total)
        .sum();

    let invoices: Vec<InvoiceSummaryResponse> = matching.iter().map(to_summary).collect();

    Ok(Json(InvoiceListResponse {
        count: invoices.len(),
        invoices,
        total_revenue,
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::testing::TestDb;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use rust_decimal_macros::dec;
    use tower::ServiceExt;

    // -----------------------------------------------------------------------
    // Harness
    // -----------------------------------------------------------------------
    //
    // One throwaway PostgreSQL database per test (`TestDb`), not a shared one. These
    // tests assert on invoice *numbers*, which come from a counter row: a shared database
    // would make `POS-2627-000001` depend on which test ran first, and the suite would go
    // green or red by scheduling accident.
    //
    // Before Lane W1-A this module ran against an in-memory store and needed no database.
    // That is exactly why it could not catch the bug this lane exists to fix — it was
    // testing the fallback, not the product.

    /// A migrated database and the production router over it.
    struct Fixture {
        db: TestDb,
        app: Router,
    }

    async fn app() -> Fixture {
        let db = TestDb::new().await;
        let app = crate::app::build_with_storage(Config::default(), db.storage().clone());
        Fixture { db, app }
    }

    async fn send(app: &Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("every response body must be JSON")
        };
        (status, json)
    }

    fn post(uri: &str, key: Option<Uuid>, body: serde_json::Value) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(key) = key {
            builder = builder.header(IDEMPOTENCY_KEY, key.to_string());
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    /// The §5 worked example: 4 × ₹100, ₹40 discount, 5% GST → grand 378.
    ///
    /// `CURRY` and `T-01` are seeded by `TestDb`: `invoice_lines.item_code` and
    /// `invoices.restaurant_table` are real foreign keys now, so an invented code is a
    /// constraint violation rather than a stored string.
    fn create_body() -> serde_json::Value {
        serde_json::json!({
            "order_id": "ORD-001",
            "table": "T-01",
            "customer_name": "Walk-in",
            "lines": [
                {"item_code": "CURRY", "item_name": "Chicken Curry", "quantity": "4", "rate": "100"}
            ],
            "discount": "40",
            "tax_rate": "0.05",
            "series": "POS",
            "posted_at": "2026-07-28T14:30:00Z"
        })
    }

    /// Creates an invoice and returns its body, asserting 201.
    async fn create(app: &Router, body: serde_json::Value) -> serde_json::Value {
        let (status, json) = send(app, post("/api/invoices", Some(Uuid::new_v4()), body)).await;
        assert_eq!(status, StatusCode::CREATED, "create failed: {json}");
        json
    }

    /// Creates a fully settled invoice and returns its id.
    async fn create_paid(app: &Router, body: serde_json::Value) -> String {
        let invoice = create(app, body).await;
        let id = invoice["invoice_id"].as_str().unwrap().to_owned();
        let due = invoice["rounded_total"].as_str().unwrap().to_owned();
        let (status, json) = send(
            app,
            post(
                &format!("/api/invoices/{id}/pay"),
                None,
                serde_json::json!({"method": "Cash", "amount": due}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "payment failed: {json}");
        assert_eq!(json["status"], "Paid");
        id
    }

    // -----------------------------------------------------------------------
    // Creation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_returns_201_with_the_domain_totals() {
        let f = app().await;
        let json = create(&f.app, create_body()).await;

        // Every figure is the one peacock_core::tax produced, as a string.
        assert_eq!(json["net_total"], "400");
        assert_eq!(json["discount"], "40");
        assert_eq!(json["taxable_value"], "360");
        assert_eq!(json["tax"]["total_tax"], "18.00");
        assert_eq!(json["tax"]["cgst"], "9.00");
        assert_eq!(json["tax"]["sgst"], "9.00");
        assert_eq!(json["tax"]["igst"], "0");
        assert_eq!(json["grand_total"], "378.00");
        assert_eq!(json["rounded_total"], "378");
        assert_eq!(json["status"], "Draft");
        assert_eq!(json["order_id"], "ORD-001");
        assert_eq!(json["table"], "T-01");
    }

    #[tokio::test]
    async fn a_created_invoice_is_a_committed_row_not_just_a_201() {
        // The assertion the in-memory backend made impossible: the response is checked
        // against the table, so a handler that answers 201 and writes nowhere fails here.
        let f = app().await;
        let json = create(&f.app, create_body()).await;
        let id = json["invoice_id"].as_str().unwrap();

        let (name, rounded): (String, rust_decimal::Decimal) =
            sqlx::query_as("SELECT name, rounded_total FROM invoices WHERE name = $1")
                .bind(id)
                .fetch_one(f.db.pool())
                .await
                .expect("the invoice must be in the database");

        assert_eq!(name, id);
        assert_eq!(rounded, dec!(378));

        let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM invoice_lines WHERE invoice = $1")
            .bind(id)
            .fetch_one(f.db.pool())
            .await
            .unwrap();
        assert_eq!(lines, 1, "the line must be persisted too");
    }

    #[tokio::test]
    async fn created_invoice_number_is_rule_46b_compliant() {
        let f = app().await;
        let json = create(&f.app, create_body()).await;
        let id = json["invoice_id"].as_str().unwrap();

        // series-fycode-counter, ≤16 chars. Posted 2026-07-28 → FY 2026-27 → "2627".
        assert_eq!(id, "POS-2627-000001");
        assert!(id.len() <= 16, "{id} exceeds the Rule 46(b) cap");
        assert_eq!(json["fiscal_year"], "2026-27");
    }

    #[tokio::test]
    async fn numbering_is_gapless_across_sequential_creates() {
        let f = app().await;
        let mut ids = Vec::new();
        for n in 0..5 {
            let mut body = create_body();
            body["order_id"] = serde_json::json!(format!("ORD-{n:03}"));
            ids.push(
                create(&f.app, body).await["invoice_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }

        assert_eq!(
            ids,
            vec![
                "POS-2627-000001",
                "POS-2627-000002",
                "POS-2627-000003",
                "POS-2627-000004",
                "POS-2627-000005",
            ],
            "the series must have no gaps"
        );

        // And the database agrees, which is the claim the counter exists to support.
        assert!(f
            .db
            .storage()
            .invoice_repo()
            .find_series_gaps("POS", "2627")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn create_requires_an_idempotency_key() {
        let f = app().await;
        let (status, json) = send(&f.app, post("/api/invoices", None, create_body())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["detail"]
            .as_str()
            .unwrap()
            .contains("Idempotency-Key"));
    }

    #[tokio::test]
    async fn a_malformed_idempotency_key_is_rejected_not_replaced() {
        // Generating a fresh UUID here would turn a retry into a duplicate invoice.
        let f = app().await;
        let request = Request::builder()
            .method("POST")
            .uri("/api/invoices")
            .header("content-type", "application/json")
            .header(IDEMPOTENCY_KEY, "not-a-uuid")
            .body(Body::from(serde_json::to_vec(&create_body()).unwrap()))
            .unwrap();

        let (status, json) = send(&f.app, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["detail"].as_str().unwrap().contains("not a UUID"));
    }

    #[tokio::test]
    async fn rejected_input_does_not_burn_an_invoice_number() {
        // The gapless guarantee: validation runs before allocation.
        let f = app().await;
        let mut bad = create_body();
        bad["lines"] = serde_json::json!([]);
        let (status, _) = send(&f.app, post("/api/invoices", Some(Uuid::new_v4()), bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let json = create(&f.app, create_body()).await;
        assert_eq!(
            json["invoice_id"], "POS-2627-000001",
            "a rejected request must not consume a number"
        );
    }

    #[tokio::test]
    async fn an_over_long_series_is_refused_without_burning_a_number() {
        let f = app().await;
        let mut body = create_body();
        body["series"] = serde_json::json!("TOOLONG");

        let (status, _) = send(&f.app, post("/api/invoices", Some(Uuid::new_v4()), body)).await;
        // InvoiceNameTooLong is a configuration fault, so 500 by the error mapping.
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

        let json = create(&f.app, create_body()).await;
        assert_eq!(json["invoice_id"], "POS-2627-000001");
    }

    #[tokio::test]
    async fn create_validates_the_payload() {
        let f = app().await;
        let cases: Vec<(&str, serde_json::Value)> = vec![
            ("order_id", serde_json::json!({"order_id": ""})),
            ("series", serde_json::json!({"series": "  "})),
            ("customer_name", serde_json::json!({"customer_name": ""})),
            ("tax_rate", serde_json::json!({"tax_rate": "-0.05"})),
            ("discount", serde_json::json!({"discount": "-1"})),
        ];

        for (field, patch) in cases {
            let mut body = create_body();
            for (k, v) in patch.as_object().unwrap() {
                body[k] = v.clone();
            }
            let (status, json) =
                send(&f.app, post("/api/invoices", Some(Uuid::new_v4()), body)).await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "{field} must be validated, got {json}"
            );
        }
    }

    #[tokio::test]
    async fn create_rejects_non_positive_quantity_and_negative_rate() {
        let f = app().await;
        for line in [
            serde_json::json!({"item_code": "X", "item_name": "X", "quantity": "0", "rate": "10"}),
            serde_json::json!({"item_code": "X", "item_name": "X", "quantity": "-1", "rate": "10"}),
            serde_json::json!({"item_code": "X", "item_name": "X", "quantity": "1", "rate": "-10"}),
            serde_json::json!({"item_code": "", "item_name": "X", "quantity": "1", "rate": "10"}),
        ] {
            let mut body = create_body();
            body["lines"] = serde_json::json!([line]);
            let (status, _) =
                send(&f.app, post("/api/invoices", Some(Uuid::new_v4()), body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn takeaway_invoice_has_no_table() {
        let f = app().await;
        let mut body = create_body();
        body.as_object_mut().unwrap().remove("table");

        let json = create(&f.app, body).await;
        assert!(
            json.get("table").is_none(),
            "an absent table must be omitted, not null"
        );
    }

    #[tokio::test]
    async fn an_unknown_item_code_is_refused_by_the_foreign_key() {
        // Only reachable with a real database: the in-memory store stored whatever string
        // it was handed, so a typo'd item code became a line on a tax document.
        let f = app().await;
        let mut body = create_body();
        body["lines"] = serde_json::json!([
            {"item_code": "NO-SUCH-ITEM", "item_name": "Ghost", "quantity": "1", "rate": "10"}
        ]);

        let (status, _) = send(&f.app, post("/api/invoices", Some(Uuid::new_v4()), body)).await;
        assert!(
            status.is_client_error() || status.is_server_error(),
            "an item that does not exist must not produce an invoice"
        );

        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices")
            .fetch_one(f.db.pool())
            .await
            .unwrap();
        assert_eq!(rows, 0, "the rejected insert must have rolled back");
    }

    // -----------------------------------------------------------------------
    // Idempotency
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn ten_replays_of_one_key_yield_one_invoice() {
        let f = app().await;
        let key = Uuid::new_v4();

        let (status, first) = send(&f.app, post("/api/invoices", Some(key), create_body())).await;
        assert_eq!(status, StatusCode::CREATED);
        let invoice_id = first["invoice_id"].as_str().unwrap().to_owned();

        for attempt in 1..=10 {
            let (status, replay) =
                send(&f.app, post("/api/invoices", Some(key), create_body())).await;
            // 200 not 201: the invoice already existed.
            assert_eq!(status, StatusCode::OK, "replay {attempt} must not create");
            assert_eq!(
                replay["invoice_id"], invoice_id,
                "replay {attempt} returned a different invoice"
            );
            assert_eq!(replay, first, "replay {attempt} body diverged");
        }

        // The counter advanced exactly once: a fresh key gets 000002.
        let (status, fresh) = send(
            &f.app,
            post("/api/invoices", Some(Uuid::new_v4()), create_body()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(fresh["invoice_id"], "POS-2627-000002");

        let (_, list) = send(&f.app, get_req("/api/invoices")).await;
        assert_eq!(list["count"], 2, "10 replays must not add rows");

        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices")
            .fetch_one(f.db.pool())
            .await
            .unwrap();
        assert_eq!(rows, 2, "and the table must agree with the list");
    }

    #[tokio::test]
    async fn a_replay_ignores_a_changed_body() {
        // The key owns the invoice. A retry with drifted content must not mutate the
        // original, or the stored total would stop matching the allocated number.
        let f = app().await;
        let key = Uuid::new_v4();
        let (_, first) = send(&f.app, post("/api/invoices", Some(key), create_body())).await;

        let mut tampered = create_body();
        tampered["lines"] = serde_json::json!([
            {"item_code": "X", "item_name": "X", "quantity": "99", "rate": "999"}
        ]);
        let (status, replay) = send(&f.app, post("/api/invoices", Some(key), tampered)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(replay["grand_total"], first["grand_total"]);
        assert_eq!(replay["invoice_id"], first["invoice_id"]);
    }

    #[tokio::test]
    async fn different_keys_get_different_numbers() {
        let f = app().await;
        let a = create(&f.app, create_body()).await;
        let b = create(&f.app, create_body()).await;
        assert_ne!(a["invoice_id"], b["invoice_id"]);
        assert_eq!(a["invoice_id"], "POS-2627-000001");
        assert_eq!(b["invoice_id"], "POS-2627-000002");
    }

    #[tokio::test]
    async fn the_response_echoes_the_owning_key() {
        let f = app().await;
        let key = Uuid::new_v4();
        let (_, json) = send(&f.app, post("/api/invoices", Some(key), create_body())).await;
        assert_eq!(json["idempotency_key"], key.to_string());
    }

    // -----------------------------------------------------------------------
    // Get
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_returns_the_created_invoice_byte_for_byte() {
        let f = app().await;
        let created = create(&f.app, create_body()).await;
        let id = created["invoice_id"].as_str().unwrap();

        let (status, mut fetched) = send(&f.app, get_req(&format!("/api/invoices/{id}"))).await;
        assert_eq!(status, StatusCode::OK);

        // A GET has no idempotency key to echo, so that one field legitimately differs;
        // every money figure must not.
        assert!(fetched["idempotency_key"].is_null());
        fetched["idempotency_key"] = created["idempotency_key"].clone();
        assert_eq!(fetched, created, "a read must not restate any money figure");
    }

    #[tokio::test]
    async fn get_unknown_invoice_returns_404_problem_details() {
        let f = app().await;
        let (status, json) = send(&f.app, get_req("/api/invoices/POS-2627-999999")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["status"], 404);
        assert_eq!(json["instance"], "/api/invoices/POS-2627-999999");
        assert!(json["detail"].as_str().unwrap().contains("not found"));
    }

    // -----------------------------------------------------------------------
    // Payment
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn full_payment_marks_the_invoice_paid_and_stores_the_method() {
        let f = app().await;
        let created = create(&f.app, create_body()).await;
        let id = created["invoice_id"].as_str().unwrap();

        let (status, json) = send(
            &f.app,
            post(
                &format!("/api/invoices/{id}/pay"),
                None,
                serde_json::json!({
                    "method": "Upi",
                    "amount": "378",
                    "reference": "txn-4242",
                    "paid_at": "2026-07-28T15:00:00Z"
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "Paid");
        assert_eq!(json["paid_amount"], "378");
        assert_eq!(json["outstanding_amount"], "0");
        assert_eq!(json["payments"][0]["method"], "Upi");
        assert_eq!(json["payments"][0]["amount"], "378");
        assert_eq!(json["payments"][0]["reference"], "txn-4242");
        assert_eq!(json["payments"][0]["paid_at"], "2026-07-28T15:00:00Z");

        // The payment row is real, and the trigger moved `invoices.paid_amount` with it.
        let (method, amount, status_col): (String, rust_decimal::Decimal, String) =
            sqlx::query_as(
                "SELECT p.method::TEXT, p.amount, i.status::TEXT
                   FROM invoice_payments p JOIN invoices i ON i.name = p.invoice
                  WHERE p.invoice = $1",
            )
            .bind(id)
            .fetch_one(f.db.pool())
            .await
            .expect("the payment must be persisted");
        assert_eq!(method, "UPI");
        assert_eq!(amount, dec!(378));
        assert_eq!(status_col, "Paid");
    }

    #[tokio::test]
    async fn payment_does_not_alter_any_invoice_total() {
        let f = app().await;
        let created = create(&f.app, create_body()).await;
        let id = created["invoice_id"].as_str().unwrap();

        let (_, paid) = send(
            &f.app,
            post(
                &format!("/api/invoices/{id}/pay"),
                None,
                serde_json::json!({"method": "Cash", "amount": "378"}),
            ),
        )
        .await;

        for field in [
            "net_total",
            "discount",
            "taxable_value",
            "grand_total",
            "rounded_total",
            "round_off",
        ] {
            assert_eq!(paid[field], created[field], "{field} moved during payment");
        }
        assert_eq!(paid["tax"], created["tax"]);
    }

    #[tokio::test]
    async fn split_tender_accumulates_then_settles() {
        let f = app().await;
        let created = create(&f.app, create_body()).await;
        let id = created["invoice_id"].as_str().unwrap();

        let (_, part) = send(
            &f.app,
            post(
                &format!("/api/invoices/{id}/pay"),
                None,
                serde_json::json!({"method": "Card", "amount": "300"}),
            ),
        )
        .await;
        assert_eq!(part["status"], "Draft", "a short payment must not settle");
        assert_eq!(part["paid_amount"], "300");
        assert_eq!(part["outstanding_amount"], "78");

        let (_, rest) = send(
            &f.app,
            post(
                &format!("/api/invoices/{id}/pay"),
                None,
                serde_json::json!({"method": "Cash", "amount": "78"}),
            ),
        )
        .await;
        assert_eq!(rest["status"], "Paid");
        assert_eq!(rest["paid_amount"], "378");
        assert_eq!(rest["outstanding_amount"], "0");
        assert_eq!(rest["payments"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn overpayment_is_refused() {
        let f = app().await;
        let created = create(&f.app, create_body()).await;
        let id = created["invoice_id"].as_str().unwrap();

        let (status, json) = send(
            &f.app,
            post(
                &format!("/api/invoices/{id}/pay"),
                None,
                serde_json::json!({"method": "Cash", "amount": "500"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["detail"].as_str().unwrap().contains("exceeds"));

        // And nothing was recorded: the repository's transaction rolled back.
        let (_, invoice) = send(&f.app, get_req(&format!("/api/invoices/{id}"))).await;
        assert_eq!(invoice["paid_amount"], "0");
        assert_eq!(invoice["status"], "Draft");
    }

    #[tokio::test]
    async fn zero_and_negative_payments_are_refused() {
        let f = app().await;
        let created = create(&f.app, create_body()).await;
        let id = created["invoice_id"].as_str().unwrap();

        for amount in ["0", "-10"] {
            let (status, _) = send(
                &f.app,
                post(
                    &format!("/api/invoices/{id}/pay"),
                    None,
                    serde_json::json!({"method": "Cash", "amount": amount}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "amount {amount}");
        }
    }

    #[tokio::test]
    async fn paying_an_unknown_invoice_returns_404() {
        let f = app().await;
        let (status, _) = send(
            &f.app,
            post(
                "/api/invoices/POS-2627-999999/pay",
                None,
                serde_json::json!({"method": "Cash", "amount": "10"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_consolidated_invoice_refuses_further_payment() {
        let f = app().await;
        let id = create_paid(&f.app, create_body()).await;
        let (status, _) = send(
            &f.app,
            post(
                &format!("/api/invoices/{id}/consolidate"),
                None,
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, json) = send(
            &f.app,
            post(
                &format!("/api/invoices/{id}/pay"),
                None,
                serde_json::json!({"method": "Cash", "amount": "1"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(json["detail"].as_str().unwrap().contains("Consolidated"));
    }

    // -----------------------------------------------------------------------
    // Consolidation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn consolidate_moves_paid_to_consolidated_without_touching_money() {
        let f = app().await;
        let id = create_paid(&f.app, create_body()).await;

        let (_, before) = send(&f.app, get_req(&format!("/api/invoices/{id}"))).await;
        let (status, after) = send(
            &f.app,
            post(
                &format!("/api/invoices/{id}/consolidate"),
                None,
                serde_json::json!({}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(after["status"], "Consolidated");
        assert_eq!(after["rounded_total"], before["rounded_total"]);
        assert_eq!(after["grand_total"], before["grand_total"]);
        assert_eq!(after["tax"], before["tax"]);
    }

    #[tokio::test]
    async fn consolidating_a_draft_invoice_is_a_conflict() {
        let f = app().await;
        let created = create(&f.app, create_body()).await;
        let id = created["invoice_id"].as_str().unwrap();

        let (status, _) = send(
            &f.app,
            post(
                &format!("/api/invoices/{id}/consolidate"),
                None,
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn consolidating_twice_is_idempotent() {
        let f = app().await;
        let id = create_paid(&f.app, create_body()).await;
        let uri = format!("/api/invoices/{id}/consolidate");

        let (first_status, first) = send(&f.app, post(&uri, None, serde_json::json!({}))).await;
        let (second_status, second) = send(&f.app, post(&uri, None, serde_json::json!({}))).await;

        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(second_status, StatusCode::OK);
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn consolidating_an_unknown_invoice_returns_404() {
        let f = app().await;
        let (status, _) = send(
            &f.app,
            post(
                "/api/invoices/POS-2627-999999/consolidate",
                None,
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // Listing and filters
    // -----------------------------------------------------------------------

    /// Three invoices: T-01 paid, T-02 draft, T-01 consolidated, on two business days.
    async fn seed_for_filters(app: &Router) {
        // 2026-07-28 20:00 IST → business day 2026-07-28.
        let mut a = create_body();
        a["table"] = serde_json::json!("T-01");
        a["posted_at"] = serde_json::json!("2026-07-28T14:30:00Z");
        create_paid(app, a).await;

        // Same business day, different table, left as Draft.
        let mut b = create_body();
        b["table"] = serde_json::json!("T-02");
        b["posted_at"] = serde_json::json!("2026-07-28T15:30:00Z");
        create(app, b).await;

        // 2026-07-30 → its own business day, consolidated.
        let mut c = create_body();
        c["table"] = serde_json::json!("T-01");
        c["posted_at"] = serde_json::json!("2026-07-30T14:30:00Z");
        let id = create_paid(app, c).await;
        send(
            app,
            post(
                &format!("/api/invoices/{id}/consolidate"),
                None,
                serde_json::json!({}),
            ),
        )
        .await;
    }

    #[tokio::test]
    async fn an_unfiltered_list_returns_everything_newest_first() {
        let f = app().await;
        seed_for_filters(&f.app).await;

        let (status, json) = send(&f.app, get_req("/api/invoices")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["count"], 3);

        let posted: Vec<&str> = json["invoices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["posted_at"].as_str().unwrap())
            .collect();
        let mut sorted = posted.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(posted, sorted, "list must be newest first");
    }

    #[tokio::test]
    async fn status_filter_selects_one_status() {
        let f = app().await;
        seed_for_filters(&f.app).await;

        for (status_name, expected) in
            [("Paid", 1), ("Draft", 1), ("Consolidated", 1), ("Return", 0)]
        {
            let (code, json) = send(
                &f.app,
                get_req(&format!("/api/invoices?status={status_name}")),
            )
            .await;
            assert_eq!(code, StatusCode::OK);
            assert_eq!(json["count"], expected, "status={status_name}");
            for inv in json["invoices"].as_array().unwrap() {
                assert_eq!(inv["status"], status_name);
            }
        }
    }

    #[tokio::test]
    async fn status_filter_is_case_insensitive_and_rejects_junk() {
        let f = app().await;
        seed_for_filters(&f.app).await;

        let (_, lower) = send(&f.app, get_req("/api/invoices?status=paid")).await;
        assert_eq!(lower["count"], 1);

        let (status, json) = send(&f.app, get_req("/api/invoices?status=settled")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["detail"].as_str().unwrap().contains("unknown status"));
    }

    #[tokio::test]
    async fn table_filter_selects_one_table() {
        let f = app().await;
        seed_for_filters(&f.app).await;

        let (_, t1) = send(&f.app, get_req("/api/invoices?table=T-01")).await;
        assert_eq!(t1["count"], 2);
        for inv in t1["invoices"].as_array().unwrap() {
            assert_eq!(inv["table"], "T-01");
        }

        let (_, t2) = send(&f.app, get_req("/api/invoices?table=T-02")).await;
        assert_eq!(t2["count"], 1);

        let (_, none) = send(&f.app, get_req("/api/invoices?table=T-99")).await;
        assert_eq!(none["count"], 0);
        assert_eq!(none["total_revenue"], "0");
    }

    #[tokio::test]
    async fn date_range_filter_is_inclusive_on_both_ends() {
        let f = app().await;
        seed_for_filters(&f.app).await;

        // A single business day: from == to.
        let (_, one_day) =
            send(&f.app, get_req("/api/invoices?from=2026-07-28&to=2026-07-28")).await;
        assert_eq!(one_day["count"], 2);
        for inv in one_day["invoices"].as_array().unwrap() {
            assert_eq!(inv["business_day"], "2026-07-28");
        }

        // Spanning both days.
        let (_, span) =
            send(&f.app, get_req("/api/invoices?from=2026-07-28&to=2026-07-30")).await;
        assert_eq!(span["count"], 3);

        // Open-ended lower bound excludes the earlier day.
        let (_, from_only) = send(&f.app, get_req("/api/invoices?from=2026-07-29")).await;
        assert_eq!(from_only["count"], 1);

        // Open-ended upper bound excludes the later day.
        let (_, to_only) = send(&f.app, get_req("/api/invoices?to=2026-07-29")).await;
        assert_eq!(to_only["count"], 2);
    }

    #[tokio::test]
    async fn an_inverted_date_range_is_rejected() {
        let f = app().await;
        let (status, json) =
            send(&f.app, get_req("/api/invoices?from=2026-07-30&to=2026-07-28")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["detail"].as_str().unwrap().contains("is after"));
    }

    #[tokio::test]
    async fn filters_combine() {
        let f = app().await;
        seed_for_filters(&f.app).await;

        let (_, json) = send(
            &f.app,
            get_req("/api/invoices?from=2026-07-28&to=2026-07-28&status=Paid&table=T-01"),
        )
        .await;
        assert_eq!(json["count"], 1);
        assert_eq!(json["invoices"][0]["table"], "T-01");
        assert_eq!(json["invoices"][0]["status"], "Paid");

        // Same range and table, but the T-01 invoice on that day is not Draft.
        let (_, empty) = send(
            &f.app,
            get_req("/api/invoices?from=2026-07-28&to=2026-07-28&status=Draft&table=T-01"),
        )
        .await;
        assert_eq!(empty["count"], 0);
    }

    #[tokio::test]
    async fn list_revenue_counts_paid_and_consolidated_only() {
        // businessday.rs bugs 3 and 4: rounded_total, over Paid + Consolidated.
        let f = app().await;
        seed_for_filters(&f.app).await;

        let (_, json) = send(&f.app, get_req("/api/invoices")).await;
        // Three invoices at 378 each; the Draft one does not count.
        assert_eq!(json["total_revenue"], "756");

        let (_, draft_only) = send(&f.app, get_req("/api/invoices?status=Draft")).await;
        assert_eq!(
            draft_only["total_revenue"], "0",
            "a Draft invoice is not revenue"
        );
    }

    #[tokio::test]
    async fn a_late_night_invoice_is_filtered_into_the_previous_business_day() {
        // Bug 2 regression at the HTTP boundary. 01:30 IST on 2026-07-29 is
        // 20:00 UTC on 2026-07-28, and with a 03:00 cutoff it belongs to business
        // day 2026-07-28 — not to the calendar date of either timestamp's local day.
        let f = app().await;
        let mut body = create_body();
        body["posted_at"] = serde_json::json!("2026-07-28T20:00:00Z");
        let created = create(&f.app, body).await;

        assert_eq!(created["business_day"], "2026-07-28");
        assert_eq!(created["posted_at"], "2026-07-28T20:00:00Z");

        let (_, in_range) =
            send(&f.app, get_req("/api/invoices?from=2026-07-28&to=2026-07-28")).await;
        assert_eq!(in_range["count"], 1);

        // And it must not also appear on the 29th: an inclusive-date filter over the
        // raw timestamp would double-count it.
        let (_, next_day) =
            send(&f.app, get_req("/api/invoices?from=2026-07-29&to=2026-07-29")).await;
        assert_eq!(next_day["count"], 0, "an invoice must bucket exactly once");
    }

    #[tokio::test]
    async fn an_invoice_just_before_the_cutoff_bucketed_to_the_previous_day() {
        // 02:59:59 IST on 2026-07-29 → 21:29:59 UTC on 2026-07-28 → business day 28th.
        let f = app().await;
        let mut before = create_body();
        before["posted_at"] = serde_json::json!("2026-07-28T21:29:59Z");
        assert_eq!(create(&f.app, before).await["business_day"], "2026-07-28");

        // 03:00:00 IST on 2026-07-29 → 21:30:00 UTC → business day 29th.
        let mut at_cutoff = create_body();
        at_cutoff["posted_at"] = serde_json::json!("2026-07-28T21:30:00Z");
        assert_eq!(create(&f.app, at_cutoff).await["business_day"], "2026-07-29");
    }

    // -----------------------------------------------------------------------
    // Money accuracy through the HTTP layer
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn odd_paisa_tax_split_survives_the_wire() {
        // 1 × 360.20 at 5% → tax 18.01, CGST 9.01, SGST 9.00. The pair must still sum
        // to total_tax after serialisation.
        let f = app().await;
        let mut body = create_body();
        body["lines"] = serde_json::json!([
            {"item_code": "X", "item_name": "X", "quantity": "1", "rate": "360.20"}
        ]);
        body["discount"] = serde_json::json!("0");

        let json = create(&f.app, body).await;
        assert_eq!(json["tax"]["total_tax"], "18.01");
        assert_eq!(json["tax"]["cgst"], "9.01");
        assert_eq!(json["tax"]["sgst"], "9.00");

        let cgst: rust_decimal::Decimal = json["tax"]["cgst"].as_str().unwrap().parse().unwrap();
        let sgst: rust_decimal::Decimal = json["tax"]["sgst"].as_str().unwrap().parse().unwrap();
        let total: rust_decimal::Decimal =
            json["tax"]["total_tax"].as_str().unwrap().parse().unwrap();
        assert_eq!(cgst + sgst, total, "a paisa was lost on the wire");
    }

    #[tokio::test]
    async fn round_off_invariant_holds_on_the_response() {
        let f = app().await;
        let mut body = create_body();
        body["lines"] = serde_json::json!([
            {"item_code": "X", "item_name": "X", "quantity": "1", "rate": "360.38"}
        ]);
        body["discount"] = serde_json::json!("0");

        let json = create(&f.app, body).await;
        // Grand 378.40 → rounded 378, round_off -0.40.
        assert_eq!(json["grand_total"], "378.40");
        assert_eq!(json["rounded_total"], "378");
        assert_eq!(json["round_off"], "-0.40");

        let grand: rust_decimal::Decimal = json["grand_total"].as_str().unwrap().parse().unwrap();
        let rounded: rust_decimal::Decimal =
            json["rounded_total"].as_str().unwrap().parse().unwrap();
        let round_off: rust_decimal::Decimal =
            json["round_off"].as_str().unwrap().parse().unwrap();
        assert_eq!(rounded - grand, round_off);
    }

    #[tokio::test]
    async fn a_money_figure_survives_the_round_trip_through_numeric_18_6() {
        // Only reachable with a real database, and the reason `unpad` exists in the
        // repository: `NUMERIC(18,6)` returns `378` as `378.000000`, and `Money`
        // serialises its `Decimal` verbatim. Without the unpad the wire contract silently
        // changes shape the moment the value comes back out of a column instead of a
        // `HashMap`.
        let f = app().await;
        let created = create(&f.app, create_body()).await;
        let id = created["invoice_id"].as_str().unwrap();

        let (_, fetched) = send(&f.app, get_req(&format!("/api/invoices/{id}"))).await;
        assert_eq!(fetched["rounded_total"], "378");
        assert_eq!(fetched["grand_total"], "378.00");
        assert_eq!(fetched["lines"][0]["rate"], "100");
        assert_eq!(fetched["lines"][0]["quantity"], "4");
        assert_eq!(fetched["paid_amount"], "0");
    }

    #[tokio::test]
    async fn every_money_field_on_the_response_is_a_json_string() {
        let f = app().await;
        let json = create(&f.app, create_body()).await;

        for field in [
            "net_total",
            "discount",
            "taxable_value",
            "grand_total",
            "rounded_total",
            "round_off",
            "paid_amount",
            "outstanding_amount",
        ] {
            assert!(
                json[field].is_string(),
                "{field} must be a string, got {:?}",
                json[field]
            );
        }
        for field in ["cgst", "sgst", "igst", "total_tax"] {
            assert!(
                json["tax"][field].is_string(),
                "tax.{field} must be a string"
            );
        }
        assert!(json["lines"][0]["rate"].is_string());
        assert!(json["lines"][0]["amount"].is_string());
        assert!(json["lines"][0]["quantity"].is_string());
    }

    #[tokio::test]
    async fn the_api_agrees_with_compute_totals_to_the_paisa() {
        // The parity gate at the HTTP boundary: the response must equal what the domain
        // layer produces for the same input, field for field.
        let f = app().await;
        let json = create(&f.app, create_body()).await;

        let expected = compute_totals(
            &[InvoiceLine {
                item_name: "Chicken Curry".into(),
                quantity: dec!(4),
                rate: Money::new(dec!(100)),
                hsn_sac: None,
            }],
            Money::new(dec!(40)),
            dec!(0.05),
            peacock_core::tax::SupplyType::Intrastate,
            peacock_core::tax::DiscountBasis::NetTotal,
        )
        .unwrap();

        assert_eq!(json["net_total"], expected.net_total.to_string());
        assert_eq!(json["taxable_value"], expected.taxable_value.to_string());
        assert_eq!(json["tax"]["total_tax"], expected.tax.total_tax.to_string());
        assert_eq!(json["tax"]["cgst"], expected.tax.cgst.to_string());
        assert_eq!(json["tax"]["sgst"], expected.tax.sgst.to_string());
        assert_eq!(json["grand_total"], expected.grand_total.to_string());
        assert_eq!(json["rounded_total"], expected.rounded_total.to_string());
        assert_eq!(json["round_off"], expected.round_off.to_string());
    }

    #[tokio::test]
    async fn interstate_invoice_reports_igst_only() {
        let f = app().await;
        let mut body = create_body();
        body["supply_type"] = serde_json::json!("Interstate");

        let json = create(&f.app, body).await;
        assert_eq!(json["tax"]["igst"], "18.00");
        assert_eq!(json["tax"]["cgst"], "0");
        assert_eq!(json["tax"]["sgst"], "0");
        assert_eq!(json["tax"]["total_tax"], "18.00");
    }

    #[tokio::test]
    async fn grand_total_discount_basis_is_honoured() {
        let f = app().await;
        let mut body = create_body();
        body["lines"] = serde_json::json!([
            {"item_code": "X", "item_name": "X", "quantity": "1", "rate": "100"}
        ]);
        body["discount"] = serde_json::json!("10");
        body["discount_basis"] = serde_json::json!("GrandTotal");

        let json = create(&f.app, body).await;
        // Taxable stays 100, tax 5, grand = 100 + 5 - 10 = 95.
        assert_eq!(json["taxable_value"], "100");
        assert_eq!(json["tax"]["total_tax"], "5.00");
        assert_eq!(json["grand_total"], "95.00");
    }

    #[tokio::test]
    async fn line_amount_is_the_unrounded_product() {
        let f = app().await;
        let mut body = create_body();
        body["lines"] = serde_json::json!([
            {"item_code": "X", "item_name": "X", "quantity": "3", "rate": "100.11"}
        ]);
        body["discount"] = serde_json::json!("0");

        let json = create(&f.app, body).await;
        assert_eq!(json["lines"][0]["amount"], "300.33");
        assert_eq!(json["net_total"], "300.33");
    }

    // -----------------------------------------------------------------------
    // Wiring
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn all_five_endpoints_are_registered_and_carry_a_request_id() {
        let f = app().await;
        let id = create_paid(&f.app, create_body()).await;

        let requests = vec![
            post("/api/invoices", Some(Uuid::new_v4()), create_body()),
            get_req("/api/invoices"),
            get_req(&format!("/api/invoices/{id}")),
            post(
                &format!("/api/invoices/{id}/pay"),
                None,
                serde_json::json!({"method": "Cash", "amount": "1"}),
            ),
            post(
                &format!("/api/invoices/{id}/consolidate"),
                None,
                serde_json::json!({}),
            ),
        ];

        for request in requests {
            let uri = request.uri().to_string();
            let method = request.method().clone();
            let response = f.app.clone().oneshot(request).await.unwrap();
            assert_ne!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{method} {uri} must be registered"
            );
            assert!(
                response.headers().get("x-request-id").is_some(),
                "{method} {uri} must carry x-request-id"
            );
        }
    }

    #[tokio::test]
    async fn each_app_instance_has_its_own_database() {
        // Guards the tests above: a shared database would make every numbering assertion
        // order-dependent.
        let first = app().await;
        create(&first.app, create_body()).await;

        let second = app().await;
        let (_, list) = send(&second.app, get_req("/api/invoices")).await;
        assert_eq!(list["count"], 0);
        assert_eq!(
            create(&second.app, create_body()).await["invoice_id"],
            "POS-2627-000001"
        );
    }
}
