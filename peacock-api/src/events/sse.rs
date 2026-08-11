//! `GET /api/events/stream` — the Server-Sent Events endpoint.
//!
//! One connection = one `broadcast::Receiver`. The response stream is the replay tail
//! (from `Last-Event-ID`) chained onto live events, so a reconnecting client sees a
//! continuous, ordered id sequence.
//!
//! ## Frames on the wire
//!
//! ```text
//! event: kot.generated
//! id: 42
//! data: {"kot_id":"KOT-001","invoice":"ACC-PSINV-2026-00001"}
//!
//! ```
//!
//! Two non-domain frames also appear:
//!
//! - a `stream.open` comment plus `retry:` hint as the first frame, which flushes headers
//!   immediately so the browser fires `onopen` without waiting for restaurant traffic;
//! - `: stream.lagged n=<count>` when this connection fell behind the channel buffer, so
//!   the client knows to refetch rather than assume it has the full picture.
//!
//! Keep-alive comments go out every 15s to survive proxy idle timeouts.
//!
//! ## Slow clients
//!
//! Nothing here awaits a client. Backpressure lands on that connection's own broadcast
//! buffer; when it overflows the oldest events are dropped for that receiver only and the
//! next poll reports [`BroadcastStreamRecvError::Lagged`]. Publishers are never blocked,
//! which is what lets 50+ tabs share one bus.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::error::ApiError;
use crate::state::AppState;

use super::{DomainEvent, EventId, EventKind};

/// Header a browser resends after a dropped connection.
pub const LAST_EVENT_ID: &str = "last-event-id";

/// Reconnect delay advertised to the client, in milliseconds.
const RETRY_MS: u64 = 3_000;

/// Idle comment interval. Well under the 60s default idle timeout of common proxies.
const KEEP_ALIVE_SECS: u64 = 15;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/events/stream", get(stream))
}

/// Optional query parameters.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct StreamQuery {
    /// Comma-separated event kinds to receive. Absent or empty means all kinds.
    pub events: Option<String>,
    /// Fallback for `Last-Event-ID` when a client cannot set headers (e.g. the browser
    /// `EventSource` API on first connect after a manual refresh).
    pub last_event_id: Option<EventId>,
}

/// Which kinds a connection wants.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    kinds: Option<Vec<EventKind>>,
}

impl Filter {
    pub fn all() -> Self {
        Self { kinds: None }
    }

