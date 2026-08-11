//! Order creation and modification endpoints.
//!
//! Lane 3D.
//!
//! ## Endpoints
//!
//! | method | path | job |
//! |---|---|---|
//! | `POST` | `/api/orders` | create an order |
//! | `GET` | `/api/orders/:id` | read one order |
//! | `PATCH` | `/api/orders/:id` | modify items and header fields |
//! | `POST` | `/api/orders/:id/invoice` | convert to an invoice, printing KOTs |
//! | `DELETE` | `/api/orders/:id` | cancel |
//!
//! ## Idempotency
//!
//! `POST /api/orders` and `POST /api/orders/:id/invoice` honour an `Idempotency-Key`
//! header. A replay returns the original resource — the same order id, the same invoice
//! number — and creates nothing new. `201` marks the call that created; a replay answers
//! `200`, so a client can tell them apart without guessing.
//!
//! The key must be a UUID. A malformed key is a `400` rather than a silent fallback to
//! non-idempotent behaviour: silently ignoring it would let a retry double-charge.
//!
//! ## Concurrency
//!
//! `PATCH` runs inside the store's per-order lock, so two waiters patching the same
//! order serialise instead of clobbering each other, and both writes land. A client that
//! wants to detect a concurrent edit rather than merge with it sends `version`, and gets
//! `409` if the order moved underneath it.
//!
//! `items` replaces the cart; `append_items` adds to it. The two are mutually exclusive
//! because a body containing both has no unambiguous meaning.
//!
//! ## Invoicing
//!
//! `POST /api/orders/:id/invoice` allocates a gapless number (CGST Rule 46(b)) and then
//! routes the lines to production units through `peacock_core::kot`, so the response
//! reports exactly which stations printed and which items printed nowhere. Routing runs
//! after the invoice exists because a ticket carries the invoice name.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use uuid::Uuid;

use peacock_core::ids::{
    BranchName, ItemCode, ItemGroupName, ProductionUnitName, RoomName, TableName, UserName,
};
use peacock_core::kot::{
    route_items_to_stations, unrouted_item_codes, KotContext, KotRepos,
};
use peacock_core::model::{OrderLine, ProductionUnit};
use peacock_core::ports::{ItemRepo, KotRepo, MenuRepo, ProductionRepo};

use crate::dto::order::{
    total_of_domain, CancelOrderResponse, CreateInvoiceRequest, CreateOrderRequest,
    InvoiceResponse, KotSummaryDto, OrderItemDto, OrderResponse, PatchOrderRequest,
};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::store::order::{OrderRecord, StoreError};

/// Header carrying the replay token.
pub const IDEMPOTENCY_KEY: &str = "idempotency-key";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/orders", post(create_order))
        .route(
            "/api/orders/:id",
            get(get_order).patch(patch_order).delete(cancel_order),
        )
        .route("/api/orders/:id/invoice", post(create_invoice))
}

// ---------------------------------------------------------------------------
// POST /api/orders
// ---------------------------------------------------------------------------

/// Create an order.
///
/// Returns `201` when the order was created and `200` when an `Idempotency-Key` replay
/// returned an existing one.
async fn create_order(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateOrderRequest>,
) -> ApiResult<(StatusCode, Json<OrderResponse>)> {
    let key = idempotency_key(&headers)?;
    validate_create(&req)?;

    let form = crate::dto::order::form_from_create(&req);
    let (record, created) = state
        .orders()
        .create(form, key)
        .await
        .map_err(map_store_error)?;

    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(to_response(&record))))
}

fn validate_create(req: &CreateOrderRequest) -> ApiResult<()> {
    if req.customer_name.trim().is_empty() {
        return Err(ApiError::invalid_input("customer_name is required"));
    }
    // An order must be somewhere: a table, or explicitly takeaway.
    if !req.take_away
        && req
            .restaurant_table
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return Err(ApiError::invalid_input(
            "either restaurant_table or take_away must be set",
        ));
    }
    if req.no_of_pax < 0 {
        return Err(ApiError::invalid_input("no_of_pax cannot be negative"));
    }
    validate_items(&req.items)
}

