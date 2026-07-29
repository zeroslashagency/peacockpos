# Peacock Parity Harness

**The gate that makes silently wrong COGS impossible.**

This harness validates the Rust money arithmetic in `peacock-core` against an independent Python reimplementation of the upstream Frappe logic. Both implementations process the same fixtures; the harness diffs the results **to the paisa**. Exit code 0 means parity; non-zero means a discrepancy that must be investigated.

---

## Why this exists

From `RUST_MIGRATION_PLAN_V2.md` §6:

> The only credible way to validate a rewrite of money code is to run both implementations over the same data and diff to the paisa. Version 1 of the plan would have shipped silently wrong COGS.

**What it proves:** Tax and COGS arithmetic match between Rust (`peacock-core`) and a faithful reimplementation of the upstream Python (`scripts/parity_reference.py`).

**What it does NOT prove:** That either implementation matches a live Frappe instance processing real POS Invoices from a production database. Before cutover, you must replay 30 consecutive days of real invoices (including month-end, midnight crossings, voids, splits, merges) and reconcile to the paisa. That is the 30-day gate in §6.

---

## How to run

```bash
# From workspace root
cargo run -p peacock-parity
```

Expected output (success):
```
═══ Peacock Parity Harness ═══

Validating Rust implementations against Python oracle.
...
✓ ALL FIXTURES MATCH TO THE PAISA

  Python and Rust agree on:
    - Tax calculations (net, taxable, CGST, SGST, IGST, rounding)
    - COGS calculations (per-unit normalisation, two-level explosion)
    - Product Bundle COGS (bundle > BOM > plain precedence, no extra depth)
    - All three unset-item lists, kept separate by label

  Tested 22 fixtures.
  COGS MAX_LEVEL = 2 (matches upstream).
  Rounding: peacock_core::money::ROUNDING vs parity_reference.ROUNDING
            (half-away-from-zero; NOT yet confirmed against the site).
```

On failure, the harness exits non-zero and prints a diff table showing which fields diverged, by how much.

---

## What's validated

### Tax arithmetic (`peacock-core/src/tax.rs`)

1. **Worked example:** 4 × ₹100, 5% GST, 10% discount → taxable 360, tax 18, total 378
2. **CGST+SGST split with odd paisa:** Tax 18.01 → CGST 9.01, SGST 9.00 (no lost paisa)
3. **Interstate IGST:** Full tax on IGST, CGST and SGST both zero
4. **Round-off positive and negative:** Residual lands correctly signed
5. **Multi-line invoice:** No rounding drift when summing
6. **Rounding applied once:** At invoice level, not per-line

### COGS arithmetic (`peacock-core/src/cogs.rs`)

7. **BOM with `quantity != 1`:** Per-unit normalisation (`batch_cost / bom.quantity`). This is the v1 bug — a ₹70 batch for 10 units must cost ₹7/unit, not ₹70/unit.
8. **Two-level BOM:** Both levels normalising correctly
9. **Three-level BOM:** Third level priced as a leaf (MAX_LEVEL = 2 matches upstream exactly)
10. **Missing Item Price:** Lands in `unset_bom_items` and contributes zero cost (visible gap, not silent understatement)

### Additional coverage

11. **Discount basis:** Net Total vs Grand Total ordering
12. Tax and COGS **serialisation through strings** (never JS `Number()`)

### Rounding strategy (fixture 14)

14. **`14_tax_rupee_midpoint_round_off`** — 1 × ₹400.48 at 5% gives a grand total of
    exactly ₹420.50, an exact rupee midpoint. Together with
    `02_tax_intrastate_odd_paisa` (CGST = 9.005, an exact paisa midpoint) the harness
    now exercises the rounding choice at **both** scales.

**What these two prove:** that Rust and Python agree on the strategy, and that a
change to it on either side is loud. Flipping only `ROUNDING` in
`scripts/parity_reference.py` to `ROUND_HALF_EVEN` produces 6 diffs across these two
fixtures (`cgst` 9.01→9.00, `sgst` 9.00→9.01, `rounded_total` 421→420,
`round_off` +0.50→−0.50). Verified by mutation.

