//! Invoice and payment DTOs.
//!
//! # Money on the wire
//!
//! Every monetary field is a **string**, never a JSON number. `peacock_core::Money`
//! wraps a `Decimal` and serialises transparently as a string (see
//! `peacock-core/src/money.rs`), and these DTOs reuse `Money` directly rather than
//! projecting to `f64`/`Decimal`-as-number. A JSON number would route the value through
//! IEEE-754 in every JS client and silently corrupt paisa — the exact bug
//! `peacock-core::money` was written to prevent.
//!
//! The invariants the parity harness pins (`cgst + sgst == total_tax`,
//! `round_off == rounded_total - grand_total`) are computed once in
//! `peacock_core::tax::compute_totals` and copied verbatim here. Nothing in this module
//! re-derives a money figure.

use chrono::{DateTime, NaiveDate, Utc};
use peacock_core::model::PosInvoiceStatus;
use peacock_core::money::Money;
use peacock_core::tax::{DiscountBasis, InvoiceTotals, SupplyType};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Wire representation of status
// ---------------------------------------------------------------------------

/// `PosInvoiceStatus` on the wire.
///
/// Spelled out as its own type so the JSON spelling ("Draft", "Paid", …) is part of the
/// API contract and cannot drift when the domain enum is refactored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvoiceStatusDto {
    Draft,
    Paid,
    Consolidated,
    Return,
}

impl From<PosInvoiceStatus> for InvoiceStatusDto {
    fn from(s: PosInvoiceStatus) -> Self {
        match s {
            PosInvoiceStatus::Draft => Self::Draft,
            PosInvoiceStatus::Paid => Self::Paid,
            PosInvoiceStatus::Consolidated => Self::Consolidated,
            PosInvoiceStatus::Return => Self::Return,
        }
    }
}

impl From<InvoiceStatusDto> for PosInvoiceStatus {
    fn from(s: InvoiceStatusDto) -> Self {
        match s {
            InvoiceStatusDto::Draft => Self::Draft,
            InvoiceStatusDto::Paid => Self::Paid,
            InvoiceStatusDto::Consolidated => Self::Consolidated,
            InvoiceStatusDto::Return => Self::Return,
        }
    }
}

impl InvoiceStatusDto {
    /// Parses the `status` query-string value. Case-insensitive because query strings
    /// are hand-typed far more often than bodies.
    pub fn parse_filter(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "draft" => Some(Self::Draft),
            "paid" => Some(Self::Paid),
            "consolidated" => Some(Self::Consolidated),
            "return" => Some(Self::Return),
            _ => None,
        }
    }
}

/// Payment instrument. Mirrors the ERPNext `Mode of Payment` values URY ships with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaymentMethodDto {
    Cash,
    Card,
    Upi,
    Wallet,
    Credit,
}

impl PaymentMethodDto {
    /// True for instruments that count toward the CGST Rule 56 cash drawer total.
    pub fn is_cash(self) -> bool {
        matches!(self, Self::Cash)
    }
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// A line as the POS client submits it.
///
/// `rate` is a string on the wire (`Money`), `quantity` is a string-encoded `Decimal`.
/// Quantity is not money but is still `Decimal`: a fractional kg must not become 0.30000000000000004.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceLineRequest {
    pub item_code: String,
    pub item_name: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    pub rate: Money,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hsn_sac: Option<String>,
}

/// `POST /api/invoices` body — create an invoice from an order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateInvoiceRequest {
    /// The `URY Order` form this invoice is billed from.
    pub order_id: String,
    /// Table being billed. Absent for takeaway/aggregator orders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    pub customer_name: String,
    pub lines: Vec<InvoiceLineRequest>,
    /// Invoice-level discount. Defaults to zero.
    #[serde(default)]
    pub discount: Money,
    /// GST rate as a fraction (0.05 for 5%), string-encoded.
    #[serde(with = "rust_decimal::serde::str")]
    pub tax_rate: Decimal,
    #[serde(default = "default_supply_type")]
    pub supply_type: SupplyTypeDto,
    #[serde(default)]
    pub discount_basis: DiscountBasisDto,
    /// Naming-series prefix. Combined with the fiscal-year code to form the invoice
    /// name; the pair must fit CGST Rule 46(b)'s 16-character cap.
    pub series: String,
    /// Posting instant. Defaults to now; supplied explicitly by replay and by tests so
    /// the business day is deterministic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posted_at: Option<DateTime<Utc>>,
}

