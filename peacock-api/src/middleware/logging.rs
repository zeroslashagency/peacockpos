//! Structured request logging.
//!
//! One line per completed request carrying method, path, status, duration, and the
//! correlation id from [`crate::middleware::request_id`]. Level is derived from the
//! status so 5xx is visible without a query: `error` for 5xx, `warn` for 4xx, `info`
//! otherwise.
//!
//! Query strings are not logged. They routinely carry customer identifiers and the
//! path plus request id is enough to correlate with the access log.

use std::time::Instant;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use tracing::field::Empty;

use crate::config::LogFormat;
use crate::middleware::request_id::RequestId;

/// Installs the global `tracing` subscriber.
///
/// Idempotent: a second call is a no-op rather than a panic, which keeps `main` and
/// tests from fighting over the global default.
pub fn init_tracing(format: LogFormat) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("peacock_api=info,tower_http=info,axum=info,warn"));

    let registry = tracing_subscriber::registry().with(filter);
    let installed = match format {
        LogFormat::Json => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_current_span(true)
                    .with_span_list(false),
            )
            .try_init(),
        LogFormat::Pretty => registry
            .with(tracing_subscriber::fmt::layer().with_target(true).compact())
            .try_init(),
    };
    // Another subscriber already owns the global default (common in tests).
    let _ = installed;
}

/// Severity for a completed request.
pub fn level_for(status: StatusCode) -> tracing::Level {
    if status.is_server_error() {
        tracing::Level::ERROR
    } else if status.is_client_error() {
        tracing::Level::WARN
    } else {
        tracing::Level::INFO
    }
}

/// Middleware: wrap the handler in a span and emit one completion event.
///
/// Runs inside [`crate::middleware::request_id::propagate`] so the id is already in the
/// request extensions.
pub async fn log_requests(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_else(|| "-".to_string());

    let span = tracing::info_span!(
        "http_request",
        request_id = %request_id,
        method = %method,
        path = %path,
        status = Empty,
        duration_ms = Empty,
    );

    let _guard = span.enter();
    let started = Instant::now();
    let response = next.run(request).await;
    let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    let status = response.status();

    span.record("status", status.as_u16());
    span.record("duration_ms", duration_ms);

    // `tracing` needs a compile-time level, so the branch is on the macro call.
    match level_for(status) {
        tracing::Level::ERROR => tracing::error!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms = duration_ms,
            "request completed"
        ),
        tracing::Level::WARN => tracing::warn!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms = duration_ms,
            "request completed"
        ),
        _ => tracing::info!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms = duration_ms,
            "request completed"
        ),
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_tracks_status_class() {
        assert_eq!(level_for(StatusCode::OK), tracing::Level::INFO);
        assert_eq!(level_for(StatusCode::FOUND), tracing::Level::INFO);
        assert_eq!(level_for(StatusCode::NOT_FOUND), tracing::Level::WARN);
        assert_eq!(
            level_for(StatusCode::INTERNAL_SERVER_ERROR),
            tracing::Level::ERROR
        );
    }

    #[test]
    fn init_tracing_is_idempotent() {
        init_tracing(LogFormat::Pretty);
        // A second call must not panic even though a global default exists.
        init_tracing(LogFormat::Json);
    }
}