**What they do NOT prove:** that half-away-from-zero is what upstream does.

> Frappe rounds money through `flt(value, precision)`, which honours the site-wide
> `rounding_method` in **System Settings**. The Frappe v15 default is **Banker's
> Rounding** (half-to-even), not half-up. Frappe is not vendored here and no live
> site is reachable, so **which strategy the target deployment uses cannot be
> determined from this repo.** It is an open question, not a settled one.

Both sides pin the choice in a single named constant so it can be flipped in one
line each, and so flipping one without the other fails the harness:

| Side | Constant | Location |
|------|----------|----------|
| Rust | `ROUNDING` | `peacock-core/src/money.rs` (used by `Money::to_paisa` and `Money::to_rupee`) |
| Python | `ROUNDING` | `scripts/parity_reference.py` |

Before cutover: read `System Settings.rounding_method` off the real site. If it is
Banker's, flip both constants and update the `rounding_strategy_pin_*` tests in
`money.rs` plus `TestRoundingStrategyPin` in the oracle. Figures that move are
`total_tax`, `cgst`, `sgst`, `rounded_total` and `round_off` — and only on exact
midpoints. `net_total`, `discount`, `taxable_value` and COGS are never rounded and
cannot move.

### Product Bundle COGS (fixtures 15-22)

The third cost basis from `ury_daily_p_and_l.py:219-258`, previously absent from
`peacock-core`.

| Fixture | Proves |
|---------|--------|
| `15_cogs_bundle_plain_items` | Bundle of directly-priced items: `Σ (price × line.qty) × qty` |
| `16_cogs_bundle_line_batch_bom` | A bundle line whose BOM has `quantity != 1` is normalised to per-unit before `× line.qty` (₹104, not ₹608) |
| `17_cogs_bundle_adds_no_bom_depth` | A bundle is **not** a level of BOM depth: the line's BOM still gets both levels |
| `18_cogs_bundle_missing_price_unset_lists` | A bundle child miss and a BOM ingredient miss land in **different** lists |
| `19_cogs_bundle_fully_unpriced_guard` | Upstream's `if buying_price > 0` guard: zero cost contributed, gap still reported |
| `20_cogs_bundle_of_bundle_leaf` | Nested bundles are unsupported upstream; the inner bundle prices as a leaf |
| `21_cogs_bundle_wins_over_bom` | Precedence: an item that is both a bundle and has a BOM is priced as a **bundle** |
| `22_cogs_plain_item_missing_price` | A plain-item miss is labelled `ITEMS`, not `BOM SUB ITEMS` |

**The precedence rule, and the evidence.** `cogs_sold` runs three queries that
partition every invoice line into exactly one bucket. `d` is `tabProduct Bundle`
joined on `d.new_item_code = b.item_code`; `e` is an active/default/submitted
`tabBOM` joined on `e.item = b.item_code`:

| Bucket | Query | Predicates |
|--------|-------|-----------|
| plain | `non_pb_item_sales` (:73) | `d.new_item_code IS NULL` (:102) AND `e.item IS NULL` (:103) |
| BOM | `bom_item_sales` (:110) | `d.new_item_code IS NULL` (:139) AND `e.item IS NOT NULL` (:140) |
| bundle | `pb_item_sales` (:147) | `d.new_item_code IS NOT NULL` (:170) — **no BOM join at all** |

So precedence is **bundle → BOM → plain**, and it is a partition rather than a
fallback chain: an item that is both a Product Bundle and has an active default BOM
is priced as a bundle and its own BOM is never consulted. Fixture 21 pins this
(₹1 as a bundle vs ₹999 as a BOM); mutating the oracle to let BOM win produces a
₹998 diff.

**A bundle is not an extra level of depth.** Line :231 calls `inner_bom_process` —
the same function at the same entry point as the top-level BOM bucket at :201 — so a
bundle line's BOM still walks both levels and `MAX_LEVEL` stays 2. Getting this wrong
truncates the walk and understates COGS: mutating the oracle to enter at level 2
turns fixture 17 from ₹50 into ₹20 and pushes `BURGER` into the unset list.

