//! Menu resolution — the three strategies for determining which menu and items apply.
//!
//! Ported from `_upstream/ury-ury/ury/ury_pos/api.py`, specifically `getRestaurantMenu`
//! (lines 19–102).
//!
//! ## The three strategies
//!
//! Upstream implements menu selection with three precedence levels:
//!
//! 1. **Room-wise**: When `URY Restaurant.room_wise_menu` is enabled (a `Check` field),
//!    resolution queries the `menu_for_room` child table (istable=1) for a mapping from
//!    the given room to a menu (api.py:40–46). If no mapping exists, falls through to
//!    the default menu (api.py:46).
//!
//! 2. **Order-type-wise**: When `URY Restaurant.order_type_wise_menu` is enabled AND
//!    the user has the cashier role AND an order_type is provided, resolution queries
//!    the `order_type_menu` child table for a mapping (api.py:50–62). Falls through to
//!    default if no mapping exists.
//!
//! 3. **Default**: `URY Restaurant.active_menu` (api.py:48, 65, 69). This is the fallback
//!    when no room or order_type mapping is configured, or when neither strategy applies.
//!
//! The port models these as explicit strategies rather than cascading conditionals.
//!
//! ## Price source
//!
//! **ANSWER (api.py:79):** Price comes from `ury_menu_item.rate`, a `Currency` field on
//! the child table (`ury_menu_item.json` line 34). This is NOT `Item Price` from ERPNext's
//! price list (which is used only for the aggregator path at api.py:829), and NOT
//! `Item.standard_rate` (which is the item master's fallback rate).
//!
//! The menu item rate is the authoritative selling price for restaurant orders. The
//! aggregator flow (api.py:829) queries `Item Price.price_list_rate` from a separate
//! selling price list, but that is a distinct code path not modeled here.
//!
//! ## Active-date gating
//!
//! **ANSWER:** `URY Menu` has ONE validity field: `enabled` (Check, default=1, ury_menu.json
//! line 21). There are NO date-range fields (`valid_from`, `valid_to`, `start_date`,
//! `end_date`, etc.). Upstream filters `enabled = 1` implicitly by querying only the
//! active menu name returned by the resolution logic; it does NOT check the menu's
//! `enabled` flag at resolution time because the field lives on the menu document, not
//! on the room/order-type mapping.
//!
//! The `at` parameter is included in the signature for **symmetry with businessday.rs**
//! (which does use the timestamp for cutoff-hour logic) and for **future-proofing** if
//! date-range validity is ever added to the schema. The port could drop it, but retaining
//! it signals that menu selection is conceptually a point-in-time operation even though
//! the current schema does not enforce date ranges.
//!
//! ## Fallthrough behavior
//!
//! When `room_wise_menu = 1` but the room has no mapping in `menu_for_room`, upstream
//! falls back to `active_menu` (api.py:46). Similarly for `order_type_wise_menu` (api.py:62).
//! If `active_menu` is also NULL, upstream throws an error (api.py:72). The port returns
//! `Error::NoActiveMenu` when the final resolved menu name is None.

use crate::error::{Error, Result};
use crate::ids::{ItemCode, MenuCourseName, MenuName, RoomName};
use crate::money::Money;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// Menu resolution strategy.
///
/// Determines which menu applies based on context. See module documentation for
/// precedence order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuStrategy {
    /// Room-wise menu selection. Queries the `menu_for_room` child table on
    /// `URY Restaurant` (api.py:40–46). Falls back to default if no mapping exists.
    Room(RoomName),
    /// Order-type-wise menu selection. Queries the `order_type_menu` child table
    /// (api.py:56–62). Falls back to default if no mapping exists.
    OrderType(String),
    /// Default menu: `URY Restaurant.active_menu` (api.py:69).
    Default,
}

/// A resolved menu with its items.
///
/// Returned by [`resolve_menu`]. This is the menu that should be presented to the
/// user for ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMenu {
    /// The menu name that was resolved.
    pub menu: MenuName,
    /// Items in the menu, ordered by course sequence and then by item name within
    /// each course. Items with no course are sorted last.
    pub items: Vec<ResolvedMenuItem>,
}

