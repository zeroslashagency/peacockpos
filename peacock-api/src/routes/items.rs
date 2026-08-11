//! Item master and price lookup.
//!
//! - `GET /api/items/:item_code` — the item master row
//! - `GET /api/items/:item_code/price?pricelist=X` — one price on one named price list
//!
//! # Two endpoints because there are two different questions
//!
//! An item does not have "a price", and the split here is the whole point of the module.
//!
//! * **Selling price** lives on `menu_items.rate` (`ury_menu_item.rate`, api.py:79, 87).
//!   It is per *menu*, so an item on three menus has three selling prices and no
//!   single-item endpoint can name one. It is served by `GET /api/menu`, which has a menu.
//! * **Buying price** lives in `item_prices` against a price list. On a *buying* list it is
//!   the COGS basis (`ury_daily_p_and_l.py:30`); on a *selling* list it is what the
//!   aggregator path reads (api.py:829). Which one you get depends entirely on the list you
//!   name, so `pricelist` is part of the request and is echoed in the response.
//!
//! Consequently [`ItemDetailsResponse`] has **no rate field at all**. Anything else would
//! be a number a caller could mistake for the price to bill, and picking cost or list price
//! for it is a bug that was already caught once in this project.
//!
//! # No restaurant scope
//!
//! Unlike `menu.rs`, neither endpoint takes `X-Restaurant`. `items` and `item_prices` are
//! ERPNext-owned masters with no restaurant column (001_core_tables.sql) — they are shared
//! across branches by design, and requiring a scope that does not narrow the query would be
//! theatre. What *is* restaurant-scoped is which items appear on a menu, and that is
//! `menu.rs`.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use crate::dto::menu::{ItemDetailsResponse, ItemPriceResponse};
use crate::error::{ApiError, ApiResult};
use crate::middleware::context::require_storage;
use crate::state::AppState;
use peacock_core::ids::{ItemCode, PriceListName};
use peacock_storage::repos::PgItemDetailsRepo;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/items/:item_code", get(get_item_details))
        .route("/api/items/:item_code/price", get(get_item_price))
}

/// The default price list for `GET /api/items/:code/price`.
///
/// `Standard Selling` is ERPNext's stock selling list, and it is the safer default of the
/// two directions: a caller who forgets `pricelist` and is shown a *selling* rate sees a
/// number in the right ballpark, whereas defaulting to a buying list would quietly show
/// cost. It is still only a default — a caller that means COGS must name the buying list,
/// and `reports.rs` / `cogs.rs` do.
const DEFAULT_PRICE_LIST: &str = "Standard Selling";

/// `GET /api/items/:item_code`
///
/// The item master row: name, group, stock UOM, whether a BOM exists, whether it is
/// disabled. No price — see the module docs.
///
/// * `400` — a blank item code.
/// * `404` — no such item, or it is soft-deleted.
async fn get_item_details(
    State(state): State<AppState>,
    Path(item_code): Path<String>,
) -> ApiResult<Json<ItemDetailsResponse>> {
    let storage = require_storage(&state);
    let item = parse_item_code(&item_code)?;

    let details = PgItemDetailsRepo::new(storage.pool().clone())
        .find_async(&item)
        .await
        .map_err(|e| ApiError::internal(format!("could not read item {item}: {e}")))?
        .ok_or_else(|| ApiError::not_found(format!("item {item} does not exist")))?;

    Ok(Json(ItemDetailsResponse::from_details(details)))
}

/// Query parameters for price lookup.
#[derive(Debug, Deserialize)]
struct PriceQuery {
    /// Price list name. Defaults to [`DEFAULT_PRICE_LIST`].
    pricelist: Option<String>,
}

/// `GET /api/items/:item_code/price?pricelist=X`
///
/// The rate for one item on one price list, as of the **database server's** date — a POS
/// terminal with a skewed clock cannot select yesterday's price, because the repository
/// reads `CURRENT_DATE` server-side rather than trusting the process clock.
///
/// * `400` — a blank item code or a blank `pricelist`.
/// * `404` — the item does not exist, or no price applies to it on that list today.
///
/// The two 404s are separate messages on purpose. "No price configured" on an item that
/// does not exist sends an operator looking in the price list for a row that could never be
/// there, and a missing price is a normal, expected state: upstream's COGS walk accumulates
/// `unset_bom_items` rather than failing (GROUND-TRUTH.md §"BOM / COGS walk"), which is why
/// the repository returns `Option` and never `Money::ZERO` — zero is a real price meaning
/// "free", and collapsing the two would value an unpriced ingredient at nothing.
async fn get_item_price(
    State(state): State<AppState>,
    Path(item_code): Path<String>,
    Query(query): Query<PriceQuery>,
) -> ApiResult<Json<ItemPriceResponse>> {
    let storage = require_storage(&state);
    let item = parse_item_code(&item_code)?;

    let pricelist_name = match query.pricelist.as_deref().map(str::trim) {
        // An explicitly blank `?pricelist=` is a caller bug, not a request for the
        // default: silently substituting one would answer a question that was not asked,
        // with a rate from a list the caller never named.
        Some("") => {
            return Err(ApiError::invalid_input(
                "pricelist must not be blank; omit the parameter to use the default",
            ))
        }
        Some(name) => name.to_owned(),
        None => DEFAULT_PRICE_LIST.to_owned(),
    };
    let pricelist = PriceListName::new(pricelist_name.clone());

    // Existence first, so "no such item" cannot masquerade as "no price for this item".
    let items = PgItemDetailsRepo::new(storage.pool().clone());
    if items
        .find_async(&item)
        .await
        .map_err(|e| ApiError::internal(format!("could not read item {item}: {e}")))?
        .is_none()
    {
        return Err(ApiError::not_found(format!("item {item} does not exist")));
    }

    let price = state
        .price_repo()
        .item_price_async(&item, &pricelist)
        .await
        .map_err(|e| {
            ApiError::internal(format!(
                "could not read the price for {item} on {pricelist}: {e}"
            ))
        })?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "no price is configured for item {item} on price list {pricelist}"
            ))
        })?;

    Ok(Json(ItemPriceResponse {
        item_code: item.as_str().to_owned(),
        pricelist: pricelist_name,
        price: price.0,
    }))
}

