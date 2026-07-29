//! KOT (Kitchen Order Ticket) station routing.
//!
//! Ported from `_upstream/ury-ury/ury/ury/api/ury_kot_generate.py`:
//!
//! | Upstream | Here |
//! |---|---|
//! | `process_items_for_kot` (`ury_kot_generate.py:111`, 8 args) | [`route_items_to_stations`] |
//! | `create_kot_doc` (`ury_kot_generate.py:29`) | [`route_items_to_stations`] (builds the doc, does not write) |
//! | `process_items_for_cancel_kot` (`ury_kot_generate.py:188`) | [`route_cancel_items_to_stations`] |
//! | `create_cancel_kot_doc` (`ury_kot_generate.py:234`) | [`route_cancel_items_to_stations`] |
//! | item-group filter (`ury_kot_generate.py:148-160`, `:206-220`) | [`route_items_to_stations`] internals |
//!
//! ## What routing is
//!
//! One order fans out to one ticket **per production unit** (station). A line lands
//! on a station when the line's ERPNext `Item.item_group` is listed in that station's
//! `ury_production_item_groups` child table. Stations may share an item group, so a
//! line can legitimately print at two stations.
//!
//! ## Query budget
//!
//! Routing a 12-item / 3-station order issues **3 batched lookups** — one
//! [`ProductionRepo::list_for_branch`], one [`ItemRepo::item_groups`], one
//! [`MenuRepo::courses_for_menu`] — and that count does **not** grow with the
//! number of items or the number of stations. Upstream issued 36 item lookups for
//! the same order (bugs 6 and 7, GROUND-TRUTH.md).
//!
//! On the new-order path each *emitted* ticket adds one indexed
//! [`KotRepo::exists_for`] EXISTS probe, because the `New Order` → `Order Modified`
//! flip is defined per invoice **and** production unit (`ury_kot_generate.py:159`)
//! and [`KotRepo`] exposes no batched form. That is one probe per station that
//! actually receives items — never per item, and stations with no items are not
//! probed at all. The cancel path issues no probes, so it stays at 3 calls flat.
//!
//! ## Bugs fixed here
//!
//! - **Bug 1** (`ury_kot_validation.py:51`): `production_items = []` was allocated
//!   before the `for p in productions:` loop (`:57`) and appended without reset
//!   (`:69`), so station B's ticket carried station A's items and every station
//!   accumulated all prior stations'. Fixed by building the per-station vector
//!   fresh inside each production-unit iteration.
//! - **Bugs 6 and 7** (`ury_kot_generate.py:154`, `:214`): `frappe.db.get_value("Item", …)`
//!   and the worse `frappe.get_doc("Item", …)` ran inside a list comprehension, per
//!   item per station. Fixed by one batched [`ItemRepo::item_groups`] call before the
//!   station loop.
//!
//! ## Deviations from upstream
//!
//! 1. **Pure.** Upstream inserts and submits documents inside the routing function
//!    (`ury_kot_generate.py:83`, `:318`). Here routing returns [`Kot`] values with
//!    `name: None`; persisting them is the storage layer's job. Keeps the rules
//!    testable with no database.
//! 2. **The flip is computed per station, not carried between stations.** Upstream
//!    reassigns the shared `kot_type` local inside the loop (`ury_kot_generate.py:168`),
//!    so once any station flips to `Order Modified` every later station in the same
//!    call inherits it, even one that has never been printed. Here each station's
//!    type is derived from its own probe.
//! 3. **No production units for the branch returns no tickets.** Upstream
//!    `frappe.throw`s (`ury_kot_generate.py:182`); [`crate::error::Error`] has no
//!    variant for it and this module owns only `kot.rs`, so the degenerate case is an
//!    empty result rather than a misfiled error kind. Callers can detect it via an
//!    empty `Vec` for a non-empty order.
//! 4. **An item whose group matches no station is dropped**, matching upstream: the
//!    per-station comprehension simply never selects it (`ury_kot_generate.py:151-156`).
//!    Upstream additionally `msgprint`s an advisory (`ury_kot_generate.py:131-137`) —
//!    advisory only, it does not abort and does not route the item anywhere. The Rust
//!    port has no message bus, so [`unrouted_item_codes`] exposes the same information
//!    for a caller that wants to surface it.
//! 5. **Course lookup needs a room.** Upstream falls back to the restaurant's
//!    `active_menu` for takeaway orders with no table (`ury_kot_generate.py:69`,
//!    `:301`). [`MenuRepo`] is room-scoped only, so with `room: None` course is left
//!    `None` rather than guessed. Flagged instead of silently approximated.
//! 6. **`original_kot` de-dup keeps first-seen order.** Upstream uses
//!    `[*set(original_kots)]` (`ury_kot_generate.py:275`), whose order is arbitrary;
//!    the CSV is otherwise identical.
//! 7. **The KOT list for `original_kot` is supplied by the caller** as
//!    [`ExistingKot`] values. Upstream queries it inline (`ury_kot_generate.py:251`)
//!    and then re-loads every KOT document inside a nested loop (`:264`) — an N×M
//!    document load. [`crate::ports`] has no list-KOTs port, so the caller passes the
//!    rows it already has.

use crate::error::Result;
use crate::ids::{
    BranchName, CustomerName, ItemCode, ItemGroupName, KotName, PosProfileName,
    ProductionUnitName, RoomName, TableName,
};
use crate::model::{Kot, KotItem, KotType, OrderLine, ProductionUnit};
use crate::ports::{ItemRepo, KotRepo, MenuRepo, ProductionRepo};
use chrono::{NaiveDate, NaiveTime};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

/// Everything about the invoice that ends up copied onto each ticket.
///
/// Upstream reads these off the POS Invoice and POS Profile inside
/// `create_kot_doc` (`ury_kot_generate.py:40-59`) and `create_cancel_kot_doc`
/// (`:246-291`), which is why that function needs a live database. Gathering them
/// into one value keeps routing pure and keeps the argument list from growing to
/// upstream's 8-10 positional parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct KotContext {
    /// POS Invoice name. `Data` on `URY KOT`, not a Link — see [`Kot::invoice`].
    pub invoice: String,
    /// Resolved from `POS Profile.branch` (`ury_kot_generate.py:123`); decides which
    /// production units are in play.
    pub branch: BranchName,
    /// `POS Profile.custom_kot_naming_series`, or the `CNCL-` prefixed variant for
    /// cancellations (`ury_kot_generate.py:345`).
    pub naming_series: String,
    /// Business date of the ticket.
    pub date: NaiveDate,
    pub time: Option<NaiveTime>,
    pub restaurant_table: Option<TableName>,
    /// Room of `restaurant_table`. Scopes the course lookup to the room's menu
    /// (`ury_kot_generate.py:63-67`). `None` for takeaway — see deviation 5.
    pub room: Option<RoomName>,
    pub customer_name: Option<CustomerName>,
    pub pos_profile: Option<PosProfileName>,
    pub comments: Option<String>,
    /// `POS Invoice.custom_ury_order_number` (`ury_kot_generate.py:41`).
    pub order_no: Option<String>,
    pub table_takeaway: bool,
    /// `POS Invoice.order_type == "Aggregators"` (`ury_kot_generate.py:43`).
    pub is_aggregator: bool,
    pub aggregator_id: Option<String>,
}

