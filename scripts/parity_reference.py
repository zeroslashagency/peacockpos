#!/usr/bin/env python3
"""
Parity reference implementation — faithful port of upstream Frappe COGS and tax logic.

This is the oracle against which the Rust implementation is validated. It MUST be an
independent reimplementation from the upstream Python (ury_daily_p_and_l.py), not a
transliteration of the Rust, so both sides can catch each other's mistakes.

NO EXTERNAL DEPENDENCIES. Uses only the Python 3 stdlib (decimal, json, unittest).
"""

import json
import sys
import unittest
from decimal import Decimal, ROUND_HALF_EVEN, ROUND_HALF_UP
from typing import Dict, List, Optional, Tuple


# ============================================================================
# Money arithmetic
# ============================================================================

# The one rounding mode used by every money rounding in this oracle.
#
# OPEN QUESTION — not confirmed against the target site.
#
# Frappe rounds through flt(value, precision), which honours the site-wide
# `rounding_method` setting in System Settings. In Frappe v15 the DEFAULT is
# Banker's Rounding (ROUND_HALF_EVEN); the alternative is "Commercial Rounding"
# (ROUND_HALF_UP). Frappe is not vendored in this repo and no live site is
# reachable from here, so which one the target deployment uses CANNOT be
# determined here. It must be read off System Settings.rounding_method on the
# real site before cutover.
#
# This must stay in lockstep with peacock-core/src/money.rs `ROUNDING`. Both
# sides are single constants precisely so they can be flipped together; flipping
# only one makes the parity harness fail, which is the intended alarm. Flipping
# to ROUND_HALF_EVEN moves total_tax, cgst, sgst (paisa boundary) and
# rounded_total, round_off (rupee boundary) on exact midpoints only.
ROUNDING = ROUND_HALF_UP

# Kept only so the strategy-pin self-tests can state what the other choice
# produces. Never used by the oracle itself.
_ROUNDING_ALTERNATIVE = ROUND_HALF_EVEN


def to_paisa(d: Decimal) -> Decimal:
    """Round to 2 decimal places using ROUNDING."""
    return d.quantize(Decimal("0.01"), rounding=ROUNDING)


def to_rupee(d: Decimal) -> Decimal:
    """Round to nearest whole rupee using ROUNDING."""
    return d.quantize(Decimal("1"), rounding=ROUNDING)


# ============================================================================
# Tax calculation (tax.rs equivalent)
# ============================================================================

def compute_tax_intrastate(taxable: Decimal, rate: Decimal) -> Dict[str, Decimal]:
    """
    CGST + SGST, each exactly half the total tax to the paisa.
    Reference: peacock-core/src/tax.rs:73-84
    """
    total_tax_raw = taxable * rate
    total_tax = to_paisa(total_tax_raw)
    
    # CGST = total / 2, rounded to paisa
    cgst = to_paisa(total_tax / Decimal("2"))
    # SGST = total - CGST ensures no lost paisa
    sgst = total_tax - cgst
    
    return {
        "cgst": cgst,
        "sgst": sgst,
        "igst": Decimal("0"),
        "total_tax": total_tax,
    }


def compute_tax_interstate(taxable: Decimal, rate: Decimal) -> Dict[str, Decimal]:
    """
    Full tax as IGST.
    Reference: peacock-core/src/tax.rs:86-95
    """
    total_tax_raw = taxable * rate
    total_tax = to_paisa(total_tax_raw)
    
    return {
        "cgst": Decimal("0"),
        "sgst": Decimal("0"),
        "igst": total_tax,
        "total_tax": total_tax,
    }


