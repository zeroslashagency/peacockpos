//! Menu resolution endpoints.
//!
//! - `GET /api/menu?room=X&order_type=Y` — resolve the menu (3 strategies)
//! - `GET /api/menu/:menu_id/items` — items for a menu already known
//!
//! Both require the `X-Restaurant` header. See [`crate::middleware::context`] for why the
//! restaurant is a header rather than a path segment, and for the fact that it is
//! spoofable while this API has no authentication.
//!
//! # The fallback chain is the domain's, not this file's
//!
//! Menu selection has three strategies with precedence (api.py:19–73), and it is split
//! across three layers on purpose. Nothing here re-decides any of it:
//!
//! * **This handler** turns query parameters into one [`MenuStrategy`]. That is the whole
//!   of its judgement: `room` wins over `order_type` when both are sent, and neither means
//!   [`MenuStrategy::Default`].
//! * **[`peacock_core::menu::resolve_menu`]** owns the fallback: a strategy whose lookup
//!   returns `None` falls through to `default_menu`, and a `None` there is
//!   `Error::NoActiveMenu` (`menu.rs:204-224`). It also owns the sort — course sequence
//!   ascending, unsequenced courses last, then item name.
//! * **`PgMenuResolutionRepo`** owns the `room_wise_menu` / `order_type_wise_menu` flags,
//!   enforced in SQL. With a flag off it returns `None` even when a mapping row exists,
//!   which sends `resolve_menu` down that same documented fallback
//!   (`peacock-storage/src/repos/menu.rs`).
//!
//! So a request for a room in a restaurant with `room_wise_menu` off gets the default
//! menu, and it gets there through the fallback rather than through a branch in this file.
//! The handler reports which strategy actually won (`strategy` / `fell_back` on the
//! response) by asking the repository what it resolved, not by predicting it.
//!
//! # Prices
//!
//! Item rates on these responses come from `menu_items.rate` — `ury_menu_item.rate`
//! upstream (api.py:79, 87), the authoritative selling price. **Not** ERPNext `Item Price`,
//! which is the buying list used for COGS (`ury_daily_p_and_l.py:30`), and not
//! `Item.standard_rate`. The repository query is the only place that reads a rate, and it
//! reads it from the menu child table; this handler never touches
//! [`crate::state::AppState::price_repo`], which is the COGS-side lookup.
//!
//! # Blocking
//!
//! `MenuResolutionRepo` is synchronous (the domain's port traits are, `ports.rs:7-9`), so
//! `resolve_menu` reaches Postgres through `peacock-storage`'s `block_on` bridge, which
//! parks a worker thread for the duration. That is legal here because the API runs on a
//! multi-threaded runtime; the bridge panics with an actionable message on a
//! current-thread one, so the tests below are `flavor = "multi_thread"`. The alternative —
//! duplicating the resolution and sort in async code — is a second implementation of the
//! precedence rule, which is the thing this module's split exists to avoid. The budget is
//! 3–4 queries per request regardless of item count (`menu.rs::query_budget_is_bounded`).

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;

use crate::dto::menu::{
    MenuItemResponse, MenuItemsResponse, MenuResponse, MenuStrategyResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::middleware::context::RestaurantContext;
use crate::state::AppState;
use peacock_core::ids::{MenuName, RoomName};
use peacock_core::menu::{MenuResolutionRepo, MenuStrategy};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/menu", get(resolve_menu))
        .route("/api/menu/:menu_id/items", get(get_menu_items))
}

/// Query parameters for menu resolution.
#[derive(Debug, Deserialize)]
struct MenuQuery {
    /// Room name, for the room-wise strategy.
    room: Option<String>,
    /// Order type, for the order-type-wise strategy.
    order_type: Option<String>,
}

impl MenuQuery {
    /// The strategy these parameters ask for.
    ///
    /// `room` beats `order_type` when both are present, matching upstream's order of
    /// checks (api.py:36 before :50) and the domain's documented precedence
    /// (`peacock-core/src/menu.rs:190-193`). Blank values are treated as absent: a POS
    /// sending `?room=` has no room, and passing `""` down would look up a room named
    /// empty string and fall back, which is the right answer reached by accident.
    fn strategy(&self) -> MenuStrategy {
        match (nonblank(&self.room), nonblank(&self.order_type)) {
            (Some(room), _) => MenuStrategy::Room(RoomName::new(room)),
            (None, Some(order_type)) => MenuStrategy::OrderType(order_type.to_owned()),
            (None, None) => MenuStrategy::Default,
        }
    }
}

