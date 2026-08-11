//! CORS policy for the Vercel-hosted frontend.
//!
//! Credentials are allowed, which rules out `Access-Control-Allow-Origin: *` — the
//! Fetch spec rejects that combination. Origins are therefore an explicit allow-list
//! from configuration; an unlisted origin gets no CORS headers and the browser blocks
//! the read.

use axum::http::{header, HeaderName, HeaderValue, Method};
use tower_http::cors::CorsLayer;

/// Headers the frontend is allowed to send.
///
/// `Idempotency-Key` is required by the mutation endpoints in later lanes;
/// `X-Request-ID` lets the client seed its own trace id. `X-Restaurant` carries the
/// restaurant scope ([`crate::middleware::context`]) — a header the browser will not send
/// unless it is advertised here, so omitting it would make every menu request from the
/// Vercel frontend fail preflight while working fine from curl.
pub fn allowed_headers() -> Vec<HeaderName> {
    vec![
        header::CONTENT_TYPE,
        header::AUTHORIZATION,
        HeaderName::from_static("idempotency-key"),
        HeaderName::from_static("x-request-id"),
        crate::middleware::context::X_RESTAURANT,
    ]
}

/// Methods the API exposes. No PUT: the API uses PATCH for partial updates.
pub fn allowed_methods() -> Vec<Method> {
    vec![
        Method::GET,
        Method::POST,
        Method::PATCH,
        Method::DELETE,
        // Preflight requests use OPTIONS; tower-http answers them itself, but listing
        // it keeps the advertised set honest.
        Method::OPTIONS,
    ]
}

/// Builds the CORS layer.
///
/// Origins that do not parse as header values are skipped with a warning rather than
/// aborting startup, so one typo in a comma-separated list cannot take the API down.
pub fn layer(allowed_origins: &[String]) -> CorsLayer {
    let origins: Vec<HeaderValue> = allowed_origins
        .iter()
        .filter_map(|origin| match HeaderValue::from_str(origin) {
            Ok(value) => Some(value),
            Err(_) => {
                tracing::warn!(origin = %origin, "ignoring unparsable CORS origin");
                None
            }
        })
        .collect();

    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods(allowed_methods())
        .allow_headers(allowed_headers())
        .allow_credentials(true)
        // Lets the browser hand `X-Request-ID` to page scripts for support tickets.
        .expose_headers(vec![HeaderName::from_static("x-request-id")])
        .max_age(std::time::Duration::from_secs(600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_the_documented_method_set() {
        let methods = allowed_methods();
        for expected in [Method::GET, Method::POST, Method::PATCH, Method::DELETE] {
            assert!(methods.contains(&expected), "{expected} must be allowed");
        }
        assert!(
            !methods.contains(&Method::PUT),
            "the API uses PATCH, not PUT"
        );
    }

    #[test]
    fn advertises_the_documented_header_set() {
        let headers = allowed_headers();
        for expected in ["content-type", "authorization", "idempotency-key", "x-request-id"] {
            assert!(
                headers.iter().any(|h| h.as_str() == expected),
                "{expected} must be allowed"
            );
        }
    }

    #[test]
    fn unparsable_origins_do_not_panic() {
        // A newline is illegal in a header value; the layer must still build.
        let _ = layer(&["https://ok.example".to_string(), "bad\norigin".to_string()]);
    }
}
