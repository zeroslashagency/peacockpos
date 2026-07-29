# Plan Review — Four Independent Model Verdicts

Four reviewers, four different models, each read `PLAN.md`, `RUST_MIGRATION_PLAN.md`, and the actual `_upstream/ury-ury/` source independently. Reviewers A–C had no visibility into each other. Reviewer D was given A–C's consolidated verdict *after* forming its own view, and was asked specifically to dissent where the consensus was wrong.

| Reviewer | Model | Verdict |
|---|---|---|
| A | Claude Opus 5 | Fork URY, **keep Frappe** + Vercel-hosted branded UI |
| B | GPT-5.6 Sol | Fork URY, **strip Frappe** → FastAPI on Fly.io, React on Vercel |
| C | GPT-5.6 Terra | Fork URY, **strip Frappe** → FastAPI on Railway, React on Vercel |
| D | Grok 4.5 | Fork URY, **keep Frappe** + Vercel UI — sided with A on the split |

**Unanimous: 4 of 4 rejected both the Rust rewrite and the TypeScript rewrite.** All four said fork the open-source URY and modify it. This is what you already said you wanted ("we have open source, we modify this thing") — the reviewers independently confirmed it is also the correct engineering call.

**The keep-vs-strip Frappe split resolved 2–2 on headcount but decisively on argument.** Reviewer D broke the tie toward keeping Frappe and priced the alternative: stripping it means rebuilding the tax engine (2–3 wks), `Version` audit log (1 wk), the Desk admin CRUD screens (4–6 wks), and India Compliance e-invoice/GSTR (8+ wks) — **18–26 weeks of uncosted work**, plus COGS becomes manual entry and every POS Invoice lands on GSTR-1 unconsolidated.

---

## 1. Verdict comparison

| Path | Opus 5 | GPT Sol | GPT Terra |
|---|---|---|---|
| (a) Rust rewrite | ✗ Disqualified — no Rust runtime on Vercel, 40–60 wks solo | ✗ Buys performance nobody needs | ✗ 24–32 wks, Vercel-incompatible |
| (b) TypeScript rewrite | ✗ 28–40 wks real, you own the tax engine forever | ✗ Rewrites proven code | ✗ 16 wks to rebuild what works |
| **(c) Fork URY** | **✓ + Vercel UI on top** | **✓** | **✓** |
| (d) Keep URY, reskin only | ~ high ops burden | ~ high ops burden | ~ Frappe dependency |

### Time to first real production order

| Path | Opus 5 | GPT Sol | GPT Terra |
|---|---|---|---|
| Rust rewrite | 40–60 weeks (solo) | 24–32 weeks | 24–32 weeks |
| TypeScript rewrite | 28–40 weeks | 16 weeks paper / longer real | 16 weeks |
| **Fork URY** | **2–4 weeks** | **4–6 weeks** | **2–3 weeks** |

### Cost and reuse

| Path | Upstream logic reused | Monthly cost |
|---|---|---|
| Rust rewrite | ~0% | $50–250 |
| TypeScript rewrite | ~15% | $20–90 |
| **Fork URY** | **~95–100%** | **$15–60** |

---

## 2. Errors found in `RUST_MIGRATION_PLAN.md`

Opus 5 read the upstream source line by line and found the plan does not match reality. These are real defects in what I wrote:

**The doctype table is largely invented.** Roughly 60% of the 36 "mapped" doctypes do not exist in `_upstream/ury-ury/ury/ury/doctype/`. `ury_bom`, `ury_pricelist`, `ury_customer`, `ury_tax_rule`, `ury_discount`, `ury_settings`, `ury_shift`, `ury_payment_entry`, `ury_modifier`, `ury_course`, `ury_item_group`, `ury_order_type`, `ury_pos_opening_entry`, `ury_void_reason` and others were fabricated. The real ones include `sub_pos_closing`, `sub_pos_invoices`, `ury_kot_error_log`, `menu_for_room`, `order_type_menu`, `aggregator_settings`, `item_add_on`, `pos_item_variants`, `ury_report_settings`, `ury_fixed_expenses`, `ury_cost_of_goods`, `ury_printer_settings`, `ury_restaurant`.