    /// Parses a `?events=` value.
    ///
    /// # Errors
    /// Returns 400 for an unknown kind. Silently ignoring it would leave a client waiting
    /// forever on a stream that can never match — a typo must be loud.
    pub fn parse(raw: Option<&str>) -> Result<Self, ApiError> {
        let Some(raw) = raw else {
            return Ok(Self::all());
        };
        let mut kinds = Vec::new();
        for token in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            let kind = EventKind::parse(token).ok_or_else(|| {
                ApiError::invalid_input(format!(
                    "unknown event kind {token:?}; expected one of {}",
                    EventKind::ALL
                        .iter()
                        .map(|k| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        if kinds.is_empty() {
            // `?events=` or `?events=,,` — treat as unfiltered rather than "match nothing",
            // which would look like a broken stream.
            return Ok(Self::all());
        }
        Ok(Self { kinds: Some(kinds) })
    }

    pub fn accepts(&self, kind: EventKind) -> bool {
        match &self.kinds {
            None => true,
            Some(kinds) => kinds.contains(&kind),
        }
    }

    /// `None` when unfiltered.
    pub fn kinds(&self) -> Option<&[EventKind]> {
        self.kinds.as_deref()
    }
}

/// Reads `Last-Event-ID` from the header, falling back to the query parameter.
///
/// A malformed value resumes from 0 rather than failing the request: a client with a
/// corrupted id should still get a working stream.
pub fn resume_from(headers: &HeaderMap, query: &StreamQuery) -> EventId {
    headers
        .get(LAST_EVENT_ID)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<EventId>().ok())
        .or(query.last_event_id)
        .unwrap_or(0)
}

/// Renders one domain event as an SSE frame.
pub fn to_sse_event(event: &DomainEvent) -> Event {
    Event::default()
        .event(event.kind.as_str())
        .id(event.id.to_string())
        .data(event.data_json())
}

/// The opening frame: a comment (ignored by clients) plus the reconnect hint.
///
/// Comments are legal SSE and never surface as a message, so this cannot be mistaken for
/// a domain event.
fn open_event(resume: EventId, filter: &Filter) -> Event {
    let scope = match filter.kinds() {
        None => "all".to_string(),
        Some(kinds) => kinds
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join(","),
    };
    Event::default()
        .comment(format!("stream.open resume={resume} events={scope}"))
        .retry(Duration::from_millis(RETRY_MS))
}

/// GET /api/events/stream
///
/// ## Query
/// - `events` — optional comma-separated filter, e.g. `?events=kot.generated,kot.prepared`
/// - `last_event_id` — optional resume point when headers are unavailable
///
/// ## Headers
/// - `Last-Event-ID` — resume point; takes precedence over the query parameter
///
/// ## Errors
/// - 400: unknown event kind in `events`
async fn stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let filter = Filter::parse(query.events.as_deref())?;
    let resume = resume_from(&headers, &query);
    let broadcaster = state.events().clone();

    // Subscribe *before* reading the replay tail. The other order has a hole: an event
    // published between the replay snapshot and the subscribe would be in neither.
    // Overlap is fine — the dedup below drops replayed ids from the live stream.
    let receiver = broadcaster.subscribe();
    let replay = broadcaster.replay_since(resume);
    let missed = replay.missed();
    let replayed = replay.into_events();
    let highest_replayed = replayed.last().map(|e| e.id).unwrap_or(resume);

    tracing::debug!(
        resume,
        missed,
        replayed = replayed.len(),
        subscribers = broadcaster.subscriber_count(),
        "sse client connected"
    );

    let mut prologue: Vec<Result<Event, Infallible>> = Vec::with_capacity(replayed.len() + 2);
    prologue.push(Ok(open_event(resume, &filter)));
    if missed > 0 {
        // Told, not hidden: the client must know to refetch state it can never be sent.
        prologue.push(Ok(Event::default().comment(format!("stream.gap missed={missed}"))));
    }
    let replay_filter = filter.clone();
    prologue.extend(
        replayed
            .into_iter()
            .filter(move |event| replay_filter.accepts(event.kind))
            .map(|event| Ok(to_sse_event(&event))),
    );

    let live = BroadcastStream::new(receiver).filter_map(move |item| match item {
        Ok(event) => {
            // Skip anything already delivered in the replay tail.
            if event.id <= highest_replayed || !filter.accepts(event.kind) {
                None
            } else {
                Some(Ok(to_sse_event(&event)))
            }
        }
        Err(BroadcastStreamRecvError::Lagged(skipped)) => {
            tracing::warn!(skipped, "sse client lagged; events dropped for this connection");
            Some(Ok(Event::default().comment(format!("stream.lagged n={skipped}"))))
        }
    });

    let stream = tokio_stream::iter(prologue).chain(live);

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(KEEP_ALIVE_SECS))
            .text("stream.keep-alive"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::events::{EventBroadcaster, InvoicePaidPayload, KotPayload};
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use peacock_core::money::Money;
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use std::time::Duration as StdDuration;
    use tower::ServiceExt;

    /// App with an isolated bus, and the bus handle so tests can publish into it.
    fn app_with_bus() -> (Router, EventBroadcaster) {
        let state = AppState::new(Config::default());
        let bus = state.events().clone();
        let app = routes()
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::middleware::error::handle_errors,
            ))
            .with_state(state);
        (app, bus)
    }

    /// Reads SSE frames until `min_frames` blank-line-terminated blocks have arrived or
    /// the deadline expires. Returns the raw text so tests can assert on the wire format.
    async fn read_frames(body: Body, min_frames: usize, timeout: StdDuration) -> String {
        let mut stream = body.into_data_stream();
        let mut text = String::new();
        let deadline = tokio::time::Instant::now() + timeout;

        while text.matches("\n\n").count() < min_frames {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let next = tokio::time::timeout(remaining, futures_next(&mut stream)).await;
            match next {
                Ok(Some(chunk)) => text.push_str(&String::from_utf8_lossy(&chunk)),
                Ok(None) => break,
                Err(_) => break,
            }
        }
        text
    }

    /// `StreamExt::next` without pulling in `futures_util`: poll the stream by hand.
    async fn futures_next<S>(stream: &mut S) -> Option<axum::body::Bytes>
    where
        S: Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin,
    {
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx))
            .await
            .and_then(Result::ok)
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn endpoint_responds_with_the_sse_content_type() {
        let (app, _bus) = app_with_bus();
        let response = app.oneshot(get("/api/events/stream")).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache",
            "SSE must not be cached"
        );
    }

    #[tokio::test]
    async fn open_frame_arrives_before_any_domain_event() {
        let (app, _bus) = app_with_bus();
        let response = app.oneshot(get("/api/events/stream")).await.unwrap();
        let text = read_frames(response.into_body(), 1, StdDuration::from_secs(2)).await;

        assert!(
            text.contains(": stream.open"),
            "headers must flush immediately; got {text:?}"
        );
        // axum writes the field without a space after the colon, which is legal SSE.
        assert!(
            text.contains(&format!("retry:{RETRY_MS}")),
            "clients need the reconnect hint; got {text:?}"
        );
    }

    #[tokio::test]
    async fn client_receives_a_live_event_in_sse_format() {
        let (app, bus) = app_with_bus();
        let response = app.oneshot(get("/api/events/stream")).await.unwrap();
        let body = response.into_body();

        // Publish after the connection exists so this exercises the live path.
        let publisher = bus.clone();
        tokio::spawn(async move {
            tokio::time::sleep(StdDuration::from_millis(20)).await;
            publisher
                .publish_typed(
                    EventKind::KotGenerated,
                    &KotPayload::new("KOT-001", "ACC-PSINV-2026-00001"),
                )
                .unwrap();
        });

        let text = read_frames(body, 2, StdDuration::from_secs(2)).await;

        assert!(text.contains("event: kot.generated"), "got {text:?}");
        assert!(text.contains("id: 1"), "got {text:?}");
        assert!(text.contains(r#""kot_id":"KOT-001""#), "got {text:?}");
        assert!(
            text.contains("\n\n"),
            "frames must be terminated by a blank line; got {text:?}"
        );
    }

    #[tokio::test]
    async fn event_ordering_is_preserved_on_the_wire() {
        let (app, bus) = app_with_bus();
        bus.publish(EventKind::OrderCreated, serde_json::json!({ "n": 1 }));
        bus.publish(EventKind::KotGenerated, serde_json::json!({ "n": 2 }));
        bus.publish(EventKind::KotPrepared, serde_json::json!({ "n": 3 }));
        bus.publish(EventKind::InvoicePaid, serde_json::json!({ "n": 4 }));

        let response = app.oneshot(get("/api/events/stream")).await.unwrap();
        let text = read_frames(response.into_body(), 5, StdDuration::from_secs(2)).await;

        let ids: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("id: "))
            .collect();
        assert_eq!(ids, vec!["1", "2", "3", "4"], "ids must arrive ascending");

        let kinds: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("event: "))
            .collect();
        assert_eq!(
            kinds,
            vec!["order.created", "kot.generated", "kot.prepared", "invoice.paid"]
        );
    }

    #[tokio::test]
    async fn reconnect_with_last_event_id_header_resumes_after_that_id() {
        let (app, bus) = app_with_bus();
        for n in 1..=5 {
            bus.publish(EventKind::OrderUpdated, serde_json::json!({ "n": n }));
        }

        let request = Request::builder()
            .uri("/api/events/stream")
            .header(LAST_EVENT_ID, "3")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let text = read_frames(response.into_body(), 3, StdDuration::from_secs(2)).await;

        let ids: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("id: "))
            .collect();
        assert_eq!(ids, vec!["4", "5"], "already-seen events must not be resent");
        assert!(text.contains("stream.open resume=3"), "got {text:?}");
    }

    #[tokio::test]
    async fn resume_falls_back_to_the_query_parameter() {
        let (app, bus) = app_with_bus();
        for n in 1..=3 {
            bus.publish(EventKind::OrderUpdated, serde_json::json!({ "n": n }));
        }

        let response = app
            .oneshot(get("/api/events/stream?last_event_id=2"))
            .await
            .unwrap();
        let text = read_frames(response.into_body(), 2, StdDuration::from_secs(2)).await;

        let ids: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("id: "))
            .collect();
        assert_eq!(ids, vec!["3"]);
    }

    #[tokio::test]
    async fn a_resume_past_the_replay_window_is_reported_as_a_gap() {
        let state = AppState::with_broadcaster(
            Config::default(),
            EventBroadcaster::with_capacity(64, 2),
        );
        let bus = state.events().clone();
        let app = routes().with_state(state);

        for n in 1..=6 {
            bus.publish(EventKind::KotPrepared, serde_json::json!({ "n": n }));
        }

        let request = Request::builder()
            .uri("/api/events/stream")
            .header(LAST_EVENT_ID, "1")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        // open + gap comment + the two retained events.
        let text = read_frames(response.into_body(), 4, StdDuration::from_secs(2)).await;

        assert!(
            text.contains("stream.gap missed=3"),
            "an unreplayable gap must be announced, not hidden; got {text:?}"
        );
        let ids: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("id: "))
            .collect();
        assert_eq!(ids, vec!["5", "6"], "only retained events can be resent");
    }

    #[tokio::test]
    async fn malformed_last_event_id_resumes_from_the_start() {
        let (app, bus) = app_with_bus();
        bus.publish(EventKind::OrderCreated, serde_json::json!({ "n": 1 }));

        let request = Request::builder()
            .uri("/api/events/stream")
            .header(LAST_EVENT_ID, "not-a-number")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "a bad id must not 4xx");

        let text = read_frames(response.into_body(), 2, StdDuration::from_secs(2)).await;
        assert!(text.contains("id: 1"), "got {text:?}");
    }

    #[tokio::test]
    async fn events_filter_narrows_the_stream() {
        let (app, bus) = app_with_bus();
        bus.publish(EventKind::OrderCreated, serde_json::json!({ "n": 1 }));
        bus.publish(EventKind::KotGenerated, serde_json::json!({ "n": 2 }));
        bus.publish(EventKind::InvoicePaid, serde_json::json!({ "n": 3 }));
        bus.publish(EventKind::KotPrepared, serde_json::json!({ "n": 4 }));

        let response = app
            .oneshot(get("/api/events/stream?events=kot.generated,kot.prepared"))
            .await
            .unwrap();
        let text = read_frames(response.into_body(), 3, StdDuration::from_secs(2)).await;

        let kinds: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("event: "))
            .collect();
        assert_eq!(kinds, vec!["kot.generated", "kot.prepared"]);
        assert!(
            !text.contains("invoice.paid"),
            "filtered kinds must not leak; got {text:?}"
        );
        assert!(
            text.contains("id: 2") && text.contains("id: 4"),
            "ids stay global, not renumbered per filter; got {text:?}"
        );
    }

    #[tokio::test]
    async fn an_unknown_event_kind_is_a_400_problem_document() {
        let (app, _bus) = app_with_bus();
        let response = app
            .oneshot(get("/api/events/stream?events=order.exploded"))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], 400);
        assert!(
            json["detail"].as_str().unwrap().contains("order.exploded"),
            "the detail must name the offending value; got {json}"
        );
    }

    #[tokio::test]
    async fn ten_concurrent_clients_all_receive_the_same_event() {
        let (app, bus) = app_with_bus();

        let mut bodies = Vec::new();
        for _ in 0..10 {
            let response = app.clone().oneshot(get("/api/events/stream")).await.unwrap();
            bodies.push(response.into_body());
        }
        assert_eq!(bus.subscriber_count(), 10, "every connection must subscribe");

        bus.publish_typed(
            EventKind::InvoicePaid,
            &InvoicePaidPayload::new("INV-77", Money::new(Decimal::from_str("250.00").unwrap())),
        )
        .unwrap();

        let mut received = 0;
        for (i, body) in bodies.into_iter().enumerate() {
            let text = read_frames(body, 2, StdDuration::from_secs(2)).await;
            assert!(
                text.contains("event: invoice.paid"),
                "client {i} missed the event; got {text:?}"
            );
            assert!(
                text.contains(r#""grand_total":"250.00""#),
                "client {i} must see money as a string; got {text:?}"
            );
            received += 1;
        }
        assert_eq!(received, 10);
    }

    #[tokio::test]
    async fn fifty_concurrent_clients_connect_and_receive() {
        let (app, bus) = app_with_bus();

        let mut bodies = Vec::new();
        for _ in 0..50 {
            let response = app.clone().oneshot(get("/api/events/stream")).await.unwrap();
            bodies.push(response.into_body());
        }
        assert_eq!(bus.subscriber_count(), 50);

        bus.publish(EventKind::KotGenerated, serde_json::json!({ "kot_id": "KOT-50" }));

        // Read only a sample: the assertion under test is that 50 subscribers coexist and
        // that fan-out reaches an arbitrary one of them.
        for body in bodies.into_iter().take(5) {
            let text = read_frames(body, 2, StdDuration::from_secs(2)).await;
            assert!(text.contains("KOT-50"), "got {text:?}");
        }
    }

    #[tokio::test]
    async fn a_slow_client_does_not_stall_publishing_or_other_clients() {
        let state = AppState::with_broadcaster(
            Config::default(),
            // Depth 2 so a client that reads nothing overflows almost immediately.
            EventBroadcaster::with_capacity(2, 512),
        );
        let bus = state.events().clone();
        let app = routes().with_state(state);

        // Connect a client and never read from it.
        let stalled = app.clone().oneshot(get("/api/events/stream")).await.unwrap();
        let _stalled_body = stalled.into_body();

        // Publishing must return promptly regardless.
        let publish = tokio::time::timeout(StdDuration::from_secs(1), async {
            for n in 1..=50 {
                bus.publish(EventKind::OrderUpdated, serde_json::json!({ "n": n }));
            }
        })
        .await;
        assert!(publish.is_ok(), "a stalled reader must never block publishers");

        // A fresh client still works and sees new traffic. Resume from the current head so
        // the assertion is about live delivery, not the replay tail.
        let request = Request::builder()
            .uri("/api/events/stream")
            .header(LAST_EVENT_ID, bus.last_event_id().to_string())
            .body(Body::empty())
            .unwrap();
        let healthy = app.oneshot(request).await.unwrap();
        let body = healthy.into_body();
        bus.publish(EventKind::KotPrepared, serde_json::json!({ "n": "after" }));
        let text = read_frames(body, 2, StdDuration::from_secs(2)).await;
        assert!(text.contains("kot.prepared"), "got {text:?}");
    }

    #[tokio::test]
    async fn disconnecting_releases_the_subscription() {
        let (app, bus) = app_with_bus();
        {
            let response = app.clone().oneshot(get("/api/events/stream")).await.unwrap();
            let body = response.into_body();
            assert_eq!(bus.subscriber_count(), 1);
            drop(body);
        }
        // Dropping the body drops the receiver; give the runtime a tick to settle.
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        assert_eq!(
            bus.subscriber_count(),
            0,
            "a disconnected client must not leak a subscription"
        );

        // And a reconnect works.
        let response = app.oneshot(get("/api/events/stream")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(bus.subscriber_count(), 1);
    }

    #[tokio::test]
    async fn events_reach_a_connected_client_in_under_one_second() {
        let (app, bus) = app_with_bus();
        let response = app.oneshot(get("/api/events/stream")).await.unwrap();
        let body = response.into_body();

        let publisher = bus.clone();
        tokio::spawn(async move {
            publisher.publish(EventKind::KotGenerated, serde_json::json!({ "kot_id": "K-1" }));
        });

        let started = tokio::time::Instant::now();
        let text = read_frames(body, 2, StdDuration::from_secs(1)).await;
        let elapsed = started.elapsed();

        assert!(text.contains("K-1"), "got {text:?}");
        assert!(
            elapsed < StdDuration::from_secs(1),
            "KDS latency budget is 1s, took {elapsed:?}"
        );
    }

    #[test]
    fn filter_parsing_is_tolerant_of_spacing_and_case() {
        let filter = Filter::parse(Some(" Order.Created , kot.prepared ")).unwrap();
        assert!(filter.accepts(EventKind::OrderCreated));
        assert!(filter.accepts(EventKind::KotPrepared));
        assert!(!filter.accepts(EventKind::InvoicePaid));

        assert_eq!(Filter::parse(None).unwrap(), Filter::all());
        assert_eq!(
            Filter::parse(Some(" , ,")).unwrap(),
            Filter::all(),
            "an empty filter means unfiltered, not match-nothing"
        );
        assert!(Filter::parse(Some("kot.generated,bogus")).is_err());

        // Duplicates collapse.
        let deduped = Filter::parse(Some("kot.generated,kot.generated")).unwrap();
        assert_eq!(deduped.kinds().unwrap().len(), 1);
    }

    #[test]
    fn resume_from_prefers_the_header_over_the_query() {
        let mut headers = HeaderMap::new();
        headers.insert(LAST_EVENT_ID, "42".parse().unwrap());
        let query = StreamQuery {
            events: None,
            last_event_id: Some(7),
        };
        assert_eq!(resume_from(&headers, &query), 42);
        assert_eq!(resume_from(&HeaderMap::new(), &query), 7);
        assert_eq!(resume_from(&HeaderMap::new(), &StreamQuery::default()), 0);

        let mut blank = HeaderMap::new();
        blank.insert(LAST_EVENT_ID, "   ".parse().unwrap());
        assert_eq!(
            resume_from(&blank, &StreamQuery::default()),
            0,
            "a blank header must not be treated as an id"
        );
    }
}