def compute_totals(
    lines: List[Dict],
    discount: Decimal,
    tax_rate: Decimal,
    supply_type: str,
    discount_basis: str,
) -> Dict[str, Decimal]:
    """
    Complete invoice totals with GST.
    
    Ports peacock-core/src/tax.rs:175-220.
    
    Args:
        lines: [{"quantity": Decimal, "rate": Decimal}, ...]
        discount: Discount amount
        tax_rate: e.g. Decimal("0.05") for 5%
        supply_type: "intrastate" or "interstate"
        discount_basis: "net_total" or "grand_total"
    
    Returns:
        {
            "net_total": Decimal,
            "discount": Decimal,
            "taxable_value": Decimal,
            "cgst": Decimal,
            "sgst": Decimal,
            "igst": Decimal,
            "total_tax": Decimal,
            "grand_total": Decimal,
            "rounded_total": Decimal,
            "round_off": Decimal,
        }
    """
    # Sum all line amounts
    net_total = sum(line["quantity"] * line["rate"] for line in lines)
    
    # Taxable value depends on discount basis
    if discount_basis == "net_total":
        taxable_value = net_total - discount
    elif discount_basis == "grand_total":
        taxable_value = net_total
    else:
        raise ValueError(f"unknown discount_basis: {discount_basis}")
    
    # Compute tax
    if supply_type == "intrastate":
        tax = compute_tax_intrastate(taxable_value, tax_rate)
    elif supply_type == "interstate":
        tax = compute_tax_interstate(taxable_value, tax_rate)
    else:
        raise ValueError(f"unknown supply_type: {supply_type}")
    
    # Grand total depends on discount basis
    if discount_basis == "net_total":
        grand_total = taxable_value + tax["total_tax"]
    elif discount_basis == "grand_total":
        grand_total = net_total + tax["total_tax"] - discount
    else:
        raise ValueError(f"unknown discount_basis: {discount_basis}")
    
    # Apply rounding once, at invoice level
    rounded_total = to_rupee(grand_total)
    round_off = rounded_total - grand_total
    
    return {
        "net_total": net_total,
        "discount": discount,
        "taxable_value": taxable_value,
        "cgst": tax["cgst"],
        "sgst": tax["sgst"],
        "igst": tax["igst"],
        "total_tax": tax["total_tax"],
        "grand_total": grand_total,
        "rounded_total": rounded_total,
        "round_off": round_off,
    }


# ============================================================================
# COGS calculation (cogs.rs equivalent)
# ============================================================================

def cogs_for_item(
    item: str,
    qty: Decimal,
    buying_price_list: str,
    boms: Dict[str, Dict],
    prices: Dict[str, Decimal],
) -> Tuple[Decimal, List[str]]:
    """
    Compute COGS for a single item.
    
    Ports _upstream/ury-ury/ury/ury/doctype/ury_daily_p_and_l/ury_daily_p_and_l.py:10-58
    (inner_bom_process and inner_inner_bom_process).
    
    Critical arithmetic:
    1. Exactly TWO levels (not depth-3 recursion)
    2. Divide by bom["quantity"] for per-unit cost (line 38, 57)
    3. Price from Item Price on buying_price_list (line 30, 49)
    4. Accumulate unset_bom_items (visible data gap, not silent understatement)
    
    Args:
        item: item code
        qty: quantity sold
        buying_price_list: the price list name (not used in this fake, but required in signature)
        boms: {"ITEM-CODE": {"quantity": Decimal, "items": [{"item_code": str, "qty": Decimal}]}}
        prices: {"ITEM-CODE": Decimal}
    
    Returns:
        (cost, unset_bom_items)
    """
    return _cogs_at_level(item, qty, buying_price_list, boms, prices, level=1)


MAX_LEVEL = 2  # Matches upstream's two hardcoded functions


def _cogs_at_level(
    item: str,
    qty: Decimal,
    buying_price_list: str,
    boms: Dict[str, Dict],
    prices: Dict[str, Decimal],
    level: int,
) -> Tuple[Decimal, List[str]]:
    """Inner recursion with depth tracking."""

    # If at max depth OR no BOM, price as a leaf
    if level > MAX_LEVEL or item not in boms:
        if item in prices:
            return (prices[item] * qty, [])
        else:
            # No price: record in unset_bom_items, contribute zero cost
            return (Decimal("0"), [item])

    # Explode BOM
    bom = boms[item]

    if bom["quantity"] == 0:
        raise ValueError(f"BOM for {item} has quantity zero (would divide by zero)")

    batch_cost = Decimal("0")
    unset = []

    for line in bom["items"]:
        child_cost, child_unset = _cogs_at_level(
            line["item_code"],
            line["qty"],
            buying_price_list,
            boms,
            prices,
            level + 1,
        )
        batch_cost += child_cost
        unset.extend(child_unset)

    # Normalise batch to per-unit cost (ury_daily_p_and_l.py:38, :57)
    per_unit = batch_cost / bom["quantity"]
    total = per_unit * qty

    # Deduplicate unset items
    unset_deduped = sorted(set(unset))

    return (total, unset_deduped)


