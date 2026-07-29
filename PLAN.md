# Peacock POS — build plan

Created 2026-07-27. This is the plan of record for **Peacock POS**, an Indian
restaurant POS built by porting URY's proven restaurant logic and wearing
Petpooja's design language.

**Decisions taken (by Jack):**

1. **Clone the full URY repo first**, then port from it. Done — see §1.
2. **Fresh start in `peacock-pos`. Do not touch `zerosky-repo`** — it is a
   separate product and is neither copied from nor modified. The schema is
   designed from URY's doctypes plus Petpooja's real API contract.
3. Licence is not a consideration in selecting sources.

**Inputs this plan rests on:**

| Document | What it provides |
| --- | --- |
| `restaurant-pos/URY-PORT-AND-GAP-PLAN.md` | 120 missing features, prioritised, with effort |
| `restaurant-pos/OPEN-SOURCE-POS-RESEARCH.md` | 50-row matrix across 6 open-source POS |
| `restaurant-pos/COMPETITIVE-ANALYSIS.md` (reference copy) | Petpooja evidence: real UI, real API, Pantheon design system |
| `restaurant-pos/rewrite.md` | Jack's URY stack analysis — see §2 for two corrections |

---

## 1. Upstream sources — cloned, full history

`peacock-pos/_upstream/` holds the real repos we port from. They stay
untouched as reference; nothing is edited in place.

| Repo | Path | Size | Commits | What we take |
| --- | --- | --- | --- | --- |
| `ury-erp/ury` | `_upstream/ury-ury` | 7.9 MB | 475 | Order engine, KOT, menu resolution, shift, P&L, 14 reports, 36 doctypes |
| `ury-erp/pos` | `_upstream/ury-pos` | 5.9 MB | 49 | POS screen layout and order-taking flow (Vue) |
| `ury-erp/mosaic` | `_upstream/ury-mosaic` | 1.3 MB | 15 | Kitchen Display System (Vue) |

**Port surface: 6,075 lines of Python** (excluding patches and setup), measured.
This is what makes the port tractable — it is not ERPNext's 500k lines.

### 1.1 The 36 URY doctypes to port

Confirmed present in `_upstream/ury-ury/ury/ury/doctype/`:

**Core operations** — `ury_order`, `ury_order_item`, `ury_kot`, `ury_kot_items`,
`ury_kot_error_log`, `ury_restaurant`, `ury_room`, `ury_table`, `ury_user`

**Menu** — `ury_menu`, `ury_menu_item`, `ury_menu_course`, `pos_item_variants`,
`item_add_on`, `menu_for_room`, `order_type_menu`, `multiple_rooms`

**Kitchen routing** — `ury_production_unit`, `ury_production_item_groups`,
`kds_order_type`, `ury_printer_settings`

**Shift and cash** — `sub_pos_closing`, `sub_pos_closing_payment`,
`sub_pos_invoices`, `ury_merged_pos_invoice_detail`

**Costing and P&L** — `ury_daily_p_and_l`, `ury_cost_of_goods`,
`ury_p_and_l_breakup`, `ury_p_and_l_materials`, `ury_materials`,
`ury_fixed_expenses`, `ury_variable_expenses`

**Config** — `aggregator_settings`, `role_permitted`, `ury_report_settings`,
`ury_notification_recipient`

### 1.2 The 10 API modules to port

`_upstream/ury-ury/ury/ury/api/`: `ury_kot_generate.py` (405 lines),
`ury_print.py` (189), `ury_kot_display.py` (166), `ury_kot_validation.py` (136),
`pos_extend.py` (112), `ury_kot_order_number.py` (99), `ury_kot_notification.py`
(77), `ury_kot_reprint.py`, `ury_menu_course_validation.py`,
`button_permission.py`.

Plus 6 lifecycle hooks in `ury/ury/hooks/`: `ury_pos_invoice.py` (334 lines),
`ury_sales_invoice.py`, `ury_pos_closing_entry.py`, `ury_pos_opening_entry.py`,
`ury_pos_profile.py`, `ury_item.py`.

