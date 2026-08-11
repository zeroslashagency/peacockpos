//! Station routing repositories — Lane 4A-3.
//!
//! [`peacock_core::kot::route_items_to_stations`] needs four ports: [`ItemRepo`],
//! [`ProductionRepo`], [`KotRepo`] and [`MenuRepo`]. Lane 2E supplied [`KotRepo`]
//! ([`super::kot::PgKotRepo`]) and Lane 2C supplied [`MenuRepo`]
//! ([`super::menu::PgMenuRepo`]). The other two land here, because nothing before this
//! lane needed to route an order to a kitchen.
//!
//! # Prefetch, not blocking
//!
//! The port traits are synchronous (`ports.rs:7-9`), so a repository serving them from
//! Postgres has to block somewhere — [`super::blocking::block_on`] is that place, and it
//! parks a worker thread per lookup.
//!
//! Routing does not need it. Its query budget is fixed and known before any lookup runs:
//! the item set comes from `required_item_codes`, and the production units come from the
//! branch. So [`RoutingSnapshot`] loads both in **three queries**, then serves all four
//! ports out of memory with no blocking at all. That is the same shape
//! [`super::bom::PgBomRepo::snapshot_for_items`] uses, and it matters more here: routing
//! runs on the hot path of every order that reaches the kitchen.
//!
//! This is also the bug 6 / bug 7 fix carried through to storage. Upstream ran
//! `frappe.db.get_value("Item", …)` once per item per station
//! (`ury_kot_generate.py:154`) and `frappe.get_doc("Item", …)` — a full document load —
//! in the same pattern (`:214`): 36 queries for 12 items across 3 stations. The snapshot
//! is 3, independent of item and station count.
//!
//! # What a missing row means
//!
//! An item with no row, or with a NULL `item_group`, is **absent** from the map rather
//! than an error. `unrouted_item_codes` reads that absence as "routes nowhere", which is
//! upstream's `None not in productionItemGroups` behaviour (deviation 4): the item is
//! dropped from every ticket and reported to the caller, not allowed to abort the order.
//! Turning it into an error here would refuse to feed a table because one item is
//! mis-configured.

use std::collections::HashMap;

use peacock_core::error::Result as DomainResult;
use peacock_core::ids::{
    BranchName, ItemCode, ItemGroupName, MenuCourseName, ProductionUnitName, RoomName,
};
use peacock_core::model::ProductionUnit;
use peacock_core::ports::{ItemRepo, KotRepo, MenuRepo, ProductionRepo};
use sqlx::PgPool;

use crate::error::StorageResult;
use crate::repos::blocking::block_on;
use crate::repos::to_domain_error;
use crate::Storage;

// ---------------------------------------------------------------------------
// PgItemRepo
// ---------------------------------------------------------------------------

/// [`ItemRepo`] over Postgres.
///
/// Exists for callers that want the port directly. Routing should prefer
/// [`RoutingSnapshot`], which answers the same question without blocking.
#[derive(Clone, Debug)]
pub struct PgItemRepo {
    pool: PgPool,
}

impl PgItemRepo {
    pub fn new(storage: Storage) -> Self {
        PgItemRepo {
            pool: storage.pool().clone(),
        }
    }

    pub fn from_pool(pool: PgPool) -> Self {
        PgItemRepo { pool }
    }

    /// Item group per code, in one query.
    ///
    /// Items with a NULL `item_group`, soft-deleted items and disabled items are all
    /// omitted. A disabled item still on an open order routes nowhere and is reported as
    /// unrouted, which is the safe direction: the kitchen never silently receives a line
    /// for something the menu has withdrawn.
    pub async fn item_groups_async(
        &self,
        codes: &[ItemCode],
    ) -> StorageResult<HashMap<ItemCode, ItemGroupName>> {
        if codes.is_empty() {
            return Ok(HashMap::new());
        }

        let wanted: Vec<&str> = codes.iter().map(|c| c.as_str()).collect();
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT code, item_group FROM items
              WHERE code = ANY($1)
                AND item_group IS NOT NULL
                AND NOT disabled
                AND deleted_at IS NULL",
        )
        .bind(&wanted)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(code, group)| (ItemCode::from(code.as_str()), ItemGroupName::from(group.as_str())))
            .collect())
    }
}

