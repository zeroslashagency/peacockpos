//! # peacock-api
//!
//! HTTP layer for Peacock POS. Wraps [`peacock_core`] domain logic in an Axum server.
//!
//! ## Shape
//!
//! - [`app::build_with_storage`] assembles routes, middleware, and state over a live
//!   [`peacock_storage::Storage`]. `main.rs` only binds a socket; everything testable
//!   lives behind this function. There is no storage-less variant: see [`state`].
//! - [`error`] maps `peacock_core::Error` to HTTP status codes and RFC 7807 Problem
//!   Details. Handlers return [`error::ApiError`], never raw status codes.
//! - [`middleware`] carries request-id propagation, structured logging, error
//!   normalisation, and CORS.
//! - [`routes`] is the attachment point for the remaining Phase 3 lanes.
//! - [`events`] is the realtime layer: handlers publish to
//!   [`events::EventBroadcaster`], KDS clients read `GET /api/events/stream`.
//!
//! ## Error contract
//!
//! Every 4xx/5xx response is `application/problem+json` with `type`, `title`, `status`,
//! `detail`, `instance`, and `request_id`. 5xx bodies carry a fixed detail string; the
//! real cause is logged against the same `request_id`.

pub mod app;
pub mod config;
pub mod dto;
pub mod error;
pub mod events;
pub mod middleware;
pub mod routes;
pub mod state;
pub mod store;
#[cfg(test)]
mod testing;

pub use app::{build_with_state, build_with_storage};
pub use config::{Config, LogFormat};
pub use events::{DomainEvent, EventBroadcaster, EventId, EventKind};
pub use error::{ApiError, ApiResult, ProblemDetails, ProblemKind};
pub use state::AppState;