/// Line-level rules shared by create and patch.
fn validate_items(items: &[OrderItemDto]) -> ApiResult<()> {
    for item in items {
        if item.item.trim().is_empty() {
            return Err(ApiError::invalid_input("item code cannot be empty"));
        }
        // Zero is rejected as well as negative: a zero-qty line is a removal expressed
        // as an addition, which would print a meaningless ticket row.
        if item.qty <= 0 {
            return Err(ApiError::invalid_input(format!(
                "item {} must have a positive qty, got {}",
                item.item, item.qty
            )));
        }
        if item.rate.is_sign_negative() {
            return Err(ApiError::invalid_input(format!(
                "item {} cannot have a negative rate",
                item.item
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GET /api/orders/:id
// ---------------------------------------------------------------------------

async fn get_order(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<OrderResponse>> {
    let record = state.orders().get(&id).await.map_err(map_store_error)?;
    Ok(Json(to_response(&record)))
}

// ---------------------------------------------------------------------------
// PATCH /api/orders/:id
// ---------------------------------------------------------------------------

/// Modify an order.
///
/// The mutation closure runs inside the store's row lock, so what it sees is the current
/// record and nothing can interleave before the write.
async fn patch_order(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PatchOrderRequest>,
) -> ApiResult<Json<OrderResponse>> {
    if req.is_empty() {
        return Err(ApiError::invalid_input(
            "request body must change at least one field",
        ));
    }
    if req.items.is_some() && req.append_items.is_some() {
        return Err(ApiError::invalid_input(
            "items and append_items are mutually exclusive",
        ));
    }
    if let Some(items) = &req.items {
        validate_items(items)?;
    }
    if let Some(items) = &req.append_items {
        if items.is_empty() {
            return Err(ApiError::invalid_input("append_items cannot be empty"));
        }
        validate_items(items)?;
    }
    if let Some(pax) = req.no_of_pax {
        if pax < 0 {
            return Err(ApiError::invalid_input("no_of_pax cannot be negative"));
        }
    }

    let version = req.version;
    let record = state
        .orders()
        .modify(
            &id,
            version,
            Box::new(move |form| {
                if let Some(items) = req.items {
                    form.items = items.iter().map(OrderItemDto::to_domain).collect();
                }
                if let Some(items) = req.append_items {
                    form.items
                        .extend(items.iter().map(OrderItemDto::to_domain));
                }
                if let Some(pax) = req.no_of_pax {
                    // Same floor as create: the schema's `orders_no_of_pax_positive` CHECK
                    // makes zero covers unrepresentable, and a patch setting it to 0 means
                    // "I am not tracking this", not "the table is empty".
                    form.no_of_pax = pax.max(1);
                }
                if let Some(name) = req.customer_name {
                    form.customer_name = peacock_core::ids::CustomerName::new(name);
                }
                if let Some(comments) = req.comments {
                    form.comments = Some(comments);
                }
                if let Some(waiter) = req.waiter {
                    form.waiter = Some(UserName::new(waiter));
                }
                Ok(())
            }),
        )
        .await
        .map_err(map_store_error)?;

    Ok(Json(to_response(&record)))
}

// ---------------------------------------------------------------------------
// POST /api/orders/:id/invoice
// ---------------------------------------------------------------------------

/// Convert an order to an invoice and print the kitchen tickets.
///
/// `201` on the call that allocated the number, `200` on a replay.
async fn create_invoice(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<CreateInvoiceRequest>,
) -> ApiResult<(StatusCode, Json<InvoiceResponse>)> {
    let key = idempotency_key(&headers)?;

    if req.series.trim().is_empty() {
        return Err(ApiError::invalid_input("series is required"));
    }
    if req.branch.trim().is_empty() {
        return Err(ApiError::invalid_input("branch is required"));
    }

    let order = state.orders().get(&id).await.map_err(map_store_error)?;

    let (invoice, created) = state
        .orders()
        .create_invoice(&id, req.series.trim(), req.date, key)
        .await
        .map_err(map_store_error)?;

    // Tickets carry the invoice name, so routing runs after allocation.
    let lines: Vec<OrderLine> = order
        .form
        .items
        .iter()
        .map(|item| OrderLine {
            item_code: item.item.clone(),
            item_name: item.item_name.clone(),
            qty: rust_decimal::Decimal::from(item.qty),
            rate: item.rate,
            comments: item.comments.clone(),
            serve_priority: 0,
            indicate_course: false,
        })
        .collect();

    let mut ctx = KotContext::new(
        invoice.name.as_str(),
        BranchName::new(req.branch.trim()),
        req.kot_naming_series.clone(),
        req.date,
    );
    ctx.restaurant_table = order.form.restaurant_table.clone();
    ctx.room = req.room.as_deref().map(RoomName::from);
    ctx.customer_name = Some(order.form.customer_name.clone());
    ctx.pos_profile = order.form.pos_profile.clone();
    ctx.comments = order.form.comments.clone();
    ctx.table_takeaway = order.form.take_away;

    let routing = state.kot_repos();
    let repos = KotRepos {
        items: routing.items(),
        productions: routing.productions(),
        kots: routing.kots(),
        menu: routing.menu(),
    };

    let tickets = route_items_to_stations(&ctx, &lines, &repos)?;

    let units = routing.productions().list_for_branch(&ctx.branch)?;
    let codes: Vec<ItemCode> = lines.iter().map(|l| l.item_code.clone()).collect();
    let groups = routing.items().item_groups(&codes)?;
    let unrouted = unrouted_item_codes(&lines, &units, &groups);

    let kots: Vec<KotSummaryDto> = tickets
        .iter()
        .enumerate()
        .map(|(idx, kot)| {
            // Routing returns `name: None` — naming belongs to storage. Until the KOT
            // repository is wired in, derive a stable per-response id from the invoice
            // so a client has something to key on.
            let id = kot
                .name
                .as_ref()
                .map(|n| n.as_str().to_owned())
                .unwrap_or_else(|| {
                    format!("{}{}-{:02}", req.kot_naming_series, invoice.name, idx + 1)
                });
            KotSummaryDto::from_kot(id, kot)
        })
        .collect();

    let response = InvoiceResponse {
        invoice_name: invoice.name.as_str().to_owned(),
        order_id: invoice.order_id.clone(),
        grand_total: invoice.grand_total,
        rounded_total: invoice.rounded_total,
        round_off: invoice.round_off,
        status: invoice.status.clone(),
        fiscal_year: invoice.fiscal_year.clone(),
        kots,
        unrouted_items: unrouted.iter().map(|c| c.as_str().to_owned()).collect(),
    };

    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(response)))
}

// ---------------------------------------------------------------------------
// DELETE /api/orders/:id
// ---------------------------------------------------------------------------

/// Cancel an order.
///
/// A soft cancel, not a delete: the row stays for the audit trail. Idempotent, so a
/// retried cancel is not an error.
async fn cancel_order(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<CancelOrderResponse>> {
    let record = state.orders().cancel(&id).await.map_err(map_store_error)?;
    Ok(Json(CancelOrderResponse {
        id: record.id.clone(),
        status: record.status,
        version: record.version,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse the `Idempotency-Key` header.
///
/// Absent is fine. Present-but-unparseable is a `400`: treating a bad key as "no key"
/// would turn a retry into a second order.
fn idempotency_key(headers: &HeaderMap) -> ApiResult<Option<Uuid>> {
    let Some(raw) = headers.get(IDEMPOTENCY_KEY) else {
        return Ok(None);
    };
    let text = raw
        .to_str()
        .map_err(|_| ApiError::invalid_input("Idempotency-Key must be ASCII"))?
        .trim();
    if text.is_empty() {
        return Err(ApiError::invalid_input("Idempotency-Key cannot be empty"));
    }
    Uuid::parse_str(text)
        .map(Some)
        .map_err(|_| ApiError::invalid_input("Idempotency-Key must be a UUID"))
}

fn to_response(record: &OrderRecord) -> OrderResponse {
    OrderResponse {
        id: record.id.clone(),
        status: record.status,
        version: record.version,
        take_away: record.form.take_away,
        restaurant_table: record
            .form
            .restaurant_table
            .as_ref()
            .map(|t| t.as_str().to_owned()),
        customer_name: record.form.customer_name.as_str().to_owned(),
        no_of_pax: record.form.no_of_pax,
        grand_total: total_of_domain(&record.form.items),
        last_invoice: record
            .form
            .last_invoice
            .as_ref()
            .map(|i| i.as_str().to_owned()),
        items: record.form.items.iter().map(Into::into).collect(),
        waiter: record.form.waiter.as_ref().map(|w| w.as_str().to_owned()),
        pos_profile: record
            .form
            .pos_profile
            .as_ref()
            .map(|p| p.as_str().to_owned()),
        cashier: record.form.cashier.as_ref().map(|c| c.as_str().to_owned()),
        comments: record.form.comments.clone(),
        created_at: record.created_at,
        modified_at: record.modified_at,
    }
}

/// Store failures → HTTP classes.
///
/// Exhaustive so a new [`StoreError`] variant fails to compile here rather than
/// defaulting to 500 in production.
fn map_store_error(err: StoreError) -> ApiError {
    match &err {
        StoreError::NotFound(_) => ApiError::not_found(err.to_string()),
        StoreError::NotModifiable { .. } | StoreError::VersionConflict { .. } => {
            ApiError::conflict(err.to_string())
        }
        StoreError::AlreadyInvoiced { .. } => ApiError::already_exists(err.to_string()),
        StoreError::Invalid(_) => ApiError::invalid_input(err.to_string()),
        // A misconfigured series is an operator problem, not something the caller can fix
        // by changing the request.
        StoreError::InvoiceNameTooLong { .. } => ApiError::internal(err.to_string()),
    }
}

// ---------------------------------------------------------------------------
// KOT routing ports
// ---------------------------------------------------------------------------

/// The four `peacock_core::kot` ports, in a form the API can hold in state.
///
/// Shipped with a configurable in-memory implementation so invoicing really does route
/// items to stations and the response is derived rather than asserted. Swapping in the
/// Postgres repositories replaces this type and nothing in the handlers.
#[derive(Clone, Default)]
pub struct KotRouting {
    units: Vec<ProductionUnit>,
    item_groups: HashMap<ItemCode, ItemGroupName>,
    printed: Vec<(String, ProductionUnitName)>,
}

impl KotRouting {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a production unit and the item groups it handles.
    pub fn with_unit(mut self, name: &str, branch: &str, groups: &[&str]) -> Self {
        self.units.push(ProductionUnit {
            name: ProductionUnitName::from(name),
            branch: BranchName::from(branch),
            item_groups: groups.iter().map(|g| ItemGroupName::from(*g)).collect(),
        });
        self
    }

    /// Register an item's group. Items with no entry route nowhere.
    pub fn with_item(mut self, code: &str, group: &str) -> Self {
        self.item_groups
            .insert(ItemCode::from(code), ItemGroupName::from(group));
        self
    }

    /// Record an already-printed ticket, which flips a station to `Order Modified`.
    pub fn with_printed(mut self, invoice: &str, unit: &str) -> Self {
        self.printed
            .push((invoice.to_owned(), ProductionUnitName::from(unit)));
        self
    }

    pub fn items(&self) -> &dyn ItemRepo {
        self
    }

    pub fn productions(&self) -> &dyn ProductionRepo {
        self
    }

    pub fn kots(&self) -> &dyn KotRepo {
        self
    }

    pub fn menu(&self) -> &dyn MenuRepo {
        self
    }
}

impl ItemRepo for KotRouting {
    fn item_groups(
        &self,
        codes: &[ItemCode],
    ) -> peacock_core::Result<HashMap<ItemCode, ItemGroupName>> {
        Ok(codes
            .iter()
            .filter_map(|c| self.item_groups.get(c).map(|g| (c.clone(), g.clone())))
            .collect())
    }
}

impl ProductionRepo for KotRouting {
    fn list_for_branch(&self, branch: &BranchName) -> peacock_core::Result<Vec<ProductionUnit>> {
        Ok(self
            .units
            .iter()
            .filter(|u| &u.branch == branch)
            .cloned()
            .collect())
    }
}

impl KotRepo for KotRouting {
    fn exists_for(
        &self,
        invoice: &str,
        production: &ProductionUnitName,
    ) -> peacock_core::Result<bool> {
        Ok(self
            .printed
            .iter()
            .any(|(inv, unit)| inv == invoice && unit == production))
    }
}

impl MenuRepo for KotRouting {
    fn courses_for_menu(
        &self,
        _room: &RoomName,
        _codes: &[ItemCode],
    ) -> peacock_core::Result<HashMap<ItemCode, peacock_core::ids::MenuCourseName>> {
        Ok(HashMap::new())
    }
}

/// Silence the unused-import warning for `TableName`, which the doc comments reference.
#[allow(dead_code)]
fn _table_name_is_used(t: TableName) -> TableName {
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::state::AppState;
    use crate::testing::TestDb;
    use axum::body::Body;
    use axum::http::{header, Request};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    // Lane W1-A converted this module off the deleted `InMemoryOrderStore`. Every test now
    // gets its own migrated PostgreSQL database, because order ids and invoice numbers come
    // from a `BIGSERIAL` and a counter row: on a shared database `ORD-1` and
    // `PCK-2627-000001` would depend on which test ran first.

    /// A branch with two stations: hot food and drinks.
    fn routing() -> KotRouting {
        KotRouting::new()
            .with_unit("Hot Kitchen", "Peacock - Main", &["Main Course"])
            .with_unit("Bar", "Peacock - Main", &["Beverages"])
            .with_item("BIRYANI", "Main Course")
            .with_item("DOSA", "Main Course")
            .with_item("TEA", "Beverages")
            .with_item("STICKER", "Merchandise") // routes nowhere
    }

    /// A throwaway database and the router over it.
    ///
    /// The `TestDb` must outlive the router: dropping it drops the database, and the
    /// handlers would then answer every request with a connection error.
    struct Fixture {
        db: TestDb,
        app: axum::Router,
    }

    impl Fixture {
        async fn new() -> Fixture {
            Self::with_routing(routing()).await
        }

        async fn with_routing(routing: KotRouting) -> Fixture {
            let db = TestDb::new().await;
            let state = AppState::builder(Config::default(), db.storage().clone())
                .with_kot_routing(routing)
                .build();
            Fixture {
                db,
                app: crate::app::build_with_state(state),
            }
        }

        /// Rows in `orders`, for the assertions that used to read
        /// `InMemoryOrderStore::order_count`.
        async fn order_count(&self) -> i64 {
            sqlx::query_scalar("SELECT count(*) FROM orders")
                .fetch_one(self.db.pool())
                .await
                .expect("count orders")
        }
    }

    async fn send_to(app: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, json)
    }

    fn post(uri: &str, body: &Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    }

    fn post_with_key(uri: &str, body: &Value, key: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(IDEMPOTENCY_KEY, key)
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    }

    fn patch(uri: &str, body: &Value) -> Request<Body> {
        Request::builder()
            .method("PATCH")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn delete(uri: &str) -> Request<Body> {
        Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    fn new_order_body() -> Value {
        json!({
            "restaurant_table": "T-01",
            "customer_name": "Walk-in",
            "no_of_pax": 2,
            "items": [
                {"item": "BIRYANI", "item_name": "Chicken Biryani", "qty": 2, "rate": 250},
                {"item": "TEA", "item_name": "Masala Tea", "qty": 2, "rate": 20}
            ]
        })
    }

    fn invoice_body() -> Value {
        json!({
            "series": "PCK",
            "date": "2026-07-31",
            "branch": "Peacock - Main",
            "room": "Main Hall"
        })
    }

    /// Create an order and return its id.
    async fn create(app: &axum::Router) -> String {
        let (status, body) = send_to(app, post("/api/orders", &new_order_body())).await;
        assert_eq!(status, StatusCode::CREATED, "create failed: {body}");
        body["id"].as_str().unwrap().to_owned()
    }

    // -- create ------------------------------------------------------------

    #[tokio::test]
    async fn create_returns_201_with_a_computed_total() {
        let f = Fixture::new().await;
        let app = &f.app;
        let (status, body) = send_to(app, post("/api/orders", &new_order_body())).await;

        assert_eq!(status, StatusCode::CREATED);
        assert!(body["id"].as_str().unwrap().starts_with("ORD-"));
        assert_eq!(body["status"], "open");
        assert_eq!(body["version"], 1);
        // 2×250 + 2×20 = 540, as a string so no float can mangle it.
        assert_eq!(body["grand_total"], "540.00");
        assert_eq!(body["items"].as_array().unwrap().len(), 2);
        assert!(body["last_invoice"].is_null());
    }

    #[tokio::test]
    async fn create_ignores_a_client_supplied_total() {
        let f = Fixture::new().await;
        let app = &f.app;
        let mut body = new_order_body();
        body["grand_total"] = json!("1.00");
        let (status, response) = send_to(app, post("/api/orders", &body)).await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(response["grand_total"], "540.00", "server computes the total");
    }

    #[tokio::test]
    async fn create_accepts_an_empty_cart() {
        let f = Fixture::new().await;
        let app = &f.app;
        let (status, body) = send_to(
            app,
            post(
                "/api/orders",
                &json!({"restaurant_table": "T-02", "customer_name": "Walk-in"}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["grand_total"], "0.00");
        assert_eq!(body["items"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn create_requires_a_customer_name() {
        let f = Fixture::new().await;
        let app = &f.app;
        let (status, body) = send_to(
            app,
            post(
                "/api/orders",
                &json!({"restaurant_table": "T-01", "customer_name": "  "}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["detail"].as_str().unwrap().contains("customer_name"));
    }

    #[tokio::test]
    async fn create_requires_a_table_or_takeaway() {
        let f = Fixture::new().await;
        let app = &f.app;
        let (status, body) =
            send_to(app, post("/api/orders", &json!({"customer_name": "Walk-in"}))).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["detail"].as_str().unwrap().contains("take_away"));
    }

    #[tokio::test]
    async fn takeaway_needs_no_table() {
        let f = Fixture::new().await;
        let app = &f.app;
        let (status, body) = send_to(
            app,
            post(
                "/api/orders",
                &json!({"take_away": true, "customer_name": "Swiggy"}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["take_away"], true);
        assert!(body["restaurant_table"].is_null());
    }

    #[tokio::test]
    async fn create_rejects_a_non_positive_qty() {
        let f = Fixture::new().await;
        let app = &f.app;
        for qty in [0, -1] {
            let body = json!({
                "restaurant_table": "T-01",
                "customer_name": "Walk-in",
                "items": [{"item": "TEA", "item_name": "Tea", "qty": qty, "rate": 20}]
            });
            let (status, response) = send_to(app, post("/api/orders", &body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "qty {qty} must be rejected");
            assert!(response["detail"].as_str().unwrap().contains("positive qty"));
        }
    }

    #[tokio::test]
    async fn create_rejects_a_negative_rate() {
        let f = Fixture::new().await;
        let app = &f.app;
        let body = json!({
            "restaurant_table": "T-01",
            "customer_name": "Walk-in",
            "items": [{"item": "TEA", "item_name": "Tea", "qty": 1, "rate": -5}]
        });
        let (status, response) = send_to(app, post("/api/orders", &body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["detail"].as_str().unwrap().contains("negative rate"));
    }

    #[tokio::test]
    async fn create_rejects_negative_pax() {
        let f = Fixture::new().await;
        let app = &f.app;
        let body = json!({
            "restaurant_table": "T-01",
            "customer_name": "Walk-in",
            "no_of_pax": -2
        });
        let (status, _) = send_to(app, post("/api/orders", &body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_rejects_malformed_json_as_problem_details() {
        let f = Fixture::new().await;
        let app = &f.app;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/orders")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{not json"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            crate::error::PROBLEM_JSON
        );
    }

    // -- idempotency -------------------------------------------------------

    #[tokio::test]
    async fn ten_replays_of_one_key_return_one_order_id() {
        let f = Fixture::new().await;
        let app = &f.app;
        let key = Uuid::new_v4().to_string();

        let mut ids = Vec::new();
        let mut statuses = Vec::new();
        for _ in 0..10 {
            let (status, body) =
                send_to(app, post_with_key("/api/orders", &new_order_body(), &key)).await;
            statuses.push(status);
            ids.push(body["id"].as_str().unwrap().to_owned());
        }

        assert_eq!(statuses[0], StatusCode::CREATED, "first call creates");
        assert!(
            statuses[1..].iter().all(|s| *s == StatusCode::OK),
            "replays answer 200, not 201: {statuses:?}"
        );
        assert!(
            ids.windows(2).all(|w| w[0] == w[1]),
            "all 10 replays must return one id: {ids:?}"
        );
    }

    #[tokio::test]
    async fn a_replay_does_not_add_a_second_order() {
        let f = Fixture::new().await;
        let app = &f.app;
        let key = Uuid::new_v4().to_string();

        for _ in 0..10 {
            send_to(app, post_with_key("/api/orders", &new_order_body(), &key)).await;
        }

        // Counted in the table, not in a store handle: the claim is that ten replays
        // produced one committed row.
        assert_eq!(f.order_count().await, 1);
    }

    // Multi-threaded: ten `tokio::spawn`ed requests really have to overlap for this to test
    // anything, and on the default single-threaded test runtime they would interleave at
    // await points on one thread instead.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_replays_of_one_key_return_one_order() {
        let f = Fixture::new().await;
        let key = Uuid::new_v4().to_string();

        let mut handles = Vec::new();
        for _ in 0..10 {
            let app = f.app.clone();
            let key = key.clone();
            handles.push(tokio::spawn(async move {
                let (_, body) =
                    send_to(&app, post_with_key("/api/orders", &new_order_body(), &key)).await;
                body["id"].as_str().unwrap_or_default().to_owned()
            }));
        }

        let mut ids = Vec::new();
        for h in handles {
            ids.push(h.await.unwrap());
        }

        assert_eq!(f.order_count().await, 1, "only one order was inserted");
        assert!(
            ids.iter().all(|id| !id.is_empty()),
            "every concurrent replay must get an answer: {ids:?}"
        );
        assert!(ids.windows(2).all(|w| w[0] == w[1]), "one id: {ids:?}");
    }

    #[tokio::test]
    async fn distinct_keys_create_distinct_orders() {
        let f = Fixture::new().await;
        let app = &f.app;
        let (_, first) = send_to(
            app,
            post_with_key("/api/orders", &new_order_body(), &Uuid::new_v4().to_string()),
        )
        .await;
        let (_, second) = send_to(
            app,
            post_with_key("/api/orders", &new_order_body(), &Uuid::new_v4().to_string()),
        )
        .await;

        assert_ne!(first["id"], second["id"]);
    }

    #[tokio::test]
    async fn a_malformed_idempotency_key_is_rejected() {
        let f = Fixture::new().await;
        let app = &f.app;
        for key in ["not-a-uuid", " ", ""] {
            let (status, _) =
                send_to(app, post_with_key("/api/orders", &new_order_body(), key)).await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "key {key:?} must be rejected, not silently ignored"
            );
        }
    }

    #[tokio::test]
    async fn no_key_means_each_post_creates_an_order() {
        let f = Fixture::new().await;

        for _ in 0..3 {
            let (status, _) = send_to(&f.app, post("/api/orders", &new_order_body())).await;
            assert_eq!(status, StatusCode::CREATED);
        }
        assert_eq!(f.order_count().await, 3);
    }

    // -- read --------------------------------------------------------------

    #[tokio::test]
    async fn get_returns_the_created_order() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (status, body) = send_to(app, get_req(&format!("/api/orders/{id}"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], id);
        assert_eq!(body["customer_name"], "Walk-in");
        assert_eq!(body["no_of_pax"], 2);
        assert_eq!(body["restaurant_table"], "T-01");
    }

    #[tokio::test]
    async fn get_unknown_order_is_404_problem_details() {
        let f = Fixture::new().await;
        let app = &f.app;
        let response = app
            .clone()
            .oneshot(get_req("/api/orders/ORD-missing"))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            crate::error::PROBLEM_JSON
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["status"], 404);
        assert_eq!(body["instance"], "/api/orders/ORD-missing");
        assert!(body["request_id"].is_string());
    }

    // -- patch -------------------------------------------------------------

    #[tokio::test]
    async fn patch_replaces_the_cart() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (status, body) = send_to(
            app,
            patch(
                &format!("/api/orders/{id}"),
                &json!({"items": [{"item": "DOSA", "item_name": "Masala Dosa", "qty": 1, "rate": 80}]}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        assert_eq!(body["grand_total"], "80.00");
        assert_eq!(body["version"], 2);
    }

    #[tokio::test]
    async fn patch_appends_without_touching_existing_lines() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (status, body) = send_to(
            app,
            patch(
                &format!("/api/orders/{id}"),
                &json!({"append_items": [{"item": "DOSA", "item_name": "Dosa", "qty": 1, "rate": 80}]}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["items"].as_array().unwrap().len(), 3);
        assert_eq!(body["grand_total"], "620.00");
    }

    #[tokio::test]
    async fn patch_updates_header_fields() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (status, body) = send_to(
            app,
            patch(
                &format!("/api/orders/{id}"),
                &json!({
                    "no_of_pax": 6,
                    "customer_name": "Table 1 Party",
                    "comments": "no chilli",
                    "waiter": "waiter@peacock.test"
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["no_of_pax"], 6);
        assert_eq!(body["customer_name"], "Table 1 Party");
        assert_eq!(body["comments"], "no chilli");
        assert_eq!(body["waiter"], "waiter@peacock.test");
    }

    #[tokio::test]
    async fn patch_rejects_an_empty_body() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (status, body) = send_to(app, patch(&format!("/api/orders/{id}"), &json!({}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("at least one field"));
    }

    #[tokio::test]
    async fn patch_rejects_items_and_append_items_together() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (status, body) = send_to(
            app,
            patch(
                &format!("/api/orders/{id}"),
                &json!({
                    "items": [{"item": "TEA", "item_name": "Tea", "qty": 1, "rate": 20}],
                    "append_items": [{"item": "DOSA", "item_name": "Dosa", "qty": 1, "rate": 80}]
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("mutually exclusive"));
    }

    #[tokio::test]
    async fn patch_can_empty_the_cart_but_not_append_nothing() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        // An explicit empty replacement is a legitimate "clear the cart".
        let (status, body) = send_to(
            app,
            patch(&format!("/api/orders/{id}"), &json!({"items": []})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["grand_total"], "0.00");

        // An empty append is a no-op dressed as a write.
        let (status, _) = send_to(
            app,
            patch(&format!("/api/orders/{id}"), &json!({"append_items": []})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn patch_validates_appended_lines() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (status, _) = send_to(
            app,
            patch(
                &format!("/api/orders/{id}"),
                &json!({"append_items": [{"item": "TEA", "item_name": "Tea", "qty": 0, "rate": 20}]}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn patch_honours_a_matching_version() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (status, body) = send_to(
            app,
            patch(
                &format!("/api/orders/{id}"),
                &json!({"no_of_pax": 4, "version": 1}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["version"], 2);
    }

    #[tokio::test]
    async fn patch_rejects_a_stale_version_with_409() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        // Move the order on, so the client's version 1 is stale.
        send_to(
            app,
            patch(&format!("/api/orders/{id}"), &json!({"no_of_pax": 3})),
        )
        .await;

        let (status, body) = send_to(
            app,
            patch(
                &format!("/api/orders/{id}"),
                &json!({"no_of_pax": 4, "version": 1}),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["detail"].as_str().unwrap().contains("stale write"));
    }

    #[tokio::test]
    async fn patch_unknown_order_is_404() {
        let f = Fixture::new().await;
        let app = &f.app;
        let (status, _) = send_to(
            app,
            patch("/api/orders/ORD-missing", &json!({"no_of_pax": 2})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn concurrent_patches_to_one_order_both_succeed_and_neither_is_lost() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        // 10 concurrent appends. The row lock makes one wait for the other; without it
        // the read-modify-write would drop items.
        let mut handles = Vec::new();
        for i in 0..10 {
            let app = app.clone();
            let id = id.clone();
            handles.push(tokio::spawn(async move {
                send_to(
                    &app,
                    patch(
                        &format!("/api/orders/{id}"),
                        &json!({"append_items": [{
                            "item": "DOSA",
                            "item_name": format!("Dosa {i}"),
                            "qty": 1,
                            "rate": 10
                        }]}),
                    ),
                )
                .await
                .0
            }));
        }

        for h in handles {
            assert_eq!(h.await.unwrap(), StatusCode::OK, "every patch must succeed");
        }

        let (_, body) = send_to(app, get_req(&format!("/api/orders/{id}"))).await;
        assert_eq!(
            body["items"].as_array().unwrap().len(),
            12,
            "2 original + 10 appended; a lost update would show fewer"
        );
        assert_eq!(body["version"], 11, "one version per accepted write");
        assert_eq!(body["grand_total"], "640.00");
    }

    // -- invoice -----------------------------------------------------------

    #[tokio::test]
    async fn invoice_returns_a_gapless_number_and_routes_kots() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (status, body) =
            send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["invoice_name"], "PCK-2627-000001");
        assert_eq!(body["order_id"], id);
        assert_eq!(body["grand_total"], "540.00");
        assert_eq!(body["rounded_total"], "540");
        assert_eq!(body["round_off"], "0.00");
        assert_eq!(body["fiscal_year"], "2026-27");

        // Biryani → Hot Kitchen, Tea → Bar: two stations, one ticket each.
        let kots = body["kots"].as_array().unwrap();
        assert_eq!(kots.len(), 2, "one ticket per station with work: {kots:?}");
        let mut stations: Vec<&str> = kots
            .iter()
            .map(|k| k["production"].as_str().unwrap())
            .collect();
        stations.sort();
        assert_eq!(stations, vec!["Bar", "Hot Kitchen"]);
        assert!(kots.iter().all(|k| k["kot_type"] == "New Order"));
        assert!(kots.iter().all(|k| k["item_count"] == 1));
    }

    #[tokio::test]
    async fn invoice_marks_the_order_invoiced() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;
        send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;

        let (_, body) = send_to(app, get_req(&format!("/api/orders/{id}"))).await;
        assert_eq!(body["status"], "invoiced");
        assert_eq!(body["last_invoice"], "PCK-2627-000001");
    }

    #[tokio::test]
    async fn invoice_numbers_are_consecutive_across_orders() {
        let f = Fixture::new().await;
        let app = &f.app;

        let mut names = Vec::new();
        for _ in 0..3 {
            let id = create(app).await;
            let (_, body) =
                send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;
            names.push(body["invoice_name"].as_str().unwrap().to_owned());
        }

        assert_eq!(
            names,
            vec!["PCK-2627-000001", "PCK-2627-000002", "PCK-2627-000003"],
            "CGST Rule 46(b): no gaps"
        );
    }

    #[tokio::test]
    async fn ten_invoice_replays_of_one_key_return_one_number() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;
        let key = Uuid::new_v4().to_string();

        let mut names = Vec::new();
        let mut statuses = Vec::new();
        for _ in 0..10 {
            let (status, body) = send_to(
                app,
                post_with_key(&format!("/api/orders/{id}/invoice"), &invoice_body(), &key),
            )
            .await;
            statuses.push(status);
            names.push(body["invoice_name"].as_str().unwrap().to_owned());
        }

        assert_eq!(statuses[0], StatusCode::CREATED);
        assert!(statuses[1..].iter().all(|s| *s == StatusCode::OK));
        assert!(
            names.windows(2).all(|w| w[0] == w[1]),
            "a replay must not burn a second number: {names:?}"
        );
    }

    #[tokio::test]
    async fn invoicing_twice_without_a_key_returns_the_first_number() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (first_status, first) =
            send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;
        let (second_status, second) =
            send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;

        assert_eq!(first_status, StatusCode::CREATED);
        assert_eq!(second_status, StatusCode::OK);
        assert_eq!(first["invoice_name"], second["invoice_name"]);
    }

    #[tokio::test]
    async fn invoice_reports_items_that_routed_nowhere() {
        let f = Fixture::new().await;
        let app = &f.app;
        let (_, order) = send_to(
            app,
            post(
                "/api/orders",
                &json!({
                    "restaurant_table": "T-01",
                    "customer_name": "Walk-in",
                    "items": [
                        {"item": "TEA", "item_name": "Tea", "qty": 1, "rate": 20},
                        {"item": "STICKER", "item_name": "Fridge Magnet", "qty": 1, "rate": 50}
                    ]
                }),
            ),
        )
        .await;
        let id = order["id"].as_str().unwrap();

        let (status, body) =
            send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["unrouted_items"], json!(["STICKER"]));
        // Only the Bar prints; Merchandise matches no station.
        assert_eq!(body["kots"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_station_that_already_printed_flips_to_order_modified() {
        // The first invoice of the day is PCK-2627-000001, and that ticket is recorded
        // as already printed at the Bar.
        let f = Fixture::with_routing(routing().with_printed("PCK-2627-000001", "Bar")).await;
        let app = &f.app;
        let id = create(app).await;

        let (_, body) =
            send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;

        let kots = body["kots"].as_array().unwrap();
        let bar = kots
            .iter()
            .find(|k| k["production"] == "Bar")
            .expect("Bar prints");
        let kitchen = kots
            .iter()
            .find(|k| k["production"] == "Hot Kitchen")
            .expect("Hot Kitchen prints");

        assert_eq!(bar["kot_type"], "Order Modified");
        assert_eq!(
            kitchen["kot_type"], "New Order",
            "the flip is per station, not shared"
        );
    }

    #[tokio::test]
    async fn a_branch_with_no_production_units_prints_nothing() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let mut body = invoice_body();
        body["branch"] = json!("Peacock - Unknown");
        let (status, response) =
            send_to(app, post(&format!("/api/orders/{id}/invoice"), &body)).await;

        assert_eq!(status, StatusCode::CREATED, "the invoice still stands");
        assert_eq!(response["kots"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn an_empty_order_cannot_be_invoiced() {
        let f = Fixture::new().await;
        let app = &f.app;
        let (_, order) = send_to(
            app,
            post(
                "/api/orders",
                &json!({"restaurant_table": "T-09", "customer_name": "Walk-in"}),
            ),
        )
        .await;
        let id = order["id"].as_str().unwrap();

        let (status, body) =
            send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["detail"].as_str().unwrap().contains("no items"));
    }

    #[tokio::test]
    async fn invoice_requires_a_series_and_a_branch() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        for (field, value) in [("series", ""), ("branch", "  ")] {
            let mut body = invoice_body();
            body[field] = json!(value);
            let (status, response) =
                send_to(app, post(&format!("/api/orders/{id}/invoice"), &body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{field} must be required");
            assert!(response["detail"].as_str().unwrap().contains(field));
        }
    }

    #[tokio::test]
    async fn an_over_long_series_is_refused_without_burning_a_number() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let mut body = invoice_body();
        body["series"] = json!("WAY-TOO-LONG-SERIES");
        let (status, _) = send_to(app, post(&format!("/api/orders/{id}/invoice"), &body)).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "a misconfigured series is an operator fault"
        );

        // The series counter must not have moved.
        let (status, ok) =
            send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(ok["invoice_name"], "PCK-2627-000001");
    }

    #[tokio::test]
    async fn invoicing_an_unknown_order_is_404() {
        let f = Fixture::new().await;
        let app = &f.app;
        let (status, _) = send_to(
            app,
            post("/api/orders/ORD-missing/invoice", &invoice_body()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_invoiced_order_cannot_be_patched() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;
        send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;

        let (status, body) = send_to(
            app,
            patch(&format!("/api/orders/{id}"), &json!({"no_of_pax": 8})),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["detail"].as_str().unwrap().contains("invoiced"));
    }

    #[tokio::test]
    async fn concurrent_invoicing_of_one_order_allocates_one_number() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let mut handles = Vec::new();
        for _ in 0..8 {
            let app = app.clone();
            let id = id.clone();
            handles.push(tokio::spawn(async move {
                let (_, body) =
                    send_to(&app, post(&format!("/api/orders/{id}/invoice"), &invoice_body()))
                        .await;
                body["invoice_name"].as_str().unwrap().to_owned()
            }));
        }

        let mut names = Vec::new();
        for h in handles {
            names.push(h.await.unwrap());
        }

        assert!(
            names.windows(2).all(|w| w[0] == w[1]),
            "the row lock must stop a second allocation: {names:?}"
        );
    }

    // -- cancel ------------------------------------------------------------

    #[tokio::test]
    async fn delete_cancels_the_order() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (status, body) = send_to(app, delete(&format!("/api/orders/{id}"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "cancelled");
        assert_eq!(body["version"], 2);
    }

    #[tokio::test]
    async fn a_cancelled_order_is_still_readable() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;
        send_to(app, delete(&format!("/api/orders/{id}"))).await;

        // Soft cancel: the row stays for the audit trail.
        let (status, body) = send_to(app, get_req(&format!("/api/orders/{id}"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "cancelled");
    }

    #[tokio::test]
    async fn cancel_is_idempotent() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let (first, body_one) = send_to(app, delete(&format!("/api/orders/{id}"))).await;
        let (second, body_two) = send_to(app, delete(&format!("/api/orders/{id}"))).await;

        assert_eq!(first, StatusCode::OK);
        assert_eq!(second, StatusCode::OK, "a retried cancel is not an error");
        assert_eq!(body_one["version"], body_two["version"]);
    }

    #[tokio::test]
    async fn a_cancelled_order_cannot_be_patched_or_invoiced() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;
        send_to(app, delete(&format!("/api/orders/{id}"))).await;

        let (patch_status, _) = send_to(
            app,
            patch(&format!("/api/orders/{id}"), &json!({"no_of_pax": 4})),
        )
        .await;
        assert_eq!(patch_status, StatusCode::CONFLICT);

        let (invoice_status, _) =
            send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;
        assert_eq!(invoice_status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn an_invoiced_order_cannot_be_cancelled() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;
        send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;

        let (status, body) = send_to(app, delete(&format!("/api/orders/{id}"))).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["detail"].as_str().unwrap().contains("PCK-2627-000001"));
    }

    #[tokio::test]
    async fn cancelling_an_unknown_order_is_404() {
        let f = Fixture::new().await;
        let app = &f.app;
        let (status, _) = send_to(app, delete("/api/orders/ORD-missing")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // -- contract ----------------------------------------------------------

    #[tokio::test]
    async fn every_endpoint_carries_a_request_id() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let requests = vec![
            post("/api/orders", &new_order_body()),
            get_req(&format!("/api/orders/{id}")),
            patch(&format!("/api/orders/{id}"), &json!({"no_of_pax": 3})),
            post(&format!("/api/orders/{id}/invoice"), &invoice_body()),
            delete(&format!("/api/orders/{id}")),
        ];

        for request in requests {
            let uri = request.uri().to_string();
            let method = request.method().to_string();
            let response = app.clone().oneshot(request).await.unwrap();
            assert!(
                response.headers().get("x-request-id").is_some(),
                "{method} {uri} must carry x-request-id"
            );
        }
    }

    #[tokio::test]
    async fn an_unsupported_method_is_405() {
        let f = Fixture::new().await;
        let app = &f.app;
        let id = create(app).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/orders/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn a_full_order_lifecycle_runs_end_to_end() {
        let f = Fixture::new().await;
        let app = &f.app;

        // Open a table with one round.
        let id = create(app).await;

        // Waiter adds a second round.
        let (status, body) = send_to(
            app,
            patch(
                &format!("/api/orders/{id}"),
                &json!({"append_items": [{"item": "DOSA", "item_name": "Dosa", "qty": 2, "rate": 80}]}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["grand_total"], "700.00");

        // Bill it: number allocated, tickets printed.
        let (status, invoice) =
            send_to(app, post(&format!("/api/orders/{id}/invoice"), &invoice_body())).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(invoice["invoice_name"], "PCK-2627-000001");
        assert_eq!(invoice["grand_total"], "700.00");
        assert_eq!(invoice["kots"].as_array().unwrap().len(), 2);

        // Closed to further edits.
        let (status, _) = send_to(
            app,
            patch(&format!("/api/orders/{id}"), &json!({"no_of_pax": 9})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // Final read reflects everything.
        let (_, final_state) = send_to(app, get_req(&format!("/api/orders/{id}"))).await;
        assert_eq!(final_state["status"], "invoiced");
        assert_eq!(final_state["last_invoice"], "PCK-2627-000001");
        assert_eq!(final_state["items"].as_array().unwrap().len(), 3);
    }
}
