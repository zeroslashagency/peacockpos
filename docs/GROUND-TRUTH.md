# Ground Truth — verified facts about `_upstream/ury-ury`

Every number here was produced by a command against the actual source tree, not from memory. Any plan lane that contradicts this file is wrong and must be corrected against the source.

Generated: 2026-07-28

---

## The 36 real doctypes

Produced by `ls -d */` in `_upstream/ury-ury/ury/ury/doctype/`. This is the complete, authoritative list.

| # | Doctype | # | Doctype |
|---|---|---|---|
| 1 | `aggregator_settings` | 19 | `ury_menu` |
| 2 | `item_add_on` | 20 | `ury_menu_course` |
| 3 | `kds_order_type` | 21 | `ury_menu_item` |
| 4 | `menu_for_room` | 22 | `ury_merged_pos_invoice_detail` |
| 5 | `multiple_rooms` | 23 | `ury_notification_recipient` |
| 6 | `order_type_menu` | 24 | `ury_order` |
| 7 | `pos_item_variants` | 25 | `ury_order_item` |
| 8 | `role_permitted` | 26 | `ury_p_and_l_breakup` |
| 9 | `sub_pos_closing` | 27 | `ury_p_and_l_materials` |
| 10 | `sub_pos_closing_payment` | 28 | `ury_printer_settings` |
| 11 | `sub_pos_invoices` | 29 | `ury_production_item_groups` |
| 12 | `ury_cost_of_goods` | 30 | `ury_production_unit` |
| 13 | `ury_daily_p_and_l` | 31 | `ury_report_settings` |
| 14 | `ury_fixed_expenses` | 32 | `ury_restaurant` |
| 15 | `ury_kot` | 33 | `ury_room` |
| 16 | `ury_kot_error_log` | 34 | `ury_table` |
| 17 | `ury_kot_items` | 35 | `ury_user` |
| 18 | `ury_materials` | 36 | `ury_variable_expenses` |

### Root vs child split — 12 root, 24 child

Determined by the `"istable": 1` flag in each doctype JSON. This matters: child tables are embed-only and get a FK plus an ordering column, not standalone CRUD.

**12 root doctypes** (standalone CRUD):
`sub_pos_closing`, `ury_daily_p_and_l`, `ury_kot`, `ury_kot_error_log`, `ury_menu`, `ury_menu_course`, `ury_order`, `ury_production_unit`, `ury_report_settings`, `ury_restaurant`, `ury_room`, `ury_table`

**24 child tables** (embed-only):
`aggregator_settings`, `item_add_on`, `kds_order_type`, `menu_for_room`, `multiple_rooms`, `order_type_menu`, `pos_item_variants`, `role_permitted`, `sub_pos_closing_payment`, `sub_pos_invoices`, `ury_cost_of_goods`, `ury_fixed_expenses`, `ury_kot_items`, `ury_materials`, `ury_menu_item`, `ury_merged_pos_invoice_detail`, `ury_notification_recipient`, `ury_order_item`, `ury_p_and_l_breakup`, `ury_p_and_l_materials`, `ury_printer_settings`, `ury_production_item_groups`, `ury_user`, `ury_variable_expenses`

Note `ury_menu_item` and `ury_user` are **child tables**, not root entities — a plan that models them as standalone tables with their own API is wrong.

### Doctypes that DO NOT EXIST

`RUST_MIGRATION_PLAN.md` §2.2 invented all of these. Never reference them:

`ury_bom`, `ury_bom_item`, `ury_pricelist`, `ury_pricelist_item`, `ury_customer`, `ury_tax_rule`, `ury_discount`, `ury_settings`, `ury_sync_log`, `ury_analytics`, `ury_permission`, `ury_user_role`, `ury_void_reason`, `ury_shift`, `ury_payment_entry`, `ury_modifier`, `ury_modifier_group`, `ury_order_modifier`, `ury_kitchen_station`, `ury_course`, `ury_item_group`, `ury_order_type`, `ury_pos_opening_entry`, `ury_pos_closing_entry`, `ury_report_config`, `ury_kot_item` (real name is `ury_kot_items`, plural).