/// A menu item with all fields needed for display and ordering.
///
/// Transcribed from `ury_menu_item.json` plus the course's sequence number for ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMenuItem {
    /// ERPNext `Item` code.
    pub item: ItemCode,
    /// Fetched from `Item.item_name` (api.py:79).
    pub item_name: String,
    /// **The authoritative selling price** — from `ury_menu_item.rate` (api.py:79, 87).
    /// This is NOT `Item Price` and NOT `Item.standard_rate`.
    pub rate: Money,
    /// Whether this item is marked as a special dish (ury_menu_item.json line 42).
    pub special_dish: bool,
    /// The course this item belongs to, if any (ury_menu_item.json line 54).
    pub course: Option<MenuCourseName>,
    /// Sequence number for course ordering. Items with the same course are grouped
    /// together, and courses are ordered by this value. Items with no course have
    /// `course_sequence: None` and are sorted last, then by item name.
    pub course_sequence: Option<i32>,
}

/// Storage port for menu resolution.
///
/// Implementers must handle the room-wise and order-type-wise lookups and return the
/// resolved menu name plus its items. The existing [`crate::ports::MenuRepo`] is
/// insufficient because it only provides `courses_for_menu`, which requires a room
/// and item codes already known — a narrower use case (KOT routing).
pub trait MenuResolutionRepo {
    /// Resolve the menu name for a room-wise strategy.
    ///
    /// Queries the `menu_for_room` child table on the restaurant that owns the given
    /// room (api.py:40–44). Returns `None` if no mapping exists, which signals fallback
    /// to the default menu.
    fn menu_for_room(&self, room: &RoomName) -> Result<Option<MenuName>>;

    /// Resolve the menu name for an order-type-wise strategy.
    ///
    /// Queries the `order_type_menu` child table on the restaurant for the branch
    /// (api.py:56–60). Returns `None` if no mapping exists.
    fn menu_for_order_type(&self, order_type: &str) -> Result<Option<MenuName>>;

    /// The default menu for the branch.
    ///
    /// Corresponds to `URY Restaurant.active_menu` (api.py:46, 62, 69). Returns `None`
    /// if no default is configured, which will result in `Error::NoActiveMenu`.
    fn default_menu(&self) -> Result<Option<MenuName>>;

    /// Fetch all items for a menu, with course sequence numbers for ordering.
    ///
    /// Upstream queries `ury_menu_item` with `filters={"parent": menu, "disabled": 0}`
    /// (api.py:76–80) and then joins course data for ordering. The sequence number
    /// comes from `URY Menu Course`, which is a root doctype (istable=0) with a single
    /// `course` field (Data, unique). Upstream does NOT store sequence numbers; the
    /// port assumes they exist or can be derived from insertion order.
    ///
    /// Returns items in **arbitrary order** — the caller must sort by course_sequence
    /// and item_name. An empty Vec is valid (an empty menu is not an error).
    fn menu_items(&self, menu: &MenuName) -> Result<Vec<ResolvedMenuItem>>;

    /// Fetch course sequence numbers for ordering items within a resolved menu.
    ///
    /// Upstream has a separate `getMenuCourses` endpoint (api.py:106–108) that returns
    /// all courses, but it does not expose sequence numbers. The schema has no `idx`
    /// or `sequence` field on `URY Menu Course` (ury_menu_course.json). The port
    /// assumes sequence can be derived from `name` sort order or must be added to the
    /// schema if deterministic course ordering is required.
    ///
    /// This method returns a map of course name → sequence. If a course is not in the
    /// map, its sequence is undefined and items in that course sort by name only.
    fn course_sequences(&self) -> Result<HashMap<MenuCourseName, i32>>;
}

