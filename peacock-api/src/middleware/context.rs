//! Request context extractors — which restaurant is this request for?
//!
//! # The problem
//!
//! Menu resolution cannot start without a restaurant. Upstream never has to ask: every
//! caller arrives holding a POS Profile, and `getRestaurantMenu` derives the restaurant
//! from its branch (`restaurant = frappe.db.get_value("URY Restaurant", {"branch":
//! branch_name}, "name")`, api.py:33). Peacock has no session and no POS Profile on the
//! request yet, so both `menu.rs` handlers had nothing to scope themselves to and both
//! returned a placeholder error before this lane.
//!
//! # The decision: a required `X-Restaurant` header
//!
//! Three options were on the table.
//!
//! * **A path prefix, `/api/restaurants/:restaurant/menu`.** The most REST-shaped, and
//!   self-documenting in a log line. Rejected because the restaurant is not what these
//!   endpoints address — `GET /api/menu` returns *the* menu for the caller's own
//!   location, the way `getRestaurantMenu` does, and there is no meaningful
//!   cross-restaurant browse. Making it a path segment would also renumber the URL of
//!   every endpoint that later needs the scope (orders, tables, KOT, shifts — nearly all
//!   of them), and each of those has tests and a frontend contract to move with it.
//! * **A query parameter, `?restaurant=X`.** Cheapest to add and the easiest to get
//!   wrong: query strings are per-endpoint, so every handler needing the scope grows its
//!   own optional field, and "optional field the handler happens to require" is exactly
//!   the shape that decays into a silent default. It also puts the scope in the same
//!   namespace as `room` and `order_type`, which are genuinely per-request filters, so a
//!   reader cannot tell the ambient context from the query.
//! * **A required header, `X-Restaurant`** — chosen. The restaurant is ambient context
//!   for the whole session, not an argument to one call, and a header is where ambient
//!   context belongs; it is the same slot `Idempotency-Key` and `X-Request-ID` already
//!   occupy in this API. One extractor serves every future handler with no URL change
//!   and no per-endpoint field, which is the reusability constraint this lane was given.
//!   It is also the slot a real auth layer will overwrite: when a session exists, the
//!   restaurant comes from the session and the header stops being trusted, and that is a
//!   change to [`RestaurantContext::from_request_parts`] and nowhere else.
//!
//! # Security: this header is spoofable, and that is not fixed here
//!
//! **There is no authentication in this API.** Anyone who can reach the port can send any
//! `X-Restaurant` value and read that restaurant's menu and prices. Validating the value
//! against the `restaurants` table proves the restaurant *exists*; it proves nothing about
//! whether the caller is entitled to it. A path prefix or a query parameter would be
//! exactly as spoofable — the exposure is the missing auth layer, not the transport slot —
//! so this choice neither creates nor worsens it, but it must not be mistaken for a
//! control. The menu is low-value data (a price list a guest reads off a physical menu
//! anyway); the same header slot MUST NOT be allowed to scope anything that writes, or to
//! scope revenue reporting, until a session carries the restaurant instead. Wave 4-B owns
//! enumerating this.
//!
//! # Guarantees
//!
//! * **Explicit.** No default. There is no "first restaurant", no "the only one", no
//!   fallback to `default_room`'s owner. A single-restaurant deployment must send the
//!   header too, so the day a second restaurant is configured nothing silently
//!   re-points.
//! * **400 when missing** or when the value is blank or not header-safe text.
//! * **404 when the named restaurant does not exist** or has been soft-deleted.
//! * **Reusable.** Any handler that adds a `RestaurantContext` argument gets the same
//!   validation; there is no parsing to copy.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::HeaderName;
use peacock_core::ids::{BranchName, MenuName, RestaurantName, RoomName};
use peacock_storage::repos::{PgRestaurantRepo, RestaurantSummary};
use peacock_storage::Storage;

use crate::error::ApiError;
use crate::state::AppState;

/// The header carrying the restaurant scope.
///
/// `X-` prefixed, because this is a Peacock-specific extension with no registered
/// equivalent, and it is expected to be *removed* rather than standardised once a session
/// carries the scope.
pub const X_RESTAURANT: HeaderName = HeaderName::from_static("x-restaurant");

/// Upper bound on an accepted value.
///
/// `restaurants.name` is TEXT with no length cap (001_core_tables.sql), but a docname is
/// a human-typed label. The bound exists so an unbounded header cannot become an
/// unbounded bind parameter, the same reasoning as
/// [`crate::middleware::request_id`]'s cap.
const MAX_NAME_LEN: usize = 140;

/// A validated restaurant scope for one request.
///
/// Carries the row, not just the name: a handler that has gone to the trouble of
/// proving the restaurant exists should not have to read it again to learn its branch or
/// its `active_menu`. The two `*_wise_menu` flags are here for the same reason, and are
/// **reported, never enforced** — enforcement is in SQL inside
/// `PgMenuResolutionRepo`, which returns `None` to trigger the domain's fallback. A
/// handler branching on these flags itself would be a second implementation of the
/// precedence rule, free to disagree with the first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestaurantContext(RestaurantSummary);