**The "Python original" code snippets are wrong.** `_get_merge_cluster` actually takes `(table)`, returns `(members, table_by_name)`, reads `merged_with` from `URY Table` (not from the order), scopes the BFS to `restaurant_room`, and is wrapped by rules enforcing same-room, rejecting already-merged and occupied targets. `process_items_for_kot` takes 8 arguments, resolves production units per `POS Profile.branch`, resolves course per item, and flips to `"OrderModified"` when a KOT already exists. My 8-line grouping function modeled none of it.

**The BOM/P&L port is wrong in three ways.** Real code is two hardcoded levels, not `MAX_BOM_DEPTH = 3`. It divides by `bom.quantity` for per-unit normalisation — my version dropped that entirely. It prices from `Item Price` on the configured buying price list, not from stock valuation. So the Rust port would silently compute different COGS.

**"Keep the React frontends" is wrong.** URY ships **one** React app (`pos/`) and **two Vue 3** apps (`urypos/`, `URYMosaic/` — the KDS). All three reviewers independently confirmed this from the `package.json` files.

**The real dependency was never costed.** Both plans priced this as "6,075 lines of Python to port." Those lines are glue. The money is computed by ERPNext: `calculate_taxes_and_totals()`, tax templates, Price List resolution, `is_pos=1` + `update_stock=1` driving the Stock Ledger and GL entries, POS Invoice → consolidated Sales Invoice at day close. `ury/hooks.py` ships ~110 custom field fixtures across ~15 **ERPNext** doctypes. That is the actual surface, and a rewrite must reimplement all of it.

### Vercel incompatibilities (all three reviewers flagged these)

| Plan section | Problem |
|---|---|
| §4.2 WebSocket + `tokio::sync::broadcast` | Vercel functions are request-scoped with no shared memory. A broadcast sender fans out to one instance's subscribers only. |
| `sqlx::Pool` connection pooling | N cold instances × pool size exhausts Postgres. Serverless needs an external pooler. |
| "Frappe background jobs → Tokio tasks" | Tokio tasks need a process that stays alive. Vercel functions terminate after the response. |
| §5 "Docker + Kubernetes, 3 replicas, PG primary+replica, Redis Cluster" | Not Vercel at all, and directly violates your "simple architecture" constraint. |
| Rust runtime | Vercel has no first-class Rust runtime. The plan silently requires a second host. |
| §9 performance targets | Every number is unmeasured. The real bottleneck is an N+1: `frappe.db.get_value("Item", …)` per item inside a per-production-unit loop. Fixing the query in Python gets most of the win. |

Also: Frappe already supports PostgreSQL, so the MariaDB → Postgres migration was never a rewrite prerequisite.

---

## 3. Errors found in `PLAN.md`

- **"Backend rewrite cost: 0"** then schedules 16 weeks across 7 phases and ~60 Prisma models. That is not a comparison.
- **"The 503 passing tests are the single largest asset we have"** — those tests live in `zerosky-repo`, which §4 declares off-limits. Net tests carried into `peacock-pos`: zero.
- **"URY does not get discount-before-tax right either"** — unverified. URY sets `additional_discount_percentage` and delegates; ERPNext controls this via `apply_discount_on` = *Net Total* vs *Grand Total*. It is a one-field config, not a defect. (Your worked example — 4×₹100, 5% GST, 10% off → 360 / 18 / 378 — is arithmetically correct.)
- **"Serialise money with `Number(...)` at the API boundary"** — `Number()` produces an IEEE-754 double, which is exactly the float-money bug the same paragraph forbids. Serialise as strings.
- **"Vercel for web, Docker for Postgres/Redis/API"** — self-contradictory, and hides a VPS you would operate anyway.
- **"Thermal printing via `@peacock/print`"** — a Vercel function cannot open a TCP socket to `192.168.1.50:9100` inside the restaurant. This requires on-premise code, full stop.
- **Rejecting `naming_series` then adding `Order.orderNumber`** — that *is* the naming series, and you need it for gapless invoice numbering under CGST Rule 46(b).
- **Offline queue in Phase 6** — offline is a Phase-0 data-model constraint for a POS, not a week-14 feature.

---

## 4. Blind spots both plans missed entirely

All three reviewers converged on the same gaps. Ranked by how badly each one can hurt you.

### Blocks go-live