### 1.3 The largest files, in port order

| File | Lines | Becomes |
| --- | --- | --- |
| `doctype/ury_order/ury_order.py` | 1,371 | order engine: create, split, merge, totals, invoice |
| `ury_pos/api.py` | 1,031 | menu resolution, POS bootstrap, payment modes |
| `doctype/ury_daily_p_and_l/*.py` | 541 | P&L with nested-BOM COGS |
| `api/ury_kot_generate.py` | 405 | KOT generation + station routing |
| `hooks/ury_pos_invoice.py` | 334 | order lifecycle side effects |
| `doctype/sub_pos_closing/*.py` | 124 | multi-cashier shift close |
| `doctype/ury_kot/ury_kot.py` | 114 | KOT model rules |
| 14 report definitions | — | reports module |

---

## 2. Corrections to `rewrite.md`

`rewrite.md` is a useful stack analysis, but two claims are wrong and one
recommendation needs revisiting. Verified by cloning and reading `package.json`.

| `rewrite.md` claims | Verified reality |
| --- | --- |
| "React frontends (POS, KDS, POS v2) — keep, no rewrite needed" | **Mixed frameworks.** `_upstream/ury-pos/urypos` is **Vue 3**; `URYMosaic` (the KDS) is **Vue 3**. Only `ury/pos` and `ury/frontend` are React |
| "Keep React — only API endpoint adjustments" | We are not keeping URY's frontend at all. The whole point is a **Petpooja-skinned UI** built on Pantheon tokens. URY's screens are reference material, not shipped code |
| Python → **Rust** (Axum + SQLx), 15–24 weeks, 3–4 developers | The effort estimate is honest, and that is the problem: it is a 4–6 month backend rewrite before a single new feature ships |

### 2.1 The language decision

`rewrite.md` proposes Rust. The alternative is TypeScript. Since Peacock POS
starts fresh, this is a genuinely open choice — there is no existing codebase
here to preserve either way.