impl KotContext {
    /// The four fields with no sensible default. Everything else starts empty and
    /// is set field-by-field.
    pub fn new(
        invoice: impl Into<String>,
        branch: BranchName,
        naming_series: impl Into<String>,
        date: NaiveDate,
    ) -> Self {
        KotContext {
            invoice: invoice.into(),
            branch,
            naming_series: naming_series.into(),
            date,
            time: None,
            restaurant_table: None,
            room: None,
            customer_name: None,
            pos_profile: None,
            comments: None,
            order_no: None,
            table_takeaway: false,
            is_aggregator: false,
            aggregator_id: None,
        }
    }
}

/// The four ports routing needs, bundled so the signatures stay readable.
pub struct KotRepos<'a> {
    /// Batched item-group lookup. The fix for bugs 6 and 7.
    pub items: &'a dyn ItemRepo,
    /// Production units for the branch (`ury_kot_generate.py:123`).
    pub productions: &'a dyn ProductionRepo,
    /// Drives the `New Order` → `Order Modified` flip (`ury_kot_generate.py:159`).
    pub kots: &'a dyn KotRepo,
    /// Course per item, scoped to the room's menu (`ury_kot_generate.py:72`).
    pub menu: &'a dyn MenuRepo,
}

/// Which kind of cancellation is being printed.
///
/// Upstream passes the label as a bare string from two call sites — `"Cancelled"`
/// for a whole-order cancel (`ury_order.py:1333`) and `"Partially cancelled"` for a
/// line-level change (`ury_kot_generate.py:375`) — so an unrelated string reaches the
/// document unchecked. An enum makes the invalid case unrepresentable, which is why
/// this takes no `KotType` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelKind {
    /// The whole order is being voided (`ury_order.py:1325-1334`, `cancel_kot`).
    WholeOrder,
    /// Some lines were reduced or removed (`ury_kot_generate.py:367-377`).
    Partial,
}

impl CancelKind {
    /// The `URY KOT.type` this prints as.
    pub fn kot_type(self) -> KotType {
        match self {
            CancelKind::WholeOrder => KotType::Cancelled,
            CancelKind::Partial => KotType::PartiallyCancelled,
        }
    }
}

/// A submitted `New Order` / `Order Modified` KOT, as far as back-linking cares.
///
/// Supplied by the caller; see deviation 7. `items` is the ticket's `kot_items`
/// item codes.
#[derive(Debug, Clone, PartialEq)]
pub struct ExistingKot {
    pub name: KotName,
    pub items: Vec<ItemCode>,
}

impl ExistingKot {
    /// Convenience constructor for a row read out of storage.
    pub fn new(name: KotName, items: Vec<ItemCode>) -> Self {
        ExistingKot { name, items }
    }

    fn contains(&self, item: &ItemCode) -> bool {
        self.items.iter().any(|i| i == item)
    }
}

/// The distinct item codes an order touches, in first-seen order.
///
/// This is the prefetch key set: hand it to [`ItemRepo::item_groups`] and
/// [`MenuRepo::courses_for_menu`] once instead of querying per line per station
/// (`ury_kot_generate.py:154`, `:214`). Duplicated lines — the same item added
/// twice with different comments, which the POS does allow — collapse to one code.
pub fn required_item_codes(lines: &[OrderLine]) -> Vec<ItemCode> {
    let mut seen: HashSet<&ItemCode> = HashSet::new();
    lines
        .iter()
        .filter(|line| seen.insert(&line.item_code))
        .map(|line| line.item_code.clone())
        .collect()
}

/// Item codes that route to no station at all, in first-seen order.
///
/// Upstream warns about these with `frappe.msgprint`
/// (`ury_kot_generate.py:131-137`) and then drops them — the ticket comprehension
/// never selects them, and nothing aborts. Routing here keeps that behaviour
/// (deviation 4); this function exists so a caller can surface the same advisory.
/// An item missing from `groups` (no `item_group` on the ERPNext Item) counts as
/// unrouted, exactly as upstream's `None not in productionItemGroups`.
pub fn unrouted_item_codes(
    lines: &[OrderLine],
    units: &[ProductionUnit],
    groups: &HashMap<ItemCode, ItemGroupName>,
) -> Vec<ItemCode> {
    required_item_codes(lines)
        .into_iter()
        .filter(|code| match groups.get(code) {
            None => true,
            Some(group) => !units.iter().any(|u| u.item_groups.contains(group)),
        })
        .collect()
}

/// Fan an order out to one [`Kot`] per production unit that has work to do.
///
/// Port of `process_items_for_kot` (`ury_kot_generate.py:111`) fused with
/// `create_kot_doc` (`:29`). Nothing is written: each returned [`Kot`] has
/// `name: None` and is ready for the storage layer to insert and submit.
///
/// ## Behaviour
///
/// - Lines group to a station by `Item.item_group` ∈ the unit's `item_groups`
///   (`ury_kot_generate.py:148-156`).
/// - A station with no matching lines produces **no** ticket (upstream's
///   `if production_items:`, `ury_kot_generate.py:158`).
/// - A line whose group is listed by two stations appears on **both** tickets, since
///   each station filters the full order independently.
/// - A line whose group matches no station is dropped; see [`unrouted_item_codes`].
/// - `kot_type` starts at [`KotType::NewOrder`] and flips to
///   [`KotType::OrderModified`] for a station that already has a submitted KOT for
///   this invoice (`ury_kot_generate.py:159-168`), independently per station.
/// - `course` comes from the room's menu (`ury_kot_generate.py:72`); `None` when the
///   item has no course there, or when `ctx.room` is `None` (deviation 5).
/// - An empty order, or a branch with no production units, yields no tickets
///   (deviation 3).
///
/// ## Query budget
///
/// 3 batched lookups regardless of item count, plus one [`KotRepo::exists_for`]
/// probe per emitted ticket. See the module docs.
///
/// # Errors
///
/// Propagates repo failures unchanged. Routing itself cannot fail.
pub fn route_items_to_stations(
    ctx: &KotContext,
    lines: &[OrderLine],
    repos: &KotRepos<'_>,
) -> Result<Vec<Kot>> {
    if lines.is_empty() {
        return Ok(Vec::new());
    }

    let units = repos.productions.list_for_branch(&ctx.branch)?;
    if units.is_empty() {
        // Deviation 3: upstream frappe.throw at ury_kot_generate.py:182.
        return Ok(Vec::new());
    }

    let codes = required_item_codes(lines);
    // FIX BUGS 6 AND 7: one batched lookup for the whole order, hoisted above the
    // station loop. Upstream ran `frappe.db.get_value("Item", …)` per item per
    // station (ury_kot_generate.py:154) and `frappe.get_doc("Item", …)` — a full
    // document load — in the same pattern (ury_kot_generate.py:214).
    let groups = repos.items.item_groups(&codes)?;
    let courses = courses_for(ctx, &codes, repos)?;

    let mut tickets = Vec::new();

    for unit in &units {
        // FIX BUG 1: the per-station vector is allocated HERE, inside the loop.
        // Upstream allocated it once before the loop (ury_kot_validation.py:51,
        // loop at :57) and appended without reset (:69), so station B's ticket
        // carried station A's items and each station accumulated all prior ones.
        let station_items: Vec<KotItem> = lines
            .iter()
            .filter(|line| routes_to(unit, &line.item_code, &groups))
            .map(|line| new_order_item(line, &courses))
            .collect();

        // ury_kot_generate.py:158 — no items, no ticket.
        if station_items.is_empty() {
            continue;
        }

        // Deviation 2: probed per station instead of mutating one shared local
        // (ury_kot_generate.py:168), so one modified station cannot mislabel the
        // stations routed after it.
        let kot_type = if repos.kots.exists_for(&ctx.invoice, &unit.name)? {
            KotType::OrderModified
        } else {
            KotType::NewOrder
        };

        tickets.push(build_kot(ctx, &unit.name, kot_type, None, station_items));
    }

    Ok(tickets)
}

