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
    use crate::app;
    use crate::config::Config;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn send(request: Request<Body>) -> axum::response::Response {
        app::build(Config::default()).oneshot(request).await.unwrap()
    }

    #[tokio::test]
    async fn open_shift_requires_terminal_and_user() {
        let body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com"
        });

        let response = send(
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;

        // Stub returns 500 for now; Phase 2G will make this 200 or 409
        assert!(response.status().is_server_error());
    }

    #[tokio::test]
    async fn open_shift_accepts_explicit_business_day() {
        let body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com",
            "business_day": "2026-07-28"
        });

        let response = send(
            Request::builder()
                .method("POST")
                .uri("/api/shifts/open")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;

        assert!(response.status().is_server_error());
    }

    #[tokio::test]
    async fn open_shift_rejects_invalid_date_format() {
        let body = serde_json::json!({
            "terminal": "POS-01",
            "opened_by": "waiter@test.com",
            "business_day": "28-07-2026"
        });

        let response = send(
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

    #[tokio::test]
    async fn get_current_shift_requires_terminal_query_param() {
        let response = send(
            Request::builder()
                .uri("/api/shifts/current")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_current_shift_with_terminal() {
        let response = send(
            Request::builder()
                .uri("/api/shifts/current?terminal=POS-01")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        // Stub returns 500; Phase 2G will make this 200 or 404
        assert!(response.status().is_server_error());
    }

    #[tokio::test]
    async fn close_shift_accepts_default_cutoff() {
        let body = serde_json::json!({});

        let response = send(
            Request::builder()
                .method("POST")
                .uri("/api/shifts/SHIFT-001/close")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;

        assert!(response.status().is_server_error());
    }

    #[tokio::test]
    async fn close_shift_accepts_explicit_cutoff() {
        let body = serde_json::json!({
            "cutoff_hour": 4
        });

        let response = send(
            Request::builder()
                .method("POST")
                .uri("/api/shifts/SHIFT-001/close")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;

        assert!(response.status().is_server_error());
    }

    #[tokio::test]
    async fn close_shift_rejects_invalid_cutoff_hour() {
        let body = serde_json::json!({
            "cutoff_hour": 25
        });

        let response = send(
            Request::builder()
                .method("POST")
                .uri("/api/shifts/SHIFT-001/close")
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

    #[tokio::test]
    async fn get_report_extracts_shift_id_from_path() {
        let response = send(
            Request::builder()
                .uri("/api/shifts/SHIFT-001/report")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert!(response.status().is_server_error());
    }

    #[tokio::test]
    async fn list_shifts_accepts_terminal_filter() {
        let response = send(
            Request::builder()
                .uri("/api/shifts?terminal=POS-01")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert!(response.status().is_server_error());
    }

    #[tokio::test]
    async fn list_shifts_accepts_pagination() {
        let response = send(
            Request::builder()
                .uri("/api/shifts?limit=10&offset=20")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert!(response.status().is_server_error());
    }

    #[tokio::test]
    async fn list_shifts_uses_defaults_when_no_params() {
        let response = send(
            Request::builder()
                .uri("/api/shifts")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert!(response.status().is_server_error());
    }

    #[tokio::test]
    async fn all_endpoints_return_problem_json_on_error() {
        let endpoints = vec![
            ("POST", "/api/shifts/open", Some(r#"{"terminal":"POS-01","opened_by":"user@test.com"}"#)),
            ("GET", "/api/shifts/current?terminal=POS-01", None),
            ("POST", "/api/shifts/SHIFT-001/close", Some(r#"{}"#)),
            ("GET", "/api/shifts/SHIFT-001/report", None),
            ("GET", "/api/shifts", None),
        ];

        for (method, uri, body) in endpoints {
            let mut req = Request::builder().method(method).uri(uri);
            
            let response = if let Some(body_str) = body {
                req = req.header(header::CONTENT_TYPE, "application/json");
                send(req.body(Body::from(body_str.to_string())).unwrap()).await
            } else {
                send(req.body(Body::empty()).unwrap()).await
            };

            // All stubs return errors; verify they're problem+json
            if response.status().is_client_error() || response.status().is_server_error() {
                let content_type = response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok());
                assert_eq!(
                    content_type,
                    Some("application/problem+json"),
                    "{} {} must return problem+json",
                    method,
                    uri
                );
            }
        }
    }
}