# ============================================================================
# Product Bundle COGS (ury_daily_p_and_l.py:219-258)
# ============================================================================
#
# Read from the upstream Python, not from the Rust. Structure of the source block:
#
#   for item in pb_item_sales:                       # :221
#       pb = <Product Bundle where new_item_code = item>   # :222-223
#       buying_price = 0                             # :224
#       for pb_item in pb.items:                     # :225
#           boms = <active/default/submitted BOM where item = pb_item.item_code>  # :227
#           if len(boms) > 0:                        # :228
#               bom_data = inner_bom_process(...)    # :231   <-- level 1, same as :201
#               buying_price += bom_buying_price * item_qty  # :234
#               <merge into unset_bom_item_prices>   # :235-237
#           else:
#               <Item Price lookup>                  # :241
#               if missing: unset_pb_item_prices     # :242-244
#               else: buying_price += rate * item_qty  # :246
#       if buying_price > 0:                         # :248
#           cogs += buying_price * qty               # :258
#
# Three things the transliteration risk would hide, all read straight off the source:
#
# 1. PRECEDENCE. pb_item_sales (:147) filters on `d.new_item_code IS NOT NULL`
#    (:170) and joins no BOM table at all. non_pb_item_sales (:102) and
#    bom_item_sales (:139) both require `d.new_item_code IS NULL`. So the three
#    buckets are a partition and Product Bundle wins outright: an item that is both
#    a bundle and has an active default BOM is priced as a bundle, its BOM ignored.
#
# 2. DEPTH. :231 calls inner_bom_process, the same entry point as :201. A bundle is
#    not an extra level, so a bundle line's BOM still gets the full two levels.
#
# 3. THE GUARD. :248 skips the cost row when buying_price <= 0, so a fully unpriced
#    bundle contributes exactly zero while its missing items stay in the unset lists.
#
# Unset lists stay separate (:262-264) because the label is what makes them
# actionable. Item codes are used in place of upstream's item_name for the same
# reason as the Rust side.


def _bom_per_unit(
    bom_item_code: str,
    buying_price_list: str,
    boms: Dict[str, Dict],
    prices: Dict[str, Decimal],
) -> Tuple[Decimal, List[str]]:
    """
    Per-unit cost of the BOM belonging to `bom_item_code`, entered at level 1.

    Thin wrapper over the level walk so the bundle path and the top-level BOM path
    demonstrably share one entry point, as :231 and :201 do.
    """
    cost, unset = _cogs_at_level(bom_item_code, Decimal("1"), buying_price_list, boms, prices, 1)
    return (cost, unset)


def cogs_for_bundle(
    item: str,
    qty: Decimal,
    buying_price_list: str,
    bundles: Dict[str, List[Dict]],
    boms: Dict[str, Dict],
    prices: Dict[str, Decimal],
) -> Tuple[Decimal, List[str], List[str]]:
    """
    COGS for a Product Bundle sold under `item` (new_item_code).

    Returns (cost, unset_bundle_items, unset_bom_items).
    """
    buying_price = Decimal("0")
    unset_bundle: List[str] = []
    unset_bom: List[str] = []

    for line in bundles[item]:
        line_code = line["item_code"]
        line_qty = line["qty"]

        if line_code in boms:
            per_unit, inner_unset = _bom_per_unit(line_code, buying_price_list, boms, prices)
            buying_price += per_unit * line_qty
            for unset_item in inner_unset:
                if unset_item not in unset_bom:
                    unset_bom.append(unset_item)
        elif line_code in prices:
            # Priced as a leaf even when line_code is itself a bundle: upstream
            # never re-queries Product Bundle inside this loop.
            buying_price += prices[line_code] * line_qty
        else:
            if line_code not in unset_bundle:
                unset_bundle.append(line_code)

    # :248 — non-positive cost appends no row, so the item contributes nothing.
    cost = buying_price * qty if buying_price > 0 else Decimal("0")

    return (cost, sorted(set(unset_bundle)), sorted(set(unset_bom)))