/// Reject a blank code before it reaches a query.
///
/// `items.code` has a `length(btrim(code)) > 0` CHECK (001_core_tables.sql), so a blank
/// can never match a row — but it would return the same 404 as a real miss, and a 400 says
/// what actually went wrong. Axum will not route a truly empty segment here; a
/// whitespace-only one it will.
fn parse_item_code(raw: &str) -> ApiResult<ItemCode> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::invalid_input("item code must not be blank"));
    }
    Ok(ItemCode::new(trimmed))
}

#[cfg(test)]
mod tests {
    //! Real Postgres, one throwaway database per test. Tests skip with a printed note when
    //! no server is reachable, matching `tests/invoice_kot_postgres.rs`.

    use super::*;
    use crate::config::Config;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::routes::menu_test_support::MenuFixture;

    async fn get(fixture: &MenuFixture, uri: &str) -> (StatusCode, serde_json::Value) {
        let app = crate::app::build_with_state(crate::state::AppState::with_storage(
            Config::default(),
            fixture.storage().clone(),
        ));
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    // -- Item details ---------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn item_details_returns_the_master_row() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = get(&fixture, "/api/items/BIRYANI").await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["item_code"], "BIRYANI");
        assert_eq!(body["item_name"], "Chicken Biryani");
        assert_eq!(body["item_group"], "Main Course");
        assert_eq!(body["stock_uom"], "Nos");
        assert_eq!(body["disabled"], false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn item_details_never_carries_a_price() {
        // The regression guard. BIRYANI has a menu rate of 250 and a buying price of 99;
        // neither belongs on this response, under any field name.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (_, body) = get(&fixture, "/api/items/BIRYANI").await;
        for absent in [
            "rate",
            "price",
            "standard_rate",
            "price_list_rate",
            "selling_rate",
        ] {
            assert!(
                body.get(absent).is_none(),
                "{absent} must not appear on item details: {body}"
            );
        }
        // And no field anywhere holds either number.
        let raw = body.to_string();
        assert!(!raw.contains("250"), "the menu rate leaked into item details: {raw}");
        assert!(!raw.contains("99"), "the buying price leaked into item details: {raw}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_item_is_404() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = get(&fixture, "/api/items/NO-SUCH-ITEM").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
        assert!(body["detail"].as_str().unwrap().contains("NO-SUCH-ITEM"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_soft_deleted_item_is_404() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        sqlx::query("UPDATE items SET deleted_at = now() WHERE code = 'STICKER'")
            .execute(fixture.pool())
            .await
            .expect("soft-delete an item");

        let (status, _) = get(&fixture, "/api/items/STICKER").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_disabled_item_is_still_returned_with_the_flag_set() {
        // Withdrawn from sale is not deleted: a detail view of a historical order line has
        // to render. Routing is where `disabled` stops an item, not here.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        sqlx::query("UPDATE items SET disabled = TRUE WHERE code = 'STICKER'")
            .execute(fixture.pool())
            .await
            .expect("disable an item");

        let (status, body) = get(&fixture, "/api/items/STICKER").await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["disabled"], true);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_item_with_no_group_omits_the_field() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        sqlx::query("UPDATE items SET item_group = NULL WHERE code = 'STICKER'")
            .execute(fixture.pool())
            .await
            .expect("clear item_group");

        let (status, body) = get(&fixture, "/api/items/STICKER").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.get("item_group").is_none(), "body: {body}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_url_encoded_item_code_round_trips() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        sqlx::query("INSERT INTO items (code, name, item_group) VALUES ($1, $2, $3)")
            .bind("ITEM WITH SPACES")
            .bind("Spaced Item")
            .bind("Merchandise")
            .execute(fixture.pool())
            .await
            .expect("seed a spaced item code");

        let (status, body) = get(&fixture, "/api/items/ITEM%20WITH%20SPACES").await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["item_code"], "ITEM WITH SPACES");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_blank_item_code_is_400() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, _) = get(&fixture, "/api/items/%20%20").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // -- Price lookup ---------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn price_returns_the_named_list_rate() {
        // The buying list, explicitly named. 99.00 is the COGS basis the fixture seeds.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = get(
            &fixture,
            &format!(
                "/api/items/BIRYANI/price?pricelist={}",
                urlencode(MenuFixture::BUYING_LIST)
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["item_code"], "BIRYANI");
        assert_eq!(body["pricelist"], MenuFixture::BUYING_LIST);
        assert_eq!(body["price"], "99.000000");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_price_endpoint_and_the_menu_disagree_on_purpose() {
        // Same item, two numbers, both correct: 250 is what the guest pays (menu rate), 99
        // is what it cost (buying list). A change that made these agree would mean one of
        // the two lookups had been pointed at the wrong table.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (_, price) = get(
            &fixture,
            &format!(
                "/api/items/BIRYANI/price?pricelist={}",
                urlencode(MenuFixture::BUYING_LIST)
            ),
        )
        .await;
        assert_eq!(price["price"], "99.000000");

        let app = crate::app::build_with_state(crate::state::AppState::with_storage(
            Config::default(),
            fixture.storage().clone(),
        ));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/menu")
                    .header("x-restaurant", crate::routes::menu_test_support::RESTAURANT)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let menu: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let biryani = menu["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["item_code"] == "BIRYANI")
            .expect("BIRYANI on the menu");

        assert_eq!(biryani["rate"], "250.000000");
        assert_ne!(
            biryani["rate"], price["price"],
            "the selling rate and the buying price must not be the same lookup"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn price_defaults_to_standard_selling_when_the_parameter_is_omitted() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = get(&fixture, "/api/items/BIRYANI/price").await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["pricelist"], DEFAULT_PRICE_LIST);
        assert_eq!(body["price"], "260.000000");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unpriced_item_is_404_not_zero() {
        // Zero is a real price meaning "free". A missing row must never be reported as one.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = get(&fixture, "/api/items/STICKER/price").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
        assert!(body["detail"].as_str().unwrap().contains("no price"));
        assert!(
            !body.to_string().contains("\"price\""),
            "a 404 must not carry a price field: {body}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_item_price_says_the_item_is_missing_not_the_price() {
        // Two different operator actions: create the item, versus price the item.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = get(&fixture, "/api/items/NO-SUCH-ITEM/price").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
        let detail = body["detail"].as_str().unwrap();
        assert!(detail.contains("does not exist"), "detail: {detail}");
        assert!(!detail.contains("no price"), "detail: {detail}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_pricelist_is_404() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, _) = get(&fixture, "/api/items/BIRYANI/price?pricelist=Nonexistent").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_blank_pricelist_is_400_not_the_default() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = get(&fixture, "/api/items/BIRYANI/price?pricelist=%20").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_expired_price_row_does_not_apply() {
        // `valid_upto` in the past means the row no longer prices the item. The precedence
        // rules live in `PgPriceRepo`; this proves the handler is going through them.
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        sqlx::query(
            "UPDATE item_prices SET valid_upto = CURRENT_DATE - 1
             WHERE item_code = 'BIRYANI' AND price_list = $1",
        )
        .bind(MenuFixture::BUYING_LIST)
        .execute(fixture.pool())
        .await
        .expect("expire the buying price");

        let (status, _) = get(
            &fixture,
            &format!(
                "/api/items/BIRYANI/price?pricelist={}",
                urlencode(MenuFixture::BUYING_LIST)
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dated_override_beats_the_open_ended_base_rate() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };
        sqlx::query(
            "INSERT INTO item_prices (item_code, price_list, rate, valid_from)
             VALUES ('BIRYANI', $1, 111.00, CURRENT_DATE)",
        )
        .bind(MenuFixture::BUYING_LIST)
        .execute(fixture.pool())
        .await
        .expect("insert a dated override");

        let (status, body) = get(
            &fixture,
            &format!(
                "/api/items/BIRYANI/price?pricelist={}",
                urlencode(MenuFixture::BUYING_LIST)
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["price"], "111.000000");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn money_is_a_string_on_the_wire() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (_, body) = get(&fixture, "/api/items/BIRYANI/price").await;
        assert!(body["price"].is_string(), "price must be a string: {body}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn extra_query_parameters_are_ignored() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let (status, body) = get(&fixture, "/api/items/BIRYANI/price?extra=ignored").await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["pricelist"], DEFAULT_PRICE_LIST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn errors_are_problem_json() {
        let Some(fixture) = MenuFixture::try_new().await else {
            return;
        };

        let app = crate::app::build_with_state(crate::state::AppState::with_storage(
            Config::default(),
            fixture.storage().clone(),
        ));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/items/NOPE")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/problem+json"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], 404);
        assert_eq!(json["instance"], "/api/items/NOPE");
        assert!(json.get("type").is_some());
    }

    /// Percent-encode a price list name for a query string.
    ///
    /// Only spaces need it for the names in play; a full encoder would be a dependency for
    /// one character class.
    fn urlencode(value: &str) -> String {
        value.replace(' ', "%20")
    }
}
