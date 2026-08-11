//! Shift Management routes.
//!
//! Lane 3G: POS opening/closing with Z-report generation.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{NaiveDate, Utc};
use chrono_tz::Asia::Kolkata;

use crate::dto::shift::{
    CloseShiftRequest, OpenShiftRequest, ShiftListQuery, ShiftListResponse, ShiftResponse,
    ZReportResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

use peacock_core::ids::{ShiftName, TerminalName, UserName};
use peacock_core::ports::ShiftRepo;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/shifts/open", post(open_shift))
        .route("/api/shifts/current", get(get_current_shift))
        .route("/api/shifts/:id/close", post(close_shift))
        .route("/api/shifts/:id/report", get(get_report))
        .route("/api/shifts", get(list_shifts))
}

/// POST /api/shifts/open
///
/// Opens a new shift. Returns 409 if a shift is already open on this terminal.
async fn open_shift(
    State(state): State<AppState>,
    Json(req): Json<OpenShiftRequest>,
) -> ApiResult<Json<ShiftResponse>> {
    // Parse business_day or default to today
    let business_day = if let Some(date_str) = &req.business_day {
        NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .map_err(|_| ApiError::invalid_input(format!("invalid date format: {}", date_str)))?
    } else {
        Utc::now().with_timezone(&Kolkata).date_naive()
    };

    let terminal = TerminalName::new(req.terminal);
    let user = UserName::new(req.opened_by);

    // Phase 2G integration (Lane 4A-4)
    let storage = state.storage();
    let shift_repo = peacock_storage::repos::PostgresShiftRepo::new(storage.clone());

    let shift = shift_repo
        .open_shift(&terminal, &user, business_day)
        .map_err(|e| match e {
            peacock_core::error::Error::Conflict { .. } => {
                ApiError::conflict(format!("shift already open on terminal {}", terminal))
            }
            other => ApiError::from(other),
        })?;

    Ok(Json(ShiftResponse::from(shift)))
}

/// GET /api/shifts/current?terminal=POS-01
///
/// Returns the currently open shift for a terminal, or 404 if none is open.
async fn get_current_shift(
    State(state): State<AppState>,
    Query(query): Query<CurrentShiftQuery>,
) -> ApiResult<Json<ShiftResponse>> {
    let terminal = TerminalName::new(
        query
            .terminal
            .ok_or_else(|| ApiError::invalid_input("terminal query parameter is required"))?,
    );

    // Phase 2G integration (Lane 4A-4)
    let storage = state.storage();
    let shift_repo = peacock_storage::repos::PostgresShiftRepo::new(storage.clone());

    let shift = shift_repo
        .get_current_shift(&terminal)
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::not_found(format!("no open shift on terminal {}", terminal))
        })?;

    Ok(Json(ShiftResponse::from(shift)))
}

#[derive(Debug, serde::Deserialize)]
struct CurrentShiftQuery {
    terminal: Option<String>,
}

/// POST /api/shifts/:id/close
///
/// Closes a shift and generates Z-report.
async fn close_shift(
    State(state): State<AppState>,
    Path(shift_id): Path<String>,
    Json(req): Json<CloseShiftRequest>,
) -> ApiResult<Json<ZReportResponse>> {
    if req.cutoff_hour > 23 {
        return Err(ApiError::invalid_input(
            "cutoff_hour must be between 0 and 23",
        ));
    }

    let shift_name = ShiftName::new(shift_id);

    // Phase 2G integration (Lane 4A-4)
    let storage = state.storage();
    let shift_repo = peacock_storage::repos::PostgresShiftRepo::new(storage.clone());

    let report = shift_repo
        .close_shift(&shift_name, req.cutoff_hour, Kolkata)
        .map_err(ApiError::from)?;

    Ok(Json(ZReportResponse::from(report)))
}