**Why they don't exist:** most of those concepts live in **ERPNext**, not URY. BOM, Item Price, Price List, Customer, Sales Taxes and Charges Template, POS Opening Entry, POS Closing Entry, POS Invoice, and Item are all ERPNext doctypes that URY *extends* via custom fields. That is the real integration surface.

---

## API surface

`wc -l` over the API modules:

| Module | Lines |
|---|---|
| `ury/ury_pos/api.py` | 1,031 |
| `ury/ury/api/ury_kot_generate.py` | 405 |
| `ury/ury/api/ury_print.py` | 189 |
| `ury/ury/api/ury_kot_display.py` | 166 |
| `ury/ury/api/ury_kot_validation.py` | 136 |
| `ury/ury/api/pos_extend.py` | 112 |
| `ury/ury/api/ury_kot_order_number.py` | 99 |
| `ury/ury/api/ury_kot_notification.py` | 77 |
| `ury/ury/api/ury_kot_reprint.py` | 49 |
| `ury/ury/api/ury_menu_course_validation.py` | 14 |
| `ury/ury/api/button_permission.py` | 12 |
| **Total** | **2,290** |

Plus `ury/ury/doctype/ury_order/ury_order.py` at **1,371 lines** (the order engine).

### 59 whitelisted endpoints

`grep -c "@frappe.whitelist"`:

| File | Count |
|---|---|
| `ury/ury_pos/api.py` | 23 |
| `ury/ury/doctype/ury_order/ury_order.py` | 16 |
| `ury/ury/api/ury_print.py` | 6 |
| `ury/ury/api/ury_kot_display.py` | 5 |
| `ury/ury/doctype/sub_pos_closing/sub_pos_closing.py` | 3 |
| `ury_daily_p_and_l.py`, `ury_kot_reprint.py`, `ury_kot_notification.py`, `ury_kot_generate.py`, `pos_extend.py`, `button_permission.py` | 1 each |
| **Total** | **59** |

Any REST API design must account for 59 endpoints, not the "10 API modules" framing.

### The real endpoint names

**`ury_pos/api.py` (23):** `getRestaurantMenu`, `getMenuCourses`, `getBranch`, `getBranchRoom`, `getRoom`, `getModeOfPayment`, `get_split_group`, `getInvoiceForCashier`, `getPosInvoice`, `searchPosInvoice`, `get_select_field_options`, `fav_items`, `getCashier`, `getPosProfile`, `getPosInvoiceItems`, `posOpening`, `getAggregator`, `getAggregatorItem`, `getAggregatorMOP`, `create_customer`, `validate_pos_close`, `merge_bills`

**`ury_order.py` (16):** `merge_free_tables`, `merge_tables_batch`, `release_tables_after_print`, `unmerge_tables`, `split_bill`, `get_order_invoice`, `sync_order`, `item_query_restaurant`, `get_restaurant_and_menu_name`, `get_menu_name`, `pos_opening_check`, `table_transfer`, `captain_transfer`, `customer_favourite_item`, `cancel_order`, `make_invoice`

Note the naming is inconsistent upstream (camelCase in `ury_pos/api.py`, snake_case in `ury_order.py`). A REST redesign should normalise, but the frontends currently call these exact names — see `pos/src/lib/*.ts`.

### Realtime channels

`grep publish_realtime` finds exactly **one** statically-named channel: `reload_ro`. Other channels are constructed dynamically (e.g. KOT updates per branch and production unit), so they do not appear as string literals. Any claim about a fixed set of socket.io channel names must be verified by reading the call sites, not by grepping for literals.

---

## Frontends — what framework each actually uses

Verified from each `package.json`:

| App | Framework | Note |
|---|---|---|
| `pos/` | **React 19** | 37 components, Zustand, TypeScript, `frappe-js-sdk`, `qz-tray@2.2.5` |
| `urypos/` | **Vue 3.3.4** | 14 components, 14 Pinia stores |
| `URYMosaic/` | **Vue 3.3.4** | The KDS |
| `frontend/` | Vite shell | near-empty scaffold |

**One React app, two Vue 3 apps.** `RUST_MIGRATION_PLAN.md` §7 claiming "React frontends (POS, KDS, POS v2)" is wrong.

---

## Verified function signatures

Anything porting these must match the real signature.

