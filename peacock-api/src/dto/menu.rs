//! Menu and item DTOs for HTTP API.

use serde::{Deserialize, Serialize};

/// Which of the three strategies produced the menu on a [`MenuResponse`].
///
/// Reported because the fallback chain is invisible in the result otherwise: a POS
/// showing the default menu when the operator configured a room-wise one has no way to
/// tell that from a correctly-resolved default, and "why is this table showing the wrong
/// menu" is the support call that follows. `resolve_menu` falls back silently by design
/// (`peacock-core/src/menu.rs:205-221`), so the handler records which branch won.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MenuStrategyResponse {
    /// The room's `menu_for_room` mapping applied (api.py:40–46).
    Room,
    /// The order type's `order_type_menu` mapping applied (api.py:56–62).
    OrderType,
    /// `URY Restaurant.active_menu` — either asked for directly, or fallen back to
    /// because the requested strategy had no mapping or its `*_wise_menu` flag is off.
    Default,
}

/// Response for `GET /api/menu` — the resolved menu with its items.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MenuResponse {
    /// The restaurant the menu was resolved for — echoed back from `X-Restaurant` so a
    /// cached or proxied response can be told apart from another restaurant's.
    pub restaurant: String,
    /// The menu name that was resolved.
    pub menu: String,
    /// Which strategy won. See [`MenuStrategyResponse`].
    pub strategy: MenuStrategyResponse,
    /// True when the requested strategy did not apply and `active_menu` was used
    /// instead — either no mapping row exists, or the restaurant's corresponding
    /// `*_wise_menu` flag is off (enforced in SQL, `peacock-storage/src/repos/menu.rs`).
    pub fell_back: bool,
    /// Items in the menu, ordered by course sequence and then by item name.
    pub items: Vec<MenuItemResponse>,
}

impl MenuResponse {
    pub fn from_resolved(
        restaurant: &peacock_core::ids::RestaurantName,
        resolved: peacock_core::menu::ResolvedMenu,
        strategy: MenuStrategyResponse,
        fell_back: bool,
    ) -> Self {
        Self {
            restaurant: restaurant.as_str().to_owned(),
            menu: resolved.menu.as_str().to_owned(),
            strategy,
            fell_back,
            items: resolved
                .items
                .into_iter()
                .map(MenuItemResponse::from_resolved)
                .collect(),
        }
    }
}

/// A single item within a resolved menu.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MenuItemResponse {
    pub item_code: String,
    pub item_name: String,
    /// Selling price from `ury_menu_item.rate`.
    #[serde(with = "rust_decimal::serde::str")]
    pub rate: rust_decimal::Decimal,
    pub special_dish: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub course: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub course_sequence: Option<i32>,
}

impl MenuItemResponse {
    pub fn from_resolved(item: peacock_core::menu::ResolvedMenuItem) -> Self {
        Self {
            item_code: item.item.as_str().to_owned(),
            item_name: item.item_name,
            rate: item.rate.0,
            special_dish: item.special_dish,
            course: item.course.as_ref().map(|c| c.as_str().to_owned()),
            course_sequence: item.course_sequence,
        }
    }
}

/// Response for `GET /api/menu/:menu_id/items` — items with courses for a known menu.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MenuItemsResponse {
    pub restaurant: String,
    pub menu: String,
    pub items: Vec<MenuItemResponse>,
}

/// Response for `GET /api/items/:item_code` — single item details.
///
/// # No price field, deliberately
///
/// An earlier draft of this struct carried `standard_rate`. Three things were wrong with
/// it, and they are worth recording because the mistake is easy to repeat:
///
/// 1. **The column does not exist.** `items` holds only what Peacock reads — code, name,
///    item_group, stock_uom, is_bom, disabled (001_core_tables.sql). ERPNext's
///    `Item.standard_rate` was deliberately not carried over.
/// 2. **An item has no single price.** The selling price is `menu_items.rate`, which is
///    per *menu* (api.py:79, 87), so an item on three menus has three selling prices and
///    a `/api/items/:code` response cannot name one. `GET /api/menu` is where a price
///    belongs, because that call has a menu.
/// 3. **The other price is a different quantity entirely.** `item_prices` on a *buying*
///    list is the COGS basis (`ury_daily_p_and_l.py:30`). Serving it as "the item's
///    price" on a POS detail screen would show cost where the cashier expects the sale
///    price. That is served, explicitly and with the list named, by
///    `GET /api/items/:code/price?pricelist=X` → [`ItemPriceResponse`].
///
/// So this is the item master and nothing else. `description` is gone for the same
/// reason as `standard_rate`: there is no such column.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemDetailsResponse {
    pub item_code: String,
    pub item_name: String,
    /// Absent when the item has no group. The absence is meaningful: such an item routes
    /// to no kitchen station.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_group: Option<String>,
    pub stock_uom: String,
    /// Whether a default active BOM exists for this item. A hint for the COGS screen,
    /// not an authoritative flag (001_core_tables.sql).
    pub is_bom: bool,
    /// Withdrawn from sale. Still returned, so a detail view of a historical order line
    /// renders.
    pub disabled: bool,
}

