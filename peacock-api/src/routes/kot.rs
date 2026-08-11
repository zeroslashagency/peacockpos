//! KOT generation and routing endpoints.
//!
//! Maps HTTP requests to `peacock_core::kot` routing logic.
//!
//! | method | path | purpose |
//! |---|---|---|
//! | POST | `/api/kot/generate` | fan an order out to one ticket per station |
//! | GET | `/api/kot/:id` | fetch one ticket |
//! | GET | `/api/production-units/:unit_id/pending-kots` | kitchen display |
//! | POST | `/api/kot/:id/mark-prepared` | kitchen finished a ticket |
//!
//! # Storage (Lane 4A-3)
//!
//! Every handler needs a database: a KOT has no meaning without the production units it
//! routes to, and those live in Postgres. So unlike `invoices.rs` there is no in-memory
//! fallback — without a pool these return 503, which is honest, rather than an empty list
//! that a kitchen display would render as "no work to do".
//!
//! # Routing
//!
//! The decision of which items go to which station is
//! [`peacock_core::kot::route_items_to_stations`], unchanged and un-duplicated. This
//! module supplies its four ports through [`RoutingSnapshot`], which prefetches them in
//! three queries — the bug 6 / bug 7 fix (upstream issued 36 queries for 12 items across
//! 3 stations, `ury_kot_generate.py:154` and `:214`).
//!
//! # Numbering
//!
//! KOT numbers come from `kot_number_seq` at SERIALIZABLE with retry
//! ([`PgKotRepo::create`]). Unlike invoice numbers they are **not** gapless and do not
//! need to be: a KOT is a kitchen instruction, not a tax document, so CGST Rule 46(b)
//! does not apply and `nextval`'s rollback exemption is harmless here.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};

use peacock_core::ids::{BranchName, KotName, ProductionUnitName, RoomName};
use peacock_core::kot::{required_item_codes, route_items_to_stations, unrouted_item_codes};
use peacock_storage::repos::{PgKotRepo, RoutingSnapshot};