**Printing is not a line item.** No printer model, transport, or codepage in either plan. Real work: ESC/POS over raw TCP 9100, USB/serial via a local agent, 58mm vs 80mm column math, and **Devanagari/Tamil bills must be rendered as bitmaps via `GS v0`** because cheap thermal heads cannot render Indic glyphs from codepages. Plus paper-cut and drawer-kick commands, printer-offline retry queue, and reprint audit counts. URY's `ury_printer_settings` child rows on both `URY Room` and `POS Profile` with `bill`/`kot` flags is a working model — copy it. URY already solves the browser path with QZ Tray (signed cert, `qz-tray@2.2.5`).

**Payment terminals.** Both plans say "Razorpay" and stop. Razorpay is online UPI/card. Indian restaurants use physical EDC terminals (Pine Labs, Paytm, Ingenico): send amount → cardholder enters PIN → approval code → slip → and end-of-day settlement must reconcile with POS card totals to the paisa. Merchant onboarding is measured in weeks. Neither plan mentions it.

**Idempotency.** Neither plan contains the word. Waiters double-tap on bad Wi-Fi and you get two KOTs and two invoice numbers — which also breaks GST sequencing. Every mutating endpoint needs a client-generated `Idempotency-Key` persisted with its response.

**Gapless invoice numbering.** CGST Rule 46(b): consecutive series, unique per financial year, ≤16 chars, no gaps. A failed submit cannot burn a number, so numbers must be allocated at commit inside the transaction — never optimistically on the client. Voided numbers cannot be reused and the gap must be explainable in an audit.

**Two waiters on one table.** URY has partial protection (`restrict_existing_order`, `invoice_printed`, `URY Table.occupied`, compare-and-set on `last_modified_time`) which is better than either plan — neither mentions optimistic concurrency at all. Still racy: read-then-write with no row lock. Fix: unique partial index on `(table_id) WHERE status='open'` plus `SELECT … FOR UPDATE`, keep the compare-and-set as a 409.

**Offline on the floor.** Cloud-only POS means one ISP outage = no orders, no KOTs. URY's on-prem deployment is *more* resilient than either proposed cloud rewrite. Minimum: PWA + IndexedDB write queue keyed by idempotency key, KOT printing driven from the local agent so the kitchen keeps working, and a hard rule that the client never invents invoice numbers.

### Costs money or credibility later

**GST reality.** Restaurant service is generally **5% GST without ITC** for non-specified premises — which makes ITC-style tax modelling wrong from the start. Credit notes (not row deletes) reverse a submitted invoice. HSN/SAC on every line — and `ury_menu_item.json` has no HSN field today, so that is a data-migration gap before go-live. E-invoice IRN via the IRP once turnover crosses the threshold. GSTR-1/3B export. ERPNext + the **India Compliance** app gives you e-invoice, e-way bill and GST returns for free; a rewrite drops all of it. Also: auto-added service charge is restricted under the CCPA 2022 guidelines — make it opt-in, not a default line.

**Business-day boundary — and a live upstream bug.** `sub_pos_closing.py` filters invoices with `"posting_date": ["between", [period_start, period_end]]` — a date-level comparison against datetime bounds, so **every dinner service that crosses midnight mis-buckets invoices**. It also sums `grand_total` while `ury_daily_p_and_l` uses `rounded_total` — two different definitions of revenue in one product. URY does have the right primitive (`URY Report Settings.hours` for the cutoff); neither plan mentions it.

**Money rounding.** Round-off applied once, at invoice level, to the nearest rupee, with the delta posted to a round-off ledger account (it shows on the P&L, it is not a display tweak). Per-line GST must be computed *before* rounding so CGST and SGST each equal half the total tax to the paisa. Neither plan specifies rounding direction or where the residual lands.

**Void/discount audit trail.** GST audit needs: original order state, void reason (structured, not free text), manager who approved, exact timestamp, and append-only storage. URY has `cancel_reason`, `verified`/`verified_by`, plus Frappe's built-in `Version` doctype giving field-level change history for free. A rewrite drops `Version`; both plans list `AuditLog` as one model with no spec.