### `_get_merge_cluster` — `ury_order.py:240`
```python
def _get_merge_cluster(table):
    # returns (members, table_by_name)
    # reads merged_with from URY Table rows, NOT from the order
    # scopes the entire BFS to one restaurant_room
```
Wrapped by `merge_tables_batch`, which enforces: same room, rejects already-merged targets, rejects occupied targets, and refuses when `_count_separate_active_orders(cluster) > 1`.

### `process_items_for_kot` — `ury_kot_generate.py:111`
Takes **8 arguments**. Resolves production units per `POS Profile.branch`, resolves `course` per item from `URY Menu Item` scoped by the room's menu, and flips `kot_type` to `"Order Modified"` when a submitted KOT already exists for that invoice + production unit. Has a sibling `process_items_for_cancel_kot` that back-links `original_kot`.

### BOM / COGS walk — `ury_daily_p_and_l.py:10` and `:42`
```python
inner_bom_process(...)        # level 1
  inner_inner_bom_process(...)  # level 2 — stops here
```
**Two hardcoded levels, not a depth-3 recursion.** Critically:
- Line 38: divides by `bom.quantity` for per-unit normalisation
- Line 30: prices from **`Item Price` on the configured `buying_price_list`**, not from stock valuation
- Accumulates `unset_bom_items` so the operator sees ingredients with no price

Omitting the `bom.quantity` division or switching the cost basis to stock valuation silently produces wrong COGS.

---

## ERPNext integration surface

`ury/hooks.py` ships roughly **110 custom field fixtures** across ~15 **ERPNext** doctypes: POS Invoice, POS Invoice Item, Sales Invoice, POS Profile, POS Profile User, POS Opening Entry, POS Closing Entry, Branch, Price List, Printer Settings, Customer, Item, Employee.

`ury_order.py` calls ERPNext's `calculate_taxes_and_totals()` at lines **637, 641, 1226, 1232**. That is ERPNext's tax engine, not URY code.

Money, tax, GL posting, and stock valuation are all ERPNext. Any migration plan must state explicitly what replaces each.

### Scheduler
Exactly one entry in `hooks.py`:
```
"* * * * *": ury.ury.api.ury_kot_validation.kotValidationThread
```

---

## Seven verified bugs

Each confirmed by reading the source at the stated line.

| # | Bug | Location |
|---|---|---|
| 1 | `production_items = []` allocated before the `for p in productions` loop and appended without reset → station B's KOT carries station A's items | `ury_kot_validation.py:51`, loop at `:57`, append at `:69` |
| 2 | `posting_date` (DATE) filtered `between` two **datetime** bounds → MariaDB casts to date, matching both whole days; every midnight-crossing shift mis-buckets | `sub_pos_closing.py:42` |
| 3 | Revenue definition split: `grand_total` vs `rounded_total` | `sub_pos_closing.py:45` vs `ury_daily_p_and_l.py:297` |
| 4 | Status filter split: `status = "Paid"` vs `status IN ("Consolidated","Paid")` → shift close omits invoices the P&L counts | `sub_pos_closing.py:41` vs `ury_daily_p_and_l.py:94,131,162,305` |
| 5 | Dead ternary `owner = waiter if not invoice.restaurant_table else waiter` — both branches identical, and reads `.restaurant_table` off a name string | `ury_kot_validation.py:41` |
| 6 | N+1: `frappe.db.get_value("Item", …)` inside a list comprehension, per item per station (12 items × 3 stations = 36 queries) | `ury_kot_generate.py:154` |
| 7 | Second N+1, worse: `frappe.get_doc("Item", …)` loads full documents in the same pattern | `ury_kot_generate.py:214` |

---

## Hosting constraints (owner-imposed, non-negotiable)

- Deployment target is **Vercel** for the web UI. Vercel has no Rust runtime, functions are request-scoped with no shared in-process memory, and there are no long-lived WebSocket servers.
- Therefore: realtime, printing (raw TCP to `:9100` on the restaurant LAN), and the per-minute cron **cannot** live on Vercel. One always-on box is required regardless of language.
- Owner wants **simple architecture** — no microservices, no Kubernetes, minimal moving parts.
- Money is `Decimal`. Never serialise money through JS `Number()`; use strings across the wire.