def cogs_dispatch(
    item: str,
    qty: Decimal,
    buying_price_list: str,
    bundles: Dict[str, List[Dict]],
    boms: Dict[str, Dict],
    prices: Dict[str, Decimal],
) -> Dict[str, object]:
    """
    Resolve `item` into exactly one of upstream's three buckets and price it.

    Precedence: bundle -> BOM -> plain. See the note above for the SQL evidence.
    """
    if item in bundles:
        cost, unset_bundle, unset_bom = cogs_for_bundle(
            item, qty, buying_price_list, bundles, boms, prices
        )
        return {
            "cost": cost,
            "unset_item_prices": [],
            "unset_bundle_items": unset_bundle,
            "unset_bom_items": unset_bom,
        }

    if item in boms:
        per_unit, unset_bom = _bom_per_unit(item, buying_price_list, boms, prices)
        # :208 guard, same shape as :248.
        cost = per_unit * qty if per_unit > 0 else Decimal("0")
        return {
            "cost": cost,
            "unset_item_prices": [],
            "unset_bundle_items": [],
            "unset_bom_items": unset_bom,
        }

    # Plain bucket (:178-193): no guard, and a miss is labelled ITEMS.
    if item in prices:
        return {
            "cost": prices[item] * qty,
            "unset_item_prices": [],
            "unset_bundle_items": [],
            "unset_bom_items": [],
        }

    return {
        "cost": Decimal("0"),
        "unset_item_prices": [item],
        "unset_bundle_items": [],
        "unset_bom_items": [],
    }


# ============================================================================
# Fixture runner
# ============================================================================

def run_fixture(fixture: Dict) -> Dict:
    """
    Process a fixture and return computed results.
    
    Fixture schema:
    {
        "name": str,
        "kind": "tax" | "cogs",
        
        # For tax fixtures:
        "lines": [{"quantity": str, "rate": str}, ...],
        "discount": str,
        "tax_rate": str,
        "supply_type": "intrastate" | "interstate",
        "discount_basis": "net_total" | "grand_total",
        
        # For COGS fixtures:
        "item": str,
        "qty": str,
        "buying_price_list": str,
        "boms": {"ITEM": {"quantity": str, "items": [{"item_code": str, "qty": str}]}},
        "bundles": {"NEW-ITEM-CODE": [{"item_code": str, "qty": str}]},   # optional
        "prices": {"ITEM": str},
    }
    """
    kind = fixture["kind"]
    
    if kind == "tax":
        lines = [
            {"quantity": Decimal(ln["quantity"]), "rate": Decimal(ln["rate"])}
            for ln in fixture["lines"]
        ]
        discount = Decimal(fixture["discount"])
        tax_rate = Decimal(fixture["tax_rate"])
        supply_type = fixture["supply_type"]
        discount_basis = fixture["discount_basis"]
        
        result = compute_totals(lines, discount, tax_rate, supply_type, discount_basis)
        
        # Serialise back to strings
        return {
            "name": fixture["name"],
            "kind": "tax",
            "net_total": str(result["net_total"]),
            "discount": str(result["discount"]),
            "taxable_value": str(result["taxable_value"]),
            "cgst": str(result["cgst"]),
            "sgst": str(result["sgst"]),
            "igst": str(result["igst"]),
            "total_tax": str(result["total_tax"]),
            "grand_total": str(result["grand_total"]),
            "rounded_total": str(result["rounded_total"]),
            "round_off": str(result["round_off"]),
        }
    
    elif kind == "cogs":
        item = fixture["item"]
        qty = Decimal(fixture["qty"])
        buying_price_list = fixture["buying_price_list"]

        # Parse BOMs
        boms = {}
        for item_code, bom_def in fixture["boms"].items():
            boms[item_code] = {
                "quantity": Decimal(bom_def["quantity"]),
                "items": [
                    {"item_code": ln["item_code"], "qty": Decimal(ln["qty"])}
                    for ln in bom_def["items"]
                ],
            }

        # Parse Product Bundles. Absent in pre-bundle fixtures, which then exercise
        # only the BOM and plain buckets.
        bundles = {
            new_item_code: [
                {"item_code": ln["item_code"], "qty": Decimal(ln["qty"])} for ln in lines
            ]
            for new_item_code, lines in fixture.get("bundles", {}).items()
        }

        # Parse prices
        prices = {code: Decimal(price_str) for code, price_str in fixture["prices"].items()}

        result = cogs_dispatch(item, qty, buying_price_list, bundles, boms, prices)

        return {
            "name": fixture["name"],
            "kind": "cogs",
            "cost": str(result["cost"]),
            "unset_item_prices": result["unset_item_prices"],
            "unset_bundle_items": result["unset_bundle_items"],
            "unset_bom_items": result["unset_bom_items"],
        }


    else:
        raise ValueError(f"unknown fixture kind: {kind}")


