//! Order request/response DTOs.
//!
//! Lane 3D. Separate from [`peacock_core::model::UryOrderForm`] so the wire format can
//! evolve without forcing a domain change, and so the API can carry fields the domain
//! deliberately does not model: the order id, a lifecycle status and an optimistic
//! concurrency version.
//!
//! ## Why the API has a status and the domain does not
//!
//! `URY Order` upstream is a Frappe **UI form** with no status field
//! (GROUND-TRUTH.md); the record of state is the POS Invoice reached through
//! `last_invoice`. A client still has to be told whether an order is open, already
//! invoiced or cancelled, so [`OrderStatus`] lives here — at the API boundary — rather
//! than being invented inside `peacock_core`.
//!
//! ## Money on the wire
//!
//! `qty` is an integer because `ury_order_item.qty` is `Int` upstream; fractional
//! quantities are a schema change, not a port. `rate` is a decimal and `grand_total`
//! serialises as a string via [`Money`], so no JSON parser can turn a paisa into a
//! float.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use peacock_core::ids::{CustomerName, ItemCode, PosProfileName, TableName, UserName};
use peacock_core::model::{Kot, KotType, OrderItem, OrderLine, UryOrderForm};
use peacock_core::money::Money;

/// Deserialise a [`Decimal`] from a JSON number *or* a JSON string.
///
/// The workspace enables `rust_decimal`'s `serde-str` feature, which makes the derived
/// impl accept only strings. That is the right default for money on the way *out* — a
/// float can silently lose a paisa — but on the way in a client sending `"rate": 250` is
/// being reasonable, and a 400 there is a bug in us, not in them. Parsing goes through
/// the string form either way, so no value ever passes through an `f64`.
mod decimal_flexible {
    use rust_decimal::Decimal;
    use serde::de::{Error, Unexpected, Visitor};
    use serde::Deserializer;
    use std::fmt;
    use std::str::FromStr;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Decimal, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DecimalVisitor)
    }

    struct DecimalVisitor;

    impl<'de> Visitor<'de> for DecimalVisitor {
        type Value = Decimal;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a decimal as a number or a string")
        }

        fn visit_str<E: Error>(self, value: &str) -> Result<Decimal, E> {
            Decimal::from_str(value.trim())
                .or_else(|_| Decimal::from_scientific(value.trim()))
                .map_err(|_| E::invalid_value(Unexpected::Str(value), &self))
        }

        fn visit_u64<E: Error>(self, value: u64) -> Result<Decimal, E> {
            Ok(Decimal::from(value))
        }

        fn visit_i64<E: Error>(self, value: i64) -> Result<Decimal, E> {
            Ok(Decimal::from(value))
        }

        fn visit_f64<E: Error>(self, value: f64) -> Result<Decimal, E> {
            // Via the shortest round-tripping decimal string rather than the binary
            // value, so 12.50 does not arrive as 12.499999999999998.
            Decimal::from_str(&value.to_string())
                .map_err(|_| E::invalid_value(Unexpected::Float(value), &self))
        }
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Where an order is in its life.
///
/// `Invoiced` is terminal for modification: once a POS Invoice exists the invoice is
/// the record and the KOTs are printed, so a line change has to go through a
/// cancellation KOT rather than a silent edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Open,
    Invoiced,
    Cancelled,
}

impl OrderStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            OrderStatus::Open => "open",
            OrderStatus::Invoiced => "invoiced",
            OrderStatus::Cancelled => "cancelled",
        }
    }

    /// Whether items may still be changed.
    pub fn is_modifiable(self) -> bool {
        matches!(self, OrderStatus::Open)
    }
}

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

/// One cart line, in and out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderItemDto {
    /// ERPNext `Item.item_code`.
    pub item: String,
    pub item_name: String,
    /// `Int` upstream — see the module docs.
    pub qty: i32,
    /// Unit rate, held as a decimal so no arithmetic here ever goes through a float.
    ///
    /// Accepts either a JSON number (`250`, `12.50`) or a string (`"12.50"`) on the way
    /// in — see [`decimal_flexible`]. Real POS clients send both, and rejecting the
    /// number form would break them for no gain.
    #[serde(deserialize_with = "decimal_flexible::deserialize")]
    pub rate: Decimal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments: Option<String>,
    /// Course sequencing hint for the kitchen. Defaults to 0 (no preference).
    #[serde(default)]
    pub serve_priority: i32,
    /// Print a course separator above this line on the ticket.
    #[serde(default)]
    pub indicate_course: bool,
}

