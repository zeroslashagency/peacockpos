//! Restaurant lookups — the storage half of the request's restaurant context.
//!
//! Every menu resolution path upstream is restaurant-scoped: `getRestaurantMenu` derives
//! the restaurant from the branch (api.py:33) and then reads the `menu_for_room` /
//! `order_type_menu` child tables with `{"parent": restaurant, ...}`. Peacock's
//! [`PgMenuResolutionRepo`](super::menu::PgMenuResolutionRepo) is therefore constructed
//! *for* one restaurant, and something has to decide which one and prove it exists.
//!
//! That is this file. It answers two questions and nothing else:
//!
//! * **Does this restaurant exist?** — so the API can answer 404 rather than resolving a
//!   menu for a name nobody configured. A repository scoped to a non-existent restaurant
//!   is not an error at construction time (the scope is just a string), so the check has
//!   to be explicit and it has to happen before resolution.
//! * **What does it say about itself?** — `branch`, `active_menu`, `default_room`, and the
//!   two `*_wise_menu` flags. The flags are enforced in SQL inside
//!   [`PgMenuResolutionRepo`], so a caller does not need them to resolve a menu; they are
//!   returned because an API response that explains *which* strategy applied is worth more
//!   to a POS screen than one that only names the winning menu.
//!
//! # Why there is no "pick the restaurant for me"
//!
//! There is deliberately no `first()`, no `only()`, and no `default()`. `for_branch`
//! exists on [`PgMenuResolutionRepo`] and mirrors api.py:33, but it takes a branch the
//! caller named. Silently selecting a restaurant would work perfectly on a single-branch
//! deployment and serve the wrong branch's menu the day a second one is added — the same
//! class of bug the `menu.rs` module docs call out for a repository scoped to nothing.
//!
//! # Soft deletes
//!
//! `restaurants` carries `deleted_at` (001_core_tables.sql). A retired restaurant reads
//! as absent here, so the API answers 404 for it rather than serving a menu from a
//! location that has been closed. Historical invoices still reference the row by name;
//! this is a resolution-time filter, not a delete.

use peacock_core::ids::{BranchName, MenuName, RestaurantName, RoomName};
use sqlx::PgPool;

use crate::error::{StorageError, StorageResult};

/// What `restaurants` says about one restaurant, for the columns resolution reads.
///
/// Deliberately not the whole row: `company`, `address`, the invoice series prefixes and
/// the tax template belong to the invoicing and reporting lanes, and carrying them here
/// would invite a caller to read a stale copy of a column it should have asked the
/// invoice repository for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestaurantSummary {
    pub name: RestaurantName,
    pub branch: BranchName,
    /// `URY Restaurant.active_menu` — strategy 3, the default menu (api.py:48, 65, 69).
    /// `None` when unset, which `resolve_menu` turns into `Error::NoActiveMenu`.
    pub active_menu: Option<MenuName>,
    pub default_room: Option<RoomName>,
    /// When false, the `menu_for_room` mapping is not consulted at all and the default
    /// menu wins even if a mapping row exists (api.py:36–46).
    pub room_wise_menu: bool,
    /// Same, for `order_type_menu` (api.py:50–62).
    pub order_type_wise_menu: bool,
}

/// Read-only access to `restaurants`.
#[derive(Clone, Debug)]
pub struct PgRestaurantRepo {
    pool: PgPool,
}

impl PgRestaurantRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The restaurant with this exact name, or `None` when it does not exist or is
    /// soft-deleted.
    ///
    /// `Option` rather than an error: "no such restaurant" is a caller mistake the API
    /// turns into a 404, and `peacock_core::Error` has no variant for it (it has one
    /// per entity, and `RestaurantNotFound` is not among them — see
    /// [`crate::error::on_missing`]). Returning the absence keeps the status decision in
    /// the layer that owns status codes.
    pub async fn find_async(
        &self,
        restaurant: &RestaurantName,
    ) -> StorageResult<Option<RestaurantSummary>> {
        #[allow(clippy::type_complexity)]
        let row: Option<(String, String, Option<String>, Option<String>, bool, bool)> =
            sqlx::query_as(
                r#"
                SELECT name, branch, active_menu, default_room,
                       room_wise_menu, order_type_wise_menu
                FROM restaurants
                WHERE name = $1 AND deleted_at IS NULL
                "#,
            )
            .bind(restaurant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?;

        Ok(row.map(
            |(name, branch, active_menu, default_room, room_wise, order_type_wise)| {
                RestaurantSummary {
                    name: RestaurantName::new(name),
                    branch: BranchName::new(branch),
                    active_menu: active_menu.map(MenuName::new),
                    default_room: default_room.map(RoomName::new),
                    room_wise_menu: room_wise,
                    order_type_wise_menu: order_type_wise,
                }
            },
        ))
    }

    /// Every live restaurant, by name.
    ///
    /// For an operator-facing picker, not for resolution: a handler that needs *one*
    /// restaurant must be told which, never handed this list and told to choose.
    pub async fn list_async(&self) -> StorageResult<Vec<RestaurantName>> {
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM restaurants WHERE deleted_at IS NULL ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(names.into_iter().map(RestaurantName::new).collect())
    }
}
