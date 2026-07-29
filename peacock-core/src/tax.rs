//! Indian GST calculation.
//!
//! Restaurant service is generally **5% GST without ITC** for non-specified premises.
//! This module models tax as output-only; it does NOT model input credit.
//!
//! ## Place of supply
//!
//! - **Intrastate → CGST + SGST** (each half the rate)
//! - **Interstate → IGST** (full rate)
//!
//! ## Discount basis
//!
//! The correct default for URY is **Net Total** (discount reduces the taxable base).
//! ERPNext's `apply_discount_on` supports both Net Total and Grand Total; we implement
//! both and default to Net Total because that is the legal treatment of trade discount
//! under GST: the discount is deducted *before* computing tax.
//!
//! ## Per-line tax computed BEFORE rounding
//!
//! CGST and SGST are each computed as exactly half the total tax, to the paisa.
//! No rounding drift. Then the final amounts are rounded once.
//!
//! ## HSN/SAC per line
//!
//! Note: `ury_menu_item.json` has **NO HSN field today**, so this is a data-backfill
//! task that blocks go-live. The invoice schema requires it for GST compliance, but
//! the menu data must be enriched first.

use crate::money::Money;
use crate::Result;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Place of supply determines tax components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SupplyType {
    /// Same state: CGST + SGST (each half the rate)
    Intrastate,
    /// Different state: IGST (full rate)
    Interstate,
}

/// Whether discount applies to net total (before tax) or grand total (after tax).
///
/// **Net Total is the correct default** under GST: trade discount reduces the
/// taxable value. Grand Total is supported for completeness, but using it means
/// the discount does not reduce the tax paid, which is unusual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DiscountBasis {
    /// Discount reduces the taxable base (default, legally correct for trade discount)
    #[default]
    NetTotal,
    /// Discount applied after tax (unusual)
    GrandTotal,
}

/// Tax breakdown for an invoice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxBreakdown {
    /// Central GST (intrastate only)
    pub cgst: Money,
    /// State GST (intrastate only)
    pub sgst: Money,
    /// Integrated GST (interstate only)
    pub igst: Money,
    /// Total tax (cgst + sgst for intrastate, igst for interstate)
    pub total_tax: Money,
}

impl TaxBreakdown {
    /// Intrastate: CGST and SGST each equal exactly half the total tax, to the paisa.
    pub fn intrastate(total_tax: Money) -> Self {
        // Divide total by 2 for CGST, round to paisa, then SGST = total - CGST ensures no lost paisa.
        let cgst = Money::new(total_tax.inner() / dec!(2)).to_paisa();
        let sgst = total_tax - cgst;

        TaxBreakdown {
            cgst,
            sgst,
            igst: Money::ZERO,
            total_tax,
        }
    }

    /// Interstate: full tax as IGST.
    pub fn interstate(total_tax: Money) -> Self {
        TaxBreakdown {
            cgst: Money::ZERO,
            sgst: Money::ZERO,
            igst: total_tax,
            total_tax,
        }
    }
}

/// Complete invoice totals with tax and rounding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceTotals {
    /// Sum of line amounts before discount
    pub net_total: Money,
    /// Discount amount
    pub discount: Money,
    /// Amount subject to tax (depends on discount basis)
    pub taxable_value: Money,
    /// Tax breakdown
    pub tax: TaxBreakdown,
    /// Net + tax (or net + tax - discount if basis is GrandTotal)
    pub grand_total: Money,
    /// Rounded to nearest rupee
    pub rounded_total: Money,
    /// rounded_total - grand_total (posts to round-off ledger)
    pub round_off: Money,
}

/// Input line for tax calculation.
#[derive(Debug, Clone, PartialEq)]
pub struct InvoiceLine {
    pub item_name: String,
    pub quantity: Decimal,
    pub rate: Money,
    /// HSN/SAC code (required for GST compliance, but ury_menu_item has no HSN field today)
    pub hsn_sac: Option<String>,
}