fn default_supply_type() -> SupplyTypeDto {
    SupplyTypeDto::Intrastate
}

/// Wire form of [`SupplyType`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SupplyTypeDto {
    Intrastate,
    Interstate,
}

impl From<SupplyTypeDto> for SupplyType {
    fn from(s: SupplyTypeDto) -> Self {
        match s {
            SupplyTypeDto::Intrastate => SupplyType::Intrastate,
            SupplyTypeDto::Interstate => SupplyType::Interstate,
        }
    }
}

/// Wire form of [`DiscountBasis`]. Defaults to `NetTotal`, the legally correct
/// treatment of trade discount under GST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DiscountBasisDto {
    #[default]
    NetTotal,
    GrandTotal,
}

impl From<DiscountBasisDto> for DiscountBasis {
    fn from(b: DiscountBasisDto) -> Self {
        match b {
            DiscountBasisDto::NetTotal => DiscountBasis::NetTotal,
            DiscountBasisDto::GrandTotal => DiscountBasis::GrandTotal,
        }
    }
}

/// `POST /api/invoices/:id/pay` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordPaymentRequest {
    pub method: PaymentMethodDto,
    /// Amount tendered.
    pub amount: Money,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

/// A persisted invoice line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceLineResponse {
    pub item_code: String,
    pub item_name: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    pub rate: Money,
    /// `rate * quantity`, unrounded — the same figure `compute_totals` summed.
    pub amount: Money,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hsn_sac: Option<String>,
}

/// Tax breakdown, copied from [`peacock_core::tax::TaxBreakdown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxBreakdownResponse {
    pub cgst: Money,
    pub sgst: Money,
    pub igst: Money,
    pub total_tax: Money,
}

/// Every money figure on an invoice, copied verbatim from [`InvoiceTotals`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceTotalsResponse {
    pub net_total: Money,
    pub discount: Money,
    pub taxable_value: Money,
    pub tax: TaxBreakdownResponse,
    pub grand_total: Money,
    pub rounded_total: Money,
    pub round_off: Money,
}

impl From<&InvoiceTotals> for InvoiceTotalsResponse {
    /// Field-for-field copy. No arithmetic here: re-deriving any of these would be a
    /// second source of truth for money and a parity failure waiting to happen.
    fn from(t: &InvoiceTotals) -> Self {
        Self {
            net_total: t.net_total,
            discount: t.discount,
            taxable_value: t.taxable_value,
            tax: TaxBreakdownResponse {
                cgst: t.tax.cgst,
                sgst: t.tax.sgst,
                igst: t.tax.igst,
                total_tax: t.tax.total_tax,
            },
            grand_total: t.grand_total,
            rounded_total: t.rounded_total,
            round_off: t.round_off,
        }
    }
}

/// A recorded payment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaymentResponse {
    pub method: PaymentMethodDto,
    pub amount: Money,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub paid_at: DateTime<Utc>,
}

/// Full invoice representation, returned by create/get/pay/consolidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceResponse {
    /// The allocated invoice name, e.g. `"POS-2627-000001"`. Gapless per Rule 46(b).
    pub invoice_id: String,
    pub order_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    pub customer_name: String,
    pub status: InvoiceStatusDto,
    pub posted_at: DateTime<Utc>,
    /// Business day this invoice belongs to. Not the calendar date of `posted_at`: a
    /// 01:30 invoice belongs to the previous business day (see
    /// `peacock_core::businessday`).
    pub business_day: NaiveDate,
    pub fiscal_year: String,
    pub lines: Vec<InvoiceLineResponse>,
    #[serde(flatten)]
    pub totals: InvoiceTotalsResponse,
    pub payments: Vec<PaymentResponse>,
    /// Sum of recorded payments.
    pub paid_amount: Money,
    /// `rounded_total - paid_amount`. Zero once the invoice is settled.
    pub outstanding_amount: Money,
    /// Echoed so a client can confirm which key owns this invoice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<Uuid>,
}

