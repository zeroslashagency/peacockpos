//! Realtime event stream (Lane 3H).
//!
//! Domain mutations publish a [`DomainEvent`] to the [`broadcaster::EventBroadcaster`]
//! held in [`crate::state::AppState`]; connected KDS/POS clients read them off
//! `GET /api/events/stream` as Server-Sent Events.
//!
//! ## Architecture decision: in-memory broadcast channel
//!
//! The transport is `tokio::sync::broadcast` plus a bounded replay ring, **not**
//! Postgres `NOTIFY`/`LISTEN`.
//!
//! Why:
//!
//! - A restaurant branch runs one API instance. `NOTIFY`/`LISTEN` buys cross-instance
//!   fan-out that nothing in Phase 3 needs, at the cost of a dedicated connection per
//!   listener plus a reconnect/replay path that still has to be written, because
//!   `NOTIFY` payloads are *not* persisted either — a client offline during the notify
//!   misses it exactly like it misses a broadcast message.
//! - `broadcast` gives per-subscriber buffering with drop-oldest semantics, which is the
//!   behaviour we want for slow clients: a stalled KDS tab can never block a publisher
//!   or another subscriber. A blocking `NOTIFY` consumer can wedge the whole listener.
//! - Reconnection is served from [`broadcaster::EventBroadcaster::replay_since`], a
//!   bounded in-process ring of the most recent events. `Last-Event-ID` resumes exactly
//!   when the gap is inside that window, and the client is told when it is not.
//!
//! What this costs, stated plainly: events do not survive a process restart, and two API
//! instances behind a load balancer would each fan out only their own writes. Both are
//! acceptable for a single-instance branch deployment and both are visible at the seam —
//! `EventBroadcaster` is the only publisher API, so swapping its body for
//! `NOTIFY`/`LISTEN` (or Redis) later does not touch handlers or the SSE endpoint.
//!
//! ## Wire format
//!
//! ```text
//! event: kot.generated
//! id: 123
//! data: {"kot_id":"KOT-001","invoice":"INV-001"}
//!
//! ```
//!
//! `id` is a process-local monotonic sequence, which is what the client echoes back in
//! `Last-Event-ID`.
//!
//! ## Publishing from a handler
//!
//! Publish **after** the mutation is durable, never before: a client that acts on
//! `kot.generated` for a KOT that then failed to persist is worse than a client that
//! learns about it a beat late.
//!
//! ```no_run
//! use peacock_api::events::{EventKind, KotPayload};
//! use peacock_api::AppState;
//!
//! fn after_kot_insert(state: &AppState, kot_id: &str, invoice: &str) {
//!     // Publishing cannot fail and does not block, so it needs no error handling.
//!     let _ = state.events().publish_typed(
//!         EventKind::KotGenerated,
//!         &KotPayload::new(kot_id, invoice).with_production_unit("Kitchen"),
//!     );
//! }
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use peacock_core::money::Money;

pub mod broadcaster;
pub mod sse;

pub use broadcaster::EventBroadcaster;

/// Monotonic, process-local event sequence number.
///
/// Starts at 1; `0` means "no event yet", which is why `Last-Event-ID: 0` replays
/// everything still buffered.
pub type EventId = u64;

/// The closed set of events the API publishes.
///
/// Wire names are dotted (`order.created`) and stable — clients subscribe by them, so
/// renaming one is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EventKind {
    OrderCreated,
    OrderUpdated,
    KotGenerated,
    KotPrepared,
    InvoicePaid,
}

impl EventKind {
    /// Every kind, in declaration order. Used by the `?events=` filter validation and by
    /// tests that must cover the full set.
    pub const ALL: [EventKind; 5] = [
        Self::OrderCreated,
        Self::OrderUpdated,
        Self::KotGenerated,
        Self::KotPrepared,
        Self::InvoicePaid,
    ];

    /// The SSE `event:` field value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OrderCreated => "order.created",
            Self::OrderUpdated => "order.updated",
            Self::KotGenerated => "kot.generated",
            Self::KotPrepared => "kot.prepared",
            Self::InvoicePaid => "invoice.paid",
        }
    }

    /// Parses a wire name. Case-insensitive and whitespace-tolerant so a hand-written
    /// query string (`?events=Order.Created, kot.prepared`) is not a silent no-match.
    pub fn parse(raw: &str) -> Option<Self> {
        let needle = raw.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|k| k.as_str() == needle)
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EventKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!("unknown event kind {raw:?}"))
        })
    }
}

/// One published event: sequence number, kind, timestamp, and a JSON payload.
///
/// The payload is `serde_json::Value` rather than a typed enum so a lane can extend its
/// own payload shape without a cross-lane edit here. The typed payload structs below are
/// the recommended way to build it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DomainEvent {
    pub id: EventId,
    pub kind: EventKind,
    /// Publish time, UTC. Clients use it to age out stale KDS tickets.
    pub at: DateTime<Utc>,
    pub data: serde_json::Value,
}

impl DomainEvent {
    /// The `data:` field body.
    ///
    /// Serialising a `serde_json::Value` cannot fail, so this returns a `String` rather
    /// than a `Result`; the fallback is only there to keep the signature infallible.
    pub fn data_json(&self) -> String {
        serde_json::to_string(&self.data).unwrap_or_else(|_| "null".to_string())
    }
}

