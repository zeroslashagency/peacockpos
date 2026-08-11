//! Middleware stack.
//!
//! Layer order matters. Outermost first, as applied in [`crate::app`]:
//!
//! 1. `request_id` — so every inner layer can log a correlation id
//! 2. `logging` — measures the full inner duration, sees the final status
//! 3. `error` — rewrites error bodies before logging reads the status
//! 4. `cors` — innermost of the four, so its headers are set on real and error
//!    responses alike and survive the error rewrite
//!
//! `tower` applies layers bottom-up, so [`crate::app::build`] adds them in reverse.

pub mod context;
pub mod cors;
pub mod error;
pub mod logging;
pub mod request_id;
