//! Fan-out to connected SSE clients.
//!
//! One [`EventBroadcaster`] lives in [`crate::state::AppState`]. Handlers publish; each
//! SSE connection holds a `broadcast::Receiver` and drains it independently.
//!
//! ## Why this cannot block on a slow client
//!
//! `tokio::sync::broadcast::Sender::send` never awaits. When a subscriber's buffer is
//! full the oldest message is dropped **for that subscriber only**, and its next `recv`
//! returns [`RecvError::Lagged`]. So a KDS tab that stops reading costs itself missed
//! events and costs the publisher nothing. The SSE layer turns `Lagged` into a visible
//! `stream.lagged` comment rather than a silent hole.
//!
//! ## Sequence numbers and replay
//!
//! Ids come from an `AtomicU64` starting at 1, assigned under the same lock that appends
//! to the replay ring. That ordering matters: if ids were assigned outside the lock two
//! concurrent publishers could append out of order and `replay_since` would return an
//! unsorted tail.
//!
//! The ring keeps the last [`EventBroadcaster::replay_capacity`] events so a client that
//! reconnects with `Last-Event-ID` can be handed the gap. Beyond that window the gap is
//! reported instead of faked.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use tokio::sync::broadcast;

use super::{DomainEvent, EventId, EventKind};

/// Per-subscriber buffer depth.
///
/// A KDS client that falls this many events behind starts losing the oldest ones. 256 is
/// roughly a minute of a busy dinner service, which is far longer than a browser needs to
/// paint a ticket.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// How many recent events are retained for `Last-Event-ID` replay.
///
/// Sized to cover a reconnect across a brief network blip, not a long outage.
pub const DEFAULT_REPLAY_CAPACITY: usize = 512;

/// The outcome of a `Last-Event-ID` resume request.
#[derive(Debug, Clone, PartialEq)]
pub enum Replay {
    /// Every event after the requested id is available and returned in order.
    Complete(Vec<DomainEvent>),
    /// The requested id has fallen out of the replay window. `events` is everything still
    /// retained (also in order); `missed` counts what was evicted and cannot be resent.
    Gap {
        events: Vec<DomainEvent>,
        missed: u64,
    },
}

impl Replay {
    /// The events to send, whichever variant this is.
    pub fn events(&self) -> &[DomainEvent] {
        match self {
            Self::Complete(events) => events,
            Self::Gap { events, .. } => events,
        }
    }

    pub fn into_events(self) -> Vec<DomainEvent> {
        match self {
            Self::Complete(events) => events,
            Self::Gap { events, .. } => events,
        }
    }

    /// Number of events permanently lost for this resume, if any.
    pub fn missed(&self) -> u64 {
        match self {
            Self::Complete(_) => 0,
            Self::Gap { missed, .. } => *missed,
        }
    }
}

struct Inner {
    sender: broadcast::Sender<DomainEvent>,
    next_id: AtomicU64,
    /// Guards the replay ring *and* id assignment, so ring order matches id order.
    recent: Mutex<VecDeque<DomainEvent>>,
    replay_capacity: usize,
}

/// Publishes [`DomainEvent`]s to every connected client.
///
/// Cheap to clone (one `Arc`); `AppState` clones it per request.
#[derive(Clone)]
pub struct EventBroadcaster {
    inner: Arc<Inner>,
}

