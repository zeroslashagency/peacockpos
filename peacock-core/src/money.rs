//! Money. `Decimal` only — never `f64`.
//!
//! PLAN.md proposed serialising money through JS `Number(...)`, which is IEEE-754
//! and silently corrupts paisa. Money crosses the wire as a **string**.

use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// The one rounding strategy used by every money rounding in this crate.
///
/// # OPEN QUESTION — not confirmed against the target site
///
/// Frappe rounds money through `flt(value, precision)`, which honours the
/// site-wide `rounding_method` setting in `System Settings`. In Frappe v15 the
/// **default is Banker's Rounding** (half-to-even); the alternative is
/// "Commercial Rounding" (half-away-from-zero). Frappe is not vendored in this
/// repo and there is no live site reachable from here, so **which of the two the
/// target deployment actually uses cannot be determined here**. It must be read
/// off `System Settings.rounding_method` on the real site before cutover.
///
/// This crate currently pins half-away-from-zero. Every rounding in the crate
/// goes through this constant, so flipping to
/// `RoundingStrategy::MidpointNearestEven` is a one-line change here.
///
/// ## Exactly which invoice figures move if this flips
///
/// Only values landing on an exact midpoint change; everything else is
/// strategy-independent.
///
/// - `InvoiceTotals::tax.total_tax` — `taxable_value * rate` at the paisa
///   boundary (`.005`).
/// - `InvoiceTotals::tax.cgst` — `total_tax / 2` at the paisa boundary. Any odd
///   number of paisa halves onto `.005`, so this is the most frequently hit case.
/// - `InvoiceTotals::tax.sgst` — derived as `total_tax - cgst`, so it absorbs
///   the CGST shift in the opposite direction.
/// - `InvoiceTotals::rounded_total` and `round_off` — `grand_total` at the rupee
///   boundary (`.50`).
///
/// `net_total`, `discount` and `taxable_value` are never rounded and cannot move.
/// COGS is not rounded either (`cogs.rs` keeps full `Decimal` precision), so
/// `CogsResult::cost` is unaffected.
///
/// The Python parity oracle pins the same choice in a single constant
/// (`scripts/parity_reference.py`, `ROUNDING`). Both sides must be flipped
/// together or the harness will fail — which is the point.
pub const ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

/// A monetary amount in INR, serialised as a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Money(#[serde(with = "rust_decimal::serde::str")] pub Decimal);

impl Money {
    pub const ZERO: Money = Money(Decimal::ZERO);

    pub fn new(d: Decimal) -> Self {
        Money(d)
    }

    pub fn inner(&self) -> Decimal {
        self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// Round to 2 decimal places (paisa) using [`ROUNDING`].
    pub fn to_paisa(self) -> Money {
        Money(self.0.round_dp_with_strategy(2, ROUNDING))
    }

    /// Round to the nearest whole rupee using [`ROUNDING`]. Used once, at invoice level.
    pub fn to_rupee(self) -> Money {
        Money(self.0.round_dp_with_strategy(0, ROUNDING))
    }
}

impl std::ops::Add for Money {
    type Output = Money;
    fn add(self, rhs: Money) -> Money {
        Money(self.0 + rhs.0)
    }
}

impl std::ops::Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Money) -> Money {
        Money(self.0 - rhs.0)
    }
}

impl std::ops::Mul<Decimal> for Money {
    type Output = Money;
    fn mul(self, rhs: Decimal) -> Money {
        Money(self.0 * rhs)
    }
}

impl std::iter::Sum for Money {
    fn sum<I: Iterator<Item = Money>>(iter: I) -> Money {
        iter.fold(Money::ZERO, |a, b| a + b)
    }
}

impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The round-off residual on an invoice.
///
/// `round_off = rounded_total - grand_total`. This delta posts to a round-off
/// ledger account and appears on the P&L — it is not a display adjustment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundOff {
    pub grand_total: Money,
    pub rounded_total: Money,
    pub round_off: Money,
}

