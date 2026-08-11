//! Request correlation ID.
//!
//! An inbound `X-Request-ID` is honoured when it looks sane (printable ASCII, bounded
//! length) so a trace started at the edge proxy survives into our logs. Anything else
//! is replaced with a fresh UUID v4: an attacker-controlled header must not be able to
//! inject newlines into log lines or unbounded junk into memory.
//!
//! The value is stored in request extensions as [`RequestId`] and echoed on the
//! response.

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

pub const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Upper bound on an accepted inbound request id.
const MAX_INBOUND_LEN: usize = 128;

/// Correlation id for one request. Cloned into logs and error bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

impl RequestId {
    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Accepts an inbound id only if it is short, non-empty, and printable ASCII.
fn sanitize_inbound(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let acceptable = !trimmed.is_empty()
        && trimmed.len() <= MAX_INBOUND_LEN
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_graphic() || b == b'-' || b == b'_');
    acceptable.then(|| trimmed.to_string())
}

/// Resolves the request id for a request without touching it.
pub fn resolve(request: &Request) -> RequestId {
    request
        .headers()
        .get(X_REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .and_then(sanitize_inbound)
        .map(RequestId)
        .unwrap_or_else(RequestId::generate)
}

/// Middleware: attach the id to the request extensions and the response headers.
pub async fn propagate(mut request: Request, next: Next) -> Response {
    let request_id = resolve(&request);
    request.extensions_mut().insert(request_id.clone());

    let mut response = next.run(request).await;

    // `sanitize_inbound` guarantees a header-safe value, and generated UUIDs are
    // hex+dashes, so this conversion cannot realistically fail. If it ever does, the
    // response simply carries no id rather than failing the request.
    if let Ok(value) = HeaderValue::from_str(request_id.as_str()) {
        response.headers_mut().insert(X_REQUEST_ID, value);
    }
    // Also expose it to handlers further out (error middleware renders it into the body).
    response.extensions_mut().insert(request_id);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn request_with(header: Option<&str>) -> Request {
        let mut builder = Request::builder().uri("/health");
        if let Some(value) = header {
            builder = builder.header(X_REQUEST_ID, value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn generates_a_uuid_when_header_absent() {
        let id = resolve(&request_with(None));
        assert!(
            Uuid::parse_str(id.as_str()).is_ok(),
            "generated id {id} must be a UUID"
        );
    }

    #[test]
    fn honours_a_sane_inbound_id() {
        let id = resolve(&request_with(Some("edge-trace-42")));
        assert_eq!(id.as_str(), "edge-trace-42");
    }

    #[test]
    fn replaces_hostile_or_oversized_inbound_ids() {
        // Whitespace-only, spaces (log injection surface), and over-long values.
        for hostile in ["   ", "has space", "line\tbreak"] {
            let id = resolve(&request_with(Some(hostile)));
            assert!(
                Uuid::parse_str(id.as_str()).is_ok(),
                "{hostile:?} must be replaced by a generated UUID, got {id}"
            );
        }

        let long = "a".repeat(MAX_INBOUND_LEN + 1);
        let id = resolve(&request_with(Some(&long)));
        assert!(Uuid::parse_str(id.as_str()).is_ok());
    }

    #[test]
    fn generated_ids_are_unique() {
        let a = RequestId::generate();
        let b = RequestId::generate();
        assert_ne!(a, b);
    }
}