impl RestaurantContext {
    pub fn name(&self) -> &RestaurantName {
        &self.0.name
    }

    pub fn branch(&self) -> &BranchName {
        &self.0.branch
    }

    /// `URY Restaurant.active_menu` — strategy 3's menu, `None` when unset.
    pub fn active_menu(&self) -> Option<&MenuName> {
        self.0.active_menu.as_ref()
    }

    pub fn default_room(&self) -> Option<&RoomName> {
        self.0.default_room.as_ref()
    }

    pub fn room_wise_menu(&self) -> bool {
        self.0.room_wise_menu
    }

    pub fn order_type_wise_menu(&self) -> bool {
        self.0.order_type_wise_menu
    }

    pub fn summary(&self) -> &RestaurantSummary {
        &self.0
    }
}

/// Accepts a value only if it is non-empty, bounded, and free of control characters.
///
/// Control characters are rejected rather than stripped: a name containing one is not a
/// name an operator configured, and quietly repairing it would let two different headers
/// address one restaurant. SQL injection is not the concern — the value is a bind
/// parameter everywhere it is used — log injection and unbounded input are.
fn sanitize(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let acceptable = !trimmed.is_empty()
        && trimmed.len() <= MAX_NAME_LEN
        && !trimmed.chars().any(|c| c.is_control());
    acceptable.then_some(trimmed)
}

// `#[async_trait]` because this crate is on axum 0.7, whose `FromRequestParts` still
// predates native async-in-trait. Dropping it on an axum 0.8 upgrade is mechanical.
#[async_trait::async_trait]
impl<S> FromRequestParts<S> for RestaurantContext
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let state = AppState::from_ref(state);

        let raw = parts
            .headers
            .get(X_RESTAURANT)
            .ok_or_else(|| {
                ApiError::invalid_input(
                    "the X-Restaurant header is required: it names the restaurant this \
                     request is for. There is no default.",
                )
            })?
            .to_str()
            .map_err(|_| {
                ApiError::invalid_input("the X-Restaurant header must be valid UTF-8 text")
            })?;

        let name = sanitize(raw).ok_or_else(|| {
            ApiError::invalid_input(format!(
                "the X-Restaurant header must be non-blank printable text of at most \
                 {MAX_NAME_LEN} characters"
            ))
        })?;
        let name = RestaurantName::new(name);

        let summary = PgRestaurantRepo::new(state.storage().pool().clone())
            .find_async(&name)
            .await
            .map_err(|e| {
                ApiError::internal(format!("could not read restaurant {name}: {e}"))
            })?
            // Existence is checked here, once, rather than left to the first query that
            // happens to join `restaurants`: an unknown restaurant resolves to no
            // room mapping and no active_menu, which would surface as
            // `NoActiveMenu` — a 404 saying "no menu is configured" when the truth is
            // "no such restaurant". Two different operator actions, so two different
            // messages.
            .ok_or_else(|| {
                ApiError::not_found(format!("restaurant {name} does not exist"))
            })?;

        Ok(RestaurantContext(summary))
    }
}

/// The pool, for handlers in this lane.
///
/// **Lane W1-A landed**: `AppState::storage` returns `&Storage`, not `Option<&Storage>`,
/// so there is no "storage might be missing" case left to handle and no `is_none()` check
/// in any handler here. This is now a one-line accessor kept only so the handlers read
/// uniformly and so a future change of shape has one place to happen again.
pub(crate) fn require_storage(state: &AppState) -> &Storage {
    state.storage()
}

use axum::extract::FromRef;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_or_control_bearing_value_is_rejected() {
        // Blank in every form an operator or a proxy might send.
        for hostile in ["", "   ", "\t", "\n"] {
            assert!(
                sanitize(hostile).is_none(),
                "{hostile:?} must not be accepted as a restaurant name"
            );
        }
        // A newline in a header value is a log-injection surface, and no docname
        // contains one.
        assert!(sanitize("Peacock\nGrand").is_none());
        assert!(sanitize("Peacock\u{0}Grand").is_none());
    }

    #[test]
    fn an_over_long_value_is_rejected() {
        let long = "a".repeat(MAX_NAME_LEN + 1);
        assert!(sanitize(&long).is_none());
        let at_limit = "a".repeat(MAX_NAME_LEN);
        assert_eq!(sanitize(&at_limit), Some(at_limit.as_str()));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_but_inner_spaces_survive() {
        // "Peacock Grand" is a realistic docname; trimming must not collapse it.
        assert_eq!(sanitize("  Peacock Grand  "), Some("Peacock Grand"));
    }

    #[test]
    fn the_header_name_is_the_documented_one() {
        // A rename here silently 400s every client, so pin it.
        assert_eq!(X_RESTAURANT.as_str(), "x-restaurant");
    }
}