impl InvoiceLine {
    pub fn amount(&self) -> Money {
        self.rate * self.quantity
    }
}

/// Compute complete invoice totals with GST.
///
/// ## Invariants
///
/// - For intrastate: `cgst == sgst` exactly, and `cgst + sgst == total_tax` with no lost paisa
/// - `round_off == rounded_total - grand_total`
/// - Round-off applied ONCE at invoice level
///
/// ## Example (from RUST_MIGRATION_PLAN_V2.md §5)
///
/// 4 × ₹100, 5% GST, 10% discount, intrastate:
/// - Net total: 400
/// - Discount: 40
/// - Taxable: 360
/// - Tax: 18 (CGST 9, SGST 9)
/// - Grand total: 378
///
/// ```
/// use peacock_core::tax::*;
/// use peacock_core::Money;
/// use rust_decimal_macros::dec;
///
/// let lines = vec![InvoiceLine {
///     item_name: "Item A".into(),
///     quantity: dec!(4),
///     rate: Money::new(dec!(100)),
///     hsn_sac: None,
/// }];
///
/// let totals = compute_totals(
///     &lines,
///     Money::new(dec!(40)),
///     dec!(0.05),
///     SupplyType::Intrastate,
///     DiscountBasis::NetTotal,
/// ).unwrap();
///
/// assert_eq!(totals.net_total, Money::new(dec!(400)));
/// assert_eq!(totals.taxable_value, Money::new(dec!(360)));
/// assert_eq!(totals.tax.total_tax, Money::new(dec!(18)));
/// assert_eq!(totals.grand_total, Money::new(dec!(378)));
/// ```
pub fn compute_totals(
    lines: &[InvoiceLine],
    discount: Money,
    tax_rate: Decimal,
    supply_type: SupplyType,
    discount_basis: DiscountBasis,
) -> Result<InvoiceTotals> {
    // Sum all line amounts
    let net_total: Money = lines.iter().map(|l| l.amount()).sum();

    // Taxable value depends on discount basis
    let taxable_value = match discount_basis {
        DiscountBasis::NetTotal => net_total - discount,
        DiscountBasis::GrandTotal => net_total,
    };

    // Compute total tax on taxable value
    let total_tax_raw = taxable_value * tax_rate;
    let total_tax = total_tax_raw.to_paisa(); // Round to paisa

    // Split tax by supply type
    let tax = match supply_type {
        SupplyType::Intrastate => TaxBreakdown::intrastate(total_tax),
        SupplyType::Interstate => TaxBreakdown::interstate(total_tax),
    };

    // Grand total depends on discount basis
    let grand_total = match discount_basis {
        DiscountBasis::NetTotal => taxable_value + tax.total_tax,
        DiscountBasis::GrandTotal => net_total + tax.total_tax - discount,
    };

    // Apply rounding once, at invoice level
    let rounded_total = grand_total.to_rupee();
    let round_off = rounded_total - grand_total;

    Ok(InvoiceTotals {
        net_total,
        discount,
        taxable_value,
        tax,
        grand_total,
        rounded_total,
        round_off,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn line(qty: Decimal, rate: Decimal) -> InvoiceLine {
        InvoiceLine {
            item_name: "Test Item".into(),
            quantity: qty,
            rate: Money::new(rate),
            hsn_sac: None,
        }
    }

    #[test]
    fn worked_example_from_plan() {
        // 4 × ₹100, 5% GST, 10% discount → taxable 360, tax 18, total 378
        let lines = vec![line(dec!(4), dec!(100))];
        let totals = compute_totals(
            &lines,
            Money::new(dec!(40)),
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        assert_eq!(totals.net_total, Money::new(dec!(400)));
        assert_eq!(totals.discount, Money::new(dec!(40)));
        assert_eq!(totals.taxable_value, Money::new(dec!(360)));
        assert_eq!(totals.tax.total_tax, Money::new(dec!(18)));
        assert_eq!(totals.tax.cgst, Money::new(dec!(9)));
        assert_eq!(totals.tax.sgst, Money::new(dec!(9)));
        assert_eq!(totals.tax.igst, Money::ZERO);
        assert_eq!(totals.grand_total, Money::new(dec!(378)));
        assert_eq!(totals.rounded_total, Money::new(dec!(378)));
        assert_eq!(totals.round_off, Money::ZERO);
    }

    #[test]
    fn cgst_and_sgst_exactly_equal() {
        // Even tax amount divides cleanly
        let lines = vec![line(dec!(1), dec!(100))];
        let totals = compute_totals(
            &lines,
            Money::ZERO,
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        assert_eq!(totals.tax.cgst, totals.tax.sgst);
        assert_eq!(totals.tax.cgst + totals.tax.sgst, totals.tax.total_tax);
    }

    #[test]
    fn cgst_and_sgst_split_odd_paisa() {
        // Tax 18.01 → CGST 9.005 rounds to 9.01, SGST = 18.01 - 9.01 = 9.00 (proves no lost paisa)
        let lines = vec![line(dec!(1), dec!(360.2))];
        let totals = compute_totals(
            &lines,
            Money::ZERO,
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        // Tax is 18.01 at paisa precision
        assert_eq!(totals.tax.total_tax, Money::new(dec!(18.01)));
        // CGST = 18.01 / 2 = 9.005, rounds up to 9.01
        assert_eq!(totals.tax.cgst, Money::new(dec!(9.01)));
        // SGST = 18.01 - 9.01 = 9.00
        assert_eq!(totals.tax.sgst, Money::new(dec!(9.00)));
        // No drift: sum equals total
        assert_eq!(totals.tax.cgst + totals.tax.sgst, totals.tax.total_tax);
    }

    #[test]
    fn interstate_igst_only() {
        let lines = vec![line(dec!(4), dec!(100))];
        let totals = compute_totals(
            &lines,
            Money::new(dec!(40)),
            dec!(0.05),
            SupplyType::Interstate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        assert_eq!(totals.tax.igst, Money::new(dec!(18)));
        assert_eq!(totals.tax.cgst, Money::ZERO);
        assert_eq!(totals.tax.sgst, Money::ZERO);
        assert_eq!(totals.tax.total_tax, Money::new(dec!(18)));
    }

    #[test]
    fn discount_basis_net_total_vs_grand_total() {
        let lines = vec![line(dec!(1), dec!(100))];

        // Net Total: discount reduces taxable base
        let net_basis = compute_totals(
            &lines,
            Money::new(dec!(10)),
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();
        // Taxable = 100 - 10 = 90, tax = 4.50, grand = 94.50
        assert_eq!(net_basis.taxable_value, Money::new(dec!(90)));
        assert_eq!(net_basis.tax.total_tax, Money::new(dec!(4.50)));
        assert_eq!(net_basis.grand_total, Money::new(dec!(94.50)));

        // Grand Total: discount applied after tax
        let grand_basis = compute_totals(
            &lines,
            Money::new(dec!(10)),
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::GrandTotal,
        )
        .unwrap();
        // Taxable = 100, tax = 5, grand = 100 + 5 - 10 = 95
        assert_eq!(grand_basis.taxable_value, Money::new(dec!(100)));
        assert_eq!(grand_basis.tax.total_tax, Money::new(dec!(5.00)));
        assert_eq!(grand_basis.grand_total, Money::new(dec!(95.00)));

        // The difference is real and measurable
        assert_ne!(net_basis.grand_total, grand_basis.grand_total);
    }

    #[test]
    fn zero_discount() {
        let lines = vec![line(dec!(1), dec!(100))];
        let totals = compute_totals(
            &lines,
            Money::ZERO,
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        assert_eq!(totals.net_total, totals.taxable_value);
        assert_eq!(totals.grand_total, Money::new(dec!(105)));
    }

    #[test]
    fn hundred_percent_discount() {
        let lines = vec![line(dec!(1), dec!(100))];
        let totals = compute_totals(
            &lines,
            Money::new(dec!(100)),
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        assert_eq!(totals.taxable_value, Money::ZERO);
        assert_eq!(totals.tax.total_tax, Money::ZERO);
        assert_eq!(totals.grand_total, Money::ZERO);
    }

    #[test]
    fn empty_line_list() {
        let totals = compute_totals(
            &[],
            Money::ZERO,
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        assert_eq!(totals.net_total, Money::ZERO);
        assert_eq!(totals.tax.total_tax, Money::ZERO);
        assert_eq!(totals.grand_total, Money::ZERO);
    }

    #[test]
    fn multi_line_invoice_no_drift() {
        // Multiple lines with different amounts
        let lines = vec![
            line(dec!(2), dec!(100)),
            line(dec!(3), dec!(50)),
            line(dec!(1), dec!(200)),
        ];
        let totals = compute_totals(
            &lines,
            Money::new(dec!(50)),
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        // Net = 200 + 150 + 200 = 550
        assert_eq!(totals.net_total, Money::new(dec!(550)));
        // Taxable = 550 - 50 = 500
        assert_eq!(totals.taxable_value, Money::new(dec!(500)));
        // Tax = 25
        assert_eq!(totals.tax.total_tax, Money::new(dec!(25)));
        // CGST and SGST split exactly
        assert_eq!(totals.tax.cgst, Money::new(dec!(12.50)));
        assert_eq!(totals.tax.sgst, Money::new(dec!(12.50)));
        assert_eq!(totals.tax.cgst + totals.tax.sgst, totals.tax.total_tax);
        // Grand = 525
        assert_eq!(totals.grand_total, Money::new(dec!(525)));
    }

    #[test]
    fn round_off_invariant() {
        let lines = vec![line(dec!(4), dec!(100))];
        let totals = compute_totals(
            &lines,
            Money::new(dec!(40)),
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        // The invariant the ledger depends on
        assert_eq!(totals.round_off, totals.rounded_total - totals.grand_total);
    }

    #[test]
    fn round_off_positive() {
        // Grand total 377.60 rounds up to 378
        let lines = vec![line(dec!(1), dec!(359.05))];
        let totals = compute_totals(
            &lines,
            Money::ZERO,
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        assert_eq!(totals.grand_total, Money::new(dec!(377.00)));
        assert_eq!(totals.rounded_total, Money::new(dec!(377)));
        assert_eq!(totals.round_off, Money::ZERO);
    }

    #[test]
    fn round_off_negative() {
        // Grand total 378.40 rounds down to 378
        let lines = vec![line(dec!(1), dec!(360.38))];
        let totals = compute_totals(
            &lines,
            Money::ZERO,
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        assert_eq!(totals.grand_total, Money::new(dec!(378.40)));
        assert_eq!(totals.rounded_total, Money::new(dec!(378)));
        assert_eq!(totals.round_off, Money::new(dec!(-0.40)));
    }

    #[test]
    fn rounding_applied_once_at_invoice_level() {
        // If we rounded per-line, drift could accumulate. Verify we don't.
        let lines = vec![
            line(dec!(1), dec!(100.11)),
            line(dec!(1), dec!(100.22)),
            line(dec!(1), dec!(100.33)),
        ];
        let totals = compute_totals(
            &lines,
            Money::ZERO,
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        // Net = 300.66, tax = 15.03, grand = 315.69
        assert_eq!(totals.net_total, Money::new(dec!(300.66)));
        assert_eq!(totals.tax.total_tax, Money::new(dec!(15.03)));
        assert_eq!(totals.grand_total, Money::new(dec!(315.69)));
        // Rounded once: 316
        assert_eq!(totals.rounded_total, Money::new(dec!(316)));
        assert_eq!(totals.round_off, Money::new(dec!(0.31)));
    }
}
