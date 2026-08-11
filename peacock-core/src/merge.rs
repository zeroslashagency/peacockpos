//! Table merge clustering.
//!
//! Ported from `_upstream/ury-ury/ury/ury/doctype/ury_order/ury_order.py`:
//!
//! | Upstream | Here |
//! |---|---|
//! | `_get_merge_cluster` (`ury_order.py:240`) | [`get_merge_cluster`] |
//! | `merge_tables_batch` (`ury_order.py:26`) | [`merge_tables_batch`] |
//! | `merge_free_tables` (`ury_order.py:20`) | [`merge_free_tables`] |
//! | `unmerge_tables` (`ury_order.py:458`) | [`unmerge_tables`] |
//! | symmetric write loop (`ury_order.py:110-127`) | [`plan_symmetric_writes`] |
//! | `_parse_merged_with` (`ury_order.py:217`) | [`crate::model::MergedWith::parse`] |
//!
//! ## Facts this module depends on
//!
//! - `merged_with` is a CSV `Data` field on **URY Table**, not on the order.
//! - A merge is **symmetric**: every member lists every other member. Upstream
//!   rewrites the whole cluster on each merge (`ury_order.py:110`), which is why
//!   the write plan here is "full cluster", not a delta.
//! - Clustering is **room-scoped**. A table in another room can never join
//!   (`ury_order.py:64-73`, and the BFS index at `ury_order.py:245`).
//! - Production `merged_with` data contains cycles and self-references, so the
//!   walk is a visited-set BFS and always terminates.
//!
//! ## Deviations from upstream
//!
//! 1. **One query per call.** Upstream calls `frappe.db.get_value` /
//!    `frappe.get_all` again for every hop and every target
//!    (`ury_order.py:64`, `:75`, `:88`, `:245`). Here the room is fetched once
//!    via [`TableRepo::list_by_room`] and indexed; every cluster walk reuses that
//!    index. Same result, O(1) queries.
//! 2. **No writes, no commit.** Upstream writes rows and commits inside the
//!    guard function (`ury_order.py:119`, `:138`). This module is pure: it
//!    returns the cluster and a write plan, so the SQL layer stays dumb and the
//!    rules stay testable without a database.
//! 3. **Two upstream guards throw errors that have no variant in
//!    [`crate::error::Error`]** (owned by another module): "Select at least one
//!    table to merge." (`ury_order.py:39`) and "Table is not merged."
//!    (`ury_order.py:463`). Both are degenerate no-ops here rather than silently
//!    mapped onto an unrelated error kind — see [`merge_tables_batch`] and
//!    [`unmerge_tables`] for the per-function note.
//! 4. **`unmerge_tables` removes one table** from its cluster and repairs the
//!    reciprocal references on the rest. Upstream dissolves the entire cluster
//!    (`ury_order.py:476-483`), which loses merges the operator did not ask to
//!    undo. Full dissolve is still expressible: call [`plan_symmetric_writes`]
//!    on each remaining member as its own single-table cluster.
//! 5. **Invoice-side reconciliation is out of scope.** Upstream also touches
//!    POS Invoice `custom_merged_tables` and table `occupied` /
//!    `latest_invoice_time` (`_sync_active_order_with_merge_cluster`,
//!    `ury_order.py:157`; `_reconcile_open_invoices_for_tables`, `:349`). Those
//!    are invoice concerns and live outside this module.

use crate::error::{Error, Result};
use crate::ids::{RoomName, TableName};
use crate::model::{MergedWith, Table};
use crate::ports::{OrderRepo, TableRepo};
use std::collections::{HashMap, HashSet, VecDeque};

/// A merge cluster: the connected component of `merged_with` reachable from a
/// seed table, plus the room index the walk ran against.
///
/// Upstream returns the bare tuple `(members, table_by_name)`
/// (`ury_order.py:274`); this is the same pair with names on it.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeCluster {
    room: RoomName,
    seed: TableName,
    /// Members. For [`get_merge_cluster`] this is BFS order, seed first, exactly
    /// like upstream's `members` list. For [`merge_tables_batch`] it is the
    /// post-merge cluster, sorted (`ury_order.py:108`).
    members: Vec<TableName>,
    table_by_name: HashMap<TableName, Table>,
}

impl MergeCluster {
    /// The room every member belongs to. Merges never cross this boundary.
    pub fn room(&self) -> &RoomName {
        &self.room
    }

    /// The table the walk started from.
    pub fn seed(&self) -> &TableName {
        &self.seed
    }

    /// Cluster members. Always contains at least the seed.
    pub fn members(&self) -> &[TableName] {
        &self.members
    }

    /// Members in a stable, deterministic order — what gets persisted.
    pub fn sorted_members(&self) -> Vec<TableName> {
        let mut sorted = self.members.clone();
        sorted.sort();
        sorted
    }