impl EventBroadcaster {
    /// Broadcaster with the default capacities.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CHANNEL_CAPACITY, DEFAULT_REPLAY_CAPACITY)
    }

    /// Broadcaster with explicit capacities. Tests use tiny values to force lag and
    /// eviction deterministically.
    ///
    /// # Panics
    /// Panics if `channel_capacity` is zero, which `broadcast::channel` rejects.
    pub fn with_capacity(channel_capacity: usize, replay_capacity: usize) -> Self {
        assert!(
            channel_capacity > 0,
            "channel capacity must be non-zero; broadcast::channel panics on 0"
        );
        let (sender, _) = broadcast::channel(channel_capacity);
        Self {
            inner: Arc::new(Inner {
                sender,
                next_id: AtomicU64::new(1),
                recent: Mutex::new(VecDeque::with_capacity(replay_capacity.min(1024))),
                replay_capacity,
            }),
        }
    }

    /// Subscribes to events published from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<DomainEvent> {
        self.inner.sender.subscribe()
    }

    /// Number of live subscribers. Used by `/api/events/stream` diagnostics and tests.
    pub fn subscriber_count(&self) -> usize {
        self.inner.sender.receiver_count()
    }

    /// Highest assigned event id, or `0` before anything is published.
    pub fn last_event_id(&self) -> EventId {
        self.inner.next_id.load(Ordering::SeqCst).saturating_sub(1)
    }

    pub fn replay_capacity(&self) -> usize {
        self.inner.replay_capacity
    }

    /// Publishes an event and returns the record that was sent.
    ///
    /// Succeeds with zero subscribers: the event is still recorded in the replay ring, so
    /// a client connecting a moment later with `Last-Event-ID` still sees it. That is why
    /// the `broadcast::send` error is deliberately ignored rather than surfaced — "nobody
    /// is listening" is not a failure for a fire-and-forget notification.
    pub fn publish(&self, kind: EventKind, data: serde_json::Value) -> DomainEvent {
        // Lock first: id assignment and ring append must be one atomic step or a race
        // between two publishers can put id 8 in the ring ahead of id 7.
        let mut recent = self
            .inner
            .recent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let event = DomainEvent {
            id,
            kind,
            at: Utc::now(),
            data,
        };

        if self.inner.replay_capacity > 0 {
            if recent.len() == self.inner.replay_capacity {
                recent.pop_front();
            }
            recent.push_back(event.clone());
        }
        drop(recent);

        let _ = self.inner.sender.send(event.clone());
        event
    }

    /// Publishes a typed payload.
    ///
    /// # Errors
    /// Returns the serialisation error if `payload` cannot become JSON. Practically this
    /// only happens for a map with non-string keys; the payload types in
    /// [`super`] cannot fail.
    pub fn publish_typed<T: serde::Serialize>(
        &self,
        kind: EventKind,
        payload: &T,
    ) -> Result<DomainEvent, serde_json::Error> {
        Ok(self.publish(kind, serde_json::to_value(payload)?))
    }

    /// Events retained for replay, oldest first.
    pub fn recent(&self) -> Vec<DomainEvent> {
        self.inner
            .recent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Events with `id > after`, for a client resuming from `Last-Event-ID: after`.
    ///
    /// `after == 0` means "everything you still have".
    pub fn replay_since(&self, after: EventId) -> Replay {
        let recent = self
            .inner
            .recent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let oldest_retained = recent.front().map(|e| e.id);
        let events: Vec<DomainEvent> = recent
            .iter()
            .filter(|e| e.id > after)
            .cloned()
            .collect();
        drop(recent);

        match oldest_retained {
            // The next id the client wants is still in the ring (or the ring is empty and
            // there was nothing to miss), so the tail is complete.
            Some(oldest) if oldest > after + 1 => {
                let missed = oldest - (after + 1);
                Replay::Gap { events, missed }
            }
            _ => Replay::Complete(events),
        }
    }
}