impl ItemRepo for PgItemRepo {
    fn item_groups(&self, codes: &[ItemCode]) -> DomainResult<HashMap<ItemCode, ItemGroupName>> {
        block_on(self.item_groups_async(codes))
            .map_err(to_domain_error)?
            .map_err(to_domain_error)
    }
}

// ---------------------------------------------------------------------------
// PgProductionRepo
// ---------------------------------------------------------------------------

/// [`ProductionRepo`] over Postgres.
#[derive(Clone, Debug)]
pub struct PgProductionRepo {
    pool: PgPool,
}

impl PgProductionRepo {
    pub fn new(storage: Storage) -> Self {
        PgProductionRepo {
            pool: storage.pool().clone(),
        }
    }

    pub fn from_pool(pool: PgPool) -> Self {
        PgProductionRepo { pool }
    }

    /// Active production units for a branch, each with its item groups.
    ///
    /// Two queries, not one per unit: the units, then every child row for that set.
    /// `item_groups` is a `Vec` on the domain type, so the child rows come back ordered
    /// by `idx` — a station's ticket layout follows the order the units were configured
    /// in, and a `HashMap` iteration order would make that non-deterministic.
    pub async fn list_for_branch_async(
        &self,
        branch: &BranchName,
    ) -> StorageResult<Vec<ProductionUnit>> {
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM production_units
              WHERE branch = $1 AND deleted_at IS NULL
              ORDER BY name",
        )
        .bind(branch.as_str())
        .fetch_all(&self.pool)
        .await?;

        if names.is_empty() {
            return Ok(Vec::new());
        }

        let group_rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT production_unit, item_group FROM production_unit_item_groups
              WHERE production_unit = ANY($1)
              ORDER BY production_unit, idx",
        )
        .bind(&names)
        .fetch_all(&self.pool)
        .await?;

        let mut groups: HashMap<String, Vec<ItemGroupName>> = HashMap::new();
        for (unit, group) in group_rows {
            groups
                .entry(unit)
                .or_default()
                .push(ItemGroupName::from(group.as_str()));
        }

        Ok(names
            .into_iter()
            .map(|name| ProductionUnit {
                item_groups: groups.remove(&name).unwrap_or_default(),
                name: ProductionUnitName::from(name.as_str()),
                branch: branch.clone(),
            })
            .collect())
    }
}

impl ProductionRepo for PgProductionRepo {
    fn list_for_branch(&self, branch: &BranchName) -> DomainResult<Vec<ProductionUnit>> {
        block_on(self.list_for_branch_async(branch))
            .map_err(to_domain_error)?
            .map_err(to_domain_error)
    }
}

// ---------------------------------------------------------------------------
// RoutingSnapshot
// ---------------------------------------------------------------------------

/// Everything [`route_items_to_stations`] reads, loaded up front.
///
/// [`route_items_to_stations`]: peacock_core::kot::route_items_to_stations
///
/// Serves all four ports from memory. Build it with [`RoutingSnapshot::load`], hand out
/// [`RoutingSnapshot::repos`], and routing runs with zero further I/O — which is what
/// lets it be called from any runtime flavour, including a current-thread one that
/// [`block_on`] would panic in.
///
/// # Staleness
///
/// The snapshot is a point-in-time read. A production unit reconfigured between `load`
/// and the routing call is not seen. That window is one function call wide and the
/// alternative — re-reading per station per item — is the N+1 this exists to remove.
#[derive(Clone, Debug)]
pub struct RoutingSnapshot {
    units: Vec<ProductionUnit>,
    item_groups: HashMap<ItemCode, ItemGroupName>,
    courses: HashMap<ItemCode, MenuCourseName>,
    /// `(invoice, production unit)` pairs that already have a submitted ticket. Drives
    /// the `NewOrder` → `OrderModified` flip, per station.
    printed: Vec<(String, ProductionUnitName)>,
}