/// GET /api/shifts/:id/report
///
/// Retrieves Z-report for a closed shift. Returns 409 if shift is still open.
async fn get_report(
    State(state): State<AppState>,
    Path(shift_id): Path<String>,
) -> ApiResult<Json<ZReportResponse>> {
    let shift_name = ShiftName::new(shift_id);

    // Phase 2G integration (Lane 4A-4)
    let storage = state.storage();
    let shift_repo = peacock_storage::repos::PostgresShiftRepo::new(storage.clone());

    let report = shift_repo
        .get_report(&shift_name)
        .map_err(ApiError::from)?;

    Ok(Json(ZReportResponse::from(report)))
}

/// GET /api/shifts?terminal=POS-01&limit=50&offset=0
///
/// Lists shifts with optional filters and pagination.
async fn list_shifts(
    State(state): State<AppState>,
    Query(query): Query<ShiftListQuery>,
) -> ApiResult<Json<ShiftListResponse>> {
    let terminal = query.terminal.as_ref().map(|s| TerminalName::new(s.clone()));

    // Phase 2G integration (Lane 4A-4)
    let storage = state.storage();
    let shift_repo = peacock_storage::repos::PostgresShiftRepo::new(storage.clone());

    let shifts = shift_repo
        .list_shifts(terminal.as_ref(), query.limit, query.offset)
        .map_err(ApiError::from)?;

    let responses: Vec<ShiftResponse> = shifts.into_iter().map(ShiftResponse::from).collect();

    Ok(Json(ShiftListResponse {
        count: responses.len(),
        shifts: responses,
    }))
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::testing::TestDb;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::Router;
    use http_body_util::BodyExt;
    use peacock_storage::Storage;
    use tower::ServiceExt;

    async fn test_db() -> TestDb {
        TestDb::new().await
    }

    fn app_with_storage(storage: Storage) -> Router {
        crate::app::build_with_storage(Config::default(), storage)
    }

    async fn send(app: Router, request: Request<Body>) -> axum::response::Response {
        app.oneshot(request).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_shift_valid_returns_200_with_generated_name() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        let body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com"
        });

        let response = send(
            app,
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;

        // Valid open must succeed now that storage is real — 200 (Json) is the
        // handler's actual status; accept 201 as well per spec tolerance.
        assert!(
            response.status() == StatusCode::OK || response.status() == StatusCode::CREATED,
            "expected 200 or 201, got {}",
            response.status()
        );

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let name = json["name"].as_str().expect("shift name must be string");
        assert!(
            name.starts_with("SHIFT-"),
            "shift name {} must start with SHIFT-",
            name
        );
        assert_eq!(json["terminal"], "POS-01");
        assert_eq!(json["opened_by"], "waiter@test.com");
        assert!(json["business_day"].as_str().is_some(), "business_day must be present");
        assert!(json["opened_at"].as_str().is_some(), "opened_at must be present");
        assert!(json["closed_at"].is_null(), "newly opened shift must have no closed_at");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_shift_accepts_explicit_business_day() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        let body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com",
            "business_day": "2026-07-28"
        });

        let response = send(
            app,
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;

        assert!(
            response.status() == StatusCode::OK || response.status() == StatusCode::CREATED,
            "expected 200 or 201, got {}",
            response.status()
        );

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["business_day"], "2026-07-28");
        assert!(json["name"].as_str().unwrap().starts_with("SHIFT-"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_shift_rejects_invalid_date_format() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        let body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com",
            "business_day": "28-07-2026"
        });

        let response = send(
            app,
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], 400);
        assert!(json["detail"].as_str().unwrap().contains("invalid date format"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_shift_duplicate_terminal_returns_409() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        let body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com"
        });

        // First open must succeed.
        let first = send(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
        assert!(
            first.status() == StatusCode::OK || first.status() == StatusCode::CREATED,
            "first open must succeed, got {}",
            first.status()
        );

        // Second open on same terminal must be a conflict.
        let second = send(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;

        assert_eq!(second.status(), StatusCode::CONFLICT);
        let bytes = second.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["detail"].as_str().unwrap().to_lowercase().contains("already open")
            || json["detail"].as_str().unwrap().contains("POS-01"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_current_shift_requires_terminal_query_param() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        let response = send(
            app,
            Request::builder()
                .uri("/api/shifts/current")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_current_shift_returns_404_when_no_open_shift() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        let response = send(
            app,
            Request::builder()
                .uri("/api/shifts/current?terminal=POS-99")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], 404);
        assert!(json["detail"].as_str().unwrap().contains("POS-99"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_current_shift_returns_200_when_open() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        // Open a shift first.
        let open_body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com"
        });
        let open_resp = send(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&open_body).unwrap()))
                .unwrap(),
        )
        .await;
        assert!(open_resp.status().is_success());
        let open_bytes = open_resp.into_body().collect().await.unwrap().to_bytes();
        let open_json: serde_json::Value = serde_json::from_slice(&open_bytes).unwrap();
        let shift_name = open_json["name"].as_str().unwrap().to_owned();

        // Now fetch current.
        let response = send(
            app.clone(),
            Request::builder()
                .uri("/api/shifts/current?terminal=POS-01")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["name"], shift_name);
        assert_eq!(json["terminal"], "POS-01");
        assert!(json["name"].as_str().unwrap().starts_with("SHIFT-"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn close_shift_with_default_cutoff_returns_200_with_report() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        // Open a shift to close.
        let open_body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com"
        });
        let open_resp = send(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&open_body).unwrap()))
                .unwrap(),
        )
        .await;
        assert!(open_resp.status().is_success());
        let open_bytes = open_resp.into_body().collect().await.unwrap().to_bytes();
        let open_json: serde_json::Value = serde_json::from_slice(&open_bytes).unwrap();
        let shift_name = open_json["name"].as_str().unwrap().to_owned();

        let close_body = serde_json::json!({});
        let response = send(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri(format!("/api/shifts/{}/close", shift_name))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&close_body).unwrap()))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(json["shift_name"], shift_name);
        assert_eq!(json["terminal"], "POS-01");
        // New report must contain Z-report fields with string money.
        assert!(json["invoice_count"].is_number(), "invoice_count must be number");
        assert!(json["cash_total"].is_string(), "cash_total must be string");
        assert!(json["card_total"].is_string(), "card_total must be string");
        assert!(json["total_revenue"].is_string(), "total_revenue must be string");
        // With no invoices, totals are zero. Money::ZERO displays as "0".
        assert_eq!(json["invoice_count"], 0);
        // Accept either "0" or "0.00" depending on formatting; both are valid zero.
        let cash = json["cash_total"].as_str().unwrap();
        assert!(
            cash == "0" || cash == "0.00" || cash.parse::<rust_decimal::Decimal>().unwrap().is_zero(),
            "cash_total must be zero, got {}",
            cash
        );
        assert!(json["closed_at"].as_str().is_some());
        assert!(json["opened_at"].as_str().is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn close_shift_with_explicit_cutoff_returns_200() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        let open_body = serde_json::json!({
            "terminal": "POS-02",
            "opened_by": "waiter@test.com"
        });
        let open_resp = send(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&open_body).unwrap()))
                .unwrap(),
        )
        .await;
        assert!(open_resp.status().is_success());
        let open_bytes = open_resp.into_body().collect().await.unwrap().to_bytes();
        let open_json: serde_json::Value = serde_json::from_slice(&open_bytes).unwrap();
        let shift_name = open_json["name"].as_str().unwrap().to_owned();

        let close_body = serde_json::json!({ "cutoff_hour": 4 });
        let response = send(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri(format!("/api/shifts/{}/close", shift_name))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&close_body).unwrap()))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["shift_name"], shift_name);
        assert!(json["invoice_count"].is_number());
        assert!(json["cash_total"].is_string());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn close_shift_rejects_invalid_cutoff_hour() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        // Need a real shift name but validation happens before DB lookup, so any id works.
        // Use an open shift's name to hit the cutoff validation first.
        let open_body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com"
        });
        let open_resp = send(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&open_body).unwrap()))
                .unwrap(),
        )
        .await;
        assert!(open_resp.status().is_success());
        let open_bytes = open_resp.into_body().collect().await.unwrap().to_bytes();
        let open_json: serde_json::Value = serde_json::from_slice(&open_bytes).unwrap();
        let shift_name = open_json["name"].as_str().unwrap().to_owned();

        let body = serde_json::json!({
            "cutoff_hour": 25
        });

        let response = send(
            app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/shifts/{}/close", shift_name))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["detail"].as_str().unwrap().contains("cutoff_hour"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_report_returns_report_for_closed_shift() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        let open_body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com"
        });
        let open_resp = send(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&open_body).unwrap()))
                .unwrap(),
        )
        .await;
        assert!(open_resp.status().is_success());
        let open_bytes = open_resp.into_body().collect().await.unwrap().to_bytes();
        let open_json: serde_json::Value = serde_json::from_slice(&open_bytes).unwrap();
        let shift_name = open_json["name"].as_str().unwrap().to_owned();

        let close_resp = send(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri(format!("/api/shifts/{}/close", shift_name))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&serde_json::json!({})).unwrap()))
                .unwrap(),
        )
        .await;
        assert_eq!(close_resp.status(), StatusCode::OK);

        // Fetch the Z-report.
        let response = send(
            app,
            Request::builder()
                .uri(format!("/api/shifts/{}/report", shift_name))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["shift_name"], shift_name);
        assert_eq!(json["terminal"], "POS-01");
        assert!(json["invoice_count"].is_number());
        assert!(json["cash_total"].is_string());
        assert!(json["card_total"].is_string());
        assert!(json["total_revenue"].is_string());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_report_returns_404_for_nonexistent_shift() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        let response = send(
            app,
            Request::builder()
                .uri("/api/shifts/SHIFT-99999/report")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_shifts_returns_all_when_no_filters() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        // Open 3 shifts on distinct terminals.
        for terminal in ["POS-01", "POS-02", "POS-03"] {
            let body = serde_json::json!({
                "terminal": terminal,
                "opened_by": "waiter@test.com"
            });
            let resp = send(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/api/shifts/open")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await;
            assert!(resp.status().is_success(), "failed to open {}", terminal);
        }

        let response = send(
            app.clone(),
            Request::builder()
                .uri("/api/shifts")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["count"], 3);
        assert_eq!(json["shifts"].as_array().unwrap().len(), 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_shifts_filters_by_terminal() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        for terminal in ["POS-01", "POS-02"] {
            let body = serde_json::json!({
                "terminal": terminal,
                "opened_by": "waiter@test.com"
            });
            let resp = send(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/api/shifts/open")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await;
            assert!(resp.status().is_success());
        }

        let response = send(
            app,
            Request::builder()
                .uri("/api/shifts?terminal=POS-01")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["count"], 1);
        let shifts = json["shifts"].as_array().unwrap();
        assert_eq!(shifts[0]["terminal"], "POS-01");
        assert!(shifts[0]["name"].as_str().unwrap().starts_with("SHIFT-"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_shifts_supports_pagination() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        for i in 1..=3 {
            let body = serde_json::json!({
                "terminal": format!("POS-{:02}", i),
                "opened_by": "waiter@test.com"
            });
            let resp = send(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/api/shifts/open")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await;
            assert!(resp.status().is_success());
        }

        // Request with limit/offset.
        let response = send(
            app.clone(),
            Request::builder()
                .uri("/api/shifts?limit=1&offset=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // The repository limits correctly.
        assert_eq!(json["count"], 1);
        assert_eq!(json["shifts"].as_array().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_shifts_uses_defaults_when_no_params() {
        let db = test_db().await;
        let app = app_with_storage(db.storage().clone());

        let body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com"
        });
        let resp = send(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
        assert!(resp.status().is_success());

        let response = send(
            app,
            Request::builder()
                .uri("/api/shifts")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["count"], 1);
        assert!(json["shifts"][0]["name"].as_str().unwrap().starts_with("SHIFT-"));
    }
}