impl OrderItemDto {
    /// Line total, unrounded. Rounding happens once, at invoice level.
    pub fn line_total(&self) -> Money {
        Money::new(self.rate) * Decimal::from(self.qty)
    }

    /// The domain child-table row.
    pub fn to_domain(&self) -> OrderItem {
        OrderItem {
            item: ItemCode::new(&self.item),
            item_name: self.item_name.clone(),
            qty: self.qty,
            rate: Money::new(self.rate),
            comments: self.comments.clone(),
        }
    }

    /// The shape KOT routing consumes.
    pub fn to_order_line(&self) -> OrderLine {
        OrderLine {
            item_code: ItemCode::new(&self.item),
            item_name: self.item_name.clone(),
            qty: Decimal::from(self.qty),
            rate: Money::new(self.rate),
            comments: self.comments.clone(),
            serve_priority: self.serve_priority,
            indicate_course: self.indicate_course,
        }
    }
}

impl From<&OrderItem> for OrderItemDto {
    fn from(item: &OrderItem) -> Self {
        OrderItemDto {
            item: item.item.as_str().to_owned(),
            item_name: item.item_name.clone(),
            qty: item.qty,
            rate: item.rate.inner(),
            comments: item.comments.clone(),
            serve_priority: 0,
            indicate_course: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// `POST /api/orders`.
///
/// Either `restaurant_table` or `take_away` must identify where the order lives; an
/// order with neither has nowhere to be served.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateOrderRequest {
    #[serde(default)]
    pub take_away: bool,
    #[serde(default)]
    pub restaurant_table: Option<String>,
    pub customer_name: String,
    #[serde(default)]
    pub no_of_pax: i32,
    #[serde(default)]
    pub waiter: Option<String>,
    #[serde(default)]
    pub pos_profile: Option<String>,
    #[serde(default)]
    pub cashier: Option<String>,
    #[serde(default)]
    pub comments: Option<String>,
    /// May be empty: a table is often opened before the first round is taken.
    #[serde(default)]
    pub items: Vec<OrderItemDto>,
}

/// `PATCH /api/orders/:id`.
///
/// Every field is optional; absent means "leave alone". `items` and `append_items` are
/// the two ways to touch the cart and are mutually exclusive:
///
/// - `items` replaces the whole list — what a screen holding the full cart sends.
/// - `append_items` adds lines — what a waiter adding a round sends, and the form that
///   is safe under concurrency because it does not overwrite a line another waiter
///   just added.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchOrderRequest {
    #[serde(default)]
    pub items: Option<Vec<OrderItemDto>>,
    #[serde(default)]
    pub append_items: Option<Vec<OrderItemDto>>,
    #[serde(default)]
    pub no_of_pax: Option<i32>,
    #[serde(default)]
    pub customer_name: Option<String>,
    #[serde(default)]
    pub comments: Option<String>,
    #[serde(default)]
    pub waiter: Option<String>,
    /// Optimistic concurrency guard. When present it must equal the current version or
    /// the write is rejected with 409 rather than clobbering a newer state.
    #[serde(default)]
    pub version: Option<u64>,
}

impl PatchOrderRequest {
    /// True when the body asks for nothing at all.
    pub fn is_empty(&self) -> bool {
        self.items.is_none()
            && self.append_items.is_none()
            && self.no_of_pax.is_none()
            && self.customer_name.is_none()
            && self.comments.is_none()
            && self.waiter.is_none()
    }
}

/// `POST /api/orders/:id/invoice`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvoiceRequest {
    /// Naming series prefix for the invoice, e.g. `"PCK"`. CGST Rule 46(b) caps the
    /// whole name at 16 characters, so this is where an over-long prefix is caught.
    pub series: String,
    /// Business date. Decides the fiscal year segment of the invoice name.
    pub date: NaiveDate,
    /// Branch, which decides which production units the KOTs route to.
    pub branch: String,
    /// Naming series for the generated KOTs, e.g. `"KOT-"`.
    #[serde(default = "default_kot_series")]
    pub kot_naming_series: String,
    /// Room of the table, used to resolve courses. `None` for takeaway.
    #[serde(default)]
    pub room: Option<String>,
}