**Three unset lists, not one.** Upstream keeps `unset_item_prices`,
`unset_pb_item_prices` and `unset_bom_item_prices` separate and renders them under
the labels `ITEMS`, `BUNDLE SUB ITEMS` and `BOM SUB ITEMS` (:261-266). They are
diffed separately here, because merging them keeps the cost identical while
destroying the only routing information the operator gets: fixing a BOM ingredient's
buying price is a different action from fixing a bundle component's. Note that a miss
inside a bundle line's BOM goes to the **BOM** list (:236-237), not the bundle list.

**What these fixtures still do NOT prove:**

- **Bundle selling-price allocation.** Only the buying-side COGS is ported. How
  ERPNext splits a bundle's *selling* price across children (Packed Item rows,
  delivery-note valuation) is untouched.
- **No `docstatus` filter on Product Bundle.** Upstream's lookup at :222 filters only
  on `new_item_code` — unlike the BOM lookup at :227, which requires
  `is_active=1, is_default=1, docstatus=1`. A draft bundle therefore still captures
  the item. That asymmetry is preserved but not exercised: the fake repo has no
  docstatus, so the harness cannot detect a change to it.
- **Item-name vs item-code in the unset lists.** Upstream stores `item_name`
  (:34, :182, :240); this port stores `item_code`. The set of flagged items is the
  same, but the rendered `remarks` string would differ. Not diffed.
- **Bundle-level aggregation over a real day's invoices.** Fixtures price one item at
  a time. `cogs_for_order_with_bundles` aggregation is covered only by
  `peacock-core` unit tests, not by the harness.
- **`qty` truncation.** Upstream casts the plain bucket's qty with `int()` (:184) but
  the BOM and bundle buckets with `float()` (:209, :249). Fixtures use whole
  quantities, so this divergence is not exercised.

---

## The fixture format

Fixtures live in `peacock-parity/fixtures/*.json`. Two kinds:

### Tax fixture
```json
{
  "kind": "tax",
  "name": "descriptive_name",
  "lines": [
    {"quantity": "4", "rate": "100"}
  ],
  "discount": "40",
  "tax_rate": "0.05",
  "supply_type": "intrastate",
  "discount_basis": "net_total"
}
```

### COGS fixture
```json
{
  "kind": "cogs",
  "name": "descriptive_name",
  "item": "MASALA-CHAI",
  "qty": "5",
  "buying_price_list": "Buying",
  "bundles": {
    "COMBO": [
      {"item_code": "MASALA-CHAI", "qty": "2"}
    ]
  },
  "boms": {
    "MASALA-CHAI": {
      "quantity": "10",
      "items": [
        {"item_code": "TEA-LEAVES", "qty": "10"},
        {"item_code": "MILK", "qty": "100"}
      ]
    }
  },
  "prices": {
    "TEA-LEAVES": "2.00",
    "MILK": "0.50"
  }
}
```

`bundles` is keyed by `new_item_code` and is **optional** — fixtures 07-10 omit it and
exercise only the BOM and plain buckets. Every COGS fixture runs through
`cogs_for_item_with_bundles`, so the three-way precedence is on the path even when the
map is empty.

All numerics are **strings** (serialisation safety check).

---

## The Python oracle

`scripts/parity_reference.py` is a standalone, zero-dependency reimplementation of:

- `peacock-core/src/tax.rs` (`compute_totals`)
- `_upstream/ury-ury/ury/ury/doctype/ury_daily_p_and_l/ury_daily_p_and_l.py` (`inner_bom_process`, `inner_inner_bom_process`)
- the same file's Product Bundle block, :219-258 (`cogs_for_bundle`), and the
  three-bucket partition from the SQL at :73-175 (`cogs_dispatch`)

**Critical:** It must be an *independent* reimplementation from the upstream Python, **not** a transliteration of the Rust. If both sides share a mistake, the harness proves nothing. This is the harness's main limitation — it validates that Rust and Python agree with each other, not that either matches Frappe on real data.