use crate::dto::kot::{
    GenerateKotRequest, GenerateKotResponse, KotDto, MarkPreparedRequest, PendingKotsResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Map a storage failure onto the HTTP vocabulary.
///
/// `PgKotRepo` reports a missing ticket as `StorageError::Constraint` with constraint
/// `"not_found"`, which would otherwise surface as a 500. Everything else keeps the
/// classification `peacock_core::Error` already gives it.
fn storage_error(err: peacock_storage::StorageError) -> ApiError {
    use peacock_storage::StorageError as SE;

    match err {
        SE::Constraint {
            ref constraint,
            ref message,
            ..
        } if constraint == "not_found" => ApiError::not_found(message.clone()),
        SE::Domain(domain) => ApiError::from(domain),
        // A lost race at SERIALIZABLE is the caller's to retry, not a server fault.
        SE::Retryable { sqlstate, message } => ApiError::conflict(format!(
            "the write lost a race ({sqlstate}) and can be retried: {message}"
        )),
        other => ApiError::internal(other.to_string()),
    }
}

/// Lane W1-A: `kot_repo()` returns the repository, so this cannot fail. Kept as a
/// one-liner rather than inlined at five call sites.
///
/// The `storage_unavailable()` helper that used to sit here — the 503 these handlers
/// returned when no pool was configured — is gone with the state that could produce that
/// condition.
fn kot_repo(state: &AppState) -> PgKotRepo {
    state.kot_repo()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/kot/generate", post(generate_kot))
        .route("/api/kot/:id", get(get_kot))
        .route(
            "/api/production-units/:unit_id/pending-kots",
            get(pending_kots),
        )
        .route("/api/kot/:id/mark-prepared", post(mark_prepared))
}

/// POST /api/kot/generate
///
/// Generate KOTs for an order, routing items to production units.
async fn generate_kot(
    State(state): State<AppState>,
    Json(req): Json<GenerateKotRequest>,
) -> ApiResult<Json<GenerateKotResponse>> {
    validate_generate(&req)?;

    let storage = state.storage();
    let repo = kot_repo(&state);

    let ctx = req.to_context();
    let lines = req.to_order_lines();

    // The prefetch key set: distinct codes in first-seen order. Duplicated lines — the
    // same item added twice with different comments, which the POS does allow — collapse
    // to one code, so the lookups below stay one per code rather than one per line.
    let codes = required_item_codes(&lines);
    let room = req.room.as_deref().map(RoomName::from);

    let snapshot = RoutingSnapshot::load(
        storage,
        &req.invoice,
        &BranchName::from(req.branch.as_str()),
        room.as_ref(),
        &codes,
    )
    .await
    .map_err(storage_error)?;

    // The routing decision itself: unchanged domain logic, served from the snapshot.
    let routed = route_items_to_stations(&ctx, &lines, &snapshot.repos())?;

    // Items that match no station. Upstream warned and dropped them
    // (`ury_kot_generate.py:131-137`); we report them so the caller can surface the same
    // advisory instead of silently under-cooking an order.
    let unrouted: Vec<String> = unrouted_item_codes(&lines, snapshot.units(), snapshot.item_groups_map())
        .into_iter()
        .map(|c| c.as_str().to_owned())
        .collect();

    // Persisted one ticket at a time, each in its own SERIALIZABLE transaction: the
    // sequence hands out one number per ticket and a station whose insert fails must not
    // take the other stations' tickets down with it — the grill should still get its
    // order when the bar's ticket hits a constraint.
    let mut stored = Vec::with_capacity(routed.len());
    for kot in routed {
        let created = repo.create(kot).await.map_err(storage_error)?;
        stored.push(KotDto::from(created));
    }

    // Published after the writes commit, never before: a kitchen display that reacts to
    // a ticket which then failed to persist shows work that does not exist.
    for kot in &stored {
        state.events().publish(
            crate::events::EventKind::KotGenerated,
            serde_json::json!({
                "kot_id": kot.id,
                "invoice": kot.invoice,
                "production_unit": kot.production,
                "kot_type": kot.kot_type,
            }),
        );
    }

    Ok(Json(GenerateKotResponse {
        kots: stored,
        unrouted_items: unrouted,
    }))
}

/// Rejects requests the domain would otherwise route into nothing.
fn validate_generate(req: &GenerateKotRequest) -> ApiResult<()> {
    if req.items.is_empty() {
        return Err(ApiError::invalid_input("items cannot be empty"));
    }
    if req.branch.trim().is_empty() {
        return Err(ApiError::invalid_input("branch is required"));
    }
    if req.naming_series.trim().is_empty() {
        return Err(ApiError::invalid_input("naming_series is required"));
    }
    if req.invoice.trim().is_empty() {
        return Err(ApiError::invalid_input("invoice is required"));
    }
    for item in &req.items {
        if item.item_code.trim().is_empty() {
            return Err(ApiError::invalid_input("every item needs an item_code"));
        }
        // Zero is refused rather than dropped: a zero-quantity line on a ticket tells the
        // kitchen to cook nothing, which is a client bug worth surfacing.
        if item.qty <= rust_decimal::Decimal::ZERO {
            return Err(ApiError::invalid_input(format!(
                "item {} has non-positive qty {}",
                item.item_code, item.qty
            )));
        }
    }
    Ok(())
}

/// GET /api/kot/:id
///
/// Get details of a specific KOT.
async fn get_kot(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<KotDto>> {
    if id.trim().is_empty() {
        return Err(ApiError::invalid_input("kot id is required"));
    }

    let kot = kot_repo(&state)
        .get(&KotName::from(id.as_str()))
        .await
        .map_err(storage_error)?;

    Ok(Json(KotDto::from(kot)))
}

/// GET /api/production-units/:unit_id/pending-kots
///
/// List all pending KOTs for a production unit (kitchen view).
/// Filtered by production unit, ordered by creation time.
async fn pending_kots(
    State(state): State<AppState>,
    Path(unit_id): Path<String>,
) -> ApiResult<Json<PendingKotsResponse>> {
    if unit_id.trim().is_empty() {
        return Err(ApiError::invalid_input("unit_id is required"));
    }

    // "Not yet prepared", not "not yet cancelled": a ticket the kitchen has finished must
    // leave the display, or the queue only ever grows. One batched item query regardless
    // of how many tickets come back (the bug 6/7 fix carried into the read path).
    let kots = kot_repo(&state)
        .list_unprepared_for_production(&ProductionUnitName::from(unit_id.as_str()))
        .await
        .map_err(storage_error)?;

    Ok(Json(PendingKotsResponse {
        production_unit: unit_id,
        kots: kots.into_iter().map(KotDto::from).collect(),
    }))
}

/// POST /api/kot/:id/mark-prepared
///
/// Mark a KOT as prepared (kitchen completed it).
async fn mark_prepared(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<MarkPreparedRequest>,
) -> ApiResult<Json<KotDto>> {
    if id.trim().is_empty() {
        return Err(ApiError::invalid_input("kot id is required"));
    }

    // Idempotent in the repository: a double-tapped kitchen display returns the ticket
    // unchanged rather than moving the timestamp the service-time report measures against.
    let kot = kot_repo(&state)
        .mark_prepared(&KotName::from(id.as_str()), req.prepared_at)
        .await
        .map_err(storage_error)?;

    let dto = KotDto::from(kot);

    state.events().publish(
        crate::events::EventKind::KotPrepared,
        serde_json::json!({
            "kot_id": dto.id,
            "invoice": dto.invoice,
            "production_unit": dto.production,
            "prepared_at": dto.start_time_prep,
        }),
    );

    Ok(Json(dto))
}

#[cfg(test)]
mod tests {
    use crate::app;
    use crate::config::Config;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_config() -> Config {
        Config::default()
    }

    async fn send(request: Request<Body>) -> axum::response::Response {
        app::build(test_config()).oneshot(request).await.unwrap()
    }

    #[tokio::test]
    async fn generate_kot_requires_items() {
        let payload = serde_json::json!({
            "invoice": "ACC-PSINV-2026-00042",
            "branch": "Peacock - Main",
            "naming_series": "KOT-",
            "date": "2026-07-28",
            "items": []
        });

        let response = send(
            Request::builder()
                .method("POST")
                .uri("/api/kot/generate")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["detail"].as_str().unwrap().contains("items cannot be empty"));
    }

    #[tokio::test]
    async fn generate_kot_requires_branch() {
        let payload = serde_json::json!({
            "invoice": "ACC-PSINV-2026-00042",
            "branch": "",
            "naming_series": "KOT-",
            "date": "2026-07-28",
            "items": [{"item_code": "CURRY", "item_name": "Curry", "qty": "1"}]
        });

        let response = send(
            Request::builder()
                .method("POST")
                .uri("/api/kot/generate")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await;

        let status = response.status();
        if status != StatusCode::BAD_REQUEST {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let body = String::from_utf8_lossy(&bytes);
            panic!("Expected 400, got {}: {}", status, body);
        }
    }

    #[tokio::test]
    async fn generate_kot_requires_naming_series() {
        let payload = serde_json::json!({
            "invoice": "ACC-PSINV-2026-00042",
            "branch": "Main",
            "naming_series": "",
            "date": "2026-07-28",
            "items": [{"item_code": "CURRY", "item_name": "Curry", "qty": "1"}]
        });

        let response = send(
            Request::builder()
                .method("POST")
                .uri("/api/kot/generate")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // -----------------------------------------------------------------------
    // Validation vs storage — Lane W1-A split
    // -----------------------------------------------------------------------
    //
    // `Config::default()` *used* to carry no pool, so the test that lived here asserted
    // 500 for every valid request — "the database is not there". Since Lane W1-A
    // `app::build(Config::default())` is `build_with_storage` over
    // `crate::testing::shared_storage()` — a real, migrated Postgres — a valid
    // `POST /api/kot/generate` now succeeds (200) and routes to real stations, while
    // invalid payloads still fail at validation with 400 before any query is issued.
    //
    // So the old `every_endpoint_reports_storage_unavailable_without_a_pool` is split:
    // - `validation_tests_still_return_400_without_db` pins the validation path that
    //   never needs a DB and must remain 4xx even though a pool now exists.
    // - `storage_tests_succeed_with_db` drives the happy path against an isolated
    //   `TestDb` (like `peacock-api/src/routes/invoices.rs`) and asserts persisted rows,
    //   proving the handler reaches Postgres rather than faking a 500.
    //
    // The behaviour *with* a shared pool — real routing, real tickets, real kitchen
    // queue — is also exercised in `peacock-api/tests/invoice_kot_postgres.rs`, which
    // needs a server and skips without one.

    /// Validation still runs before any storage access, so these invalid payloads must
    /// be 4xx even though `app::build` now carries `shared_storage`.
    #[tokio::test]
    async fn validation_tests_still_return_400_without_db() {
        let cases: Vec<(&str, serde_json::Value)> = vec![
            (
                "empty items",
                serde_json::json!({
                    "invoice": "ACC-PSINV-2026-00042",
                    "branch": "Peacock - Main",
                    "naming_series": "KOT-",
                    "date": "2026-07-28",
                    "items": []
                }),
            ),
            (
                "blank branch",
                serde_json::json!({
                    "invoice": "ACC-PSINV-2026-00042",
                    "branch": "   ",
                    "naming_series": "KOT-",
                    "date": "2026-07-28",
                    "items": [{"item_code": "CURRY", "item_name": "Curry", "qty": "1"}]
                }),
            ),
            (
                "empty naming_series",
                serde_json::json!({
                    "invoice": "ACC-PSINV-2026-00042",
                    "branch": "Peacock - Main",
                    "naming_series": "",
                    "date": "2026-07-28",
                    "items": [{"item_code": "CURRY", "item_name": "Curry", "qty": "1"}]
                }),
            ),
            (
                "zero qty",
                serde_json::json!({
                    "invoice": "ACC-PSINV-2026-00042",
                    "branch": "Peacock - Main",
                    "naming_series": "KOT-",
                    "date": "2026-07-28",
                    "items": [{"item_code": "CURRY", "item_name": "Curry", "qty": "0"}]
                }),
            ),
            (
                "empty item_code",
                serde_json::json!({
                    "invoice": "ACC-PSINV-2026-00042",
                    "branch": "Peacock - Main",
                    "naming_series": "KOT-",
                    "date": "2026-07-28",
                    "items": [{"item_code": "", "item_name": "Curry", "qty": "1"}]
                }),
            ),
        ];

        for (label, payload) in cases {
            let response = send(
                Request::builder()
                    .method("POST")
                    .uri("/api/kot/generate")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                    .unwrap(),
            )
            .await;

            assert!(
                response.status().is_client_error(),
                "{label} must be 4xx, got {}",
                response.status()
            );
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{label} must be 400"
            );
        }
    }

    /// The storage path now succeeds: a valid generate hits Postgres and persists KOT rows.
    ///
    /// Mirrors `peacock-api/src/routes/invoices.rs` — one throwaway database per test
    /// (`TestDb`), not the shared one, because we assert on row counts. The seed that
    /// `TestDb::new()` runs already inserts the graph KOT routing needs (rooms,
    /// production units, item groups, items like CURRY/NAAN/CHAI).
    #[tokio::test(flavor = "multi_thread")]
    async fn storage_tests_succeed_with_db() {
        use crate::testing::TestDb;

        let db = TestDb::new().await;
        let app = crate::app::build_with_storage(Config::default(), db.storage().clone());

        // ---- generate ------------------------------------------------------
        let payload = serde_json::json!({
            "invoice": "ACC-PSINV-2026-00042",
            "branch": "Peacock - Main",
            "naming_series": "KOT-",
            "date": "2026-07-28",
            // qty is a string: `Decimal` on the wire, never a JSON number — same reason
            // money is (`dto/invoice.rs`).
            "items": [{"item_code": "CURRY", "item_name": "Chicken Curry", "qty": "2"}]
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/kot/generate")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "valid generate must succeed with a pool"
        );

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let kots = json["kots"].as_array().expect("kots array");
        assert!(
            !kots.is_empty(),
            "at least one station must have work, got {json}"
        );
        assert!(
            kots.len() >= 1,
            "expected >=1 KOTs, got {}",
            kots.len()
        );

        // Persisted rows — the point: a 201 that writes nowhere would still pass the
        // response check, but not this one.
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM kots")
            .fetch_one(db.pool())
            .await
            .expect("count kots");
        assert!(
            rows >= 1,
            "kots table must have at least one persisted row, got {rows}"
        );

        let items: i64 = sqlx::query_scalar("SELECT count(*) FROM kot_items")
            .fetch_one(db.pool())
            .await
            .expect("count kot_items");
        assert!(items >= 1, "kot_items must be persisted, got {items}");

        // ---- fetch the created KOT by id ---------------------------------
        let kot_id = kots[0]["id"].as_str().expect("kot has id").to_owned();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/kot/{kot_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "get_kot must find the row we just created");

        // ---- pending-kots for its station --------------------------------
        let production = kots[0]["production"].as_str().unwrap_or("Hot Kitchen");
        let encoded: String = production
            .chars()
            .map(|c| if c == ' ' { "%20".to_owned() } else { c.to_string() })
            .collect();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/production-units/{encoded}/pending-kots"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let pending: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            pending["kots"].as_array().unwrap().len() >= 1,
            "pending-kots must list the ticket"
        );

        // ---- mark-prepared ------------------------------------------------
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/kot/{kot_id}/mark-prepared"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"prepared_at": "14:30:00"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "mark-prepared must succeed");
    }

    #[tokio::test]
    async fn get_kot_requires_id() {
        // No id at all is a routing miss, not a storage failure, so this stays a 404 and
        // never reaches the handler.
        let response = send(
            Request::builder()
                .uri("/api/kot/")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn mark_prepared_accepts_a_null_prepared_at() {
        // `prepared_at: null` is legal — the repository stamps the current time. What is
        // asserted here is that it deserialises rather than being rejected as malformed;
        // the request then fails on storage (KOT not found → 404), not on the payload.
        // Before W1-A this was 500 ("missing database"); now `shared_storage` exists so
        // the handler reaches the repo and the missing ticket maps to 404.
        let payload = serde_json::json!({ "prepared_at": null });

        let response = send(
            Request::builder()
                .method("POST")
                .uri("/api/kot/KOT-2026-00001/mark-prepared")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await;

        // Deserialization succeeded, so this must NOT be 400. With a pool it is 404
        // (ticket does not exist); without a pool the old 500 would also not be 400,
        // but that state is no longer expressible.
        assert_ne!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "a null prepared_at must parse as JSON, not fail validation"
        );
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "a null prepared_at must parse, then fail because the KOT does not exist (404), not because the payload is malformed"
        );
    }

    #[tokio::test]
    async fn all_endpoints_return_request_id() {
        let endpoints = vec![
            ("POST", "/api/kot/generate", Some(serde_json::json!({
                "invoice": "INV-001",
                "branch": "Main",
                "naming_series": "KOT-",
                "date": "2026-07-28",
                "items": [{"item_code": "CURRY", "item_name": "Curry", "qty": "1"}]
            }))),
            ("GET", "/api/kot/KOT-001", None),
            ("GET", "/api/production-units/Kitchen/pending-kots", None),
            ("POST", "/api/kot/KOT-001/mark-prepared", Some(serde_json::json!({
                "prepared_at": null
            }))),
        ];

        for (method, uri, body) in endpoints {
            let mut builder = Request::builder().method(method).uri(uri);
            
            let body = if let Some(json) = body {
                builder = builder.header("content-type", "application/json");
                Body::from(serde_json::to_vec(&json).unwrap())
            } else {
                Body::empty()
            };

            let response = send(builder.body(body).unwrap()).await;
            
            assert!(
                response.headers().get("x-request-id").is_some(),
                "{} {} must return x-request-id",
                method,
                uri
            );
        }
    }
}