fn default_kot_series() -> String {
    "KOT-".to_string()
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

/// A single order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderResponse {
    pub id: String,
    pub status: OrderStatus,
    /// Bumped on every accepted write. Clients echo it back in `PatchOrderRequest`.
    pub version: u64,
    pub take_away: bool,
    pub restaurant_table: Option<String>,
    pub customer_name: String,
    pub no_of_pax: i32,
    /// Sum of `qty × rate`, rounded to paisa.
    pub grand_total: Money,
    pub last_invoice: Option<String>,
    pub items: Vec<OrderItemDto>,
    pub waiter: Option<String>,
    pub pos_profile: Option<String>,
    pub cashier: Option<String>,
    pub comments: Option<String>,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
}

/// `GET /api/orders` is not part of this lane; the list wrapper exists so a future
/// listing endpoint does not have to change the item shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderListResponse {
    pub orders: Vec<OrderResponse>,
    pub count: usize,
}

/// The invoice produced from an order, plus the tickets it printed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceResponse {
    /// Gapless, ≤16 characters, unique per fiscal year (CGST Rule 46(b)).
    pub invoice_name: String,
    pub order_id: String,
    /// Unrounded total.
    pub grand_total: Money,
    /// `grand_total` rounded to the rupee — what the customer pays.
    pub rounded_total: Money,
    /// `rounded_total - grand_total`. Posts to a round-off ledger; not cosmetic.
    pub round_off: Money,
    pub status: String,
    pub fiscal_year: String,
    /// One entry per production unit that received work. Empty is legitimate: a branch
    /// with no production units configured prints nothing.
    pub kots: Vec<KotSummaryDto>,
    /// Item codes that matched no production unit and therefore printed nowhere.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unrouted_items: Vec<String>,
}

/// Enough of a KOT to confirm what was printed where.
///
/// Deliberately not `crate::dto::kot::KotDto`: this lane must not couple its response
/// shape to another lane's file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KotSummaryDto {
    pub id: String,
    pub production: Option<String>,
    pub kot_type: String,
    pub item_count: usize,
    pub date: NaiveDate,
}

impl KotSummaryDto {
    /// Build from a routed ticket. `id` is supplied because routing returns
    /// `name: None` — persisting and naming is the storage layer's job.
    pub fn from_kot(id: impl Into<String>, kot: &Kot) -> Self {
        KotSummaryDto {
            id: id.into(),
            production: kot.production.as_ref().map(|p| p.as_str().to_owned()),
            kot_type: kot_type_str(kot.kot_type).to_owned(),
            item_count: kot.kot_items.len(),
            date: kot.date,
        }
    }
}

/// Wire spelling of a KOT type. Matches the upstream `URY KOT.type` select values.
pub fn kot_type_str(t: KotType) -> &'static str {
    match t {
        KotType::NewOrder => "New Order",
        KotType::OrderModified => "Order Modified",
        KotType::Cancelled => "Cancelled",
        KotType::PartiallyCancelled => "Partially cancelled",
    }
}

/// `DELETE /api/orders/:id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderResponse {
    pub id: String,
    pub status: OrderStatus,
    pub version: u64,
}

// ---------------------------------------------------------------------------
// Domain conversion
// ---------------------------------------------------------------------------

/// Build the domain form from a create request.
///
/// `grand_total` is computed here rather than trusted from the client: a client-sent
/// total is a tampering vector and diverges from the lines on any rounding change.
pub fn form_from_create(req: &CreateOrderRequest) -> UryOrderForm {
    let items: Vec<OrderItem> = req.items.iter().map(OrderItemDto::to_domain).collect();
    UryOrderForm {
        take_away: req.take_away,
        restaurant_table: req
            .restaurant_table
            .as_deref()
            .map(TableName::from),
        customer_name: CustomerName::new(&req.customer_name),
        // `no_of_pax` is `reqd: 1` upstream and the schema agrees
        // (`orders_no_of_pax_positive`, 007_order.sql), so an absent or zero count becomes
        // one cover rather than a constraint violation. A body that omits the field is a
        // client saying "someone is sitting here, I did not count them", which is one — not
        // zero, and not a 400. A *negative* count is still rejected in `validate_create`,
        // because that can only be a bug or a tampering attempt.
        no_of_pax: req.no_of_pax.max(1),
        grand_total: total_of(&req.items),
        last_invoice: None,
        items,
        waiter: req.waiter.as_deref().map(UserName::from),
        pos_profile: req.pos_profile.as_deref().map(PosProfileName::from),
        cashier: req.cashier.as_deref().map(UserName::from),
        comments: req.comments.clone(),
        modified_time: Some(Utc::now()),
    }
}