**Auth/RBAC.** The Rust plan says "jsonwebtoken + argon2, one Permission struct." What actually exists upstream: `role_allowed_for_billing`, `role_restricted_for_table_order`, `transfer_role_permissions`, `role_permitted`, `button_permission.py`, plus 5 URY roles as fixtures — a *per-action* model with cashier/waiter split enforced server-side inside `sync_order`. Missing from both plans: PIN-based fast user switching on a shared terminal, and the fact that a JWT in localStorage on a shared floor tablet is worse than Frappe's httpOnly session cookie.

**Data migration from the live Frappe DB.** My plan's answer was five lines of `mysqldump | python | psql`. Reality: Frappe Link fields store document *names*, not IDs, so you need an old-name → new-id map. Child tables carry `parent`/`parentfield`/`parenttype`. `docstatus` → enum. `merged_with` CSV → JSONB. And MariaDB naive `datetime` → `timestamptz` where India is **UTC+5:30** — a half-hour offset that breaks naive conversion. Plus reconciliation that revenue and closing stock match to the paisa, during a window where the restaurant is taking orders. 3–6 weeks on its own.

**Multi-tenant vs one restaurant.** `PLAN.md` opens with `Tenant`/`Branch`/`Restaurant` for a single restaurant. Row-level tenant isolation done wrong is a data-leak bug class you do not need yet. URY's single-site model (Branch → Restaurant → Room → Table) is the right scope; Frappe multi-site handles customer #2.

**What ERPNext gives you free today that a rewrite silently throws away:** double-entry GL; POS Invoice → consolidated Sales Invoice; Stock Ledger with FIFO/moving-average valuation and negative-stock control; purchase cycle (PO → Receipt → Purchase Invoice) feeding real COGS; Price List / Item Price / pricing rules; CGST/SGST/IGST tax templates; Customer + loyalty; Trial Balance / P&L / Balance Sheet / GST reports; India Compliance (e-invoice, e-way bill, GSTR); the `Version` audit log; the role/permission engine; the background job queue; **the Desk admin UI for every doctype — that is your entire back-office CRUD, free**; REST + webhooks; backups.

### Licensing

`ury`, `ury-pos`, and `ury-mosaic` are all **AGPL-3.0**. `PLAN.md` says "licence is not a consideration" — that should be a decision, not an omission.

**Corrected scope (Grok 4.5 dissent, accepted).** Opus's framing — "any path that ships a network-accessible service built on that code triggers source-disclosure" — overstates the reach. The contamination boundary is the network call. Your Vercel Next.js app is a separate work that talks to the URY API over HTTP; it is an API client, not a derivative work. The AGPL §13 obligation attaches to the **forked backend**, and it runs to *users of that backend*. So: keep the fork's source available to whoever interacts with the Frappe service, and your branded frontend is not itself pulled in. This is still a decision to make deliberately with a lawyer if you ever resell the backend as SaaS — but "Vercel UI → AGPL contamination" is not the correct technical claim.

Note the irony holds either way: the Rust clean-room rewrite is the only path with zero AGPL exposure, and it is the worst path.

---

## 5. Upstream bugs to fix in week 1 of the fork

Opus 5 found these reading the real source; Grok 4.5 challenged two of them; I adjudicated all of them against the code directly. Verdicts below are verified, with line numbers.

| Bug | File:line | Effect | Status |
|---|---|---|---|
| `production_items = []` allocated **before** the `for p in productions` loop and appended to without reset | `ury_kot_validation.py:51` vs loop at `:57` | **Confirmed real.** Station B's KOT contains station A's items, and every subsequent station accumulates all prior stations' items. | ✅ Verified |
| `posting_date` date-level filter against datetime period bounds | `sub_pos_closing.py:42` | Every midnight-crossing dinner shift mis-buckets invoices — MariaDB casts the datetime bounds to date, so the filter matches **both whole days**. | ✅ Verified |
| Revenue definition split: `grand_total` vs `rounded_total` | `sub_pos_closing.py:45` vs `ury_daily_p_and_l.py:297` | Shift close and P&L reconcile to two different numbers. | ✅ Verified |
| **Status filter split: `status = "Paid"` vs `status IN ("Consolidated","Paid")`** | `sub_pos_closing.py:41` vs `ury_daily_p_and_l.py:94,131,162,305` | **New — found while adjudicating.** Shift close silently omits already-consolidated invoices that P&L counts. Compounds the `grand_total`/`rounded_total` split into two independent sources of reconciliation drift. | ✅ Verified |
| Dead code: `owner = waiter if not invoice.restaurant_table else waiter` | `ury_kot_validation.py:41` | Both branches identical, so the ternary is a no-op — and it reads `.restaurant_table` off `invoice` (a **name string**, not a doc), which would raise if the branches ever diverged. Opus attributed this to `ury_order.py`; it is actually in `ury_kot_validation.py`. | ✅ Verified, file corrected |
| `frappe.db.get_value("Item", …)` N+1 inside the comprehension | `ury_kot_generate.py:154` | 12-item order × 3 stations = 36 queries. This is the real bottleneck, not the language. | ✅ Verified |
| Second N+1, worse: `frappe.get_doc("Item", …)` (full doc load, not a single field) | `ury_kot_generate.py:214` | Same pattern in the cancel-KOT path but loading entire Item documents. | ✅ Verified |