    /// Number of members. Never zero.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Always false; a cluster is at minimum its own seed. Present because
    /// clippy asks for it next to `len`.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// True when the seed is not merged with anything.
    pub fn is_single(&self) -> bool {
        self.members.len() == 1
    }

    pub fn contains(&self, table: &TableName) -> bool {
        self.members.iter().any(|m| m == table)
    }

    /// A table row from the room index. Includes non-members: the index covers
    /// the whole room, matching upstream's `table_by_name`.
    ///
    /// Rows reflect state **before** any merge this call planned; `members` is
    /// the authority on cluster shape, not `row.merged_with`.
    pub fn table(&self, name: &TableName) -> Option<&Table> {
        self.table_by_name.get(name)
    }

    /// The whole room index, as upstream's second return value.
    pub fn table_by_name(&self) -> &HashMap<TableName, Table> {
        &self.table_by_name
    }

    /// Rows for the cluster members only, in member order.
    pub fn member_tables(&self) -> Vec<&Table> {
        self.members
            .iter()
            .filter_map(|name| self.table_by_name.get(name))
            .collect()
    }

    /// The `(table, merged_with)` rows to persist for this cluster.
    /// Convenience for [`plan_symmetric_writes`].
    pub fn writes(&self) -> Vec<(TableName, MergedWith)> {
        plan_symmetric_writes(&self.members)
    }
}

/// What [`unmerge_tables`] decided, ready for the SQL layer.
#[derive(Debug, Clone, PartialEq)]
pub struct UnmergePlan {
    /// The table leaving the cluster. Its `merged_with` becomes empty.
    pub removed: TableName,
    /// Members that stay merged with each other, sorted. Empty when the removal
    /// dissolves the cluster (two-table merge, or the table was not merged).
    pub remaining: Vec<TableName>,
    /// Every row to write, including `removed`. Sorted by table name so the
    /// SQL layer produces a deterministic statement order and cannot deadlock
    /// against a concurrent merge on the same rows.
    pub writes: Vec<(TableName, MergedWith)>,
}

/// Build the merge cluster containing `seed`, scoped to `room`.
///
/// Port of `_get_merge_cluster` (`ury_order.py:240`). BFS over `merged_with`
/// with a visited set, so cyclic and self-referential production data
/// terminates. A `merged_with` entry naming a table that is not in this room —
/// deleted, renamed, or moved — is skipped, not an error (`ury_order.py:271`,
/// `if partner in table_by_name`).
///
/// The room is read **once** via [`TableRepo::list_by_room`]; upstream re-queries
/// per hop (`ury_order.py:245`).
///
/// # Errors
///
/// [`Error::TableNotFound`] when `seed` is not a table of `room` — upstream's
/// `_("Table not found.")` at `ury_order.py:253`.
pub fn get_merge_cluster(
    seed: &TableName,
    room: &RoomName,
    tables: &dyn TableRepo,
) -> Result<MergeCluster> {
    let index = index_room(room, tables)?;
    cluster_from_index(seed, room, index)
}