# ============================================================================
# CLI entry point
# ============================================================================

def main():
    if len(sys.argv) > 1:
        with open(sys.argv[1]) as f:
            fixtures = json.load(f)
    else:
        fixtures = json.load(sys.stdin)
    
    if not isinstance(fixtures, list):
        fixtures = [fixtures]
    
    results = [run_fixture(fx) for fx in fixtures]
    
    print(json.dumps(results, indent=2))


# ============================================================================
# Self-tests
# ============================================================================

class TestTax(unittest.TestCase):
    """Verify the Python oracle matches the worked examples in tax.rs."""
    
    def test_worked_example_from_plan(self):
        # 4 × ₹100, 5% GST, 10% discount → taxable 360, tax 18, total 378
        result = compute_totals(
            lines=[{"quantity": Decimal("4"), "rate": Decimal("100")}],
            discount=Decimal("40"),
            tax_rate=Decimal("0.05"),
            supply_type="intrastate",
            discount_basis="net_total",
        )
        
        self.assertEqual(result["net_total"], Decimal("400"))
        self.assertEqual(result["taxable_value"], Decimal("360"))
        self.assertEqual(result["total_tax"], Decimal("18"))
        self.assertEqual(result["cgst"], Decimal("9"))
        self.assertEqual(result["sgst"], Decimal("9"))
        self.assertEqual(result["grand_total"], Decimal("378"))
    
    def test_cgst_sgst_split_odd_paisa(self):
        # Tax 18.01 → CGST 9.01, SGST 9.00 (no lost paisa)
        result = compute_totals(
            lines=[{"quantity": Decimal("1"), "rate": Decimal("360.2")}],
            discount=Decimal("0"),
            tax_rate=Decimal("0.05"),
            supply_type="intrastate",
            discount_basis="net_total",
        )
        
        self.assertEqual(result["total_tax"], Decimal("18.01"))
        self.assertEqual(result["cgst"], Decimal("9.01"))
        self.assertEqual(result["sgst"], Decimal("9.00"))
        self.assertEqual(result["cgst"] + result["sgst"], result["total_tax"])


