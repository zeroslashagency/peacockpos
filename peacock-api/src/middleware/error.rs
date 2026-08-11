//! Outbound error normalisation.
//!
//! Handlers return [`ApiError`], but plenty of error responses never reach a handler:
//! router 404s, 405s, extractor rejections, body-limit rejections. This layer inspects
//! every response on the way out and, for any 4xx/5xx that is not already
//! `application/problem+json`, rewrites the body as RFC 7807.
//!
//! It also completes documents that *are* problem+json but are missing `instance` and
//! `request_id`, which is the normal path for handler errors: those two fields are only
//! knowable here.
//!
//! 5xx detail is replaced with a fixed string. Internal messages can name tables,
//! series, or stored values; the real message is logged against the request id instead.

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::{ApiError, ProblemDetails, ProblemKind};
use crate::middleware::request_id::RequestId;
use crate::state::AppState;

/// What a client is told when something breaks on our side.
const OPAQUE_INTERNAL_DETAIL: &str = "The server encountered an internal error.";

/// Middleware: normalise error responses to RFC 7807.
pub async fn handle_errors(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let instance = request.uri().path().to_string();
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone());

    let response = next.run(request).await;
    let base_uri = state.config().problem_base_uri.clone();
    normalize(response, &instance, request_id.as_deref(), &base_uri)
}

/// Rewrites an error response into a complete problem document.
///
/// Success responses pass through untouched.
pub fn normalize(
    response: Response,
    instance: &str,
    request_id: Option<&str>,
    base_uri: &str,
) -> Response {
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return response;
    }

    // Prefer the typed error the handler stashed; it survives body rendering and keeps
    // the original classification.
    let stashed: Option<ApiError> = response.extensions().get::<ApiError>().cloned();

    let (kind, detail) = match stashed {
        Some(err) => (err.kind(), err.detail().to_string()),
        None => {
            let kind = ProblemKind::from_status(status);
            (kind, default_detail(status))
        }
    };

    if status.is_server_error() {
        // Log the real cause, return an opaque message.
        tracing::error!(
            request_id = request_id.unwrap_or("-"),
            instance = instance,
            status = status.as_u16(),
            detail = %detail,
            "returning error response"
        );
    }

    let client_detail = if kind.status().is_server_error() {
        OPAQUE_INTERNAL_DETAIL.to_string()
    } else {
        detail
    };

    let mut problem = ProblemDetails::new(kind, client_detail, base_uri).with_instance(instance);
    if let Some(id) = request_id {
        problem = problem.with_request_id(id);
    }
    // The upstream status wins when it disagrees with the kind's canonical status, so a
    // framework 405 is not silently turned into a 400. However, Axum's JSON extractor
    // returns 422 for deserialization failures, which we normalize to 400 for consistency.
    problem.status = if status == StatusCode::UNPROCESSABLE_ENTITY {
        StatusCode::BAD_REQUEST.as_u16()
    } else {
        status.as_u16()
    };

    let mut rewritten = problem.into_response_with_status();
    // Preserve headers already set on the original response (request id, CORS), but do
    // not let a stale content-type or content-length survive.
    for (name, value) in response.headers().iter() {
        if name == header::CONTENT_TYPE || name == header::CONTENT_LENGTH {
            continue;
        }
        rewritten.headers_mut().insert(name.clone(), value.clone());
    }
    rewritten
}