impl RoutingSnapshot {
    /// Load the routing inputs for one order.
    ///
    /// Three or four queries total:
    ///   1. the branch's production units (+1 for their item groups),
    ///   2. the item groups for `codes`,
    ///   3. the room's courses for `codes` — skipped entirely when `room` is `None`,
    ///      which is the takeaway path (deviation 5: no room, no course),
    ///   4. which `(invoice, unit)` pairs already printed.
    ///
    /// Independent of how many items or stations are involved.
    pub async fn load(
        storage: &Storage,
        invoice: &str,
        branch: &BranchName,
        room: Option<&RoomName>,
        codes: &[ItemCode],
    ) -> StorageResult<Self> {
        let productions = PgProductionRepo::new(storage.clone());
        let items = PgItemRepo::new(storage.clone());

        let units = productions.list_for_branch_async(branch).await?;
        let item_groups = items.item_groups_async(codes).await?;

        let courses = match room {
            Some(room) => {
                super::menu::PgMenuRepo::new(storage.pool().clone())
                    .courses_for_menu_async(room, codes)
                    .await?
            }
            None => HashMap::new(),
        };

        // One query for every station at once, rather than the per-station probe
        // `route_items_to_stations` would otherwise make through `KotRepo::exists_for`.
        let unit_names: Vec<&str> = units.iter().map(|u| u.name.as_str()).collect();
        let printed = if unit_names.is_empty() {
            Vec::new()
        } else {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT production FROM kots
                  WHERE invoice = $1
                    AND production = ANY($2)
                    AND kot_type IN ('NewOrder', 'OrderModified')",
            )
            .bind(invoice)
            .bind(&unit_names)
            .fetch_all(storage.pool())
            .await?;

            rows.into_iter()
                .map(|(unit,)| (invoice.to_owned(), ProductionUnitName::from(unit.as_str())))
                .collect()
        };

        Ok(RoutingSnapshot {
            units,
            item_groups,
            courses,
            printed,
        })
    }

    /// The four ports, bundled for [`route_items_to_stations`].
    ///
    /// [`route_items_to_stations`]: peacock_core::kot::route_items_to_stations
    pub fn repos(&self) -> peacock_core::kot::KotRepos<'_> {
        peacock_core::kot::KotRepos {
            items: self,
            productions: self,
            kots: self,
            menu: self,
        }
    }

    /// The units this snapshot saw. `unrouted_item_codes` needs them alongside
    /// [`RoutingSnapshot::item_groups`].
    pub fn units(&self) -> &[ProductionUnit] {
        &self.units
    }

    /// The item → group map this snapshot saw.
    pub fn item_groups_map(&self) -> &HashMap<ItemCode, ItemGroupName> {
        &self.item_groups
    }
}

impl ItemRepo for RoutingSnapshot {
    fn item_groups(&self, codes: &[ItemCode]) -> DomainResult<HashMap<ItemCode, ItemGroupName>> {
        Ok(codes
            .iter()
            .filter_map(|c| self.item_groups.get(c).map(|g| (c.clone(), g.clone())))
            .collect())
    }
}

impl ProductionRepo for RoutingSnapshot {
    fn list_for_branch(&self, branch: &BranchName) -> DomainResult<Vec<ProductionUnit>> {
        // Filtered rather than returned wholesale: the snapshot is loaded for one branch,
        // and silently answering for a different one would route a table's food to
        // another restaurant's kitchen.
        Ok(self
            .units
            .iter()
            .filter(|u| &u.branch == branch)
            .cloned()
            .collect())
    }
}

impl KotRepo for RoutingSnapshot {
    fn exists_for(&self, invoice: &str, production: &ProductionUnitName) -> DomainResult<bool> {
        Ok(self
            .printed
            .iter()
            .any(|(inv, unit)| inv == invoice && unit == production))
    }
}