/// `GET /api/invoices` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceListResponse {
    pub invoices: Vec<InvoiceSummaryResponse>,
    pub count: usize,
    /// Sum of `rounded_total` over the **revenue-counting** invoices in this page
    /// (`Paid` + `Consolidated`, per `PosInvoiceStatus::REVENUE`).
    pub total_revenue: Money,
}

/// Compact invoice for list views. Keeps `rounded_total` because that is the figure
/// shift close and the P&L both aggregate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceSummaryResponse {
    pub invoice_id: String,
    pub order_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    pub customer_name: String,
    pub status: InvoiceStatusDto,
    pub posted_at: DateTime<Utc>,
    pub business_day: NaiveDate,
    pub grand_total: Money,
    pub rounded_total: Money,
    pub round_off: Money,
    pub paid_amount: Money,
    pub outstanding_amount: Money,
}

/// `GET /api/invoices` filters.
///
/// Every field is optional; an empty query lists everything. `from`/`to` are
/// **business days**, not calendar dates, and the range is inclusive on both ends
/// because a caller asking for `from=to=2026-07-28` means that one business day.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct InvoiceListQuery {
    #[serde(default)]
    pub from: Option<NaiveDate>,
    #[serde(default)]
    pub to: Option<NaiveDate>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub table: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use peacock_core::tax::{compute_totals, InvoiceLine};
    use rust_decimal_macros::dec;

    fn totals_fixture() -> InvoiceTotals {
        // The worked example from RUST_MIGRATION_PLAN_V2.md §5: 4 × ₹100, 5% GST,
        // ₹40 discount → taxable 360, tax 18, grand 378.
        compute_totals(
            &[InvoiceLine {
                item_name: "Item A".into(),
                quantity: dec!(4),
                rate: Money::new(dec!(100)),
                hsn_sac: None,
            }],
            Money::new(dec!(40)),
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap()
    }

    #[test]
    fn totals_response_copies_every_field_without_arithmetic() {
        let totals = totals_fixture();
        let dto = InvoiceTotalsResponse::from(&totals);

        assert_eq!(dto.net_total, totals.net_total);
        assert_eq!(dto.discount, totals.discount);
        assert_eq!(dto.taxable_value, totals.taxable_value);
        assert_eq!(dto.tax.cgst, totals.tax.cgst);
        assert_eq!(dto.tax.sgst, totals.tax.sgst);
        assert_eq!(dto.tax.igst, totals.tax.igst);
        assert_eq!(dto.tax.total_tax, totals.tax.total_tax);
        assert_eq!(dto.grand_total, totals.grand_total);
        assert_eq!(dto.rounded_total, totals.rounded_total);
        assert_eq!(dto.round_off, totals.round_off);
    }

    #[test]
    fn every_money_field_serialises_as_a_json_string() {
        // The guard against the PLAN.md bug: a JSON number here would be parsed as an
        // IEEE-754 double by every JS client.
        let json = serde_json::to_value(InvoiceTotalsResponse::from(&totals_fixture())).unwrap();

        for field in [
            "net_total",
            "discount",
            "taxable_value",
            "grand_total",
            "rounded_total",
            "round_off",
        ] {
            assert!(
                json[field].is_string(),
                "{field} must serialise as a string, got {:?}",
                json[field]
            );
        }
        for field in ["cgst", "sgst", "igst", "total_tax"] {
            assert!(
                json["tax"][field].is_string(),
                "tax.{field} must serialise as a string"
            );
        }
    }

    #[test]
    fn money_survives_a_json_round_trip_to_the_paisa() {
        let totals = compute_totals(
            &[InvoiceLine {
                item_name: "Odd".into(),
                quantity: dec!(1),
                rate: Money::new(dec!(360.2)),
                hsn_sac: None,
            }],
            Money::ZERO,
            dec!(0.05),
            SupplyType::Intrastate,
            DiscountBasis::NetTotal,
        )
        .unwrap();

        let dto = InvoiceTotalsResponse::from(&totals);
        let round_tripped: InvoiceTotalsResponse =
            serde_json::from_str(&serde_json::to_string(&dto).unwrap()).unwrap();

        assert_eq!(round_tripped, dto);
        // The odd-paisa CGST split survives: 18.01 → 9.01 / 9.00.
        assert_eq!(round_tripped.tax.cgst, Money::new(dec!(9.01)));
        assert_eq!(round_tripped.tax.sgst, Money::new(dec!(9.00)));
        assert_eq!(
            round_tripped.tax.cgst + round_tripped.tax.sgst,
            round_tripped.tax.total_tax
        );
    }

    #[test]
    fn a_value_that_f64_cannot_hold_round_trips_exactly() {
        // 0.1 + 0.2 in f64 is 0.30000000000000004. As a string it is exact.
        let m = Money::new(dec!(12345678.91));
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, "\"12345678.91\"");
        let back: Money = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn create_request_deserialises_money_from_strings() {
        let json = r#"{
            "order_id": "ORD-001",
            "table": "T-01",
            "customer_name": "Walk-in",
            "lines": [
                {"item_code": "CURRY", "item_name": "Curry", "quantity": "2", "rate": "180.50"}
            ],
            "discount": "10.25",
            "tax_rate": "0.05",
            "series": "POS"
        }"#;

        let req: CreateInvoiceRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.order_id, "ORD-001");
        assert_eq!(req.table.as_deref(), Some("T-01"));
        assert_eq!(req.lines[0].rate, Money::new(dec!(180.50)));
        assert_eq!(req.lines[0].quantity, dec!(2));
        assert_eq!(req.discount, Money::new(dec!(10.25)));
        assert_eq!(req.tax_rate, dec!(0.05));
        // Defaults applied.
        assert_eq!(req.supply_type, SupplyTypeDto::Intrastate);
        assert_eq!(req.discount_basis, DiscountBasisDto::NetTotal);
    }

    #[test]
    fn create_request_defaults_discount_to_zero() {
        let json = r#"{
            "order_id": "ORD-002",
            "customer_name": "Walk-in",
            "lines": [],
            "tax_rate": "0.05",
            "series": "POS"
        }"#;
        let req: CreateInvoiceRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.discount, Money::ZERO);
        assert_eq!(req.table, None);
    }

    #[test]
    fn status_dto_round_trips_through_the_domain_enum() {
        for dto in [
            InvoiceStatusDto::Draft,
            InvoiceStatusDto::Paid,
            InvoiceStatusDto::Consolidated,
            InvoiceStatusDto::Return,
        ] {
            let domain: PosInvoiceStatus = dto.into();
            assert_eq!(InvoiceStatusDto::from(domain), dto);
        }
    }

    #[test]
    fn status_filter_parsing_is_case_insensitive_and_rejects_junk() {
        assert_eq!(
            InvoiceStatusDto::parse_filter("paid"),
            Some(InvoiceStatusDto::Paid)
        );
        assert_eq!(
            InvoiceStatusDto::parse_filter("  Consolidated "),
            Some(InvoiceStatusDto::Consolidated)
        );
        assert_eq!(
            InvoiceStatusDto::parse_filter("DRAFT"),
            Some(InvoiceStatusDto::Draft)
        );
        assert_eq!(InvoiceStatusDto::parse_filter("settled"), None);
    }

    #[test]
    fn payment_request_deserialises_amount_as_string() {
        let json = r#"{"method": "Upi", "amount": "378.00", "reference": "txn-9"}"#;
        let req: RecordPaymentRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, PaymentMethodDto::Upi);
        assert_eq!(req.amount, Money::new(dec!(378.00)));
        assert_eq!(req.reference.as_deref(), Some("txn-9"));
        assert!(!req.method.is_cash());
        assert!(PaymentMethodDto::Cash.is_cash());
    }

    #[test]
    fn list_query_accepts_partial_filters() {
        // Deliberately partial: absent keys must deserialise to None rather than fail,
        // which is what makes an unfiltered `GET /api/invoices` legal.
        let q: InvoiceListQuery =
            serde_json::from_str(r#"{"from": "2026-07-28", "status": "Paid"}"#).unwrap();
        assert_eq!(q.from, Some(NaiveDate::from_ymd_opt(2026, 7, 28).unwrap()));
        assert_eq!(q.to, None);
        assert_eq!(q.status.as_deref(), Some("Paid"));
        assert_eq!(q.table, None);

        let empty: InvoiceListQuery = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, InvoiceListQuery::default());
    }
}