impl RoundOff {
    pub fn apply(grand_total: Money) -> Self {
        let rounded_total = grand_total.to_rupee();
        RoundOff {
            grand_total,
            rounded_total,
            round_off: rounded_total - grand_total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn money_serialises_as_string_not_number() {
        // Guards the PLAN.md bug: JS Number() would corrupt this.
        let m = Money(dec!(1234.56));
        assert_eq!(serde_json::to_string(&m).unwrap(), "\"1234.56\"");
    }

    #[test]
    fn round_off_residual_is_exact() {
        let r = RoundOff::apply(Money(dec!(377.60)));
        assert_eq!(r.rounded_total, Money(dec!(378)));
        assert_eq!(r.round_off, Money(dec!(0.40)));
        // The invariant the ledger depends on.
        assert_eq!(r.rounded_total - r.grand_total, r.round_off);
    }

    #[test]
    fn round_off_can_be_negative() {
        let r = RoundOff::apply(Money(dec!(378.40)));
        assert_eq!(r.rounded_total, Money(dec!(378)));
        assert_eq!(r.round_off, Money(dec!(-0.40)));
    }

    // ------------------------------------------------------------------
    // ROUNDING STRATEGY PIN
    //
    // These tests exist to make a change of `ROUNDING` loud instead of silent.
    // They assert the CURRENT choice (half-away-from-zero) and each one names the
    // value Banker's Rounding would produce instead. If `ROUNDING` is ever flipped
    // to `MidpointNearestEven` after reading the real site's
    // `System Settings.rounding_method`, these are the tests that must be updated
    // — and updating them is the record that the decision was made deliberately.
    //
    // Half-up and banker's do NOT disagree on every midpoint: they agree whenever
    // the digit being kept is already even. 9.005 and 9.015 are the pair that shows
    // this — both are exact midpoints, but only the first diverges. Verified against
    // Python's `decimal` module with ROUND_HALF_UP vs ROUND_HALF_EVEN.
    // ------------------------------------------------------------------

    #[test]
    fn rounding_strategy_pin_paisa_boundary_positive() {
        // 9.005: kept digit 0 is even → banker's rounds DOWN to 9.00.
        assert_eq!(Money(dec!(9.005)).to_paisa(), Money(dec!(9.01)));
        // 9.015: kept digit 1 is odd → banker's rounds UP to 9.02, same as half-up.
        // This midpoint does NOT discriminate between the strategies; it is here to
        // stop anyone "fixing" the pin by assuming every midpoint diverges.
        assert_eq!(Money(dec!(9.015)).to_paisa(), Money(dec!(9.02)));
        // 9.025: kept digit 2 is even → banker's rounds DOWN to 9.02. Diverges.
        assert_eq!(Money(dec!(9.025)).to_paisa(), Money(dec!(9.03)));
    }

    #[test]
    fn rounding_strategy_pin_paisa_boundary_negative() {
        // Away-from-zero means the magnitude grows in both signs.
        // -9.005: banker's would give -9.00.
        assert_eq!(Money(dec!(-9.005)).to_paisa(), Money(dec!(-9.01)));
        // -9.015: banker's also gives -9.02 (kept digit odd). No divergence.
        assert_eq!(Money(dec!(-9.015)).to_paisa(), Money(dec!(-9.02)));
        // -9.025: banker's would give -9.02.
        assert_eq!(Money(dec!(-9.025)).to_paisa(), Money(dec!(-9.03)));
    }

    #[test]
    fn rounding_strategy_pin_rupee_boundary_positive() {
        // 0.5: banker's would give 0 (zero is even).
        assert_eq!(Money(dec!(0.5)).to_rupee(), Money(dec!(1)));
        // 1.5: banker's also gives 2. No divergence.
        assert_eq!(Money(dec!(1.5)).to_rupee(), Money(dec!(2)));
        // 2.5: banker's would give 2.
        assert_eq!(Money(dec!(2.5)).to_rupee(), Money(dec!(3)));
    }

    #[test]
    fn rounding_strategy_pin_rupee_boundary_negative() {
        // -0.5: banker's would give 0.
        assert_eq!(Money(dec!(-0.5)).to_rupee(), Money(dec!(-1)));
        // -1.5: banker's also gives -2. No divergence.
        assert_eq!(Money(dec!(-1.5)).to_rupee(), Money(dec!(-2)));
        // -2.5: banker's would give -2.
        assert_eq!(Money(dec!(-2.5)).to_rupee(), Money(dec!(-3)));
    }

    #[test]
    fn rounding_strategy_pin_cgst_half_of_odd_paisa() {
        // The case that actually reaches production: an odd paisa count halved
        // lands on a .005 midpoint. Total tax 18.01 → CGST 9.005.
        // Half-up: 9.01, leaving SGST 9.00. Banker's: 9.00, leaving SGST 9.01.
        // Either way the pair sums to total_tax, but the two components swap.
        let total_tax = Money(dec!(18.01));
        let cgst = Money(total_tax.inner() / dec!(2)).to_paisa();
        assert_eq!(cgst, Money(dec!(9.01)));
        assert_eq!(total_tax - cgst, Money(dec!(9.00)));
    }

    #[test]
    fn rounding_strategy_pin_round_off_at_rupee_midpoint() {
        // A grand total ending .50 is the rupee-boundary case the parity fixture
        // `14_tax_rupee_midpoint_round_off` exercises end to end.
        // Banker's would give rounded_total 428 and round_off -0.50.
        let r = RoundOff::apply(Money(dec!(428.50)));
        assert_eq!(r.rounded_total, Money(dec!(429)));
        assert_eq!(r.round_off, Money(dec!(0.50)));
        assert_eq!(r.rounded_total - r.grand_total, r.round_off);
    }

    #[test]
    fn no_float_drift_over_many_additions() {
        // 0.1 summed 10 times is exactly 1 in Decimal; in f64 it is not.
        let total: Money = std::iter::repeat_n(Money(dec!(0.1)), 10).sum();
        assert_eq!(total, Money(dec!(1.0)));
    }
}