/// Fan a cancellation out to one [`Kot`] per production unit that printed the items.
///
/// Port of `process_items_for_cancel_kot` (`ury_kot_generate.py:188`) fused with
/// `create_cancel_kot_doc` (`:234`).
///
/// - `cancel_lines` are the reduced or removed lines. `qty` is taken as a magnitude:
///   upstream stores `abs(int(cancelItem["qty"]))` (`ury_kot_generate.py:311`)
///   because `kot_execute` passes negative deltas (`:353`) while `cancel_kot` passes
///   positive quantities (`ury_order.py:1311-1316`).
/// - `invoice_lines` are the invoice's current lines. `KotItem::quantity` is the
///   **ordered** quantity read from here, while `cancelled_qty` is the amount being
///   cancelled (`ury_kot_generate.py:304-313`). A cancel line with no matching
///   invoice line is skipped, matching upstream's inner `if` — the row is simply
///   never appended. If that empties a station, no ticket is produced for it.
/// - `existing` is the invoice's submitted `New Order` / `Order Modified` tickets.
///   [`Kot::original_kot`] is the CSV of the first ticket found to contain each
///   cancelled item (`ury_kot_generate.py:260-276`), de-duplicated; `None` when no
///   prior ticket matches.
/// - `kind` selects [`KotType::Cancelled`] vs [`KotType::PartiallyCancelled`]. The
///   cancel path never flips to `Order Modified`, so it issues no
///   [`KotRepo::exists_for`] probes and stays at 3 repo calls.
///
/// # Errors
///
/// Propagates repo failures unchanged.
pub fn route_cancel_items_to_stations(
    ctx: &KotContext,
    cancel_lines: &[OrderLine],
    invoice_lines: &[OrderLine],
    existing: &[ExistingKot],
    kind: CancelKind,
    repos: &KotRepos<'_>,
) -> Result<Vec<Kot>> {
    if cancel_lines.is_empty() {
        return Ok(Vec::new());
    }

    let units = repos.productions.list_for_branch(&ctx.branch)?;
    if units.is_empty() {
        return Ok(Vec::new());
    }

    let codes = required_item_codes(cancel_lines);
    // FIX BUG 7: upstream loaded a full Item document per item per station here
    // (`frappe.get_doc("Item", …)`, ury_kot_generate.py:214) — the same N+1 as the
    // new-order path but with document loads instead of single-field reads.
    let groups = repos.items.item_groups(&codes)?;
    let courses = courses_for(ctx, &codes, repos)?;

    let ordered_qty: HashMap<&ItemCode, Decimal> = invoice_lines
        .iter()
        .map(|line| (&line.item_code, line.qty))
        .collect();

    let mut tickets = Vec::new();

    for unit in &units {
        // FIX BUG 1 (cancel path): fresh per station, same reasoning as
        // route_items_to_stations — ury_kot_validation.py:51.
        let station_lines: Vec<&OrderLine> = cancel_lines
            .iter()
            .filter(|line| routes_to(unit, &line.item_code, &groups))
            .collect();

        let station_items: Vec<KotItem> = station_lines
            .iter()
            .filter_map(|line| {
                // ury_kot_generate.py:304 — only cancel lines that still match an
                // invoice line get a row, because `quantity` comes from the invoice.
                let ordered = ordered_qty.get(&line.item_code)?;
                Some(cancel_order_item(line, *ordered, &courses))
            })
            .collect();

        // ury_kot_generate.py:218 — no items, no ticket.
        if station_items.is_empty() {
            continue;
        }

        let original_kot = back_link(&station_items, existing);

        tickets.push(build_kot(
            ctx,
            &unit.name,
            kind.kot_type(),
            original_kot,
            station_items,
        ));
    }

    Ok(tickets)
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

/// Course per item for the room's active menu, or an empty map for takeaway.
/// One call for the whole order (`ury_kot_generate.py:72` ran one per item).
fn courses_for(
    ctx: &KotContext,
    codes: &[ItemCode],
    repos: &KotRepos<'_>,
) -> Result<HashMap<ItemCode, crate::ids::MenuCourseName>> {
    match &ctx.room {
        Some(room) => repos.menu.courses_for_menu(room, codes),
        // Deviation 5: no room, no room-scoped menu. Upstream would fall back to
        // URY Restaurant.active_menu (ury_kot_generate.py:69).
        None => Ok(HashMap::new()),
    }
}

/// `Item.item_group in production.item_groups` (`ury_kot_generate.py:154`, `:214`),
/// answered from the prefetched map instead of a query. An item with no group never
/// routes, matching upstream's `None not in productionItemGroups`.
fn routes_to(
    unit: &ProductionUnit,
    code: &ItemCode,
    groups: &HashMap<ItemCode, ItemGroupName>,
) -> bool {
    groups
        .get(code)
        .is_some_and(|group| unit.item_groups.contains(group))
}

fn new_order_item(
    line: &OrderLine,
    courses: &HashMap<ItemCode, crate::ids::MenuCourseName>,
) -> KotItem {
    KotItem {
        item: line.item_code.clone(),
        item_name: line.item_name.clone(),
        quantity: line.qty,
        cancelled_qty: Decimal::ZERO,
        comments: line.comments.clone(),
        course: courses.get(&line.item_code).cloned(),
        serve_priority: line.serve_priority,
        indicate_course: line.indicate_course,
    }
}

/// `quantity` is what was ordered, `cancelled_qty` is what is being cancelled
/// (`ury_kot_generate.py:310-313`). The magnitude matches upstream's `abs(...)`.
fn cancel_order_item(
    line: &OrderLine,
    ordered: Decimal,
    courses: &HashMap<ItemCode, crate::ids::MenuCourseName>,
) -> KotItem {
    KotItem {
        item: line.item_code.clone(),
        item_name: line.item_name.clone(),
        quantity: ordered,
        cancelled_qty: line.qty.abs(),
        comments: line.comments.clone(),
        course: courses.get(&line.item_code).cloned(),
        serve_priority: line.serve_priority,
        indicate_course: line.indicate_course,
    }
}

/// CSV of the tickets being cancelled (`ury_kot_generate.py:260-276`): first
/// matching ticket per cancelled item, de-duplicated, first-seen order (deviation 6).
fn back_link(station_items: &[KotItem], existing: &[ExistingKot]) -> Option<String> {
    let mut seen: HashSet<&KotName> = HashSet::new();
    let mut names: Vec<&str> = Vec::new();

    for item in station_items {
        // upstream `break` — only the first ticket carrying the item is linked.
        if let Some(kot) = existing.iter().find(|kot| kot.contains(&item.item)) {
            if seen.insert(&kot.name) {
                names.push(kot.name.as_str());
            }
        }
    }

    if names.is_empty() {
        // Upstream writes "" (ury_kot_generate.py:276). `None` is the same fact
        // spelled once instead of twice.
        None
    } else {
        Some(names.join(","))
    }
}

/// The `URY KOT` document upstream builds with `frappe.get_doc({...})`
/// (`ury_kot_generate.py:46-60`, `:277-292`). `name` is `None`: Frappe's naming
/// series assigns it on insert, so it belongs to the storage layer.
fn build_kot(
    ctx: &KotContext,
    production: &ProductionUnitName,
    kot_type: KotType,
    original_kot: Option<String>,
    kot_items: Vec<KotItem>,
) -> Kot {
    Kot {
        name: None,
        naming_series: ctx.naming_series.clone(),
        invoice: ctx.invoice.clone(),
        restaurant_table: ctx.restaurant_table.clone(),
        customer_name: ctx.customer_name.clone(),
        original_kot,
        date: ctx.date,
        time: ctx.time,
        kot_type,
        order_status: None,
        production: Some(production.clone()),
        start_time_prep: None,
        kot_items,
        pos_profile: ctx.pos_profile.clone(),
        branch: Some(ctx.branch.clone()),
        verified: false,
        verified_by: None,
        table_takeaway: ctx.table_takeaway,
        is_aggregator: ctx.is_aggregator,
        aggregator_id: ctx.aggregator_id.clone(),
        comments: ctx.comments.clone(),
        order_no: ctx.order_no.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MenuCourseName;
    use crate::money::Money;
    use rust_decimal_macros::dec;
    use std::cell::Cell;

    // -----------------------------------------------------------------------
    // in-memory fakes (no mocking crate, no database)
    //
    // Every fake counts its calls in a `Cell` so the query budget — the
    // regression for bugs 6 and 7 — is assertable.
    // -----------------------------------------------------------------------

    struct FakeItems {
        groups: HashMap<ItemCode, ItemGroupName>,
        calls: Cell<usize>,
    }

    impl FakeItems {
        /// `(item_code, item_group)` pairs.
        fn new(pairs: &[(&str, &str)]) -> Self {
            FakeItems {
                groups: pairs
                    .iter()
                    .map(|(code, group)| (ItemCode::from(*code), ItemGroupName::from(*group)))
                    .collect(),
                calls: Cell::new(0),
            }
        }
    }

    impl ItemRepo for FakeItems {
        fn item_groups(&self, codes: &[ItemCode]) -> Result<HashMap<ItemCode, ItemGroupName>> {
            self.calls.set(self.calls.get() + 1);
            Ok(codes
                .iter()
                .filter_map(|code| self.groups.get(code).map(|g| (code.clone(), g.clone())))
                .collect())
        }
    }

    struct FakeProductions {
        units: Vec<ProductionUnit>,
        calls: Cell<usize>,
    }

    impl FakeProductions {
        /// `(unit_name, item_groups)` per station, in branch order.
        fn new(units: &[(&str, &[&str])]) -> Self {
            FakeProductions {
                units: units
                    .iter()
                    .map(|(name, groups)| ProductionUnit {
                        name: ProductionUnitName::from(*name),
                        branch: branch(),
                        item_groups: groups.iter().map(|g| ItemGroupName::from(*g)).collect(),
                    })
                    .collect(),
                calls: Cell::new(0),
            }
        }

        fn empty() -> Self {
            FakeProductions {
                units: vec![],
                calls: Cell::new(0),
            }
        }
    }

    impl ProductionRepo for FakeProductions {
        fn list_for_branch(&self, branch: &BranchName) -> Result<Vec<ProductionUnit>> {
            self.calls.set(self.calls.get() + 1);
            Ok(self
                .units
                .iter()
                .filter(|u| u.branch == *branch)
                .cloned()
                .collect())
        }
    }

    struct FakeKots {
        /// `(invoice, production)` pairs that already have a submitted KOT.
        existing: Vec<(String, ProductionUnitName)>,
        calls: Cell<usize>,
    }

    impl FakeKots {
        fn none() -> Self {
            FakeKots {
                existing: vec![],
                calls: Cell::new(0),
            }
        }

        fn with(pairs: &[(&str, &str)]) -> Self {
            FakeKots {
                existing: pairs
                    .iter()
                    .map(|(inv, prod)| ((*inv).to_owned(), ProductionUnitName::from(*prod)))
                    .collect(),
                calls: Cell::new(0),
            }
        }
    }

    impl KotRepo for FakeKots {
        fn exists_for(&self, invoice: &str, production: &ProductionUnitName) -> Result<bool> {
            self.calls.set(self.calls.get() + 1);
            Ok(self
                .existing
                .iter()
                .any(|(inv, prod)| inv == invoice && prod == production))
        }
    }

    struct FakeMenu {
        /// Course per item, per room.
        courses: HashMap<(RoomName, ItemCode), MenuCourseName>,
        calls: Cell<usize>,
    }

    impl FakeMenu {
        fn new(entries: &[(&str, &str, &str)]) -> Self {
            FakeMenu {
                courses: entries
                    .iter()
                    .map(|(room, item, course)| {
                        (
                            (RoomName::from(*room), ItemCode::from(*item)),
                            MenuCourseName::from(*course),
                        )
                    })
                    .collect(),
                calls: Cell::new(0),
            }
        }

        fn empty() -> Self {
            FakeMenu {
                courses: HashMap::new(),
                calls: Cell::new(0),
            }
        }
    }

    impl MenuRepo for FakeMenu {
        fn courses_for_menu(
            &self,
            room: &RoomName,
            codes: &[ItemCode],
        ) -> Result<HashMap<ItemCode, MenuCourseName>> {
            self.calls.set(self.calls.get() + 1);
            Ok(codes
                .iter()
                .filter_map(|code| {
                    self.courses
                        .get(&(room.clone(), code.clone()))
                        .map(|c| (code.clone(), c.clone()))
                })
                .collect())
        }
    }

    /// One place to read the total repo traffic of a routing call.
    struct Repos {
        items: FakeItems,
        productions: FakeProductions,
        kots: FakeKots,
        menu: FakeMenu,
    }

    impl Repos {
        fn ports(&self) -> KotRepos<'_> {
            KotRepos {
                items: &self.items,
                productions: &self.productions,
                kots: &self.kots,
                menu: &self.menu,
            }
        }

        /// Batched data fetches: production units, item groups, courses.
        fn fetch_calls(&self) -> usize {
            self.productions.calls.get() + self.items.calls.get() + self.menu.calls.get()
        }

        /// EXISTS probes for the New Order → Order Modified flip.
        fn exists_probes(&self) -> usize {
            self.kots.calls.get()
        }

        fn total_calls(&self) -> usize {
            self.fetch_calls() + self.exists_probes()
        }
    }

    // -----------------------------------------------------------------------
    // builders
    // -----------------------------------------------------------------------

    fn branch() -> BranchName {
        BranchName::from("Peacock - Main")
    }

    fn room() -> RoomName {
        RoomName::from("Hall")
    }

    fn line(code: &str, qty: Decimal) -> OrderLine {
        OrderLine {
            item_code: ItemCode::from(code),
            item_name: format!("{code} name"),
            qty,
            rate: Money::new(dec!(100)),
            comments: None,
            serve_priority: 0,
            indicate_course: false,
        }
    }

    fn ctx() -> KotContext {
        let mut c = KotContext::new(
            "ACC-PSINV-2026-00042",
            branch(),
            "KOT-",
            NaiveDate::from_ymd_opt(2026, 7, 28).expect("valid date"),
        );
        c.restaurant_table = Some(TableName::from("T-01"));
        c.room = Some(room());
        c.customer_name = Some(CustomerName::from("Walk-in"));
        c.pos_profile = Some(PosProfileName::from("Peacock POS"));
        c
    }

    /// The canonical three-station branch: hot kitchen, cold kitchen, bar.
    fn three_stations() -> FakeProductions {
        FakeProductions::new(&[
            ("Hot Kitchen", &["Main Course"]),
            ("Cold Kitchen", &["Starters"]),
            ("Bar", &["Beverages"]),
        ])
    }

    fn items_for_three_stations() -> FakeItems {
        FakeItems::new(&[
            ("CURRY", "Main Course"),
            ("BIRYANI", "Main Course"),
            ("SALAD", "Starters"),
            ("SOUP", "Starters"),
            ("BEER", "Beverages"),
            ("COLA", "Beverages"),
        ])
    }

    fn codes_on(kot: &Kot) -> Vec<&str> {
        kot.kot_items.iter().map(|i| i.item.as_str()).collect()
    }

    fn ticket_for<'a>(tickets: &'a [Kot], production: &str) -> Option<&'a Kot> {
        tickets
            .iter()
            .find(|k| k.production.as_ref().is_some_and(|p| p.as_str() == production))
    }

    // -----------------------------------------------------------------------
    // required_item_codes
    // -----------------------------------------------------------------------

    #[test]
    fn required_item_codes_dedups_and_keeps_order() {
        let lines = vec![
            line("CURRY", dec!(1)),
            line("BEER", dec!(2)),
            // Same item twice: the POS allows it (different comments per line).
            line("CURRY", dec!(1)),
        ];

        let codes = required_item_codes(&lines);

        assert_eq!(
            codes,
            vec![ItemCode::from("CURRY"), ItemCode::from("BEER")],
            "one prefetch key per distinct item"
        );
    }

    #[test]
    fn required_item_codes_of_an_empty_order_is_empty() {
        assert!(required_item_codes(&[]).is_empty());
    }

    // -----------------------------------------------------------------------
    // BUG 1 — the regression that matters most
    // -----------------------------------------------------------------------

    #[test]
    fn three_stations_get_three_tickets_with_disjoint_item_sets() {
        // Regression for bug 1 (ury_kot_validation.py:51): `production_items` was
        // allocated before the station loop and appended without reset, so the Cold
        // Kitchen ticket carried the Hot Kitchen's items and the Bar carried both.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let lines = vec![
            line("CURRY", dec!(1)),
            line("SALAD", dec!(2)),
            line("BEER", dec!(3)),
        ];

        let tickets = route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        assert_eq!(tickets.len(), 3, "one ticket per station");
        assert_eq!(codes_on(ticket_for(&tickets, "Hot Kitchen").unwrap()), vec!["CURRY"]);
        assert_eq!(codes_on(ticket_for(&tickets, "Cold Kitchen").unwrap()), vec!["SALAD"]);
        assert_eq!(codes_on(ticket_for(&tickets, "Bar").unwrap()), vec!["BEER"]);

        // The bug-1 shape, stated as an invariant: no item appears on two tickets
        // and the totals add up to the order, not to a running accumulation.
        let total: usize = tickets.iter().map(|k| k.kot_items.len()).sum();
        assert_eq!(total, 3, "3 items routed once each, not 1 + 2 + 3 = 6");
    }

    #[test]
    fn later_stations_do_not_inherit_earlier_stations_items_at_scale() {
        // The same bug with more items per station: accumulation would give the
        // last station every item in the order.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let lines = vec![
            line("CURRY", dec!(1)),
            line("BIRYANI", dec!(1)),
            line("SALAD", dec!(1)),
            line("SOUP", dec!(1)),
            line("BEER", dec!(1)),
            line("COLA", dec!(1)),
        ];

        let tickets = route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        for ticket in &tickets {
            assert_eq!(ticket.kot_items.len(), 2, "each station owns exactly its two items");
        }
        assert_eq!(
            codes_on(ticket_for(&tickets, "Bar").unwrap()),
            vec!["BEER", "COLA"],
            "the last station must not carry the whole order"
        );
    }

    // -----------------------------------------------------------------------
    // BUGS 6 AND 7 — query budget
    // -----------------------------------------------------------------------

    #[test]
    fn query_budget_twelve_items_three_stations() {
        // Regression for bugs 6 and 7 (ury_kot_generate.py:154, :214): upstream ran
        // one Item lookup per item per station — 12 x 3 = 36 queries, and :214 loaded
        // whole documents. Here the data fetch is 3 batched calls, flat.
        let repos = Repos {
            items: FakeItems::new(&[
                ("CURRY", "Main Course"),
                ("BIRYANI", "Main Course"),
                ("KORMA", "Main Course"),
                ("NAAN", "Main Course"),
                ("SALAD", "Starters"),
                ("SOUP", "Starters"),
                ("TIKKA", "Starters"),
                ("PAPAD", "Starters"),
                ("BEER", "Beverages"),
                ("COLA", "Beverages"),
                ("LASSI", "Beverages"),
                ("WATER", "Beverages"),
            ]),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let lines: Vec<OrderLine> = [
            "CURRY", "BIRYANI", "KORMA", "NAAN", "SALAD", "SOUP", "TIKKA", "PAPAD", "BEER",
            "COLA", "LASSI", "WATER",
        ]
        .iter()
        .map(|c| line(c, dec!(1)))
        .collect();

        // Dine-in, so all three batched lookups are in play — the worst case for
        // the budget. (Takeaway is cheaper still: no room means no menu query.)
        let tickets = route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        assert_eq!(tickets.len(), 3);
        assert_eq!(repos.items.calls.get(), 1, "one batched item-group lookup");
        assert_eq!(repos.productions.calls.get(), 1, "one production-unit lookup");
        assert_eq!(repos.menu.calls.get(), 1, "one batched course lookup");
        assert_eq!(
            repos.fetch_calls(),
            3,
            "12 items x 3 stations costs 3 batched lookups, not 36 (bugs 6 and 7)"
        );
        // The flip is defined per invoice + production unit and KotRepo has no
        // batched form, so each emitted ticket adds one indexed EXISTS — never per
        // item, and a station with no items is not probed. Documented in the module
        // docs; asserted separately so the batched-fetch budget stays exactly 3.
        assert_eq!(repos.exists_probes(), 3, "one EXISTS per emitted ticket");
        assert_eq!(repos.total_calls(), 6);
    }

    #[test]
    fn takeaway_budget_skips_the_menu_lookup() {
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let lines: Vec<OrderLine> = ["CURRY", "SALAD", "BEER"]
            .iter()
            .map(|c| line(c, dec!(1)))
            .collect();
        let mut context = ctx();
        context.room = None;
        context.restaurant_table = None;
        context.table_takeaway = true;

        route_items_to_stations(&context, &lines, &repos.ports()).expect("routes");

        assert_eq!(repos.fetch_calls(), 2, "no room, no course query");
    }

    #[test]
    fn query_budget_does_not_grow_with_item_count() {
        // The property behind bugs 6 and 7: cost is independent of order size.
        let one = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let many = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let mut context = ctx();
        context.room = None;

        route_items_to_stations(&context, &[line("CURRY", dec!(1))], &one.ports())
            .expect("routes");
        let big: Vec<OrderLine> = ["CURRY", "BIRYANI", "SALAD", "SOUP", "BEER", "COLA"]
            .iter()
            .map(|c| line(c, dec!(2)))
            .collect();
        route_items_to_stations(&context, &big, &many.ports()).expect("routes");

        assert_eq!(one.items.calls.get(), many.items.calls.get(), "1 either way");
        assert_eq!(one.fetch_calls(), many.fetch_calls());
    }

    #[test]
    fn courses_are_fetched_in_one_batched_call() {
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::new(&[
                ("Hall", "SALAD", "Course 1"),
                ("Hall", "CURRY", "Course 2"),
            ]),
        };
        let lines = vec![
            line("CURRY", dec!(1)),
            line("SALAD", dec!(1)),
            line("BEER", dec!(1)),
        ];

        route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        assert_eq!(repos.menu.calls.get(), 1);
        assert_eq!(repos.fetch_calls(), 3);
    }

    // -----------------------------------------------------------------------
    // station selection
    // -----------------------------------------------------------------------

    #[test]
    fn station_with_no_matching_items_produces_no_ticket() {
        // ury_kot_generate.py:158 — `if production_items:`.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let lines = vec![line("CURRY", dec!(1)), line("BEER", dec!(1))];

        let tickets = route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        assert_eq!(tickets.len(), 2);
        assert!(
            ticket_for(&tickets, "Cold Kitchen").is_none(),
            "Cold Kitchen has nothing to cook"
        );
        // A skipped station is not probed either.
        assert_eq!(repos.exists_probes(), 2);
    }

    #[test]
    fn item_matching_no_station_is_dropped_and_surfaced_separately() {
        // Upstream behaviour, verified at ury_kot_generate.py:131-137: the item is
        // msgprint-warned and then never selected by any station comprehension
        // (:151-156). Nothing throws, nothing routes. Ported as: DROPPED, with the
        // advisory available via `unrouted_item_codes`.
        let repos = Repos {
            items: FakeItems::new(&[
                ("CURRY", "Main Course"),
                ("CIGAR", "Tobacco"), // no station lists Tobacco
                ("MYSTERY", "Main Course"),
            ]),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let lines = vec![
            line("CURRY", dec!(1)),
            line("CIGAR", dec!(1)),
            // Not in the Item table at all: no item_group, so also unrouted.
            line("NO-SUCH-ITEM", dec!(1)),
        ];

        let tickets = route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        assert_eq!(tickets.len(), 1);
        assert_eq!(codes_on(&tickets[0]), vec!["CURRY"]);
        for ticket in &tickets {
            assert!(!codes_on(ticket).contains(&"CIGAR"));
            assert!(!codes_on(ticket).contains(&"NO-SUCH-ITEM"));
        }

        // Same information upstream's msgprint carried.
        let units = repos.productions.list_for_branch(&branch()).expect("units");
        let groups = repos
            .items
            .item_groups(&required_item_codes(&lines))
            .expect("groups");
        assert_eq!(
            unrouted_item_codes(&lines, &units, &groups),
            vec![ItemCode::from("CIGAR"), ItemCode::from("NO-SUCH-ITEM")]
        );
    }

    #[test]
    fn item_group_listed_by_two_stations_prints_at_both() {
        // Each station filters the whole order independently
        // (ury_kot_generate.py:151), so a shared group fans out.
        let repos = Repos {
            items: FakeItems::new(&[("COFFEE", "Beverages"), ("CURRY", "Main Course")]),
            productions: FakeProductions::new(&[
                ("Bar", &["Beverages"]),
                ("Barista", &["Beverages"]),
                ("Hot Kitchen", &["Main Course"]),
            ]),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let lines = vec![line("COFFEE", dec!(2)), line("CURRY", dec!(1))];

        let tickets = route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        assert_eq!(tickets.len(), 3);
        assert_eq!(codes_on(ticket_for(&tickets, "Bar").unwrap()), vec!["COFFEE"]);
        assert_eq!(codes_on(ticket_for(&tickets, "Barista").unwrap()), vec!["COFFEE"]);
        assert_eq!(codes_on(ticket_for(&tickets, "Hot Kitchen").unwrap()), vec!["CURRY"]);
    }

    #[test]
    fn a_station_listing_several_groups_collects_all_of_them() {
        let repos = Repos {
            items: items_for_three_stations(),
            productions: FakeProductions::new(&[("Single Kitchen", &["Main Course", "Starters"])]),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let lines = vec![
            line("CURRY", dec!(1)),
            line("SALAD", dec!(1)),
            line("BEER", dec!(1)),
        ];

        let tickets = route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        assert_eq!(tickets.len(), 1);
        assert_eq!(codes_on(&tickets[0]), vec!["CURRY", "SALAD"]);
    }

    #[test]
    fn empty_order_produces_no_tickets_and_no_queries() {
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };

        let tickets = route_items_to_stations(&ctx(), &[], &repos.ports()).expect("routes");

        assert!(tickets.is_empty());
        assert_eq!(repos.total_calls(), 0, "nothing to route, nothing to ask");
    }

    #[test]
    fn branch_without_production_units_produces_no_tickets() {
        // Deviation 3: upstream frappe.throws at ury_kot_generate.py:182; there is
        // no matching Error variant, so this is an empty result.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: FakeProductions::empty(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };

        let tickets = route_items_to_stations(&ctx(), &[line("CURRY", dec!(1))], &repos.ports())
            .expect("routes");

        assert!(tickets.is_empty());
        assert_eq!(repos.items.calls.get(), 0, "no units, no item prefetch");
    }

    // -----------------------------------------------------------------------
    // New Order → Order Modified
    // -----------------------------------------------------------------------

    #[test]
    fn kot_type_flips_to_order_modified_when_one_already_exists() {
        // ury_kot_generate.py:159-168.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::with(&[("ACC-PSINV-2026-00042", "Hot Kitchen")]),
            menu: FakeMenu::empty(),
        };
        let lines = vec![line("CURRY", dec!(1)), line("BEER", dec!(1))];

        let tickets = route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        assert_eq!(
            ticket_for(&tickets, "Hot Kitchen").unwrap().kot_type,
            KotType::OrderModified
        );
        assert_eq!(
            ticket_for(&tickets, "Bar").unwrap().kot_type,
            KotType::NewOrder,
            "deviation 2: the Bar has never printed, so it stays New Order"
        );
    }

    #[test]
    fn a_flip_on_one_station_does_not_leak_to_later_stations() {
        // Deviation 2 / upstream ury_kot_generate.py:168 reassigns the shared
        // `kot_type` local, so every station after the first flip inherited it.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::with(&[("ACC-PSINV-2026-00042", "Hot Kitchen")]),
            menu: FakeMenu::empty(),
        };
        let lines = vec![
            line("CURRY", dec!(1)),
            line("SALAD", dec!(1)),
            line("BEER", dec!(1)),
        ];

        let tickets = route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        let types: Vec<KotType> = tickets.iter().map(|k| k.kot_type).collect();
        assert_eq!(
            types,
            vec![KotType::OrderModified, KotType::NewOrder, KotType::NewOrder]
        );
    }

    #[test]
    fn flip_is_scoped_to_the_invoice() {
        // A KOT for the same station on a *different* invoice must not flip this one.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::with(&[("ACC-PSINV-2026-99999", "Hot Kitchen")]),
            menu: FakeMenu::empty(),
        };

        let tickets = route_items_to_stations(&ctx(), &[line("CURRY", dec!(1))], &repos.ports())
            .expect("routes");

        assert_eq!(tickets[0].kot_type, KotType::NewOrder);
    }

    // -----------------------------------------------------------------------
    // course assignment
    // -----------------------------------------------------------------------

    #[test]
    fn course_lands_on_the_right_lines_scoped_to_the_rooms_menu() {
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::new(&[
                ("Hall", "SALAD", "Starter Course"),
                ("Hall", "CURRY", "Main Course Serve"),
                // Same item, different room — must not be picked up.
                ("Patio", "BEER", "Patio Course"),
            ]),
        };
        let lines = vec![
            line("CURRY", dec!(1)),
            line("SALAD", dec!(1)),
            line("BEER", dec!(1)),
        ];

        let tickets = route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        let hot = ticket_for(&tickets, "Hot Kitchen").unwrap();
        assert_eq!(
            hot.kot_items[0].course,
            Some(MenuCourseName::from("Main Course Serve"))
        );
        let cold = ticket_for(&tickets, "Cold Kitchen").unwrap();
        assert_eq!(
            cold.kot_items[0].course,
            Some(MenuCourseName::from("Starter Course"))
        );
        let bar = ticket_for(&tickets, "Bar").unwrap();
        assert_eq!(
            bar.kot_items[0].course, None,
            "BEER has a course in Patio's menu, not Hall's"
        );
    }

    #[test]
    fn takeaway_without_a_room_leaves_course_unset() {
        // Deviation 5: MenuRepo is room-scoped; upstream would fall back to
        // URY Restaurant.active_menu (ury_kot_generate.py:69).
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::new(&[("Hall", "CURRY", "Main Course Serve")]),
        };
        let mut context = ctx();
        context.room = None;
        context.restaurant_table = None;
        context.table_takeaway = true;

        let tickets = route_items_to_stations(&context, &[line("CURRY", dec!(1))], &repos.ports())
            .expect("routes");

        assert_eq!(tickets[0].kot_items[0].course, None);
        assert_eq!(repos.menu.calls.get(), 0, "no room, no menu query");
        assert!(tickets[0].table_takeaway);
    }

    // -----------------------------------------------------------------------
    // ticket contents
    // -----------------------------------------------------------------------

    #[test]
    fn ticket_carries_invoice_context_and_no_name_yet() {
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let mut context = ctx();
        context.comments = Some("no chilli".to_owned());
        context.order_no = Some("42".to_owned());
        context.is_aggregator = true;
        context.aggregator_id = Some("SWIGGY-1".to_owned());
        context.time = NaiveTime::from_hms_opt(19, 30, 0);

        let tickets = route_items_to_stations(&context, &[line("CURRY", dec!(2))], &repos.ports())
            .expect("routes");

        let kot = &tickets[0];
        assert_eq!(kot.name, None, "naming series assigns this on insert");
        assert_eq!(kot.naming_series, "KOT-");
        assert_eq!(kot.invoice, "ACC-PSINV-2026-00042");
        assert_eq!(kot.restaurant_table, Some(TableName::from("T-01")));
        assert_eq!(kot.customer_name, Some(CustomerName::from("Walk-in")));
        assert_eq!(kot.branch, Some(branch()));
        assert_eq!(kot.production, Some(ProductionUnitName::from("Hot Kitchen")));
        assert_eq!(kot.comments.as_deref(), Some("no chilli"));
        assert_eq!(kot.order_no.as_deref(), Some("42"));
        assert!(kot.is_aggregator);
        assert_eq!(kot.aggregator_id.as_deref(), Some("SWIGGY-1"));
        assert_eq!(kot.date, NaiveDate::from_ymd_opt(2026, 7, 28).unwrap());
        assert!(!kot.verified);
        assert_eq!(kot.original_kot, None, "new orders have nothing to back-link");

        let item = &kot.kot_items[0];
        assert_eq!(item.quantity, dec!(2));
        assert_eq!(item.cancelled_qty, Decimal::ZERO);
        assert_eq!(item.item_name, "CURRY name");
    }

    #[test]
    fn duplicate_lines_for_one_item_stay_separate_rows() {
        // The POS sends the same item twice when the comments differ; upstream
        // appends one kot_items row per input line (ury_kot_generate.py:71-82).
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let mut extra = line("CURRY", dec!(1));
        extra.comments = Some("extra hot".to_owned());
        let lines = vec![line("CURRY", dec!(1)), extra];

        let tickets = route_items_to_stations(&ctx(), &lines, &repos.ports()).expect("routes");

        assert_eq!(tickets[0].kot_items.len(), 2);
        assert_eq!(tickets[0].kot_items[1].comments.as_deref(), Some("extra hot"));
        assert_eq!(repos.items.calls.get(), 1, "still one prefetch");
    }

    // -----------------------------------------------------------------------
    // cancel path
    // -----------------------------------------------------------------------

    #[test]
    fn cancel_sets_original_kot_and_partially_cancelled_type() {
        // ury_kot_generate.py:260-284 with the "Partially cancelled" call site
        // at :375.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let invoice_lines = vec![line("CURRY", dec!(3)), line("BEER", dec!(2))];
        let cancel_lines = vec![line("CURRY", dec!(-1)), line("BEER", dec!(-2))];
        let existing = vec![
            ExistingKot::new(KotName::from("KOT-0001"), vec![ItemCode::from("CURRY")]),
            ExistingKot::new(KotName::from("KOT-0002"), vec![ItemCode::from("BEER")]),
        ];

        let tickets = route_cancel_items_to_stations(
            &ctx(),
            &cancel_lines,
            &invoice_lines,
            &existing,
            CancelKind::Partial,
            &repos.ports(),
        )
        .expect("routes");

        assert_eq!(tickets.len(), 2);
        let hot = ticket_for(&tickets, "Hot Kitchen").unwrap();
        assert_eq!(hot.kot_type, KotType::PartiallyCancelled);
        assert_eq!(hot.original_kot.as_deref(), Some("KOT-0001"));
        // quantity = ordered, cancelled_qty = the magnitude cancelled
        // (ury_kot_generate.py:311-312).
        assert_eq!(hot.kot_items[0].quantity, dec!(3));
        assert_eq!(hot.kot_items[0].cancelled_qty, dec!(1));

        let bar = ticket_for(&tickets, "Bar").unwrap();
        assert_eq!(bar.original_kot.as_deref(), Some("KOT-0002"));
        assert_eq!(bar.kot_items[0].cancelled_qty, dec!(2));

        // No flip probes on the cancel path.
        assert_eq!(repos.exists_probes(), 0);
        assert!(repos.fetch_calls() <= 3);
    }

    #[test]
    fn whole_order_cancel_uses_the_cancelled_type() {
        // ury_order.py:1325-1334 (`cancel_kot`) passes positive quantities.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let invoice_lines = vec![line("CURRY", dec!(2))];
        let existing = vec![ExistingKot::new(
            KotName::from("KOT-0001"),
            vec![ItemCode::from("CURRY")],
        )];

        let tickets = route_cancel_items_to_stations(
            &ctx(),
            &invoice_lines,
            &invoice_lines,
            &existing,
            CancelKind::WholeOrder,
            &repos.ports(),
        )
        .expect("routes");

        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0].kot_type, KotType::Cancelled);
        assert_eq!(tickets[0].original_kot.as_deref(), Some("KOT-0001"));
        assert_eq!(tickets[0].kot_items[0].cancelled_qty, dec!(2));
    }

    #[test]
    fn cancel_back_link_dedups_across_items_on_one_station() {
        // Both cancelled items came from the same ticket: `original_kot` lists it
        // once (ury_kot_generate.py:275).
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let invoice_lines = vec![line("CURRY", dec!(1)), line("BIRYANI", dec!(1))];
        let existing = vec![ExistingKot::new(
            KotName::from("KOT-0001"),
            vec![ItemCode::from("CURRY"), ItemCode::from("BIRYANI")],
        )];

        let tickets = route_cancel_items_to_stations(
            &ctx(),
            &invoice_lines,
            &invoice_lines,
            &existing,
            CancelKind::Partial,
            &repos.ports(),
        )
        .expect("routes");

        assert_eq!(tickets[0].original_kot.as_deref(), Some("KOT-0001"));
    }

    #[test]
    fn cancel_back_link_joins_multiple_source_tickets() {
        // One station, items that were printed on two successive tickets.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: FakeProductions::new(&[("Hot Kitchen", &["Main Course"])]),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let invoice_lines = vec![line("CURRY", dec!(1)), line("BIRYANI", dec!(1))];
        let existing = vec![
            ExistingKot::new(KotName::from("KOT-0001"), vec![ItemCode::from("CURRY")]),
            ExistingKot::new(KotName::from("KOT-0002"), vec![ItemCode::from("BIRYANI")]),
        ];

        let tickets = route_cancel_items_to_stations(
            &ctx(),
            &invoice_lines,
            &invoice_lines,
            &existing,
            CancelKind::Partial,
            &repos.ports(),
        )
        .expect("routes");

        assert_eq!(tickets[0].original_kot.as_deref(), Some("KOT-0001,KOT-0002"));
    }

    #[test]
    fn cancel_without_a_prior_ticket_leaves_original_kot_unset() {
        // Upstream would write "" (ury_kot_generate.py:276); deviation 6.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let invoice_lines = vec![line("CURRY", dec!(1))];

        let tickets = route_cancel_items_to_stations(
            &ctx(),
            &invoice_lines,
            &invoice_lines,
            &[],
            CancelKind::Partial,
            &repos.ports(),
        )
        .expect("routes");

        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0].original_kot, None);
    }

    #[test]
    fn cancel_line_absent_from_the_invoice_is_skipped() {
        // ury_kot_generate.py:304-306: `quantity` is read from invoiceItems, so a
        // cancel line with no invoice match never gets a row — and a station left
        // with no rows gets no ticket.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let invoice_lines = vec![line("CURRY", dec!(2))];
        let cancel_lines = vec![line("CURRY", dec!(-1)), line("BEER", dec!(-1))];

        let tickets = route_cancel_items_to_stations(
            &ctx(),
            &cancel_lines,
            &invoice_lines,
            &[],
            CancelKind::Partial,
            &repos.ports(),
        )
        .expect("routes");

        assert_eq!(tickets.len(), 1, "the Bar's only cancel line has no invoice row");
        assert_eq!(codes_on(&tickets[0]), vec!["CURRY"]);
    }

    #[test]
    fn cancel_keeps_station_item_sets_disjoint() {
        // Bug 1 again, on the cancel path.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };
        let invoice_lines = vec![
            line("CURRY", dec!(2)),
            line("SALAD", dec!(2)),
            line("BEER", dec!(2)),
        ];

        let tickets = route_cancel_items_to_stations(
            &ctx(),
            &invoice_lines,
            &invoice_lines,
            &[],
            CancelKind::WholeOrder,
            &repos.ports(),
        )
        .expect("routes");

        assert_eq!(tickets.len(), 3);
        assert_eq!(codes_on(ticket_for(&tickets, "Hot Kitchen").unwrap()), vec!["CURRY"]);
        assert_eq!(codes_on(ticket_for(&tickets, "Cold Kitchen").unwrap()), vec!["SALAD"]);
        assert_eq!(codes_on(ticket_for(&tickets, "Bar").unwrap()), vec!["BEER"]);
    }

    #[test]
    fn cancel_with_no_lines_produces_no_tickets_and_no_queries() {
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::empty(),
        };

        let tickets = route_cancel_items_to_stations(
            &ctx(),
            &[],
            &[line("CURRY", dec!(1))],
            &[],
            CancelKind::Partial,
            &repos.ports(),
        )
        .expect("routes");

        assert!(tickets.is_empty());
        assert_eq!(repos.total_calls(), 0);
    }

    #[test]
    fn cancel_carries_the_course_from_the_rooms_menu() {
        // ury_kot_generate.py:303 does the same lookup on the cancel path.
        let repos = Repos {
            items: items_for_three_stations(),
            productions: three_stations(),
            kots: FakeKots::none(),
            menu: FakeMenu::new(&[("Hall", "CURRY", "Main Course Serve")]),
        };
        let invoice_lines = vec![line("CURRY", dec!(1))];

        let tickets = route_cancel_items_to_stations(
            &ctx(),
            &invoice_lines,
            &invoice_lines,
            &[],
            CancelKind::Partial,
            &repos.ports(),
        )
        .expect("routes");

        assert_eq!(
            tickets[0].kot_items[0].course,
            Some(MenuCourseName::from("Main Course Serve"))
        );
    }

    #[test]
    fn cancel_kind_maps_to_the_upstream_labels() {
        assert_eq!(CancelKind::WholeOrder.kot_type(), KotType::Cancelled);
        assert_eq!(CancelKind::Partial.kot_type(), KotType::PartiallyCancelled);
    }
}