The Python script:
- Uses `decimal.Decimal` (never float)
- Preserves the upstream's two-level BOM walk
- Divides by `bom.quantity` (line 38 of `ury_daily_p_and_l.py`)
- Prices from `Item Price` (line 30)
- Accumulates all three unset lists separately
- Reproduces the bundle → BOM → plain partition and the `buying_price > 0` guard
- Pins the rounding strategy in one constant with the divergence documented
- Runs its own self-tests: `python3 scripts/parity_reference.py --test` (19 tests)

Its Product Bundle logic was read off the upstream Python block, with the derivation
recorded as an annotated transcription of the source's control flow in a comment above
`cogs_for_bundle` — not copied from the Rust. The mutation checks above (flipping
precedence, entering the BOM walk at the wrong level, flipping the rounding mode) each
produce diffs, which is the evidence the fixtures are load-bearing rather than
vacuously passing.

---

## Integration with CI

Add to your CI pipeline:

```yaml
- name: Parity check
  run: cargo run -p peacock-parity
```

This ensures every commit to `peacock-core/src/{tax,cogs}.rs` is validated against the oracle. A passing harness is a **necessary** condition for merge, not sufficient — the 30-day real-data replay is still required before cutover.

---

## What to do when the harness fails

1. **Read the diff table.** It shows fixture name, field, Python value, Rust value, and delta.
2. **Check the fixture.** Is it a valid test case? Does it match a known upstream behaviour?
3. **Check both implementations.** The bug could be in either Rust or Python. Read the upstream source (`ury_daily_p_and_l.py:10-58` for COGS) to confirm the ground truth.
4. **Fix and re-run.** The harness is deterministic — the same inputs always produce the same diff.

If the harness finds a **real discrepancy in `peacock-core`**, that is a **success**, not a failure. The harness exists to catch exactly that.

---

## Limitations (stated explicitly)

- **Not connected to a live Frappe instance.** The harness validates arithmetic logic, not integration with ERPNext's `calculate_taxes_and_totals()`, Stock Ledger, GL Entry, or the India Compliance app.
- **Fixture-driven, not property-based.** We test known cases, not randomly generated inputs. Expand the fixture set as new edge cases are discovered.
- **The Python oracle is also new code.** It could share a mistake with the Rust. The only way to be certain is the 30-day replay against production data.
- **No midnight-crossing shift bucketing test here.** That requires `BusinessDay` and is covered in `peacock-core`'s unit tests (`businessday.rs`).

---

## File inventory

| File | Purpose |
|------|---------|
| `peacock-parity/src/main.rs` | Harness: loads fixtures, runs Rust, invokes Python, diffs |
| `scripts/parity_reference.py` | Python oracle (stdlib only, no deps) |
| `peacock-parity/fixtures/*.json` | 22 test cases: the 9 gates from §6, the rounding midpoints, and the Product Bundle bucket |
| `peacock-parity/Cargo.toml` | Depends on `peacock-core`, `serde_json`, `colored` |
| `peacock-parity/README.md` | This file |

---

## References

- `RUST_MIGRATION_PLAN_V2.md` §6: The parity harness specification and the 9 correctness gates
- `GROUND-TRUTH.md`: Authoritative facts about the upstream codebase
- `_upstream/ury-ury/ury/ury/doctype/ury_daily_p_and_l/ury_daily_p_and_l.py:10-58`: The upstream COGS walk
- The same file, `:73-175`: the three-query bucket partition that fixes cost-basis precedence
- The same file, `:219-258`: the Product Bundle cost path

---

**Last updated:** 2026-07-29  
**Covers:** Tax arithmetic, COGS across all three upstream cost bases (plain, two-level BOM, Product Bundle), the three unset-item lists, and the rounding strategy at both the paisa and rupee midpoints  
**Does NOT cover:** Live Frappe integration, the actual site `rounding_method`, bundle selling-price allocation, gapless numbering, idempotency, concurrency, GL posting