/// Resolve the menu and its items for the given strategy at a point in time.
///
/// ## Parameters
///
/// - `strategy`: The menu selection strategy (room-wise, order-type, or default).
/// - `at`: Point in time for resolution. **Currently unused** because `URY Menu` has no
///   date-range validity fields (see module doc). Retained for symmetry with
///   [`crate::businessday`] and future schema extensions.
/// - `repo`: Storage adapter implementing [`MenuResolutionRepo`].
///
/// ## Returns
///
/// A [`ResolvedMenu`] with items ordered by course sequence (ascending), then by item
/// name within each course. Items with no course appear last. An empty item list is
/// valid (an empty menu is not an error).
///
/// ## Errors
///
/// - [`Error::NoActiveMenu`]: No menu could be resolved. This happens when the strategy's
///   lookup returns `None` AND the default menu is also `None` (api.py:71–72).
/// - Propagates repo failures unchanged.
///
/// ## Precedence
///
/// From api.py:19–73, the actual precedence is:
/// 1. Room-wise wins if configured and a mapping exists (api.py:40–46)
/// 2. Order-type-wise wins if configured and a mapping exists (api.py:50–62)
/// 3. Default wins as fallback (api.py:46, 62, 69)
///
/// The port models each as an explicit strategy rather than cascading checks. The caller
/// chooses which strategy to invoke based on the request context (is there a room? is
/// there an order type? does the user have cashier role?).
pub fn resolve_menu(
    strategy: MenuStrategy,
    _at: DateTime<Utc>,
    repo: &dyn MenuResolutionRepo,
) -> Result<ResolvedMenu> {
    // Resolve the menu name per the strategy, with fallback to default.
    let menu_name = match strategy {
        MenuStrategy::Room(ref room) => {
            let room_menu = repo.menu_for_room(room)?;
            if room_menu.is_some() {
                room_menu
            } else {
                repo.default_menu()?
            }
        }
        MenuStrategy::OrderType(ref order_type) => {
            let order_type_menu = repo.menu_for_order_type(order_type)?;
            if order_type_menu.is_some() {
                order_type_menu
            } else {
                repo.default_menu()?
            }
        }
        MenuStrategy::Default => repo.default_menu()?,
    };

    let menu = menu_name.ok_or(Error::NoActiveMenu)?;

    // Fetch items and course sequences.
    let mut items = repo.menu_items(&menu)?;
    let sequences = repo.course_sequences()?;

    // Materialize course_sequence onto each item.
    for item in &mut items {
        if let Some(ref course) = item.course {
            item.course_sequence = sequences.get(course).copied();
        }
    }

    // Sort: course_sequence ascending (None last), then item_name ascending.
    items.sort_by(|a, b| {
        match (a.course_sequence, b.course_sequence) {
            (Some(seq_a), Some(seq_b)) => seq_a.cmp(&seq_b).then_with(|| a.item_name.cmp(&b.item_name)),
            (Some(_), None) => std::cmp::Ordering::Less,  // course items before no-course
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.item_name.cmp(&b.item_name), // both no course → name order
        }
    });

    Ok(ResolvedMenu { menu, items })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::RoomName;
    use rust_decimal_macros::dec;
    use std::cell::Cell;

    // -------------------------------------------------------------------------
    // In-memory fake repo with call counters
    // -------------------------------------------------------------------------

    struct FakeMenuRepo {
        room_mappings: HashMap<RoomName, Option<MenuName>>,
        order_type_mappings: HashMap<String, Option<MenuName>>,
        default_menu: Option<MenuName>,
        items_by_menu: HashMap<MenuName, Vec<ResolvedMenuItem>>,
        course_sequences: HashMap<MenuCourseName, i32>,
        // Call counters
        room_calls: Cell<usize>,
        order_type_calls: Cell<usize>,
        default_calls: Cell<usize>,
        items_calls: Cell<usize>,
        course_calls: Cell<usize>,
    }

    impl FakeMenuRepo {
        fn new() -> Self {
            FakeMenuRepo {
                room_mappings: HashMap::new(),
                order_type_mappings: HashMap::new(),
                default_menu: None,
                items_by_menu: HashMap::new(),
                course_sequences: HashMap::new(),
                room_calls: Cell::new(0),
                order_type_calls: Cell::new(0),
                default_calls: Cell::new(0),
                items_calls: Cell::new(0),
                course_calls: Cell::new(0),
            }
        }

        fn with_room_mapping(mut self, room: &str, menu: Option<&str>) -> Self {
            self.room_mappings
                .insert(RoomName::new(room), menu.map(MenuName::new));
            self
        }

        fn with_order_type_mapping(mut self, order_type: &str, menu: Option<&str>) -> Self {
            self.order_type_mappings
                .insert(order_type.to_owned(), menu.map(MenuName::new));
            self
        }

        fn with_default_menu(mut self, menu: Option<&str>) -> Self {
            self.default_menu = menu.map(MenuName::new);
            self
        }

        fn with_menu_items(mut self, menu: &str, items: Vec<ResolvedMenuItem>) -> Self {
            self.items_by_menu.insert(MenuName::new(menu), items);
            self
        }

        fn with_course_sequence(mut self, course: &str, seq: i32) -> Self {
            self.course_sequences
                .insert(MenuCourseName::new(course), seq);
            self
        }
    }

    impl MenuResolutionRepo for FakeMenuRepo {
        fn menu_for_room(&self, room: &RoomName) -> Result<Option<MenuName>> {
            self.room_calls.set(self.room_calls.get() + 1);
            Ok(self.room_mappings.get(room).cloned().flatten())
        }

        fn menu_for_order_type(&self, order_type: &str) -> Result<Option<MenuName>> {
            self.order_type_calls.set(self.order_type_calls.get() + 1);
            Ok(self
                .order_type_mappings
                .get(order_type)
                .cloned()
                .flatten())
        }

        fn default_menu(&self) -> Result<Option<MenuName>> {
            self.default_calls.set(self.default_calls.get() + 1);
            Ok(self.default_menu.clone())
        }

        fn menu_items(&self, menu: &MenuName) -> Result<Vec<ResolvedMenuItem>> {
            self.items_calls.set(self.items_calls.get() + 1);
            Ok(self
                .items_by_menu
                .get(menu)
                .cloned()
                .unwrap_or_default())
        }

        fn course_sequences(&self) -> Result<HashMap<MenuCourseName, i32>> {
            self.course_calls.set(self.course_calls.get() + 1);
            Ok(self.course_sequences.clone())
        }
    }

    fn sample_item(item: &str, name: &str, rate: &str, course: Option<&str>) -> ResolvedMenuItem {
        ResolvedMenuItem {
            item: ItemCode::new(item),
            item_name: name.to_owned(),
            rate: Money::new(
                rate.parse::<rust_decimal::Decimal>()
                    .unwrap_or(rust_decimal::Decimal::ZERO)
            ),
            special_dish: false,
            course: course.map(MenuCourseName::new),
            course_sequence: None, // Filled by resolve_menu
        }
    }

    // -------------------------------------------------------------------------
    // Strategy tests
    // -------------------------------------------------------------------------

    #[test]
    fn room_strategy_resolves_to_room_menu() {
        // api.py:40–46
        let repo = FakeMenuRepo::new()
            .with_room_mapping("Room-A", Some("Menu-Room-A"))
            .with_menu_items("Menu-Room-A", vec![sample_item("ITEM-001", "Item 1", "100.00", None)])
            .with_default_menu(Some("Menu-Default"));

        let result = resolve_menu(
            MenuStrategy::Room(RoomName::new("Room-A")),
            Utc::now(),
            &repo,
        )
        .unwrap();

        assert_eq!(result.menu.as_str(), "Menu-Room-A");
        assert_eq!(result.items.len(), 1);
        assert_eq!(repo.room_calls.get(), 1);
        assert_eq!(repo.default_calls.get(), 0); // did not fall through
    }

    #[test]
    fn room_strategy_falls_back_to_default_when_no_mapping() {
        // api.py:46
        let repo = FakeMenuRepo::new()
            .with_room_mapping("Room-A", None) // no mapping
            .with_default_menu(Some("Menu-Default"))
            .with_menu_items("Menu-Default", vec![sample_item("ITEM-002", "Item 2", "200.00", None)]);

        let result = resolve_menu(
            MenuStrategy::Room(RoomName::new("Room-A")),
            Utc::now(),
            &repo,
        )
        .unwrap();

        assert_eq!(result.menu.as_str(), "Menu-Default");
        assert_eq!(repo.room_calls.get(), 1);
        assert_eq!(repo.default_calls.get(), 1); // fell through
    }

    #[test]
    fn order_type_strategy_resolves_correctly() {
        // api.py:56–62
        let repo = FakeMenuRepo::new()
            .with_order_type_mapping("Delivery", Some("Menu-Delivery"))
            .with_menu_items("Menu-Delivery", vec![sample_item("ITEM-003", "Item 3", "150.00", None)])
            .with_default_menu(Some("Menu-Default"));

        let result = resolve_menu(
            MenuStrategy::OrderType("Delivery".to_owned()),
            Utc::now(),
            &repo,
        )
        .unwrap();

        assert_eq!(result.menu.as_str(), "Menu-Delivery");
        assert_eq!(repo.order_type_calls.get(), 1);
        assert_eq!(repo.default_calls.get(), 0);
    }

    #[test]
    fn order_type_strategy_falls_back_to_default() {
        // api.py:62
        let repo = FakeMenuRepo::new()
            .with_order_type_mapping("Delivery", None)
            .with_default_menu(Some("Menu-Default"))
            .with_menu_items("Menu-Default", vec![]);

        let result = resolve_menu(
            MenuStrategy::OrderType("Delivery".to_owned()),
            Utc::now(),
            &repo,
        )
        .unwrap();

        assert_eq!(result.menu.as_str(), "Menu-Default");
        assert_eq!(repo.order_type_calls.get(), 1);
        assert_eq!(repo.default_calls.get(), 1);
    }

    #[test]
    fn default_strategy_uses_active_menu() {
        // api.py:69
        let repo = FakeMenuRepo::new()
            .with_default_menu(Some("Menu-Default"))
            .with_menu_items("Menu-Default", vec![sample_item("ITEM-004", "Item 4", "250.00", None)]);

        let result = resolve_menu(MenuStrategy::Default, Utc::now(), &repo).unwrap();

        assert_eq!(result.menu.as_str(), "Menu-Default");
        assert_eq!(repo.default_calls.get(), 1);
        assert_eq!(repo.room_calls.get(), 0);
        assert_eq!(repo.order_type_calls.get(), 0);
    }

    #[test]
    fn no_active_menu_returns_error() {
        // api.py:71–72
        let repo = FakeMenuRepo::new().with_default_menu(None);

        let result = resolve_menu(MenuStrategy::Default, Utc::now(), &repo);

        assert!(matches!(result, Err(Error::NoActiveMenu)));
    }

    #[test]
    fn room_with_no_mapping_and_no_default_returns_error() {
        let repo = FakeMenuRepo::new()
            .with_room_mapping("Room-B", None)
            .with_default_menu(None);

        let result = resolve_menu(
            MenuStrategy::Room(RoomName::new("Room-B")),
            Utc::now(),
            &repo,
        );

        assert!(matches!(result, Err(Error::NoActiveMenu)));
    }

    // -------------------------------------------------------------------------
    // Course ordering
    // -------------------------------------------------------------------------

    #[test]
    fn items_ordered_by_course_sequence_then_name() {
        let repo = FakeMenuRepo::new()
            .with_default_menu(Some("Menu-Main"))
            .with_menu_items(
                "Menu-Main",
                vec![
                    sample_item("ITEM-003", "Dessert Item", "300.00", Some("Desserts")),
                    sample_item("ITEM-001", "Soup A", "100.00", Some("Starters")),
                    sample_item("ITEM-004", "Main Dish Z", "400.00", Some("Mains")),
                    sample_item("ITEM-002", "Soup B", "150.00", Some("Starters")),
                ],
            )
            .with_course_sequence("Starters", 1)
            .with_course_sequence("Mains", 2)
            .with_course_sequence("Desserts", 3);

        let result = resolve_menu(MenuStrategy::Default, Utc::now(), &repo).unwrap();

        // Expected order: Starters (Soup A, Soup B), Mains (Main Dish Z), Desserts (Dessert Item)
        assert_eq!(result.items.len(), 4);
        assert_eq!(result.items[0].item.as_str(), "ITEM-001"); // Soup A
        assert_eq!(result.items[1].item.as_str(), "ITEM-002"); // Soup B
        assert_eq!(result.items[2].item.as_str(), "ITEM-004"); // Main Dish Z
        assert_eq!(result.items[3].item.as_str(), "ITEM-003"); // Dessert Item
    }

    #[test]
    fn items_with_no_course_appear_last() {
        let repo = FakeMenuRepo::new()
            .with_default_menu(Some("Menu-Mixed"))
            .with_menu_items(
                "Menu-Mixed",
                vec![
                    sample_item("ITEM-002", "No Course B", "200.00", None),
                    sample_item("ITEM-001", "Starter A", "100.00", Some("Starters")),
                    sample_item("ITEM-003", "No Course A", "300.00", None),
                ],
            )
            .with_course_sequence("Starters", 1);

        let result = resolve_menu(MenuStrategy::Default, Utc::now(), &repo).unwrap();

        assert_eq!(result.items.len(), 3);
        assert_eq!(result.items[0].item.as_str(), "ITEM-001"); // Starter A (has course)
        // No-course items sorted by name: A before B
        assert_eq!(result.items[1].item.as_str(), "ITEM-003"); // No Course A
        assert_eq!(result.items[2].item.as_str(), "ITEM-002"); // No Course B
    }

    #[test]
    fn empty_menu_resolves_without_error() {
        let repo = FakeMenuRepo::new()
            .with_default_menu(Some("Menu-Empty"))
            .with_menu_items("Menu-Empty", vec![]);

        let result = resolve_menu(MenuStrategy::Default, Utc::now(), &repo).unwrap();

        assert_eq!(result.menu.as_str(), "Menu-Empty");
        assert!(result.items.is_empty());
    }

    #[test]
    fn course_with_no_sequence_sorts_by_name_only() {
        // Course not in the sequence map → course_sequence remains None for those items
        let repo = FakeMenuRepo::new()
            .with_default_menu(Some("Menu-Partial"))
            .with_menu_items(
                "Menu-Partial",
                vec![
                    sample_item("ITEM-002", "Unknown B", "200.00", Some("Unknown")),
                    sample_item("ITEM-001", "Unknown A", "100.00", Some("Unknown")),
                ],
            );
        // No sequence for "Unknown"

        let result = resolve_menu(MenuStrategy::Default, Utc::now(), &repo).unwrap();

        // Both have course_sequence = None, so sorted by name
        assert_eq!(result.items[0].item.as_str(), "ITEM-001"); // Unknown A
        assert_eq!(result.items[1].item.as_str(), "ITEM-002"); // Unknown B
    }

    // -------------------------------------------------------------------------
    // Query budget
    // -------------------------------------------------------------------------

    #[test]
    fn query_budget_is_bounded() {
        // Resolution should issue a fixed number of queries regardless of item count.
        let many_items: Vec<ResolvedMenuItem> = (0..100)
            .map(|i| {
                sample_item(
                    &format!("ITEM-{:03}", i),
                    &format!("Item {}", i),
                    "100.00",
                    Some("Starters"),
                )
            })
            .collect();

        let repo = FakeMenuRepo::new()
            .with_room_mapping("Room-X", Some("Menu-Large"))
            .with_menu_items("Menu-Large", many_items)
            .with_course_sequence("Starters", 1);

        let _result = resolve_menu(
            MenuStrategy::Room(RoomName::new("Room-X")),
            Utc::now(),
            &repo,
        )
        .unwrap();

        // Exactly 1 room lookup, 0 default lookups (didn't fall through), 1 items fetch, 1 courses fetch
        assert_eq!(repo.room_calls.get(), 1);
        assert_eq!(repo.default_calls.get(), 0);
        assert_eq!(repo.items_calls.get(), 1);
        assert_eq!(repo.course_calls.get(), 1);
        // Total: 3 queries, regardless of 100 items
    }

    #[test]
    fn fallback_adds_one_additional_query() {
        let repo = FakeMenuRepo::new()
            .with_room_mapping("Room-Y", None) // forces fallback
            .with_default_menu(Some("Menu-Default"))
            .with_menu_items("Menu-Default", vec![]);

        let _result = resolve_menu(
            MenuStrategy::Room(RoomName::new("Room-Y")),
            Utc::now(),
            &repo,
        )
        .unwrap();

        // 1 room lookup + 1 default fallback + 1 items + 1 courses = 4 queries
        assert_eq!(repo.room_calls.get(), 1);
        assert_eq!(repo.default_calls.get(), 1);
        assert_eq!(repo.items_calls.get(), 1);
        assert_eq!(repo.course_calls.get(), 1);
    }

    // -------------------------------------------------------------------------
    // Price source verification
    // -------------------------------------------------------------------------

    #[test]
    fn rate_comes_from_ury_menu_item_not_item_price() {
        // api.py:79, 87 — rate is from ury_menu_item.rate (Currency field)
        let repo = FakeMenuRepo::new()
            .with_default_menu(Some("Menu-Price"))
            .with_menu_items(
                "Menu-Price",
                vec![ResolvedMenuItem {
                    item: ItemCode::new("ITEM-RATE"),
                    item_name: "Rate Test Item".to_owned(),
                    rate: Money::new(dec!(123.45)),
                    special_dish: false,
                    course: None,
                    course_sequence: None,
                }],
            );

        let result = resolve_menu(MenuStrategy::Default, Utc::now(), &repo).unwrap();

        assert_eq!(result.items[0].rate, Money::new(dec!(123.45)));
        // The test fixture directly sets the rate from the fake repo, which models
        // the storage layer reading ury_menu_item.rate (not Item Price, not standard_rate).
    }
}