impl ItemDetailsResponse {
    pub fn from_details(item: peacock_storage::repos::ItemDetails) -> Self {
        Self {
            item_code: item.code.as_str().to_owned(),
            item_name: item.name,
            item_group: item.item_group.map(|g| g.as_str().to_owned()),
            stock_uom: item.stock_uom,
            is_bom: item.is_bom,
            disabled: item.disabled,
        }
    }
}

/// Response for `GET /api/items/:item_code/price` — price lookup with pricelist.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemPriceResponse {
    pub item_code: String,
    pub pricelist: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub price: rust_decimal::Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use peacock_core::ids::{ItemCode, MenuCourseName, MenuName};
    use peacock_core::menu::{ResolvedMenu, ResolvedMenuItem};
    use peacock_core::money::Money;
    use rust_decimal_macros::dec;

    #[test]
    fn menu_response_serialization() {
        let resolved = ResolvedMenu {
            menu: MenuName::new("Menu-Main"),
            items: vec![
                ResolvedMenuItem {
                    item: ItemCode::new("ITEM-001"),
                    item_name: "Soup".to_owned(),
                    rate: Money::new(dec!(100.50)),
                    special_dish: false,
                    course: Some(MenuCourseName::new("Starters")),
                    course_sequence: Some(1),
                },
                ResolvedMenuItem {
                    item: ItemCode::new("ITEM-002"),
                    item_name: "Curry".to_owned(),
                    rate: Money::new(dec!(250.00)),
                    special_dish: true,
                    course: None,
                    course_sequence: None,
                },
            ],
        };

        let response = MenuResponse::from_resolved(
            &peacock_core::ids::RestaurantName::new("Peacock Grand"),
            resolved,
            MenuStrategyResponse::Room,
            false,
        );
        assert_eq!(response.restaurant, "Peacock Grand");
        assert_eq!(response.menu, "Menu-Main");
        assert_eq!(response.items.len(), 2);
        assert_eq!(response.items[0].item_code, "ITEM-001");
        assert_eq!(response.items[0].rate, dec!(100.50));
        assert_eq!(response.items[0].course, Some("Starters".to_owned()));
        assert!(response.items[1].special_dish);
        assert_eq!(response.items[1].course, None);

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"menu\":\"Menu-Main\""));
        // Money crosses the wire as a string. A JSON number here would be read back
        // through JS `Number()` and lose paisa (GROUND-TRUTH.md §"Hosting constraints").
        assert!(json.contains("\"rate\":\"100.50\""));
        assert!(json.contains("\"strategy\":\"room\""));
    }

    #[test]
    fn menu_strategy_serialises_as_snake_case() {
        // The frontend branches on these literals, so pin them.
        for (strategy, expected) in [
            (MenuStrategyResponse::Room, "\"room\""),
            (MenuStrategyResponse::OrderType, "\"order_type\""),
            (MenuStrategyResponse::Default, "\"default\""),
        ] {
            assert_eq!(serde_json::to_string(&strategy).unwrap(), expected);
        }
    }

    #[test]
    fn item_details_carries_no_price_field() {
        // The regression this guards: an item detail response that looks like it names a
        // price. There are two prices in this system and neither is a property of the
        // item master — see the struct's docs.
        let response = ItemDetailsResponse::from_details(peacock_storage::repos::ItemDetails {
            code: ItemCode::new("BIRYANI"),
            name: "Chicken Biryani".to_owned(),
            item_group: Some(peacock_core::ids::ItemGroupName::new("Main Course")),
            stock_uom: "Nos".to_owned(),
            is_bom: true,
            disabled: false,
        });

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["item_code"], "BIRYANI");
        assert_eq!(json["item_group"], "Main Course");
        assert_eq!(json["stock_uom"], "Nos");
        assert_eq!(json["is_bom"], true);
        for absent in ["standard_rate", "rate", "price", "price_list_rate"] {
            assert!(
                json.get(absent).is_none(),
                "{absent} must not appear on an item detail response"
            );
        }
    }

    #[test]
    fn item_details_omits_an_absent_item_group() {
        let response = ItemDetailsResponse::from_details(peacock_storage::repos::ItemDetails {
            code: ItemCode::new("MYSTERY"),
            name: "Unfiled Item".to_owned(),
            item_group: None,
            stock_uom: "Nos".to_owned(),
            is_bom: false,
            disabled: true,
        });

        let json = serde_json::to_value(&response).unwrap();
        assert!(json.get("item_group").is_none());
        assert_eq!(json["disabled"], true);
    }

    #[test]
    fn item_price_response_serialization() {
        let response = ItemPriceResponse {
            item_code: "ITEM-001".to_owned(),
            pricelist: "Standard Selling".to_owned(),
            price: dec!(100.00),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"price\":\"100.00\""));
        
        let roundtrip: ItemPriceResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.price, dec!(100.00));
    }
}