| | Rust (Axum + SQLx) | TypeScript (Next.js + tRPC + Prisma) |
| --- | --- | --- |
| Backend rewrite cost | 15–24 weeks (rewrite.md's own estimate) | 0 — the platform exists |
| Ecosystem for this domain | Thin — few restaurant/POS libraries | Deep — Prisma, tRPC, Recharts, escpos-buffer, Dexie all directly usable |
| Petpooja skin (Pantheon) | Frontend unaffected either way | Applies directly to Tailwind v4 tokens |
| Type safety DB→UI | Manual serde at boundary | End-to-end via tRPC |
| Runtime performance | Better | Adequate; unproven at scale either way |
| Team fit | New language for this project | Already in use |
| Mobile | Flutter app talks REST either way | Flutter app already talks to our API |

**Recommendation: TypeScript.** Rust buys runtime performance we have no
evidence of needing, at the cost of four to six months during which no
restaurant-facing feature ships. Peacock POS is competing on features and UI, not
on request latency. The 503 passing tests and 21 modelled entities are the
single largest asset we have; a Rust backend discards all of them.

**If Jack still wants Rust,** the sane sequencing is: build Peacock POS in
TypeScript now, reach feature parity with Petpooja, and only then consider
extracting hot paths (KOT dispatch, report aggregation) into a Rust service
behind the same API. That is reversible; a rewrite first is not.

This plan proceeds in TypeScript. Phase boundaries are drawn so that a language
change before Phase 3 would cost only Phases 0–2.

---

## 3. Target architecture

```
peacock-pos/
├── _upstream/              URY clones, reference only, never edited
│   ├── ury-ury/            order engine, KOT, menu, P&L, reports
│   ├── ury-pos/            POS screen reference (Vue)
│   └── ury-mosaic/         KDS reference (Vue)
├── apps/
│   ├── pos-web/            Next.js 16 — cashier + manager (Pantheon skin)
│   ├── kds-display/        Next.js — kitchen display
│   └── captain/            Flutter — waiter/captain app
├── packages/
│   ├── api/                tRPC routers (the ported URY logic)
│   ├── database/           Prisma schema + migrations
│   ├── ui/                 Pantheon-based component library
│   ├── print/              ESC/POS (escpos-buffer internals)
│   ├── offline/            sync queue (simpos Dexie pattern)
│   └── payments/           Razorpay
└── docs/                   fact-checked, one topic per file
```

**Stack:** Next.js 16 (webpack, not Turbopack) · tRPC v11 + superjson · Prisma +
PostgreSQL 16 · Redis · Tailwind v4 with Pantheon tokens · Vitest + Playwright ·
Turborepo + npm.

**Hosting:** Vercel for web, Docker for Postgres/Redis/API.

**Design language:** Pantheon — Petpooja's own system. Brand `#1770ee`, four
semantic token layers (`surface/text/border/icon`), 12-step type scale, three
radii, one shadow, POS Light and POS Dark. Our 8-palette system layers on top.

---

## 4. Independence from zerosky-repo

**Standing decision (Jack): do not touch `zerosky-repo`.** Peacock POS is a
separate product, not a rebrand and not a fork. Nothing is copied from it,
nothing in it is modified, and it is not a dependency of any phase.

Peacock POS is built from exactly two inputs:

| Input | Role |
| --- | --- |
| `_upstream/ury-*` (§1) | The source we port from — order engine, KOT, menu, shift, P&L, reports |
| Petpooja evidence (§3) | The design language (Pantheon) and API field naming we target |

Everything else is written fresh in this repo.

### 4.1 Capabilities we must build ourselves

Because we are not importing anything, these are net-new work here. They were
net-new here and are scheduled as real tasks in the phases:

| Capability | Phase | Note |
| --- | --- | --- |
| Auth, sessions, PIN login | 0 | bcrypt + JWT + Redis |
| GST-correct discount ordering | 1 | Discount applied before tax, reducing the taxable base. Worked example to assert in tests: 4×₹100, 5% GST, 10% off → taxable 360, tax 18, total 378. URY does not get this right either, so it needs its own tests |
| Seat-based bill split | 1 | Paise-exact validation; URY has amount-split only |
| ESC/POS printing | 1 | Build on `escpos-buffer` internals (§1 sources), not on any prior package |
| Payment gateway (Razorpay) | 1 | Fresh integration |
| Shift close gated on settled orders | 2 | Stricter than URY's time-bucket close — deliberate improvement |
| KDS app | 2 | Ported from `_upstream/ury-mosaic` |
| Theme system (Pantheon + palettes) | 0 and 6 | Tokens in Phase 0, full skin pass in Phase 6 |
| Offline sync queue | 6 | Follow simpos's Dexie pattern (§1 research) |
| Captain app (Flutter) | 6 | Fresh app against the Peacock API |
| Test suite | every phase | Written with each feature, per §7 |

### 4.2 What we deliberately do differently from URY

Not everything in URY is worth copying. These are conscious improvements:

- **Discount before tax.** Get the GST arithmetic right and assert it.
- **Shift close gated on unsettled orders**, rather than a time window.
- **Real floor geometry retained** (URY is strong here — we keep its X/Y/W/H
  model) but with a layout switcher from the start.
- **Course/hierarchy menu model** from URY, not a flat category list.
- **No Frappe-isms** — see §5 for the explicit reject list.

---

## 5. Schema plan

The schema is designed from three sources at once, which is the main advantage of
a fresh start: URY's doctypes for structure, Petpooja's API field names for
compatibility, and our own corrections for the things both get wrong.

**Reconciliation rules:**

- Money is `Decimal @db.Decimal(10,2)`, never float. Serialise with
  `Number(...)` at the API boundary — superjson has no Prisma `Decimal`
  serializer.
- Quantities are `Decimal @db.Decimal(10,3)`.
- Do not carry Frappe-isms: no `docstatus` integers (use enums), no
  `naming_series` (use a sequence table), no child-table `parent`/`parentfield`/
  `idx` (use real foreign keys), no comma-separated ID strings (use join tables
  or JSONB).
- Petpooja's wire format is *not* a model: they string-type every number,
  double-nest `orderinfo.OrderInfo`, and ship a misspelled
  `custome_payment_type`. Copy their concepts, not their encoding.

**Entities, grouped:**

| Group | Models |
| --- | --- |
| Tenancy | `Tenant`, `Branch`, `Restaurant`, `Room`, `Table`, `TableMerge` |
| Menu | `Category`, `Item`, `ItemVariation`, `ModifierGroup`, `Modifier`, `Menu`, `MenuItem`, `MenuCourse`, `MenuForRoom`, `OrderTypeMenu`, `PriceList`, `PriceListItem` |
| Orders | `Order`, `OrderItem`, `OrderItemModifier`, `OrderCharge`, `OrderDiscount` |
| Kitchen | `Kot`, `KotItem`, `KotErrorLog`, `ProductionUnit`, `ProductionItemGroup`, `KdsOrderType`, `PrinterSetting` |
| Payments | `Payment`, `PaymentMode`, `SplitGroup` |
| Shift | `Shift`, `SubShiftClosing`, `ShiftPaymentReconciliation`, `CashMovement` |
| Inventory | `InventoryItem`, `Uom`, `Recipe`, `RecipeItem`, `StockAdjustment`, `PurchaseOrder`, `PurchaseOrderItem`, `Supplier` |
| Costing | `DailyPnl`, `PnlBreakup`, `CostOfGoods`, `FixedExpense`, `VariableExpense` |
| CRM | `Customer`, `CustomerAddress`, `LoyaltyAccount`, `Reservation`, `Feedback` |
| Promotions | `Coupon`, `DiscountRule` |
| Platform | `User`, `Role`, `Capability`, `RoleCapability`, `AuditLog`, `Partner`, `BranchPartner` |

**Fields that fix known gaps** (all confirmed absent from the current schema by
grep — zero matches each): `Order.roundOff`, `Order.coreTotal`,
`Order.serviceCharge`, `OrderCharge` (packing/delivery, each with its own GST and
`gstLiable` party), `Item.hsnCode`, `ItemVariation` (price varies, not just
label), `Customer` (no model exists today), `Reservation`, `Recipe`, `Coupon`,
`AuditLog`, `Order.orderNumber` (sequential display number per branch),
`Order.printCount` (feeds audit counters).

---

## 6. Phases

Each phase ends in something demonstrable. No phase depends on a later one.

### Phase 0 — Skeleton (week 1)

Turborepo + npm workspaces; Prisma with the §5 schema; first migration; tRPC
wired; Pantheon tokens in Tailwind v4; auth with PIN login; seed data for one
restaurant with two rooms, twelve tables, and a real Indian menu. Vitest and
Playwright running in CI.

**Exit:** `npm run build` clean, login works, seeded menu visible.

### Phase 1 — Order to bill, correct (weeks 2–3)

Port `ury_order.py` order creation and totals. Order types all four. Cart with
variations and modifiers respecting min/max. KOT generation with station routing
from `ury_kot_generate.py`. Thermal printing via `@peacock/print` (built on
`escpos-buffer`). Payments with split and part-tender via `@peacock/payments`
(Razorpay).
**Round-off and `coreTotal` from day one.** Bill and KOT templates.

**Exit:** take a dine-in order, fire a KOT to a station printer, split the bill
across cash and UPI, print a GST bill that rounds to the rupee.

### Phase 2 — Floor, kitchen, shift (weeks 4–5)

Floor plan with real X/Y/W/H geometry and multiple layouts, ported from
`ury_table`. Table transfer, merge, hold, reservations. KDS with ticket states,
prep timers, course indication, order-type filtering, and the failed-KOT queue
from `ury_kot_error_log`. Multi-cashier shift close ported from
`sub_pos_closing.py`, per-mode float and variance, formal Z report.

**Exit:** run a full service — open till, seat tables, transfer an order, close
with variance explained.

### Phase 3 — Menu depth and compliance (weeks 6–7)

Menu resolution ported from `ury_pos/api.py`: per-room and per-order-type menus,
price lists, courses. Menu and table CRUD UIs. HSN codes, tax-inclusive pricing,
IGST vs CGST/SGST by state, tax configuration UI. Service, packing, and delivery
charges with per-charge GST. Audit log plus the audit counters Petpooja shows and
we lack (bills modified, reprinted, waived).

**Exit:** a delivery order priced from a different menu, taxed correctly, with
every mutation logged.

### Phase 4 — Inventory and money truth (weeks 8–10)

Recipe/BOM with the nested walk from `ury_daily_p_and_l.py`. Auto-deduct stock on
sale. UOM conversion. Purchase order and supplier UIs over ported procedures.
Wastage and variance reports. **Daily P&L**: COGS, gross, direct and indirect
expenses, net profit. Charts throughout — Recharts, following FinOpenPOS's
dashboard patterns. Port the remaining URY reports to reach 14, then the
multi-outlet rollup.

**Exit:** close a day and see a real P&L with COGS derived from recipes.

### Phase 5 — Growth (weeks 11–13)

Customers, then loyalty. Coupons and the discount rule engine (day, time, min
spend, max cap). QR/digital menu. Feedback. Per-capability permissions replacing
role-only checks. Outlet switcher and head-office view.

### Phase 6 — Mobile and polish (weeks 14–16)

Captain app to parity: offline queue via `@peacock/offline`, LAN printer
discovery, failed-KOT badge — the three things Petpooja's Captain drawer reveals
they consider essential. Full Pantheon skin pass including POS Dark. Performance
work against a year of seeded transactions.

**Deferred by standing decision:** Zomato/Swiggy aggregator integration (contract
documented in `COMPETITIVE-ANALYSIS.md` §6), payroll, marketplace.

---

## 7. Definition of done, per phase

No phase is complete on "code written". Each requires:

1. `npm run build` clean with `--webpack`.
2. Typecheck clean across every package.
3. Tests written *with* the feature, not after. Full suite green three
   consecutive runs (we have caught real flakiness this way before).
4. `prisma migrate status` — zero drift.
5. A manual browser walk of every affected route, light and dark.
6. Docs updated in the same commit as the code.

---

## 8. Risks

| Risk | Mitigation |
| --- | --- |
| The port becomes a transliteration of Frappe idioms | §5 lists Frappe-isms to reject explicitly. Review each ported file against that list |
| Scope creep from 120 catalogued gaps | Phases are ordered by operator pain. P2 items are not scheduled before Phase 5 |
| Language decision reopens mid-build | Phases 0–2 are the only sunk cost if Rust is chosen later; §2.1 gives the reversible path |
| Fresh start loses working code | §4 enumerates exactly what comes across, tests included |
| Money bugs | Decimal everywhere, `Number()` at the boundary, GST arithmetic asserted with worked examples in tests |
| Untested URY assumptions | We have read URY's source but never run it. Where behaviour is unclear, write our own test first and treat URY as a hint, not a spec |

---

## 9. Open questions for Jack

1. **Language** — this plan assumes TypeScript and explains why (§2.1). Confirm,
   or say Rust and I will re-cut Phases 0–2.
2. **Name** — "Peacock POS" everywhere in UI and package names (`@peacock/api`)?
3. **First vertical** — Phase 1 targets dine-in. Correct, or lead with takeaway?
4. **Reference docs** — the Petpooja research lives outside this repo. Copy the
   three research documents into `peacock-pos/docs/` so this repo is
   self-contained?