### The N+1 fix, concretely

```python
item_codes = [i["item_code"] for i in kot_items]
rows = frappe.get_all("Item", filters={"name": ["in", item_codes]},
                      fields=["name", "item_group"])
item_group_map = {r.name: r.item_group for r in rows}

production_items = [
    item for item in kot_items
    if item_group_map.get(item["item_code"]) in productionItemGroups
]
```

One query instead of N per station. That is a ~12× improvement **in Python**, which is the point: porting to Rust without fixing the query just executes a bad algorithm quickly.

### Gapless invoice numbering — "allocate at commit" was underspecified

Grok correctly flagged that neither plan says *how*. The row must be locked, not just read:

```python
next_num = frappe.db.sql("""
    SELECT next_number FROM `tabNaming Series`
    WHERE series = %s FOR UPDATE
""", (series,), as_dict=True)[0].next_number

invoice.name = f"{series}-{next_num}"
frappe.db.set_value("Naming Series", series, "next_number", next_num + 1,
                    update_modified=False)
frappe.db.insert(invoice)
frappe.db.commit()
```

If the insert fails the transaction rolls back and `next_number` is untouched, so no number is burned. A gap then only occurs on a deliberately cancelled invoice, which must carry a logged void reason for the audit.

---

## 6. The 20% that delivers 80% — reviewer consensus

Ship this and nothing else:

1. One restaurant, one branch, one menu. No tenancy, no outlet switcher.
2. Dine-in + takeaway. Defer aggregators and delivery.
3. Order → KOT → bill → payment. Cash + UPI QR + card-as-manual-entry. One bill printer, one printer per kitchen station.
4. Table state free/occupied/printed, plus merge and transfer (URY already has both, correctly rule-guarded).
5. GST correct: per-line tax, `apply_discount_on = Net Total`, one invoice series per FY allocated at commit, HSN on items, round-off at invoice level.
6. Shift open/close with declared vs expected per payment mode and a Z report. Fix the midnight bug while you are in there.
7. KDS: one screen, ticket list, mark-ready, cancel-KOT alert. `URYMosaic` already does this over Frappe socket.io.
8. Idempotency keys + optimistic concurrency on every order mutation. Day one, non-negotiable.
9. Local print/queue agent on the restaurant LAN so the kitchen survives an internet outage.