impl MenuRepo for RoutingSnapshot {
    fn courses_for_menu(
        &self,
        _room: &RoomName,
        codes: &[ItemCode],
    ) -> DomainResult<HashMap<ItemCode, MenuCourseName>> {
        // The room is fixed at load time, so it is not re-checked here: a snapshot loaded
        // for one room cannot answer for another, and `load` is the only constructor.
        Ok(codes
            .iter()
            .filter_map(|c| self.courses.get(c).map(|course| (c.clone(), course.clone())))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> RoutingSnapshot {
        RoutingSnapshot {
            units: vec![
                ProductionUnit {
                    name: ProductionUnitName::from("Hot Kitchen"),
                    branch: BranchName::from("Peacock - Main"),
                    item_groups: vec![ItemGroupName::from("Main Course")],
                },
                ProductionUnit {
                    name: ProductionUnitName::from("Bar"),
                    branch: BranchName::from("Peacock - Second"),
                    item_groups: vec![ItemGroupName::from("Beverages")],
                },
            ],
            item_groups: [(
                ItemCode::from("BIRYANI"),
                ItemGroupName::from("Main Course"),
            )]
            .into_iter()
            .collect(),
            courses: [(
                ItemCode::from("BIRYANI"),
                MenuCourseName::from("Main Course"),
            )]
            .into_iter()
            .collect(),
            printed: vec![("INV-1".to_owned(), ProductionUnitName::from("Hot Kitchen"))],
        }
    }

    #[test]
    fn item_groups_omits_codes_it_has_no_row_for() {
        // The absence is the signal `unrouted_item_codes` reads. An error or a default
        // group here would either abort the order or route the item to a wrong station.
        let snap = snapshot();
        let got = snap
            .item_groups(&[ItemCode::from("BIRYANI"), ItemCode::from("MYSTERY")])
            .unwrap();

        assert_eq!(got.len(), 1);
        assert_eq!(
            got.get(&ItemCode::from("BIRYANI")).map(|g| g.as_str()),
            Some("Main Course")
        );
        assert!(!got.contains_key(&ItemCode::from("MYSTERY")));
    }

    #[test]
    fn list_for_branch_does_not_leak_another_branch_unit() {
        let snap = snapshot();
        let main = snap
            .list_for_branch(&BranchName::from("Peacock - Main"))
            .unwrap();
        assert_eq!(main.len(), 1);
        assert_eq!(main[0].name.as_str(), "Hot Kitchen");

        assert!(snap
            .list_for_branch(&BranchName::from("Peacock - Nowhere"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn exists_for_is_scoped_to_both_invoice_and_station() {
        let snap = snapshot();
        assert!(snap
            .exists_for("INV-1", &ProductionUnitName::from("Hot Kitchen"))
            .unwrap());
        // Same invoice, different station: that station has not printed yet, so its
        // ticket must still be a NewOrder.
        assert!(!snap
            .exists_for("INV-1", &ProductionUnitName::from("Bar"))
            .unwrap());
        // Same station, different invoice.
        assert!(!snap
            .exists_for("INV-2", &ProductionUnitName::from("Hot Kitchen"))
            .unwrap());
    }

    #[test]
    fn courses_are_served_for_known_codes_only() {
        let snap = snapshot();
        let got = snap
            .courses_for_menu(
                &RoomName::from("Hall"),
                &[ItemCode::from("BIRYANI"), ItemCode::from("MYSTERY")],
            )
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(
            got.get(&ItemCode::from("BIRYANI")).map(|c| c.as_str()),
            Some("Main Course")
        );
    }

    #[test]
    fn repos_bundle_exposes_the_same_snapshot() {
        let snap = snapshot();
        let repos = snap.repos();
        assert_eq!(
            repos
                .productions
                .list_for_branch(&BranchName::from("Peacock - Main"))
                .unwrap()
                .len(),
            1
        );
        assert!(repos
            .kots
            .exists_for("INV-1", &ProductionUnitName::from("Hot Kitchen"))
            .unwrap());
    }
}