class TestRoundingStrategyPin(unittest.TestCase):
    """
    Pin the rounding strategy on the Python side.

    Mirrors peacock-core/src/money.rs `rounding_strategy_pin_*`. These assert the
    CURRENT choice and record what the alternative produces, so flipping ROUNDING
    is a deliberate act with a visible diff on both sides.

    Half-up and banker's agree whenever the kept digit is already even, so not
    every midpoint discriminates. The 9.005 / 9.015 pair shows both cases.
    """

    def test_paisa_boundary_positive(self):
        self.assertEqual(to_paisa(Decimal("9.005")), Decimal("9.01"))   # banker's: 9.00
        self.assertEqual(to_paisa(Decimal("9.015")), Decimal("9.02"))   # banker's: 9.02 (agrees)
        self.assertEqual(to_paisa(Decimal("9.025")), Decimal("9.03"))   # banker's: 9.02

    def test_paisa_boundary_negative(self):
        self.assertEqual(to_paisa(Decimal("-9.005")), Decimal("-9.01"))  # banker's: -9.00
        self.assertEqual(to_paisa(Decimal("-9.015")), Decimal("-9.02"))  # banker's: -9.02 (agrees)
        self.assertEqual(to_paisa(Decimal("-9.025")), Decimal("-9.03"))  # banker's: -9.02

    def test_rupee_boundary_both_signs(self):
        self.assertEqual(to_rupee(Decimal("0.5")), Decimal("1"))    # banker's: 0
        self.assertEqual(to_rupee(Decimal("1.5")), Decimal("2"))    # banker's: 2 (agrees)
        self.assertEqual(to_rupee(Decimal("2.5")), Decimal("3"))    # banker's: 2
        self.assertEqual(to_rupee(Decimal("-0.5")), Decimal("-1"))  # banker's: -0
        self.assertEqual(to_rupee(Decimal("-1.5")), Decimal("-2"))  # banker's: -2 (agrees)
        self.assertEqual(to_rupee(Decimal("-2.5")), Decimal("-3"))  # banker's: -2

    def test_the_alternative_really_does_diverge(self):
        # Guards against a future edit that sets ROUNDING and the alternative to
        # the same mode, which would leave the pin tests passing while proving
        # nothing about the open question.
        divergent = Decimal("9.005")
        self.assertNotEqual(
            divergent.quantize(Decimal("0.01"), rounding=ROUNDING),
            divergent.quantize(Decimal("0.01"), rounding=_ROUNDING_ALTERNATIVE),
        )

    def test_rupee_midpoint_round_off_fixture(self):
        # Mirrors fixture 14_tax_rupee_midpoint_round_off: grand total lands on .50.
        # Banker's would give rounded_total 420 and round_off -0.50.
        result = compute_totals(
            lines=[{"quantity": Decimal("1"), "rate": Decimal("400.48")}],
            discount=Decimal("0"),
            tax_rate=Decimal("0.05"),
            supply_type="intrastate",
            discount_basis="net_total",
        )

        self.assertEqual(result["total_tax"], Decimal("20.02"))
        self.assertEqual(result["grand_total"], Decimal("420.50"))
        self.assertEqual(result["rounded_total"], Decimal("421"))
        self.assertEqual(result["round_off"], Decimal("0.50"))

    def test_cgst_half_of_odd_paisa_is_a_midpoint(self):
        # total_tax 18.01 halves to 9.005. Half-up: CGST 9.01 / SGST 9.00.
        # Banker's: CGST 9.00 / SGST 9.01 — the components swap, the sum does not.
        total_tax = Decimal("18.01")
        cgst = to_paisa(total_tax / Decimal("2"))
        self.assertEqual(cgst, Decimal("9.01"))
        self.assertEqual(total_tax - cgst, Decimal("9.00"))


class TestCOGS(unittest.TestCase):
    """Verify the Python oracle matches the COGS test cases in cogs.rs."""
    
    def test_one_level_bom_with_quantity_not_one(self):
        # THE test catching v1's bug: bom.quantity != 1
        # Masala Chai: batch produces 10 cups
        # Tea: 10g @ ₹2/g = ₹20
        # Milk: 100ml @ ₹0.50/ml = ₹50
        # Total batch = ₹70
        # Per-unit = ₹70 / 10 = ₹7/cup
        # Order qty=5 → ₹35
        
        boms = {
            "MASALA-CHAI": {
                "quantity": Decimal("10"),
                "items": [
                    {"item_code": "TEA-LEAVES", "qty": Decimal("10")},
                    {"item_code": "MILK", "qty": Decimal("100")},
                ],
            }
        }
        prices = {
            "TEA-LEAVES": Decimal("2.00"),
            "MILK": Decimal("0.50"),
        }
        
        cost, unset = cogs_for_item("MASALA-CHAI", Decimal("5"), "Buying", boms, prices)
        
        self.assertEqual(cost, Decimal("35.00"))
        self.assertEqual(unset, [])
    
    def test_missing_item_price_lands_in_unset(self):
        boms = {
            "SANDWICH": {
                "quantity": Decimal("1"),
                "items": [
                    {"item_code": "BREAD", "qty": Decimal("2")},
                    {"item_code": "CHEESE", "qty": Decimal("1")},
                ],
            }
        }
        prices = {
            "BREAD": Decimal("5"),
            # CHEESE missing
        }
        
        cost, unset = cogs_for_item("SANDWICH", Decimal("2"), "Buying", boms, prices)
        
        # 2×₹5 = ₹10, ×2 = ₹20 (CHEESE contributes zero)
        self.assertEqual(cost, Decimal("20"))
        self.assertEqual(unset, ["CHEESE"])