/// Merge `targets` into the cluster anchored at `anchor`.
///
/// Port of `merge_tables_batch` (`ury_order.py:26`). Returns the resulting
/// cluster; nothing is written. Feed [`MergeCluster::writes`] (or
/// [`plan_symmetric_writes`]) to the storage layer to persist it.
///
/// Guards, in upstream order:
///
/// 1. Targets are de-duplicated; blanks and the anchor itself are dropped
///    (`ury_order.py:31-37`).
/// 2. The anchor must exist (`ury_order.py:44-52`) → [`Error::TableNotFound`].
/// 3. Each target must be in the anchor's room (`ury_order.py:64-73`) →
///    [`Error::CrossRoomMerge`].
/// 4. A target must not already belong to a cluster of its own
///    (`ury_order.py:75-85`) → [`Error::AlreadyMerged`].
/// 5. A target must not be occupied (`ury_order.py:88-97`) →
///    [`Error::TableOccupied`].
/// 6. The resulting cluster must not contain more than one separate active
///    order (`ury_order.py:101-106`) → [`Error::MultipleActiveOrders`].
///
/// Deliberately **not** guarded, matching upstream: the anchor may be occupied
/// (that is the point of `merge_free_tables` — one occupied, one free,
/// `ury_order.py:21`), and the anchor may already be a multi-table cluster, so
/// a cluster can be extended.
///
/// A target that does not exist at all reports [`Error::CrossRoomMerge`], not
/// [`Error::TableNotFound`]: upstream reads the target's room with
/// `get_value`, gets `None`, and compares it to the anchor room
/// (`ury_order.py:64-73`), so a missing table takes the different-rooms branch.
/// Preserved so client error handling does not change.
///
/// An empty `targets` list (or one that dedups to nothing) returns the anchor's
/// current cluster unchanged. Upstream throws `_("Select at least one table to
/// merge.")` (`ury_order.py:39-42`); [`crate::error::Error`] has no variant for
/// it and this module owns only `merge.rs`, so the degenerate case is a no-op
/// rather than a misfiled error kind.
pub fn merge_tables_batch(
    anchor: &TableName,
    targets: &[TableName],
    tables: &dyn TableRepo,
    orders: &dyn OrderRepo,
) -> Result<MergeCluster> {
    // Guard 1: dedup, drop blanks and self-merge (ury_order.py:31-37).
    let wanted = dedup_targets(anchor, targets);

    // Guard 2: the anchor must resolve, and it decides the room for everything
    // that follows (ury_order.py:44-52).
    let anchor_row = tables.get(anchor)?;
    let room = anchor_row.restaurant_room.clone();

    let index = index_room(&room, tables)?;
    let anchor_cluster = cluster_from_index(anchor, &room, index)?;

    let mut members: Vec<TableName> = anchor_cluster.members.clone();
    let mut seen: HashSet<TableName> = members.iter().cloned().collect();
    let index = anchor_cluster.table_by_name;

    for target in &wanted {
        // Guard 3: same room. Absence from the room index means "not in this
        // room" — either genuinely elsewhere or gone.
        let target_row = index
            .get(target)
            .ok_or_else(|| Error::CrossRoomMerge {
                seed: anchor.clone(),
                target: target.clone(),
            })?;

        // Guard 4: refuse to import another merged group. Computed from the same
        // room index instead of re-querying (ury_order.py:75-85).
        let target_cluster = walk_merged_with(target, &index);
        if target_cluster.len() > 1 {
            return Err(Error::AlreadyMerged(target.clone()));
        }

        // Guard 5: occupied targets cannot be merged (ury_order.py:88-97).
        if target_row.occupied {
            return Err(Error::TableOccupied(target.clone()));
        }

        if seen.insert(target.clone()) {
            members.push(target.clone());
        }
    }

    members.sort(); // ury_order.py:108

    // Guard 6: at most one separate active order in the whole result
    // (ury_order.py:101-106). Upstream counts one `db.exists` per member
    // (`_table_has_active_order`, ury_order.py:223); the port pushes the whole
    // set into one repo call.
    let active = orders.count_separate_active(&members)?;
    if active > 1 {
        return Err(Error::MultipleActiveOrders { count: active });
    }

    Ok(MergeCluster {
        room,
        seed: anchor.clone(),
        members,
        table_by_name: index,
    })
}

/// Merge exactly two tables. Port of `merge_free_tables` (`ury_order.py:20`),
/// which is a thin wrapper over [`merge_tables_batch`] — one occupied and one
/// free is allowed because only the target is occupancy-checked.
pub fn merge_free_tables(
    table1: &TableName,
    table2: &TableName,
    tables: &dyn TableRepo,
    orders: &dyn OrderRepo,
) -> Result<MergeCluster> {
    merge_tables_batch(table1, std::slice::from_ref(table2), tables, orders)
}

/// Remove `table` from its merge cluster.
///
/// Inverse of [`merge_tables_batch`]. Because merges are symmetric and
/// bidirectional, every other member must drop its reference to `table`, so the
/// plan rewrites the whole cluster, not just one row.
///
/// Loosely based on `unmerge_tables` (`ury_order.py:458`), with two deviations:
///
/// - Upstream dissolves the entire cluster (`ury_order.py:476-483`); this
///   removes only the requested table and keeps the rest merged. Removing one
///   table from a five-table merge should not un-merge the other four.
/// - Upstream throws `_("Table is not merged.")` when the cluster is a single
///   table (`ury_order.py:463`). No matching variant exists in
///   [`crate::error::Error`], so this returns a plan whose only write clears an
///   already-empty `merged_with` — idempotent, and the caller can detect it via
///   `plan.remaining.is_empty()`.
///
/// The upstream "Cannot unmerge active tables." guard
/// (`_has_open_pos_invoices_for_cluster`, `ury_order.py:469`) is **not**
/// enforced here: it needs open-invoice lookups (including the
/// `custom_merged_tables LIKE` scan at `ury_order.py:441`) that
/// [`OrderRepo`] does not expose. Callers that need it should check active
/// orders before calling. Flagged rather than silently approximated —
/// `count_separate_active` counts unprinted draft invoices per table but not
/// invoices that reference a member only through `custom_merged_tables`.
///
/// # Errors
///
/// [`Error::TableNotFound`] when the table does not exist, or is not a table of
/// its own room (a data inconsistency).
pub fn unmerge_tables(table: &TableName, tables: &dyn TableRepo) -> Result<UnmergePlan> {
    let row = tables.get(table)?;
    let room = row.restaurant_room.clone();
    let cluster = get_merge_cluster(table, &room, tables)?;

    let mut remaining: Vec<TableName> = cluster
        .members
        .iter()
        .filter(|m| *m != table)
        .cloned()
        .collect();
    remaining.sort();

    // The leaving table always ends up with no partners.
    let mut writes = vec![(table.clone(), MergedWith::default())];

    // A single survivor is no longer merged with anything, which
    // `plan_symmetric_writes` already yields as an empty CSV.
    writes.extend(plan_symmetric_writes(&remaining));
    writes.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(UnmergePlan {
        removed: table.clone(),
        remaining,
        writes,
    })
}