**Explicitly deferred:** recipe/BOM COGS and daily P&L (use ERPNext stock reports until the menu is stable), loyalty, coupon/discount-rule engine, QR digital menu, feedback, Flutter captain app, the full 14-report suite (Frappe's report builder covers ad-hoc needs), e-invoice/IRN until you cross the threshold (but keep the numbering design compliant now), multi-outlet rollup, payroll, marketplace.

---

## 7. The one real split — resolved

**Do you keep Frappe/ERPNext, or strip it out?**

| | Opus 5: keep Frappe | Sol + Terra: strip to FastAPI |
|---|---|---|
| Backend | Forked `ury` on ERPNext v15, untouched framework | URY's Python business logic, Frappe ORM swapped for SQLAlchemy |
| You inherit | GL, Stock Ledger, tax engine, price lists, GST reports, `Version` audit, RBAC, job queue, **Desk admin UI** | Nothing — you rebuild accounting/stock/tax yourself later |
| Realtime | Frappe's built-in socket.io, already wired for `kot_update_<branch>_<production>` | SSE from FastAPI, or Pusher, or 3s polling |
| Effort to first order | 2–4 weeks | 2–6 weeks |
| Ongoing | `bench update`, Frappe learning curve | Plain Python, no framework magic |

Opus 5's argument is the stronger one: stripping Frappe means you also strip the accounting, stock valuation, tax templates and GST reports that `ury_order.py` *calls into*. `sync_order`, `make_invoice`, and `split_bill` all delegate money math to ERPNext's `calculate_taxes_and_totals()`. Rip out Frappe and you have not saved the logic — you have just moved the rewrite into the hardest part of the product.

Sol and Terra are right that Frappe is heavy and that Railway/Fly is simpler to operate than a bench. But they are pricing the framework swap as "replace `frappe.get_doc()` with SQLAlchemy," which understates it: you also lose `Version`, the permission engine, the job queue, socket.io, and the free admin UI for all 36 doctypes.

**Grok 4.5 broke the tie toward keeping Frappe, and priced the alternative:**

| What you rebuild if you strip Frappe | Cost |
|---|---|
| Tax engine (`calculate_taxes_and_totals`, templates, `apply_discount_on`, HSN/SAC) | 2–3 weeks |
| `Version` audit log → append-only table + before/after JSON + query UI | 1 week |
| Desk admin UI → 15+ back-office CRUD screens | 4–6 weeks |
| India Compliance → e-invoice IRN, e-way bill, GSTR-1/3B | 8+ weeks |
| Double-entry GL, Stock Ledger valuation, consolidated Sales Invoice | remainder |
| **Total uncosted** | **18–26 weeks** |

And the failure modes are not just schedule: COGS becomes manual entry (so the P&L is fiction), and every POS Invoice lands on GSTR-1 unconsolidated — 500 line items where there should be one, which is where your accountant quits.

**Where all four agree:** whatever you do on the backend, your Vercel project stays a thin Next.js client. Vercel does not host the realtime layer, does not talk to printers, and does not run the cron. There is one small always-on box in the picture either way.

### Honest counterweight: the operational tax of keeping Frappe

Grok agreed with the ruling but flagged what the recommendation glosses over, and it is fair:

- `bench update` can break your fork — migrations conflict, custom fields collide. This is why the `peacock`-as-second-app split matters, and it still needs a staging bench to test updates against.
- Frappe's learning curve is steep: DocType JSON, hooks, server scripts, print formats. Debugging usually means reading Frappe/ERPNext source, not Stack Overflow.
- **If you do not have a Frappe-fluent developer, add a 2–3 week onboarding tax** that is not in the timeline below.
- One always-on box is a single point of failure during dinner service. The LAN print agent with a local queue is what keeps the kitchen running through an outage — treat it as required, not phase 5. Verify backups by performing an actual restore, not by checking that the backup ran.

---

## 8. Recommended shape

Synthesizing all four. Opus 5 and Grok 4.5 both landed here on the Frappe question:

```
[Vercel]  Next.js — marketing site + Peacock-skinned POS/manager UI
             |  route handlers as a thin BFF, Frappe API key server-side only
             v
[One box] Forked ury on ERPNext v15 + India Compliance
          Frappe Cloud private bench (~$25-50/mo) or Hetzner CX22 (~€5/mo)
          Frappe's built-in socket.io for KDS realtime  ← no new infra
             |
[LAN]     Node print agent — raw ESC/POS to :9100, subscribed to socket.io,
          local retry queue, bitmap path for Indic text  ← offline lifeline
```

**Fork hygiene:** fork to `peacock/ury`, and add `peacock` as a *second* Frappe app for your own doctypes and overrides so `ury` stays rebasable against upstream. Strip in the fork: aggregator plumbing, unused reports, `ury_daily_p_and_l` until later.

**Frontend migration:** skin URY's React 19 `pos/` app screen by screen behind flags, so every screen has a working fallback. Do not rebuild 37 components from scratch.

**Auth:** Frappe session cookies + its role engine (already enforced server-side). Add one endpoint for PIN-based user switching. Do not build a JWT layer for a shared floor tablet.

**Realtime:** Frappe socket.io, browser connects to `api.peacock.<domain>` directly. Zero new infrastructure.

**Payments:** static UPI QR + manual card entry day one. Razorpay orders + webhook (a legitimate Vercel serverless fit) in phase 2. EDC terminal only when a specific terminal is on the counter.

**Non-negotiable test suite:** GST arithmetic with worked examples, invoice-number gap-freeness under concurrent submits, and shift-close bucketing across midnight.

### Timeline

| Phase | Weeks | Outcome |
|---|---|---|
| 0. Stand up + fork | 1 | ERPNext v15 + India Compliance + forked `ury` on the box; `peacock` override app scaffolded; real menu, rooms, tables, tax template, bill + KOT printer; backups verified by an actual restore |
| 1. Correctness pass | 2 (wk 2–3) | The five upstream bugs above; `apply_discount_on = Net Total`; invoice series per FY allocated at commit; idempotency keys on `sync_order`/`make_invoice`; row lock + unique-open-order-per-table; test suite green |
| 2. Go live, one outlet | 2 (wk 4–5) | **First production order week 4 at the earliest, week 6 realistic.** Staff on URY's existing POS, print agent on the LAN, KDS on a kitchen screen, shift close + Z report drilled, ISP-failure drill run |
| 3. Peacock skin | 3 (wk 6–8) | Next.js on Vercel: login, menu/cart, table view, order panel in Pantheon tokens against the Frappe API, each screen flag-reversible |
| 4. Manager surface | 2 (wk 9–10) | Vercel dashboard: day sales, item-wise, cashier-wise, void/reprint audit, shift variance — read models straight off ERPNext |
| 5. Offline + payments | 2 (wk 11–12) | Dexie queue + conflict UX; Razorpay dynamic UPI + webhook; card EDC if a terminal is on site |
| Later, only if asked | — | Recipe/BOM COGS + P&L, loyalty, aggregators, captain app, e-invoice at threshold, second outlet |

**12 weeks to a branded POS running a real restaurant**, against 24–32 (realistically 40–60 solo) for the Rust plan and 16 (realistically 28–40) for the TypeScript plan — with accounting, stock and GST returns staying someone else's maintained code the whole way.

---

## 9. Where this review is still overconfident

Grok 4.5 was asked to attack the consensus. These held up:

**"First production order in week 4."** That assumes a clean ERPNext v15 bench (1 week if nothing fights you, 2–3 if bench or dependency issues bite), that the five bugs are quick fixes (the midnight one changes shift-close query logic, not a one-liner), that a real restaurant already has menu/rooms/tables/tax templates/LAN printer configured, and that staff train on the existing POS in two days. **Realistic: 4–6 weeks.** Week 4 is possible only if the restaurant is already set up. Falsified by: a bench install that needs debugging, or discovering the tax template needs per-item HSN backfill first.

**"The 20% scope is correct."** The list is right for go-live but the deferred items are undercosted as "optional." BOM/COGS is required for a real P&L (though you keep URY's implementation rather than build it). Aggregators are 30–40% of revenue for cloud kitchens. Deferring the captain app means waiters queue at the cashier's terminal. The 20% gets you *live*; reaching parity with Petpooja needs the other 80%.

**"Keep Frappe" is unanimous.** It is 2–2 on headcount (A and D keep, B and C strip) and only decisive on argument. The counter is operational, not technical: if you have no Frappe-fluent developer, the onboarding tax is real and the strip-to-FastAPI path is genuinely simpler to *operate* — it is just far more expensive to *build*. Falsified if you decide you will never need GL, stock valuation, or GST returns, in which case the accounting argument stops binding and Sol/Terra's path wins.

**One claim of Opus's I could not verify and have dropped:** it attributed the dead `owner = waiter` ternary to `ury_order.py`. It is in `ury_kot_validation.py:41`. Grok was right to withhold on it pending the source; the bug is real, the file was wrong.

---

**Reviewers:** Claude Opus 5, GPT-5.6 Sol, GPT-5.6 Terra (independent, no cross-visibility), Grok 4.5 (adversarial, given A–C's verdict after forming its own)
**Adjudication:** disputed bug claims verified line-by-line against `_upstream/ury-ury/` before inclusion
**Date:** 2026-07-28
