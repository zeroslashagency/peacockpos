//! Application assembly: routes + middleware + state.
//!
//! Split out from `main.rs` so integration tests can build the exact production stack
//! without binding a port or reading the environment.

use axum::Router;

use crate::config::Config;
use crate::middleware;
use crate::routes;
use crate::state::AppState;

/// Builds the fully layered application over a live database.
///
/// The **only** entry point that takes a bare [`Config`], and it requires a
/// [`peacock_storage::Storage`] alongside it. There used to be a `build(config)` that
/// needed no database and quietly assembled a state backed by in-memory stores; Lane
/// W1-A deleted it, because "the caller forgot the database" and "the caller wants a
/// pretend one" were indistinguishable at the call site, and the second one silently
/// served fabricated invoice numbers.
///
/// `tower` applies layers in reverse registration order, so the calls in
/// [`build_with_state`] read innermost-first while requests traverse them bottom-up:
/// request_id → logging → error → cors → handler.
pub fn build_with_storage(config: Config, storage: peacock_storage::Storage) -> Router {
    build_with_state(AppState::with_storage(config, storage))
}

/// The stack over the process-wide test database.
///
/// `#[cfg(test)]`: it exists only while the library's own unit tests compile, so no
/// binary and no integration test can reach it. It is the storage-less `build(config)`
/// signature the route test modules were written against, but it now hands back a state
/// over a *real* migrated database rather than in-memory stores.
///
/// Shared, not per-test, because the tests that call it assert on middleware and routing —
/// request ids, CORS, problem+json, 404s, validation rejections that never reach a query —
/// and a database each would be pure cost. Anything asserting on stored rows or invoice
/// numbers must take a [`crate::testing::TestDb`] instead: those read shared counters, and
/// on a shared database the answers depend on execution order.
#[cfg(test)]
pub(crate) fn build(config: Config) -> Router {
    build_with_storage(config, crate::testing::shared_storage())
}

/// Same stack, over a state the caller assembled.
///
/// The binary uses [`build_with_storage`]; this exists for callers that need to inject
/// collaborators — seeded KOT routing in tests, a shrunk event bus — without a second copy
/// of the layer order. The state carries a `Storage` either way, so this is not a way
/// around the requirement above.
pub fn build_with_state(state: AppState) -> Router {
    let cors = middleware::cors::layer(&state.config().cors_allowed_origins);

    routes::routes()
        .fallback(middleware::error::not_found)
        .layer(cors)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::error::handle_errors,
        ))
        .layer(axum::middleware::from_fn(middleware::logging::log_requests))
        .layer(axum::middleware::from_fn(
            middleware::request_id::propagate,
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PROBLEM_JSON;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_config() -> Config {
        Config {
            cors_allowed_origins: vec!["https://pos.vercel.app".to_string()],
            ..Config::default()
        }
    }

    async fn send(request: Request<Body>) -> axum::response::Response {
        build(test_config()).oneshot(request).await.unwrap()
    }

    #[tokio::test]
    async fn health_returns_200_with_status_ok() {
        let response = send(Request::builder().uri("/health").body(Body::empty()).unwrap()).await;
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn every_response_carries_a_request_id_header() {
        let response = send(Request::builder().uri("/health").body(Body::empty()).unwrap()).await;
        let id = response
            .headers()
            .get("x-request-id")
            .expect("x-request-id must be present")
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            uuid::Uuid::parse_str(&id).is_ok(),
            "generated request id {id} must be a UUID"
        );
    }

    #[tokio::test]
    async fn inbound_request_id_is_echoed_back() {
        let response = send(
            Request::builder()
                .uri("/health")
                .header("x-request-id", "edge-trace-7")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.headers().get("x-request-id").unwrap(), "edge-trace-7");
    }

    #[tokio::test]
    async fn unknown_route_returns_rfc7807_problem_details() {
        let response = send(
            Request::builder()
                .uri("/api/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            PROBLEM_JSON
        );

        let request_id = response
            .headers()
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(
            json["type"],
            "https://peacock-pos.example.com/errors/not-found"
        );
        assert_eq!(json["title"], "Resource Not Found");
        assert_eq!(json["status"], 404);
        assert_eq!(json["instance"], "/api/does-not-exist");
        assert_eq!(
            json["request_id"], request_id,
            "body request_id must match the response header"
        );
    }

    #[tokio::test]
    async fn wrong_method_returns_405_problem_details() {
        let response = send(
            Request::builder()
                .method("POST")
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            PROBLEM_JSON
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], 405);
        assert_eq!(json["instance"], "/health");
    }

    #[tokio::test]
    async fn allowed_origin_gets_cors_headers_with_credentials() {
        let response = send(
            Request::builder()
                .uri("/health")
                .header(header::ORIGIN, "https://pos.vercel.app")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        let headers = response.headers();
        assert_eq!(
            headers
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .expect("allow-origin must be echoed for a listed origin"),
            "https://pos.vercel.app"
        );
        assert_eq!(
            headers.get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS).unwrap(),
            "true"
        );
    }

    #[tokio::test]
    async fn unlisted_origin_gets_no_allow_origin_header() {
        let response = send(
            Request::builder()
                .uri("/health")
                .header(header::ORIGIN, "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "an unlisted origin must not receive CORS approval"
        );
    }

    #[tokio::test]
    async fn preflight_advertises_methods_and_headers() {
        let response = send(
            Request::builder()
                .method("OPTIONS")
                .uri("/health")
                .header(header::ORIGIN, "https://pos.vercel.app")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "PATCH")
                .header(
                    header::ACCESS_CONTROL_REQUEST_HEADERS,
                    "content-type,idempotency-key",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert!(response.status().is_success(), "preflight must succeed");

        let methods = response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .unwrap()
            .to_str()
            .unwrap()
            .to_ascii_uppercase();
        for method in ["GET", "POST", "PATCH", "DELETE"] {
            assert!(methods.contains(method), "{method} must be advertised");
        }

        let allowed = response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
            .unwrap()
            .to_str()
            .unwrap()
            .to_ascii_lowercase();
        for header_name in ["content-type", "authorization", "idempotency-key", "x-request-id"] {
            assert!(allowed.contains(header_name), "{header_name} must be advertised");
        }
    }

    #[tokio::test]
    async fn problem_base_uri_is_taken_from_config() {
        let config = Config {
            problem_base_uri: "https://errors.peacock.test/e".to_string(),
            ..test_config()
        };
        let response = build(config)
            .oneshot(Request::builder().uri("/nope").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["type"], "https://errors.peacock.test/e/not-found");
    }

    #[tokio::test]
    async fn request_ids_differ_across_requests() {
        let app = build(test_config());
        let first = app
            .clone()
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let second = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_ne!(
            first.headers().get("x-request-id").unwrap(),
            second.headers().get("x-request-id").unwrap()
        );
    }
}