fn nonblank(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

/// `GET /api/menu?room=X&order_type=Y`
///
/// Header: `X-Restaurant: <restaurant>` (required).
///
/// Resolves the menu using the three-strategy precedence and returns its items ordered by
/// course sequence, then item name.
///
/// * `400` — no `X-Restaurant` header, or a blank/oversized one.
/// * `404` — the restaurant does not exist, or no menu could be resolved for it
///   (`Error::NoActiveMenu`, which is api.py:71–72).
async fn resolve_menu(
    State(state): State<AppState>,
    restaurant: RestaurantContext,
    Query(query): Query<MenuQuery>,
) -> ApiResult<Json<MenuResponse>> {
    let repo = state.menu_resolution_repo(restaurant.name().clone());
    let strategy = query.strategy();

    // Which strategy actually applies, asked before resolving rather than inferred after.
    // `resolve_menu` falls back silently, so comparing its answer against `active_menu`
    // could not tell "the room maps to the default menu" from "the room mapped to
    // nothing"; those are the same menu name and different operator situations.
    let (reported, fell_back) = classify_strategy(&repo, &strategy)?;

    // The fallback chain, the NoActiveMenu decision and the course sort all live here.
    let resolved = peacock_core::menu::resolve_menu(strategy, Utc::now(), &repo).map_err(|e| {
        match e {
            // 404, not 500: nothing is broken, an operator has not set `active_menu` (or
            // any mapping reaching it) on this restaurant.
            peacock_core::error::Error::NoActiveMenu => ApiError::not_found(format!(
                "no menu is configured for restaurant {}",
                restaurant.name()
            )),
            other => ApiError::from(other),
        }
    })?;

    Ok(Json(MenuResponse::from_resolved(
        restaurant.name(),
        resolved,
        reported,
        fell_back,
    )))
}

/// Which strategy will win, and whether that means falling back.
///
/// One extra query in the room / order-type cases, none for `Default`. `resolve_menu`
/// repeats the same lookup immediately afterwards, which is a deliberate trade: the
/// alternative is threading a "how did you get here" out of the domain function, and
/// `resolve_menu`'s signature is shared with a unit-test suite that asserts on its exact
/// query budget. One extra indexed single-row read against `menu_for_room` is cheaper than
/// making the domain report its own provenance.
fn classify_strategy(
    repo: &dyn MenuResolutionRepo,
    strategy: &MenuStrategy,
) -> ApiResult<(MenuStrategyResponse, bool)> {
    Ok(match strategy {
        // `None` here covers both "no mapping row" and "`room_wise_menu` is off", because
        // the repository enforces the flag in SQL. Both mean the default menu is what the
        // caller is about to get, which is what `fell_back` says.
        MenuStrategy::Room(room) => match repo.menu_for_room(room)? {
            Some(_) => (MenuStrategyResponse::Room, false),
            None => (MenuStrategyResponse::Default, true),
        },
        MenuStrategy::OrderType(order_type) => match repo.menu_for_order_type(order_type)? {
            Some(_) => (MenuStrategyResponse::OrderType, false),
            None => (MenuStrategyResponse::Default, true),
        },
        // Asked for the default and got the default: not a fallback.
        MenuStrategy::Default => (MenuStrategyResponse::Default, false),
    })
}

/// `GET /api/menu/:menu_id/items`
///
/// Header: `X-Restaurant: <restaurant>` (required).
///
/// Items for a menu the caller already knows, with course ordering applied. Used by a POS
/// that has a menu name in hand and does not want to re-run resolution.
///
/// The restaurant is still required even though `menu_items` is keyed by menu alone: the
/// repository is restaurant-scoped, and an endpoint that would happily list another
/// branch's menu given its name is a cross-branch read waiting to be found. The menu is
/// checked to belong to the scoped restaurant's branch below.
///
/// * `404` — the restaurant does not exist, or the menu does not exist within it.
async fn get_menu_items(
    State(state): State<AppState>,
    restaurant: RestaurantContext,
    Path(menu_id): Path<String>,
) -> ApiResult<Json<MenuItemsResponse>> {
    let menu = MenuName::new(menu_id);

    let repo = state.menu_resolution_repo(restaurant.name().clone());

    // Scope check first. `menu_items` has no restaurant column — it is a child of `menus`,
    // which carries `branch` — so without this a caller could read any branch's menu by
    // naming it, and the `X-Restaurant` header would be decoration.
    let belongs = repo
        .menu_belongs_to_scope_async(&menu)
        .await
        .map_err(|e| ApiError::internal(format!("could not check menu {menu}: {e}")))?;
    if belongs != Some(true) {
        // One message for "no such menu" and "not your menu", deliberately: telling an
        // unauthenticated caller which menus exist elsewhere is free enumeration, and with
        // no auth layer there is nothing else stopping them asking.
        return Err(ApiError::not_found(format!(
            "menu {menu} does not exist for restaurant {}",
            restaurant.name()
        )));
    }

    // Two queries, then the domain's ordering. `menu_items` leaves `course_sequence`
    // unset by design and `course_sequences` supplies it, exactly as `resolve_menu` does
    // — the sort is shared rather than copied so this endpoint and `/api/menu` cannot
    // order the same menu differently.
    let items = repo
        .menu_items_async(&menu)
        .await
        .map_err(|e| ApiError::internal(format!("could not read items for menu {menu}: {e}")))?;
    let sequences = repo.course_sequences_async().await.map_err(|e| {
        ApiError::internal(format!("could not read course sequences: {e}"))
    })?;

    let ordered = peacock_core::menu::order_menu_items(items, &sequences);

    Ok(Json(MenuItemsResponse {
        restaurant: restaurant.name().as_str().to_owned(),
        menu: menu.as_str().to_owned(),
        items: ordered.into_iter().map(MenuItemResponse::from_resolved).collect(),
    }))
}

#[cfg(test)]
mod tests {
    //! Every test here drives the real handlers against a real Postgres.
    //!
    //! `#[tokio::test(flavor = "multi_thread")]` throughout: `resolve_menu` reaches the
    //! database through `peacock-storage`'s blocking bridge, which panics by design on a
    //! current-thread runtime rather than silently timing out
    //! (`peacock-storage/src/repos/blocking.rs`).
    //!
    //! Tests skip with a printed note when no server is reachable, matching
    //! `tests/invoice_kot_postgres.rs`: a bare checkout must still be able to run
    //! `cargo test`.

    // No `use super::*`: these tests drive the handlers over HTTP through the assembled
    // app, exactly as a client does, so nothing from the parent module is referenced
    // directly. Reaching into it would let a test pass against a handler the router does
    // not actually expose.
    use crate::config::Config;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::routes::menu_test_support::{MenuFixture, RESTAURANT, ROOM};

    /// A request with the restaurant header set.
    ///
    /// The restaurant goes in a *header*, where a space is legal, so `RESTAURANT` needs no
    /// encoding. Query values do: `ROOM` is "Main Hall", and a raw space in a request
    /// target is an invalid URI.
    fn scoped(uri: &str, restaurant: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header("x-restaurant", restaurant)
            .body(Body::empty())
            .unwrap()
    }

    /// Percent-encode a query value. Only spaces need it for the fixture's names.
    fn q(value: &str) -> String {
        value.replace(' ', "%20")
    }

    async fn send(fixture: &MenuFixture, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let app = crate::app::build_with_state(
            crate::state::AppState::with_storage(Config::default(), fixture.storage().clone()),
        );
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    // -- The restaurant context ------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_restaurant_header_is_400_not_a_silent_default() {
        // The whole point of the extractor: with one restaurant configured it would be
        // trivially "obvious" which one was meant, and guessing is what must not happen.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = send(
            &fixture,
            Request::builder().uri("/api/menu").body(Body::empty()).unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
        assert!(
            body["detail"].as_str().unwrap().contains("X-Restaurant"),
            "the 400 must name the header the caller is missing: {body}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_blank_restaurant_header_is_400() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, _) = send(&fixture, scoped("/api/menu", "   ")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_restaurant_is_404() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = send(&fixture, scoped("/api/menu", "No Such Restaurant")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
        assert!(
            body["detail"]
                .as_str()
                .unwrap()
                .contains("No Such Restaurant"),
            "the 404 must name what was not found: {body}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_soft_deleted_restaurant_is_404() {
        // A closed location must stop serving menus, not keep serving the last one it had.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        sqlx::query("UPDATE restaurants SET deleted_at = now() WHERE name = $1")
            .bind(RESTAURANT)
            .execute(fixture.pool())
            .await
            .expect("soft-delete the restaurant");

        let (status, _) = send(&fixture, scoped("/api/menu", RESTAURANT)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // -- Strategy 3: default ---------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn no_parameters_resolves_the_active_menu() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = send(&fixture, scoped("/api/menu", RESTAURANT)).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["menu"], MenuFixture::DEFAULT_MENU);
        assert_eq!(body["restaurant"], RESTAURANT);
        assert_eq!(body["strategy"], "default");
        assert_eq!(body["fell_back"], false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_restaurant_with_no_active_menu_is_404() {
        // api.py:71-72 throws here. `Error::NoActiveMenu` maps to 404, not 500: the
        // condition is unconfigured data, not a fault.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        sqlx::query("UPDATE restaurants SET active_menu = NULL WHERE name = $1")
            .bind(RESTAURANT)
            .execute(fixture.pool())
            .await
            .expect("clear active_menu");

        let (status, body) = send(&fixture, scoped("/api/menu", RESTAURANT)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
        assert!(body["detail"].as_str().unwrap().contains("no menu"));
    }

    // -- Prices --------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn rates_come_from_the_menu_child_table_not_item_price() {
        // The bug this guards has been made once in this project. The fixture seeds
        // deliberately different numbers for the same item: 250.00 on the menu, 99.00 on
        // the buying price list. A response showing 99.00 means a handler reached for
        // `Item Price` — the COGS basis — and would bill the guest at cost.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = send(&fixture, scoped("/api/menu", RESTAURANT)).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");

        let biryani = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["item_code"] == "BIRYANI")
            .expect("BIRYANI on the default menu");

        assert_eq!(
            biryani["rate"], "250.000000",
            "rate must be menu_items.rate (250), never item_prices.rate (99)"
        );
        assert_ne!(biryani["rate"], "99.000000");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn money_is_a_string_on_the_wire() {
        // JS `Number()` on a rate loses paisa. GROUND-TRUTH.md is explicit that money
        // crosses the wire as a string.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (_, body) = send(&fixture, scoped("/api/menu", RESTAURANT)).await;
        for item in body["items"].as_array().unwrap() {
            assert!(
                item["rate"].is_string(),
                "rate must serialise as a string, got {}",
                item["rate"]
            );
        }
    }

    // -- Strategy 1: room-wise, and its flag ---------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn room_wise_applies_when_the_flag_is_on_and_a_mapping_exists() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        fixture.enable_room_wise().await;

        let (status, body) =
            send(&fixture, scoped(&format!("/api/menu?room={}", q(ROOM)), RESTAURANT)).await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["menu"], MenuFixture::ROOM_MENU);
        assert_eq!(body["strategy"], "room");
        assert_eq!(body["fell_back"], false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn room_wise_falls_back_when_the_flag_is_off_even_with_a_mapping() {
        // api.py:36-46: with `room_wise_menu` off the mapping is never consulted. The
        // mapping row exists in the fixture, so a response naming the room menu would mean
        // the flag was ignored — enforced in SQL, honoured here through the fallback.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        // Flag left off, mapping present.

        let (status, body) =
            send(&fixture, scoped(&format!("/api/menu?room={}", q(ROOM)), RESTAURANT)).await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(
            body["menu"], MenuFixture::DEFAULT_MENU,
            "room_wise_menu is off, so active_menu must win"
        );
        assert_eq!(body["strategy"], "default");
        assert_eq!(body["fell_back"], true);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unmapped_room_falls_back_to_the_default_menu() {
        // api.py:46. Flag on, no mapping for this room.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        fixture.enable_room_wise().await;

        let (status, body) = send(
            &fixture,
            scoped("/api/menu?room=Terrace", RESTAURANT),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["menu"], MenuFixture::DEFAULT_MENU);
        assert_eq!(body["fell_back"], true);
    }

    // -- Strategy 2: order-type-wise -----------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn order_type_wise_applies_when_the_flag_is_on() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        fixture.enable_order_type_wise().await;

        let (status, body) =
            send(&fixture, scoped("/api/menu?order_type=Delivery", RESTAURANT)).await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["menu"], MenuFixture::DELIVERY_MENU);
        assert_eq!(body["strategy"], "order_type");
        assert_eq!(body["fell_back"], false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn order_type_wise_falls_back_when_the_flag_is_off() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) =
            send(&fixture, scoped("/api/menu?order_type=Delivery", RESTAURANT)).await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["menu"], MenuFixture::DEFAULT_MENU);
        assert_eq!(body["fell_back"], true);
    }

    // -- Precedence between the two ------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn room_takes_precedence_over_order_type() {
        // Both flags on, both mappings present, both parameters sent. api.py checks the
        // room first (:36 before :50) and so does the handler.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        fixture.enable_room_wise().await;
        fixture.enable_order_type_wise().await;

        let (status, body) = send(
            &fixture,
            scoped(
                &format!("/api/menu?room={}&order_type=Delivery", q(ROOM)),
                RESTAURANT,
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["menu"], MenuFixture::ROOM_MENU);
        assert_eq!(body["strategy"], "room");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn blank_parameters_are_treated_as_absent() {
        // `?room=` from a POS with an empty selection must not look up a room named "".
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        fixture.enable_room_wise().await;

        let (status, body) = send(&fixture, scoped("/api/menu?room=&order_type=", RESTAURANT)).await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["menu"], MenuFixture::DEFAULT_MENU);
        assert_eq!(body["strategy"], "default");
        assert_eq!(
            body["fell_back"], false,
            "no strategy was asked for, so nothing fell back"
        );
    }

    // -- Ordering ------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn items_are_ordered_by_course_sequence_then_name() {
        // The fixture's default menu seeds, in insert order: TEA (Beverages, idx 2),
        // BIRYANI (Mains, idx 1), DOSA (Mains, idx 1), STICKER (no course).
        // Expected out: Mains by name (BIRYANI, DOSA), then Beverages (TEA), then the
        // uncoursed STICKER last.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = send(&fixture, scoped("/api/menu", RESTAURANT)).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");

        let codes: Vec<&str> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["item_code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, vec!["BIRYANI", "DOSA", "TEA", "STICKER"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_disabled_menu_row_is_not_served() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        sqlx::query("UPDATE menu_items SET disabled = TRUE WHERE menu = $1 AND item = 'DOSA'")
            .bind(MenuFixture::DEFAULT_MENU)
            .execute(fixture.pool())
            .await
            .expect("disable a menu row");

        let (_, body) = send(&fixture, scoped("/api/menu", RESTAURANT)).await;
        let codes: Vec<&str> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["item_code"].as_str().unwrap())
            .collect();
        assert!(!codes.contains(&"DOSA"), "a disabled row must not be sold: {codes:?}");
    }

    // -- GET /api/menu/:menu_id/items ----------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn menu_items_returns_the_named_menu() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = send(
            &fixture,
            scoped(
                &format!("/api/menu/{}/items", MenuFixture::DEFAULT_MENU),
                RESTAURANT,
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["menu"], MenuFixture::DEFAULT_MENU);
        assert_eq!(body["restaurant"], RESTAURANT);
        assert_eq!(body["items"].as_array().unwrap().len(), 4);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn menu_items_orders_identically_to_resolution() {
        // Two endpoints, one sort. A divergence here means the ordering was copied rather
        // than shared.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (_, resolved) = send(&fixture, scoped("/api/menu", RESTAURANT)).await;
        let (_, listed) = send(
            &fixture,
            scoped(
                &format!("/api/menu/{}/items", MenuFixture::DEFAULT_MENU),
                RESTAURANT,
            ),
        )
        .await;

        assert_eq!(resolved["items"], listed["items"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_menu_is_404() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, _) = send(&fixture, scoped("/api/menu/Menu-Nope/items", RESTAURANT)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_menu_from_another_branch_is_404() {
        // The cross-branch read the scope check exists to close. Without it, naming
        // another branch's menu would list its items and its prices.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        fixture.seed_foreign_menu().await;

        let (status, body) = send(
            &fixture,
            scoped(
                &format!("/api/menu/{}/items", MenuFixture::FOREIGN_MENU),
                RESTAURANT,
            ),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "another branch's menu must not be readable: {body}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn menu_items_requires_the_restaurant_header_too() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, _) = send(
            &fixture,
            Request::builder()
                .uri(format!("/api/menu/{}/items", MenuFixture::DEFAULT_MENU))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_url_encoded_menu_name_round_trips() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        fixture.seed_spaced_menu().await;

        let (status, body) = send(
            &fixture,
            scoped("/api/menu/Menu%20With%20Spaces/items", RESTAURANT),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["menu"], "Menu With Spaces");
    }

    // -- Error shape ---------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn errors_are_problem_json_with_an_instance_and_request_id() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let app = crate::app::build_with_state(crate::state::AppState::with_storage(
            Config::default(),
            fixture.storage().clone(),
        ));
        let response = app
            .oneshot(Request::builder().uri("/api/menu").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/problem+json"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], 400);
        assert_eq!(json["instance"], "/api/menu");
        assert!(json.get("request_id").is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn routes_are_registered() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        for uri in ["/api/menu", "/api/menu/Menu-Main/items"] {
            let (status, _) = send(&fixture, scoped(uri, RESTAURANT)).await;
            assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{uri} must accept GET");
        }
    }
}