/// Human-readable default for responses produced by the framework rather than a handler.
fn default_detail(status: StatusCode) -> String {
    match status {
        StatusCode::NOT_FOUND => "The requested resource does not exist.".to_string(),
        StatusCode::METHOD_NOT_ALLOWED => {
            "The HTTP method is not allowed for this resource.".to_string()
        }
        StatusCode::UNAUTHORIZED => "Authentication is required.".to_string(),
        StatusCode::PAYLOAD_TOO_LARGE => "The request body is too large.".to_string(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => {
            "The request media type is not supported.".to_string()
        }
        other => other
            .canonical_reason()
            .unwrap_or("Request failed.")
            .to_string(),
    }
}

/// Fallback handler for unmatched routes. Produces a typed 404 so the normaliser has a
/// classification to work with.
pub async fn not_found() -> ApiError {
    ApiError::not_found("The requested resource does not exist.")
}

/// Empty-body helper used by the normaliser's tests.
#[cfg(test)]
fn plain(status: StatusCode, body: &'static str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(axum::body::Body::from(body))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PROBLEM_JSON;
    use axum::body::Body;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;

    const BASE: &str = "https://peacock-pos.example.com/errors";

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).expect("error bodies must be JSON")
    }

    #[tokio::test]
    async fn success_responses_pass_through_untouched() {
        let original = (StatusCode::OK, "pong").into_response();
        let normalized = normalize(original, "/health", Some("req-1"), BASE);
        assert_eq!(normalized.status(), StatusCode::OK);
        let bytes = normalized.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"pong");
    }

    #[tokio::test]
    async fn framework_404_becomes_problem_json() {
        let normalized = normalize(
            plain(StatusCode::NOT_FOUND, "Not Found"),
            "/api/nope",
            Some("req-2"),
            BASE,
        );
        assert_eq!(normalized.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            normalized
                .headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            PROBLEM_JSON
        );

        let json = body_json(normalized).await;
        assert_eq!(json["type"], format!("{BASE}/not-found"));
        assert_eq!(json["title"], "Resource Not Found");
        assert_eq!(json["status"], 404);
        assert_eq!(json["instance"], "/api/nope");
        assert_eq!(json["request_id"], "req-2");
    }

    #[tokio::test]
    async fn handler_error_keeps_its_detail_and_gains_context() {
        let response = ApiError::not_found("Table 'T-001' does not exist").into_response();
        let normalized = normalize(response, "/api/tables/T-001", Some("req-3"), BASE);

        let json = body_json(normalized).await;
        assert_eq!(json["detail"], "Table 'T-001' does not exist");
        assert_eq!(json["instance"], "/api/tables/T-001");
        assert_eq!(json["request_id"], "req-3");
        assert_eq!(json["status"], 404);
    }

    #[tokio::test]
    async fn internal_details_are_not_leaked_to_clients() {
        let response =
            ApiError::internal("naming series ACC-PSINV- missing for 2025-2026").into_response();
        let normalized = normalize(response, "/api/orders", Some("req-4"), BASE);
        assert_eq!(normalized.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let json = body_json(normalized).await;
        assert_eq!(json["detail"], OPAQUE_INTERNAL_DETAIL);
        assert!(
            !json["detail"].as_str().unwrap().contains("ACC-PSINV-"),
            "internal identifiers must not reach the client"
        );
        assert_eq!(json["type"], format!("{BASE}/internal-error"));
    }

    #[tokio::test]
    async fn upstream_status_survives_kind_normalisation() {
        // 405 classifies as InvalidInput (400 canonical) but must stay 405 on the wire.
        let normalized = normalize(
            plain(StatusCode::METHOD_NOT_ALLOWED, ""),
            "/health",
            None,
            BASE,
        );
        assert_eq!(normalized.status(), StatusCode::METHOD_NOT_ALLOWED);
        let json = body_json(normalized).await;
        assert_eq!(json["status"], 405);
        assert!(json.get("request_id").is_none());
    }

    #[tokio::test]
    async fn existing_headers_are_preserved_but_content_type_is_replaced() {
        let original = Response::builder()
            .status(StatusCode::CONFLICT)
            .header(header::CONTENT_TYPE, "text/plain")
            .header("x-request-id", "req-5")
            .body(Body::from("conflict"))
            .unwrap();

        let normalized = normalize(original, "/api/tables/T-1/merge", Some("req-5"), BASE);
        assert_eq!(
            normalized.headers().get("x-request-id").unwrap(),
            "req-5",
            "request id header must survive rewriting"
        );
        assert_eq!(
            normalized.headers().get(header::CONTENT_TYPE).unwrap(),
            PROBLEM_JSON
        );
    }
}