/// Payload for `order.created` / `order.updated`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderPayload {
    pub order_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos_profile: Option<String>,
}

impl OrderPayload {
    pub fn new(order_id: impl Into<String>) -> Self {
        Self {
            order_id: order_id.into(),
            table: None,
            status: None,
            pos_profile: None,
        }
    }

    pub fn with_table(mut self, table: impl Into<String>) -> Self {
        self.table = Some(table.into());
        self
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn with_pos_profile(mut self, profile: impl Into<String>) -> Self {
        self.pos_profile = Some(profile.into());
        self
    }
}

/// Payload for `kot.generated` / `kot.prepared`.
///
/// `production_unit` is what a KDS screen filters on, so it is carried even though the
/// client could look it up: the point of the stream is to avoid a round trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KotPayload {
    pub kot_id: String,
    pub invoice: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kot_type: Option<String>,
}

impl KotPayload {
    pub fn new(kot_id: impl Into<String>, invoice: impl Into<String>) -> Self {
        Self {
            kot_id: kot_id.into(),
            invoice: invoice.into(),
            production_unit: None,
            table: None,
            kot_type: None,
        }
    }

    pub fn with_production_unit(mut self, unit: impl Into<String>) -> Self {
        self.production_unit = Some(unit.into());
        self
    }

    pub fn with_table(mut self, table: impl Into<String>) -> Self {
        self.table = Some(table.into());
        self
    }

    pub fn with_kot_type(mut self, kot_type: impl Into<String>) -> Self {
        self.kot_type = Some(kot_type.into());
        self
    }
}

/// Payload for `invoice.paid`.
///
/// `grand_total` is [`Money`], which serialises as a decimal **string**. Money never
/// crosses this wire as a JSON number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoicePaidPayload {
    pub invoice_id: String,
    pub grand_total: Money,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode_of_payment: Option<String>,
}

impl InvoicePaidPayload {
    pub fn new(invoice_id: impl Into<String>, grand_total: Money) -> Self {
        Self {
            invoice_id: invoice_id.into(),
            grand_total,
            table: None,
            mode_of_payment: None,
        }
    }

    pub fn with_table(mut self, table: impl Into<String>) -> Self {
        self.table = Some(table.into());
        self
    }

    pub fn with_mode_of_payment(mut self, mode: impl Into<String>) -> Self {
        self.mode_of_payment = Some(mode.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn wire_names_are_the_dotted_form() {
        assert_eq!(EventKind::OrderCreated.as_str(), "order.created");
        assert_eq!(EventKind::OrderUpdated.as_str(), "order.updated");
        assert_eq!(EventKind::KotGenerated.as_str(), "kot.generated");
        assert_eq!(EventKind::KotPrepared.as_str(), "kot.prepared");
        assert_eq!(EventKind::InvoicePaid.as_str(), "invoice.paid");
    }

    #[test]
    fn every_kind_round_trips_through_parse() {
        for kind in EventKind::ALL {
            assert_eq!(EventKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(EventKind::parse("  KOT.Prepared "), Some(EventKind::KotPrepared));
        assert_eq!(EventKind::parse("order.deleted"), None);
    }

    #[test]
    fn kind_serialises_as_its_wire_name() {
        let json = serde_json::to_string(&EventKind::KotGenerated).unwrap();
        assert_eq!(json, r#""kot.generated""#);
        let back: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, EventKind::KotGenerated);

        let bad: Result<EventKind, _> = serde_json::from_str(r#""nope""#);
        assert!(bad.is_err(), "unknown kinds must not deserialise");
    }

    #[test]
    fn invoice_payload_keeps_money_as_a_string() {
        let payload = InvoicePaidPayload::new(
            "ACC-PSINV-2026-00001",
            Money::new(Decimal::from_str("1234.50").unwrap()),
        )
        .with_mode_of_payment("Cash");

        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["grand_total"], serde_json::json!("1234.50"));
        assert!(
            json["grand_total"].is_string(),
            "money must never serialise as a float"
        );
        assert!(json.get("table").is_none(), "unset fields are omitted");
        assert_eq!(json["mode_of_payment"], "Cash");
    }

    #[test]
    fn kot_payload_omits_unset_optionals() {
        let payload = KotPayload::new("KOT-001", "ACC-PSINV-2026-00001")
            .with_production_unit("Kitchen")
            .with_table("T-01");
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["kot_id"], "KOT-001");
        assert_eq!(json["production_unit"], "Kitchen");
        assert_eq!(json["table"], "T-01");
        assert!(json.get("kot_type").is_none());
    }

    #[test]
    fn order_payload_builders_accumulate() {
        let payload = OrderPayload::new("ORD-1")
            .with_table("T-07")
            .with_status("Draft")
            .with_pos_profile("Main POS");
        assert_eq!(payload.table.as_deref(), Some("T-07"));
        assert_eq!(payload.status.as_deref(), Some("Draft"));
        assert_eq!(payload.pos_profile.as_deref(), Some("Main POS"));
    }
}