/// Sum of the lines, rounded to paisa. The single definition of an order's total.
pub fn total_of(items: &[OrderItemDto]) -> Money {
    paisa_scale(items.iter().map(OrderItemDto::line_total).sum::<Money>())
}

/// Sum of domain lines, rounded to paisa. Same rule as [`total_of`], for records that
/// are already in domain shape.
pub fn total_of_domain(items: &[OrderItem]) -> Money {
    paisa_scale(
        items
            .iter()
            .map(|i| i.rate * Decimal::from(i.qty))
            .sum::<Money>(),
    )
}

/// Round to paisa and pin the scale at two places.
///
/// [`Money::to_paisa`] rounds but leaves the scale alone, so a total assembled from
/// integer rates serialises as `"540"` while the same total from `250.00` serialises as
/// `"540.00"`. Both are the same amount, but a client parsing amounts should not have to
/// cope with the format shifting according to how another client typed its rates. Money
/// on this API's wire is always two places.
///
/// This is a presentation decision at the boundary; the arithmetic is untouched, so the
/// parity harness sees the same values.
fn paisa_scale(amount: Money) -> Money {
    let mut d = amount.to_paisa().inner();
    if d.scale() < 2 {
        // Only ever widens: `to_paisa` already removed anything beyond two places.
        d.rescale(2);
    }
    Money::new(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn item(code: &str, qty: i32, rate: Decimal) -> OrderItemDto {
        OrderItemDto {
            item: code.to_string(),
            item_name: format!("{code} name"),
            qty,
            rate,
            comments: None,
            serve_priority: 0,
            indicate_course: false,
        }
    }

    #[test]
    fn line_total_multiplies_without_floating_point() {
        // 3 × 0.1 must be exactly 0.3, which an f64 cannot promise.
        let line = item("TEA", 3, dec!(0.1));
        assert_eq!(line.line_total(), Money::new(dec!(0.3)));
    }

    #[test]
    fn total_rounds_once_to_paisa() {
        let items = vec![item("A", 3, dec!(33.333)), item("B", 1, dec!(0.005))];
        // 99.999 + 0.005 = 100.004 → 100.00 at paisa.
        assert_eq!(total_of(&items), Money::new(dec!(100.00)));
    }

    #[test]
    fn empty_cart_totals_zero() {
        assert_eq!(total_of(&[]), Money::ZERO);
    }

    #[test]
    fn totals_always_serialise_with_two_decimal_places() {
        // Integer rates and decimal rates must produce the same wire format, so a client
        // never has to cope with "540" one minute and "540.00" the next.
        let from_integers = total_of(&[item("A", 2, dec!(250)), item("B", 2, dec!(20))]);
        let from_decimals = total_of(&[item("A", 2, dec!(250.00)), item("B", 2, dec!(20.00))]);

        assert_eq!(from_integers, from_decimals);
        assert_eq!(
            serde_json::to_value(from_integers).unwrap(),
            serde_json::json!("540.00")
        );
        assert_eq!(
            serde_json::to_value(total_of(&[])).unwrap(),
            serde_json::json!("0.00")
        );
    }

    #[test]
    fn pinning_the_scale_does_not_change_the_amount() {
        let items = vec![item("A", 3, dec!(33.333))];
        // 99.999 → 100.00 at paisa; the scale rule must not round a second time.
        assert_eq!(total_of(&items), Money::new(dec!(100.00)));
        assert_eq!(total_of(&items).inner(), dec!(100));
    }

    #[test]
    fn domain_total_agrees_with_dto_total() {
        let items = vec![item("A", 2, dec!(12.50)), item("B", 1, dec!(7.25))];
        let domain: Vec<OrderItem> = items.iter().map(OrderItemDto::to_domain).collect();
        assert_eq!(total_of(&items), total_of_domain(&domain));
    }

    #[test]
    fn create_request_computes_its_own_total() {
        let req = CreateOrderRequest {
            customer_name: "Walk-in".into(),
            items: vec![item("A", 2, dec!(100))],
            ..Default::default()
        };
        let form = form_from_create(&req);
        assert_eq!(form.grand_total, Money::new(dec!(200)));
        assert_eq!(form.items.len(), 1);
        assert!(form.last_invoice.is_none());
    }

    #[test]
    fn money_serialises_as_a_string() {
        let json = serde_json::to_value(Money::new(dec!(1234.50))).unwrap();
        assert_eq!(json, serde_json::json!("1234.50"));
    }

    #[test]
    fn item_round_trips_through_the_domain() {
        let dto = item("SAMOSA", 4, dec!(15.00));
        let back = OrderItemDto::from(&dto.to_domain());
        assert_eq!(back.item, dto.item);
        assert_eq!(back.qty, dto.qty);
        assert_eq!(back.rate, dto.rate);
    }

    #[test]
    fn order_line_carries_course_hints_to_the_kitchen() {
        let mut dto = item("BIRYANI", 2, dec!(250));
        dto.serve_priority = 3;
        dto.indicate_course = true;
        let line = dto.to_order_line();
        assert_eq!(line.qty, dec!(2));
        assert_eq!(line.serve_priority, 3);
        assert!(line.indicate_course);
    }

    #[test]
    fn patch_with_no_fields_is_detected_as_empty() {
        assert!(PatchOrderRequest::default().is_empty());
        assert!(!PatchOrderRequest {
            no_of_pax: Some(2),
            ..Default::default()
        }
        .is_empty());
        // A version alone changes nothing, so it still counts as empty.
        assert!(PatchOrderRequest {
            version: Some(1),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn status_gates_modification() {
        assert!(OrderStatus::Open.is_modifiable());
        assert!(!OrderStatus::Invoiced.is_modifiable());
        assert!(!OrderStatus::Cancelled.is_modifiable());
    }

    #[test]
    fn status_serialises_snake_case() {
        assert_eq!(
            serde_json::to_value(OrderStatus::Invoiced).unwrap(),
            serde_json::json!("invoiced")
        );
    }

    #[test]
    fn kot_type_wire_spelling_matches_upstream_select() {
        assert_eq!(kot_type_str(KotType::NewOrder), "New Order");
        assert_eq!(kot_type_str(KotType::OrderModified), "Order Modified");
        assert_eq!(kot_type_str(KotType::Cancelled), "Cancelled");
        assert_eq!(
            kot_type_str(KotType::PartiallyCancelled),
            "Partially cancelled"
        );
    }

    #[test]
    fn rate_accepts_a_number_or_a_string() {
        let cases = [
            (serde_json::json!(250), dec!(250)),
            (serde_json::json!(12.50), dec!(12.50)),
            (serde_json::json!("12.50"), dec!(12.50)),
            (serde_json::json!("0"), dec!(0)),
        ];
        for (raw, expected) in cases {
            let body = serde_json::json!({
                "item": "TEA", "item_name": "Tea", "qty": 1, "rate": raw
            });
            let dto: OrderItemDto = serde_json::from_value(body).unwrap();
            assert_eq!(dto.rate, expected, "rate {raw} must parse");
        }
    }

    #[test]
    fn a_float_rate_does_not_pick_up_binary_noise() {
        let dto: OrderItemDto = serde_json::from_value(serde_json::json!({
            "item": "TEA", "item_name": "Tea", "qty": 3, "rate": 12.50
        }))
        .unwrap();
        // Straight through f64 this would be 12.499999999999998.
        assert_eq!(dto.rate, dec!(12.50));
        assert_eq!(dto.line_total(), Money::new(dec!(37.50)));
    }

    #[test]
    fn a_non_numeric_rate_is_rejected() {
        let result: Result<OrderItemDto, _> = serde_json::from_value(serde_json::json!({
            "item": "TEA", "item_name": "Tea", "qty": 1, "rate": "free"
        }));
        assert!(result.is_err());
    }

    #[test]
    fn create_request_accepts_a_minimal_body() {
        let req: CreateOrderRequest =
            serde_json::from_value(serde_json::json!({"customer_name": "Walk-in"})).unwrap();
        assert!(!req.take_away);
        assert!(req.items.is_empty());
        assert_eq!(req.no_of_pax, 0);
    }

    #[test]
    fn invoice_request_defaults_the_kot_series() {
        let req: CreateInvoiceRequest = serde_json::from_value(serde_json::json!({
            "series": "PCK",
            "date": "2026-07-31",
            "branch": "Peacock - Main"
        }))
        .unwrap();
        assert_eq!(req.kot_naming_series, "KOT-");
        assert!(req.room.is_none());
    }
}