impl Default for EventBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for EventBroadcaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBroadcaster")
            .field("subscribers", &self.subscriber_count())
            .field("last_event_id", &self.last_event_id())
            .field("replay_capacity", &self.inner.replay_capacity)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{InvoicePaidPayload, KotPayload, OrderPayload};
    use peacock_core::money::Money;
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use std::time::Duration;

    fn payload(tag: &str) -> serde_json::Value {
        serde_json::json!({ "tag": tag })
    }

    #[test]
    fn ids_start_at_one_and_increment() {
        let bus = EventBroadcaster::new();
        assert_eq!(bus.last_event_id(), 0, "no events yet");

        let first = bus.publish(EventKind::OrderCreated, payload("a"));
        let second = bus.publish(EventKind::OrderUpdated, payload("b"));

        assert_eq!(first.id, 1);
        assert_eq!(second.id, 2);
        assert_eq!(bus.last_event_id(), 2);
    }

    #[tokio::test]
    async fn subscriber_receives_published_events_in_order() {
        let bus = EventBroadcaster::new();
        let mut rx = bus.subscribe();

        bus.publish(EventKind::OrderCreated, payload("1"));
        bus.publish(EventKind::KotGenerated, payload("2"));
        bus.publish(EventKind::KotPrepared, payload("3"));

        let a = rx.recv().await.unwrap();
        let b = rx.recv().await.unwrap();
        let c = rx.recv().await.unwrap();

        assert_eq!((a.id, a.kind), (1, EventKind::OrderCreated));
        assert_eq!((b.id, b.kind), (2, EventKind::KotGenerated));
        assert_eq!((c.id, c.kind), (3, EventKind::KotPrepared));
    }

    #[tokio::test]
    async fn every_subscriber_gets_every_event() {
        let bus = EventBroadcaster::new();
        let mut receivers: Vec<_> = (0..10).map(|_| bus.subscribe()).collect();
        assert_eq!(bus.subscriber_count(), 10);

        bus.publish(EventKind::InvoicePaid, payload("fanout"));

        for (i, rx) in receivers.iter_mut().enumerate() {
            let event = rx.recv().await.expect("subscriber {i} must receive");
            assert_eq!(event.kind, EventKind::InvoicePaid, "subscriber {i}");
            assert_eq!(event.data["tag"], "fanout", "subscriber {i}");
        }
    }

    #[test]
    fn publishing_with_no_subscribers_still_records_for_replay() {
        let bus = EventBroadcaster::new();
        assert_eq!(bus.subscriber_count(), 0);

        bus.publish(EventKind::OrderCreated, payload("orphan"));

        let replayed = bus.replay_since(0).into_events();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].data["tag"], "orphan");
    }

    #[test]
    fn replay_since_returns_only_newer_events() {
        let bus = EventBroadcaster::new();
        for i in 1..=5 {
            bus.publish(EventKind::OrderUpdated, payload(&i.to_string()));
        }

        let replay = bus.replay_since(3);
        assert!(matches!(replay, Replay::Complete(_)));
        let ids: Vec<EventId> = replay.events().iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![4, 5]);

        assert!(
            bus.replay_since(5).events().is_empty(),
            "a fully caught-up client gets nothing"
        );
        assert_eq!(bus.replay_since(0).events().len(), 5, "0 replays everything");
    }

    #[test]
    fn replay_reports_a_gap_when_the_window_has_evicted_events() {
        // Ring holds 3; publish 6 so ids 1..=3 are evicted.
        let bus = EventBroadcaster::with_capacity(16, 3);
        for i in 1..=6 {
            bus.publish(EventKind::KotGenerated, payload(&i.to_string()));
        }

        let replay = bus.replay_since(1);
        match &replay {
            Replay::Gap { events, missed } => {
                // Client had id 1, wanted 2 and 3, but the oldest retained is 4.
                assert_eq!(*missed, 2);
                let ids: Vec<EventId> = events.iter().map(|e| e.id).collect();
                assert_eq!(ids, vec![4, 5, 6]);
            }
            Replay::Complete(_) => panic!("an evicted range must be reported as a gap"),
        }
        assert_eq!(replay.missed(), 2);

        // Inside the window there is no gap.
        assert!(matches!(bus.replay_since(4), Replay::Complete(_)));
        assert_eq!(bus.replay_since(4).missed(), 0);
    }

    #[test]
    fn replay_ring_is_bounded() {
        let bus = EventBroadcaster::with_capacity(8, 4);
        for i in 1..=20 {
            bus.publish(EventKind::OrderUpdated, payload(&i.to_string()));
        }
        let recent = bus.recent();
        assert_eq!(recent.len(), 4, "ring must not grow past its capacity");
        let ids: Vec<EventId> = recent.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![17, 18, 19, 20], "ring keeps the newest events, in order");
    }

    #[tokio::test]
    async fn a_slow_subscriber_lags_instead_of_blocking_the_publisher() {
        let bus = EventBroadcaster::with_capacity(2, 64);
        let mut slow = bus.subscribe();
        let mut fast = bus.subscribe();

        // Publish past the per-subscriber buffer depth. This must not await or panic.
        for i in 1..=10 {
            bus.publish(EventKind::KotPrepared, payload(&i.to_string()));
        }

        // The fast reader drains normally... after the same overflow, so it also lagged;
        // what matters is that publishing completed and both receivers stay usable.
        let slow_err = slow.recv().await.expect_err("slow subscriber must report lag");
        assert!(
            matches!(slow_err, broadcast::error::RecvError::Lagged(n) if n > 0),
            "expected Lagged, got {slow_err:?}"
        );

        // After a lag the receiver resumes from the oldest retained message.
        let resumed = slow.recv().await.expect("receiver stays usable after lag");
        assert_eq!(resumed.id, 9, "resumes at the oldest still-buffered event");

        let _ = fast.recv().await;
        assert_eq!(bus.last_event_id(), 10);
    }

    #[tokio::test]
    async fn events_reach_a_subscriber_well_under_one_second() {
        let bus = EventBroadcaster::new();
        let mut rx = bus.subscribe();

        let publisher = bus.clone();
        tokio::spawn(async move {
            publisher.publish(EventKind::KotGenerated, payload("latency"));
        });

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("event must arrive within 1s")
            .expect("no lag expected");
        assert_eq!(event.data["tag"], "latency");
    }

    #[tokio::test]
    async fn concurrent_publishers_produce_dense_ordered_ids() {
        let bus = EventBroadcaster::with_capacity(1024, 1024);

        let mut tasks = Vec::new();
        for t in 0..8 {
            let bus = bus.clone();
            tasks.push(tokio::spawn(async move {
                for i in 0..25 {
                    bus.publish(EventKind::OrderUpdated, payload(&format!("{t}-{i}")));
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        assert_eq!(bus.last_event_id(), 200);
        let ids: Vec<EventId> = bus.recent().iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 200, "no publish may be lost");
        let expected: Vec<EventId> = (1..=200).collect();
        assert_eq!(ids, expected, "ring order must match id order with no duplicates");
    }

    #[test]
    fn publish_typed_serialises_each_payload_shape() {
        let bus = EventBroadcaster::new();

        let order = bus
            .publish_typed(
                EventKind::OrderCreated,
                &OrderPayload::new("ORD-1").with_table("T-01"),
            )
            .expect("order payload serialises");
        assert_eq!(order.data["order_id"], "ORD-1");
        assert_eq!(order.data["table"], "T-01");

        let kot = bus
            .publish_typed(
                EventKind::KotGenerated,
                &KotPayload::new("KOT-9", "INV-9").with_production_unit("Grill"),
            )
            .expect("kot payload serialises");
        assert_eq!(kot.data["kot_id"], "KOT-9");
        assert_eq!(kot.data["production_unit"], "Grill");

        let invoice = bus
            .publish_typed(
                EventKind::InvoicePaid,
                &InvoicePaidPayload::new(
                    "INV-9",
                    Money::new(Decimal::from_str("99.05").unwrap()),
                ),
            )
            .expect("invoice payload serialises");
        assert_eq!(invoice.data["grand_total"], "99.05");

        assert_eq!(bus.last_event_id(), 3);
    }

    #[test]
    fn clones_share_one_sequence_and_one_ring() {
        let bus = EventBroadcaster::new();
        let clone = bus.clone();

        bus.publish(EventKind::OrderCreated, payload("via-original"));
        clone.publish(EventKind::OrderUpdated, payload("via-clone"));

        assert_eq!(bus.last_event_id(), 2);
        assert_eq!(clone.last_event_id(), 2);
        assert_eq!(bus.recent().len(), 2, "both writes land in the same ring");
    }

    #[test]
    fn zero_replay_capacity_disables_replay_but_not_publishing() {
        let bus = EventBroadcaster::with_capacity(8, 0);
        let event = bus.publish(EventKind::InvoicePaid, payload("no-replay"));
        assert_eq!(event.id, 1);
        assert!(bus.recent().is_empty());
        assert!(matches!(bus.replay_since(0), Replay::Complete(events) if events.is_empty()));
    }
}