/// The exact `(table, merged_with)` rows to persist for a cluster.
///
/// Port of the symmetric write loop at `ury_order.py:110-127`: every member
/// stores all *other* members. Output is sorted by table name, and each
/// [`MergedWith`] is sorted, so writes are deterministic and idempotent.
///
/// A member with no partners gets an empty [`MergedWith`]
/// (`to_csv() == ""`). Upstream writes SQL `NULL` there (`ury_order.py:124-125`);
/// the storage layer should map empty to `NULL` to keep the column's two "not
/// merged" spellings from multiplying. [`MergedWith::parse`] treats `NULL`,
/// `""` and `" , "` identically, so reads are unaffected either way.
///
/// Duplicates in `members` are collapsed; a self-reference cannot survive
/// because a member is never its own partner.
pub fn plan_symmetric_writes(members: &[TableName]) -> Vec<(TableName, MergedWith)> {
    let mut unique: Vec<TableName> = members.iter().cloned().collect::<HashSet<_>>().into_iter().collect();
    unique.sort();

    unique
        .iter()
        .map(|table| {
            let csv = unique
                .iter()
                .filter(|partner| *partner != table)
                .map(|partner| partner.as_str())
                .collect::<Vec<_>>()
                .join(",");
            // `MergedWith`'s field is private and `parse` is its only value
            // constructor, so the CSV round-trip is the supported path. It also
            // exercises the same parser production data goes through.
            (table.clone(), MergedWith::parse(Some(&csv)))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

/// One query, then an index by name — upstream's `table_by_name`
/// (`ury_order.py:245-250`).
fn index_room(room: &RoomName, tables: &dyn TableRepo) -> Result<HashMap<TableName, Table>> {
    Ok(tables
        .list_by_room(room)?
        .into_iter()
        .map(|t| (t.name.clone(), t))
        .collect())
}

fn cluster_from_index(
    seed: &TableName,
    room: &RoomName,
    table_by_name: HashMap<TableName, Table>,
) -> Result<MergeCluster> {
    if !table_by_name.contains_key(seed) {
        // ury_order.py:252-253
        return Err(Error::TableNotFound(seed.clone()));
    }

    Ok(MergeCluster {
        room: room.clone(),
        seed: seed.clone(),
        members: walk_merged_with(seed, &table_by_name),
        table_by_name,
    })
}

/// BFS over `merged_with`, room-scoped, cycle-safe (`ury_order.py:255-272`).
///
/// The visited set is what makes cyclic data (`A→B`, `B→A`) and self-references
/// (`A→A`) terminate. Partners outside the index are skipped, so a stale name
/// never pulls in a table from another room and never errors.
fn walk_merged_with(seed: &TableName, table_by_name: &HashMap<TableName, Table>) -> Vec<TableName> {
    let mut visited: HashSet<TableName> = HashSet::new();
    let mut members: Vec<TableName> = Vec::new();
    let mut queue: VecDeque<TableName> = VecDeque::new();
    queue.push_back(seed.clone());

    while let Some(name) = queue.pop_front() {
        if !visited.insert(name.clone()) {
            continue;
        }
        members.push(name.clone());

        let Some(row) = table_by_name.get(&name) else {
            continue;
        };

        for partner in row.merged_with.iter() {
            if table_by_name.contains_key(partner) && !visited.contains(partner) {
                queue.push_back(partner.clone());
            }
        }
    }

    members
}

/// `ury_order.py:31-37`: order-preserving dedup that drops blanks and the anchor.
fn dedup_targets(anchor: &TableName, targets: &[TableName]) -> Vec<TableName> {
    let mut seen: HashSet<&TableName> = HashSet::new();
    targets
        .iter()
        .filter(|t| !t.as_str().trim().is_empty())
        .filter(|t| *t != anchor)
        .filter(|t| seen.insert(t))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{BranchName, RestaurantName};

    // -----------------------------------------------------------------------
    // in-memory fakes (no mocking crate, no database)
    // -----------------------------------------------------------------------

    struct FakeTables {
        rows: Vec<Table>,
    }

    impl FakeTables {
        fn new(rows: Vec<Table>) -> Self {
            FakeTables { rows }
        }
    }

    impl TableRepo for FakeTables {
        fn list_all(&self, room: Option<&RoomName>, occupied: Option<bool>) -> Result<Vec<Table>> {
            let mut filtered: Vec<Table> = self.rows.clone();
            
            if let Some(r) = room {
                filtered.retain(|t| &t.restaurant_room == r);
            }
            
            if let Some(o) = occupied {
                filtered.retain(|t| t.occupied == o);
            }
            
            Ok(filtered)
        }

        fn list_by_room(&self, room: &RoomName) -> Result<Vec<Table>> {
            Ok(self
                .rows
                .iter()
                .filter(|t| t.restaurant_room == *room)
                .cloned()
                .collect())
        }

        fn get(&self, name: &TableName) -> Result<Table> {
            self.rows
                .iter()
                .find(|t| t.name == *name)
                .cloned()
                .ok_or_else(|| Error::TableNotFound(name.clone()))
        }
    }

    /// Counts members that carry an active order, like
    /// `_count_separate_active_orders` (ury_order.py:236).
    struct FakeOrders {
        active: Vec<TableName>,
    }

    impl FakeOrders {
        fn none() -> Self {
            FakeOrders { active: vec![] }
        }
        fn on(names: &[&str]) -> Self {
            FakeOrders {
                active: names.iter().map(|n| TableName::from(*n)).collect(),
            }
        }
    }

    impl OrderRepo for FakeOrders {
        fn count_separate_active(&self, tables: &[TableName]) -> Result<usize> {
            Ok(tables.iter().filter(|t| self.active.contains(t)).count())
        }
    }

    // -----------------------------------------------------------------------
    // builders
    // -----------------------------------------------------------------------

    fn table(name: &str, room: &str, occupied: bool, merged: Option<&str>) -> Table {
        Table {
            name: TableName::from(name),
            no_of_seats: 4,
            minimum_seating: 1,
            restaurant: RestaurantName::from("Peacock"),
            restaurant_room: RoomName::from(room),
            branch: BranchName::from("Main"),
            is_take_away: false,
            occupied,
            latest_invoice_time: None,
            table_shape: None,
            layout_x: 0.0,
            layout_y: 0.0,
            layout_width: 0.0,
            layout_height: 0.0,
            merged_with: MergedWith::parse(merged),
        }
    }

    fn free(name: &str, room: &str) -> Table {
        table(name, room, false, None)
    }

    fn names(v: &[TableName]) -> Vec<&str> {
        v.iter().map(|t| t.as_str()).collect()
    }

    fn csv_of(writes: &[(TableName, MergedWith)], want: &str) -> String {
        writes
            .iter()
            .find(|(t, _)| t.as_str() == want)
            .map(|(_, m)| m.to_csv())
            .unwrap_or_else(|| "<missing>".to_owned())
    }

    fn room_a() -> RoomName {
        RoomName::from("Hall")
    }

    // -----------------------------------------------------------------------
    // get_merge_cluster
    // -----------------------------------------------------------------------

    #[test]
    fn single_table_cluster_is_just_itself() {
        let repo = FakeTables::new(vec![free("T-01", "Hall"), free("T-02", "Hall")]);

        let cluster = get_merge_cluster(&TableName::from("T-01"), &room_a(), &repo).unwrap();

        assert_eq!(names(cluster.members()), vec!["T-01"]);
        assert!(cluster.is_single());
        // table_by_name covers the whole room, as upstream does.
        assert_eq!(cluster.table_by_name().len(), 2);
    }

    #[test]
    fn two_table_cluster_walks_both_directions() {
        let repo = FakeTables::new(vec![
            table("T-01", "Hall", false, Some("T-02")),
            table("T-02", "Hall", false, Some("T-01")),
        ]);

        for seed in ["T-01", "T-02"] {
            let cluster = get_merge_cluster(&TableName::from(seed), &room_a(), &repo).unwrap();
            assert_eq!(cluster.len(), 2, "seed {seed}");
            assert!(cluster.contains(&TableName::from("T-01")));
            assert!(cluster.contains(&TableName::from("T-02")));
        }
    }

    #[test]
    fn transitive_chain_is_one_cluster() {
        // A-B and B-C ⟹ {A, B, C}, reachable from any seed.
        let repo = FakeTables::new(vec![
            table("T-01", "Hall", false, Some("T-02")),
            table("T-02", "Hall", false, Some("T-01,T-03")),
            table("T-03", "Hall", false, Some("T-02")),
        ]);

        for seed in ["T-01", "T-02", "T-03"] {
            let cluster = get_merge_cluster(&TableName::from(seed), &room_a(), &repo).unwrap();
            assert_eq!(
                cluster.sorted_members(),
                vec![
                    TableName::from("T-01"),
                    TableName::from("T-02"),
                    TableName::from("T-03")
                ],
                "seed {seed}"
            );
        }
    }

    #[test]
    fn cyclic_merged_with_terminates() {
        // Production data contains cycles: A→B, B→A, plus a 3-cycle.
        let repo = FakeTables::new(vec![
            table("T-01", "Hall", false, Some("T-02")),
            table("T-02", "Hall", false, Some("T-01")),
            table("T-11", "Hall", false, Some("T-12")),
            table("T-12", "Hall", false, Some("T-13")),
            table("T-13", "Hall", false, Some("T-11")),
        ]);

        let two = get_merge_cluster(&TableName::from("T-01"), &room_a(), &repo).unwrap();
        assert_eq!(two.len(), 2);

        let three = get_merge_cluster(&TableName::from("T-11"), &room_a(), &repo).unwrap();
        assert_eq!(three.len(), 3);
        // Each member appears exactly once despite the cycle.
        let mut sorted = three.sorted_members();
        sorted.dedup();
        assert_eq!(sorted.len(), 3);
    }

    #[test]
    fn self_reference_is_tolerated() {
        let repo = FakeTables::new(vec![table("T-01", "Hall", false, Some("T-01"))]);

        let cluster = get_merge_cluster(&TableName::from("T-01"), &room_a(), &repo).unwrap();

        assert_eq!(names(cluster.members()), vec!["T-01"]);
    }

    #[test]
    fn merged_with_naming_a_table_outside_the_room_is_ignored() {
        // T-99 does not exist; T-50 exists but in another room. Neither is an
        // error and neither joins (ury_order.py:271).
        let repo = FakeTables::new(vec![
            table("T-01", "Hall", false, Some("T-99,T-50,T-02")),
            free("T-02", "Hall"),
            free("T-50", "Patio"),
        ]);

        let cluster = get_merge_cluster(&TableName::from("T-01"), &room_a(), &repo).unwrap();

        assert_eq!(
            cluster.sorted_members(),
            vec![TableName::from("T-01"), TableName::from("T-02")]
        );
    }

    #[test]
    fn seed_outside_the_room_is_table_not_found() {
        let repo = FakeTables::new(vec![free("T-01", "Hall"), free("T-50", "Patio")]);

        let err = get_merge_cluster(&TableName::from("T-50"), &room_a(), &repo).unwrap_err();

        assert_eq!(err, Error::TableNotFound(TableName::from("T-50")));
    }

    // -----------------------------------------------------------------------
    // merge_tables_batch
    // -----------------------------------------------------------------------

    #[test]
    fn merges_two_tables() {
        let repo = FakeTables::new(vec![free("T-01", "Hall"), free("T-02", "Hall")]);

        let cluster = merge_tables_batch(
            &TableName::from("T-01"),
            &[TableName::from("T-02")],
            &repo,
            &FakeOrders::none(),
        )
        .unwrap();

        assert_eq!(names(&cluster.sorted_members()), vec!["T-01", "T-02"]);

        let writes = cluster.writes();
        assert_eq!(writes.len(), 2);
        assert_eq!(csv_of(&writes, "T-01"), "T-02");
        assert_eq!(csv_of(&writes, "T-02"), "T-01");
    }

    #[test]
    fn merge_free_tables_allows_one_occupied_anchor() {
        // ury_order.py:21 — the anchor is intentionally not occupancy-checked.
        let repo = FakeTables::new(vec![
            table("T-01", "Hall", true, None),
            free("T-02", "Hall"),
        ]);

        let cluster = merge_free_tables(
            &TableName::from("T-01"),
            &TableName::from("T-02"),
            &repo,
            &FakeOrders::on(&["T-01"]),
        )
        .unwrap();

        assert_eq!(cluster.len(), 2);
    }

    #[test]
    fn batch_merge_extends_an_existing_anchor_cluster_transitively() {
        // Anchor already merged with T-02; adding T-03 yields {T-01,T-02,T-03}.
        let repo = FakeTables::new(vec![
            table("T-01", "Hall", false, Some("T-02")),
            table("T-02", "Hall", false, Some("T-01")),
            free("T-03", "Hall"),
        ]);

        let cluster = merge_tables_batch(
            &TableName::from("T-01"),
            &[TableName::from("T-03")],
            &repo,
            &FakeOrders::none(),
        )
        .unwrap();

        assert_eq!(names(&cluster.sorted_members()), vec!["T-01", "T-02", "T-03"]);
        let writes = cluster.writes();
        assert_eq!(csv_of(&writes, "T-03"), "T-01,T-02");
        assert_eq!(csv_of(&writes, "T-02"), "T-01,T-03");
    }

    #[test]
    fn cross_room_merge_is_rejected() {
        let repo = FakeTables::new(vec![free("T-01", "Hall"), free("T-50", "Patio")]);

        let err = merge_tables_batch(
            &TableName::from("T-01"),
            &[TableName::from("T-50")],
            &repo,
            &FakeOrders::none(),
        )
        .unwrap_err();

        assert_eq!(
            err,
            Error::CrossRoomMerge {
                seed: TableName::from("T-01"),
                target: TableName::from("T-50"),
            }
        );
    }

    #[test]
    fn missing_target_reports_cross_room_like_upstream() {
        // ury_order.py:64-73: get_value returns None, which != room.
        let repo = FakeTables::new(vec![free("T-01", "Hall")]);

        let err = merge_tables_batch(
            &TableName::from("T-01"),
            &[TableName::from("T-99")],
            &repo,
            &FakeOrders::none(),
        )
        .unwrap_err();

        assert_eq!(
            err,
            Error::CrossRoomMerge {
                seed: TableName::from("T-01"),
                target: TableName::from("T-99"),
            }
        );
    }

    #[test]
    fn occupied_target_is_rejected() {
        let repo = FakeTables::new(vec![
            free("T-01", "Hall"),
            table("T-02", "Hall", true, None),
        ]);

        let err = merge_tables_batch(
            &TableName::from("T-01"),
            &[TableName::from("T-02")],
            &repo,
            &FakeOrders::none(),
        )
        .unwrap_err();

        assert_eq!(err, Error::TableOccupied(TableName::from("T-02")));
    }

    #[test]
    fn already_merged_target_is_rejected() {
        let repo = FakeTables::new(vec![
            free("T-01", "Hall"),
            table("T-02", "Hall", false, Some("T-03")),
            table("T-03", "Hall", false, Some("T-02")),
        ]);

        let err = merge_tables_batch(
            &TableName::from("T-01"),
            &[TableName::from("T-02")],
            &repo,
            &FakeOrders::none(),
        )
        .unwrap_err();

        assert_eq!(err, Error::AlreadyMerged(TableName::from("T-02")));
    }

    #[test]
    fn already_merged_check_runs_before_occupancy() {
        // Upstream order: cluster size (ury_order.py:80) then occupied (:94).
        let repo = FakeTables::new(vec![
            free("T-01", "Hall"),
            table("T-02", "Hall", true, Some("T-03")),
            table("T-03", "Hall", false, Some("T-02")),
        ]);

        let err = merge_tables_batch(
            &TableName::from("T-01"),
            &[TableName::from("T-02")],
            &repo,
            &FakeOrders::none(),
        )
        .unwrap_err();

        assert_eq!(err, Error::AlreadyMerged(TableName::from("T-02")));
    }

    #[test]
    fn two_active_orders_are_rejected() {
        // T-02 is free but its cluster-mate already carries an order elsewhere:
        // anchor active + target active ⟹ 2 separate orders.
        let repo = FakeTables::new(vec![
            table("T-01", "Hall", true, None),
            free("T-02", "Hall"),
        ]);

        let err = merge_tables_batch(
            &TableName::from("T-01"),
            &[TableName::from("T-02")],
            &repo,
            &FakeOrders::on(&["T-01", "T-02"]),
        )
        .unwrap_err();

        assert_eq!(err, Error::MultipleActiveOrders { count: 2 });
    }

    #[test]
    fn one_active_order_is_allowed() {
        let repo = FakeTables::new(vec![
            table("T-01", "Hall", true, None),
            free("T-02", "Hall"),
        ]);

        let cluster = merge_tables_batch(
            &TableName::from("T-01"),
            &[TableName::from("T-02")],
            &repo,
            &FakeOrders::on(&["T-01"]),
        )
        .unwrap();

        assert_eq!(cluster.len(), 2);
    }

    #[test]
    fn missing_anchor_is_table_not_found() {
        let repo = FakeTables::new(vec![free("T-02", "Hall")]);

        let err = merge_tables_batch(
            &TableName::from("T-99"),
            &[TableName::from("T-02")],
            &repo,
            &FakeOrders::none(),
        )
        .unwrap_err();

        assert_eq!(err, Error::TableNotFound(TableName::from("T-99")));
    }

    #[test]
    fn duplicate_and_self_targets_are_deduped() {
        let repo = FakeTables::new(vec![
            free("T-01", "Hall"),
            free("T-02", "Hall"),
            free("T-03", "Hall"),
        ]);

        let cluster = merge_tables_batch(
            &TableName::from("T-01"),
            &[
                TableName::from("T-02"),
                TableName::from("T-02"),
                TableName::from("T-01"), // self, dropped
                TableName::from(""),     // blank, dropped
                TableName::from("T-03"),
            ],
            &repo,
            &FakeOrders::none(),
        )
        .unwrap();

        assert_eq!(names(&cluster.sorted_members()), vec!["T-01", "T-02", "T-03"]);
    }

    #[test]
    fn empty_target_list_is_a_no_op_returning_the_anchor_cluster() {
        // Deviation: upstream throws "Select at least one table to merge."
        // (ury_order.py:39). No typed error variant exists for it.
        let repo = FakeTables::new(vec![free("T-01", "Hall")]);

        let cluster = merge_tables_batch(
            &TableName::from("T-01"),
            &[],
            &repo,
            &FakeOrders::none(),
        )
        .unwrap();

        assert_eq!(names(cluster.members()), vec!["T-01"]);
    }

    // -----------------------------------------------------------------------
    // unmerge_tables
    // -----------------------------------------------------------------------

    #[test]
    fn unmerge_removes_reciprocal_references_symmetrically() {
        // {T-01,T-02,T-03}; drop T-02 ⟹ T-01 and T-03 stay merged, and neither
        // still points at T-02.
        let repo = FakeTables::new(vec![
            table("T-01", "Hall", false, Some("T-02,T-03")),
            table("T-02", "Hall", false, Some("T-01,T-03")),
            table("T-03", "Hall", false, Some("T-01,T-02")),
        ]);

        let plan = unmerge_tables(&TableName::from("T-02"), &repo).unwrap();

        assert_eq!(plan.removed, TableName::from("T-02"));
        assert_eq!(names(&plan.remaining), vec!["T-01", "T-03"]);
        assert_eq!(plan.writes.len(), 3);
        assert_eq!(csv_of(&plan.writes, "T-02"), "");
        assert_eq!(csv_of(&plan.writes, "T-01"), "T-03");
        assert_eq!(csv_of(&plan.writes, "T-03"), "T-01");
        for (_, merged) in &plan.writes {
            assert!(!merged.contains(&TableName::from("T-02")));
        }
    }

    #[test]
    fn unmerge_of_a_pair_clears_both_sides() {
        let repo = FakeTables::new(vec![
            table("T-01", "Hall", false, Some("T-02")),
            table("T-02", "Hall", false, Some("T-01")),
        ]);

        let plan = unmerge_tables(&TableName::from("T-01"), &repo).unwrap();

        assert_eq!(names(&plan.remaining), vec!["T-02"]);
        assert_eq!(csv_of(&plan.writes, "T-01"), "");
        assert_eq!(csv_of(&plan.writes, "T-02"), "");
    }

    #[test]
    fn unmerge_of_an_unmerged_table_is_an_idempotent_no_op() {
        // Deviation: upstream throws "Table is not merged." (ury_order.py:463).
        let repo = FakeTables::new(vec![free("T-01", "Hall")]);

        let plan = unmerge_tables(&TableName::from("T-01"), &repo).unwrap();

        assert!(plan.remaining.is_empty());
        assert_eq!(plan.writes.len(), 1);
        assert_eq!(csv_of(&plan.writes, "T-01"), "");
    }

    #[test]
    fn unmerge_of_a_missing_table_is_table_not_found() {
        let repo = FakeTables::new(vec![free("T-01", "Hall")]);

        let err = unmerge_tables(&TableName::from("T-99"), &repo).unwrap_err();

        assert_eq!(err, Error::TableNotFound(TableName::from("T-99")));
    }

    #[test]
    fn unmerge_survives_cyclic_data() {
        let repo = FakeTables::new(vec![
            table("T-01", "Hall", false, Some("T-02,T-02")),
            table("T-02", "Hall", false, Some("T-01,T-01")),
        ]);

        let plan = unmerge_tables(&TableName::from("T-01"), &repo).unwrap();

        assert_eq!(names(&plan.remaining), vec!["T-02"]);
    }

    // -----------------------------------------------------------------------
    // plan_symmetric_writes
    // -----------------------------------------------------------------------

    #[test]
    fn write_plan_is_symmetric_sorted_and_deduped() {
        let writes = plan_symmetric_writes(&[
            TableName::from("T-03"),
            TableName::from("T-01"),
            TableName::from("T-03"),
            TableName::from("T-02"),
        ]);

        assert_eq!(
            names(&writes.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>()),
            vec!["T-01", "T-02", "T-03"]
        );
        assert_eq!(csv_of(&writes, "T-01"), "T-02,T-03");
        assert_eq!(csv_of(&writes, "T-02"), "T-01,T-03");
        assert_eq!(csv_of(&writes, "T-03"), "T-01,T-02");

        // Nobody is their own partner.
        for (table, merged) in &writes {
            assert!(!merged.contains(table));
        }
    }

    #[test]
    fn write_plan_for_a_lone_table_clears_the_column() {
        let writes = plan_symmetric_writes(&[TableName::from("T-01")]);

        assert_eq!(writes.len(), 1);
        // Empty CSV; the SQL layer maps this to NULL (ury_order.py:124-125).
        assert_eq!(csv_of(&writes, "T-01"), "");
    }

    #[test]
    fn write_plan_round_trips_through_the_cluster_walk() {
        // Persisting a plan then re-walking must reproduce the same cluster.
        let planned = plan_symmetric_writes(&[
            TableName::from("T-01"),
            TableName::from("T-02"),
            TableName::from("T-03"),
        ]);

        let rows: Vec<Table> = planned
            .iter()
            .map(|(name, merged)| table(name.as_str(), "Hall", false, Some(&merged.to_csv())))
            .collect();
        let repo = FakeTables::new(rows);

        let cluster = get_merge_cluster(&TableName::from("T-01"), &room_a(), &repo).unwrap();

        assert_eq!(names(&cluster.sorted_members()), vec!["T-01", "T-02", "T-03"]);
    }
}
