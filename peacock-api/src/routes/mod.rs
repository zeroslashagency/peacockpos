//! Router composition.
//!
//! Lanes 3B–3J attach their own routers here. Keep this file to wiring only: handlers
//! live in their own modules so two lanes editing different resources do not collide.

use axum::routing::get;
use axum::Router;

use crate::state::AppState;

pub mod aggregators;
pub mod auth;
pub mod cogs;
pub mod health;
pub mod invoices;
pub mod items;
pub mod kot;
pub mod menu;
/// Shared real-database fixture for the `menu` and `items` unit tests (Lane W1-B).
/// `#[cfg(test)]` inside the module, so it compiles away entirely in a release build —
/// `sqlx` is a dev-dependency of this crate and is not linkable outside tests.
#[cfg(test)]
pub mod menu_test_support;
pub mod dashboard;
pub mod orders;
pub mod reports;
pub mod shifts;
pub mod tables;
pub mod users;

/// All application routes, without middleware.
///
/// Middleware is applied by [`crate::app::build`] so tests can exercise routes bare.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::health_check))
        .route("/health/ready", get(health::readiness_check))
        .merge(auth::routes())
        .merge(tables::routes())
        .merge(menu::routes())
        .merge(items::routes())
        .merge(kot::routes())
        .merge(invoices::routes())
        .merge(aggregators::routes())
        .merge(shifts::routes())
        .merge(cogs::routes())
        .merge(reports::routes())
        .merge(orders::routes())
        .merge(users::routes())
        .merge(dashboard::routes())
        .merge(crate::events::sse::routes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_route_is_registered() {
        let app = routes().with_state(AppState::new(Config::default()));
        let response = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], br#"{"status":"ok"}"#);
    }

    #[tokio::test]
    async fn sse_stream_route_is_registered() {
        let app = routes().with_state(AppState::new(Config::default()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/events/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "text/event-stream"
        );
    }
}
