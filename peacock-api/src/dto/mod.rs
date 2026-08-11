//! Data Transfer Objects for HTTP API.
//!
//! Keep these separate from domain models so the wire format can evolve without
//! forcing domain changes.

pub mod aggregator;
pub mod invoice;
pub mod kot;
pub mod menu;
pub mod order;
pub mod reports;
pub mod shift;
pub mod table;