class TestProductBundle(unittest.TestCase):
    """Product Bundle bucket, ury_daily_p_and_l.py:219-258."""

    def test_bundle_of_plain_items(self):
        # THALI = 2× ROTI @ ₹5 + 1× DAL @ ₹30 → ₹40/unit, qty 3 → ₹120
        bundles = {"THALI": [
            {"item_code": "ROTI", "qty": Decimal("2")},
            {"item_code": "DAL", "qty": Decimal("1")},
        ]}
        prices = {"ROTI": Decimal("5"), "DAL": Decimal("30")}

        r = cogs_dispatch("THALI", Decimal("3"), "Buying", bundles, {}, prices)

        self.assertEqual(r["cost"], Decimal("120"))
        self.assertEqual(r["unset_bundle_items"], [])
        self.assertEqual(r["unset_bom_items"], [])

    def test_bundle_line_with_batch_bom(self):
        # COMBO = 2× MASALA-CHAI + 1× SAMOSA @ ₹12
        # MASALA-CHAI BOM batch of 10 costing ₹70 → ₹7/cup
        # per-unit = 2×₹7 + ₹12 = ₹26; qty 4 → ₹104
        bundles = {"COMBO": [
            {"item_code": "MASALA-CHAI", "qty": Decimal("2")},
            {"item_code": "SAMOSA", "qty": Decimal("1")},
        ]}
        boms = {"MASALA-CHAI": {
            "quantity": Decimal("10"),
            "items": [
                {"item_code": "TEA-LEAVES", "qty": Decimal("10")},
                {"item_code": "MILK", "qty": Decimal("100")},
            ],
        }}
        prices = {
            "TEA-LEAVES": Decimal("2.00"),
            "MILK": Decimal("0.50"),
            "SAMOSA": Decimal("12"),
        }

        r = cogs_dispatch("COMBO", Decimal("4"), "Buying", bundles, boms, prices)

        self.assertEqual(r["cost"], Decimal("104"))
        self.assertEqual(r["unset_bom_items"], [])

    def test_bundle_adds_no_bom_depth(self):
        # PLATTER = 1× COMBO-MEAL, whose BOM is two levels deep.
        # BURGER (level 2, batch 5) = 5×₹10 + 5×₹5 = ₹75 → ₹15
        # COMBO-MEAL = 2×₹15 + ₹20 = ₹50 → bundle ₹50
        # If the bundle consumed a level, BURGER would price as an unpriced leaf.
        bundles = {"PLATTER": [{"item_code": "COMBO-MEAL", "qty": Decimal("1")}]}
        boms = {
            "COMBO-MEAL": {"quantity": Decimal("1"), "items": [
                {"item_code": "BURGER", "qty": Decimal("2")},
                {"item_code": "FRIES", "qty": Decimal("1")},
            ]},
            "BURGER": {"quantity": Decimal("5"), "items": [
                {"item_code": "PATTY", "qty": Decimal("5")},
                {"item_code": "BUN", "qty": Decimal("5")},
            ]},
        }
        prices = {"PATTY": Decimal("10"), "BUN": Decimal("5"), "FRIES": Decimal("20")}

        r = cogs_dispatch("PLATTER", Decimal("1"), "Buying", bundles, boms, prices)

        self.assertEqual(r["cost"], Decimal("50"))
        self.assertEqual(r["unset_bom_items"], [])
        # Same BOM reached from the top level costs the same.
        direct = cogs_dispatch("COMBO-MEAL", Decimal("1"), "Buying", {}, boms, prices)
        self.assertEqual(direct["cost"], r["cost"])

    def test_bundle_line_missing_price_is_a_bundle_sub_item(self):
        bundles = {"MEAL": [
            {"item_code": "RICE", "qty": Decimal("1")},
            {"item_code": "PICKLE", "qty": Decimal("1")},
        ]}
        prices = {"RICE": Decimal("20")}

        r = cogs_dispatch("MEAL", Decimal("2"), "Buying", bundles, {}, prices)

        self.assertEqual(r["cost"], Decimal("40"))
        self.assertEqual(r["unset_bundle_items"], ["PICKLE"])
        self.assertEqual(r["unset_bom_items"], [])

    def test_bundle_bom_ingredient_miss_is_a_bom_sub_item(self):
        # :236-237 merges into unset_bom_item_prices, not unset_pb_item_prices.
        bundles = {"BOX": [{"item_code": "SANDWICH", "qty": Decimal("1")}]}
        boms = {"SANDWICH": {"quantity": Decimal("1"), "items": [
            {"item_code": "BREAD", "qty": Decimal("2")},
            {"item_code": "CHEESE", "qty": Decimal("1")},
        ]}}
        prices = {"BREAD": Decimal("5")}

        r = cogs_dispatch("BOX", Decimal("1"), "Buying", bundles, boms, prices)

        self.assertEqual(r["cost"], Decimal("10"))
        self.assertEqual(r["unset_bom_items"], ["CHEESE"])
        self.assertEqual(r["unset_bundle_items"], [])

    def test_fully_unpriced_bundle_contributes_zero_but_stays_visible(self):
        # :248 guard. Zero cost, gap still reported.
        bundles = {"MYSTERY": [
            {"item_code": "UNKNOWN-A", "qty": Decimal("1")},
            {"item_code": "UNKNOWN-B", "qty": Decimal("2")},
        ]}

        r = cogs_dispatch("MYSTERY", Decimal("7"), "Buying", bundles, {}, {})

        self.assertEqual(r["cost"], Decimal("0"))
        self.assertEqual(r["unset_bundle_items"], ["UNKNOWN-A", "UNKNOWN-B"])

    def test_nested_bundle_inner_priced_as_leaf(self):
        # Upstream never re-queries Product Bundle inside the line loop (:225-246),
        # so INNER is priced from its own Item Price and its children are ignored.
        bundles = {
            "OUTER": [
                {"item_code": "INNER", "qty": Decimal("1")},
                {"item_code": "DRINK", "qty": Decimal("1")},
            ],
            "INNER": [{"item_code": "X", "qty": Decimal("2")}],
        }
        prices = {"INNER": Decimal("40"), "DRINK": Decimal("15"), "X": Decimal("100")}

        r = cogs_dispatch("OUTER", Decimal("1"), "Buying", bundles, {}, prices)

        # ₹40 + ₹15, not 2×₹100 + ₹15.
        self.assertEqual(r["cost"], Decimal("55"))

    def test_bundle_wins_over_bom(self):
        # pb_item_sales (:147/:170) joins no BOM; the other two require
        # d.new_item_code IS NULL. Bundle takes the item outright.
        bundles = {"DUAL": [{"item_code": "CHEAP", "qty": Decimal("1")}]}
        boms = {"DUAL": {"quantity": Decimal("1"), "items": [
            {"item_code": "DEAR", "qty": Decimal("1")},
        ]}}
        prices = {"CHEAP": Decimal("1"), "DEAR": Decimal("999")}

        r = cogs_dispatch("DUAL", Decimal("1"), "Buying", bundles, boms, prices)

        self.assertEqual(r["cost"], Decimal("1"))

    def test_plain_miss_is_labelled_items_not_bom(self):
        r = cogs_dispatch("NO-PRICE", Decimal("3"), "Buying", {}, {}, {})

        self.assertEqual(r["cost"], Decimal("0"))
        self.assertEqual(r["unset_item_prices"], ["NO-PRICE"])
        self.assertEqual(r["unset_bom_items"], [])
        self.assertEqual(r["unset_bundle_items"], [])


if __name__ == "__main__":
    if "--test" in sys.argv:
        # Run self-tests
        unittest.main(argv=[sys.argv[0]])
    else:
        main()
