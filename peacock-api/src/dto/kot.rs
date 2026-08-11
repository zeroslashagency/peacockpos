//! KOT request/response DTOs.
//!
//! Separate from `peacock_core::model::Kot` so the API can evolve without forcing
//! domain changes.

use chrono::{NaiveDate, NaiveTime};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use peacock_core::ids::{
    BranchName, CustomerName, ItemCode, PosProfileName, TableName,
};
use peacock_core::model::{Kot, KotItem, KotType};

// ---------------------------------------------------------------------------
// Request DTOs
// ---------------------------------------------------------------------------

/// Request to generate KOTs for an order.
///
/// Maps to `peacock_core::kot::KotContext` + `OrderLine` slice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateKotRequest {
    /// POS Invoice name.
    pub invoice: String,
    /// Branch determines which production units are active.
    pub branch: String,
    /// Naming series for the KOT (e.g., "KOT-").
    pub naming_series: String,
    pub date: NaiveDate,
    #[serde(default)]
    pub time: Option<NaiveTime>,
    #[serde(default)]
    pub restaurant_table: Option<String>,
    /// Room name for course lookup. None for takeaway.
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub customer_name: Option<String>,
    #[serde(default)]
    pub pos_profile: Option<String>,
    #[serde(default)]
    pub comments: Option<String>,
    #[serde(default)]
    pub order_no: Option<String>,
    #[serde(default)]
    pub table_takeaway: bool,
    #[serde(default)]
    pub is_aggregator: bool,
    #[serde(default)]
    pub aggregator_id: Option<String>,
    /// The order lines to route to production units.
    pub items: Vec<OrderLineDto>,
}

/// Order line for KOT generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderLineDto {
    pub item_code: String,
    pub item_name: String,
    pub qty: Decimal,
    #[serde(default)]
    pub comments: Option<String>,
    #[serde(default)]
    pub serve_priority: i32,
    #[serde(default)]
    pub indicate_course: bool,
}

/// Request to mark a KOT as prepared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkPreparedRequest {
    /// Time preparation completed.
    pub prepared_at: Option<NaiveTime>,
}

// ---------------------------------------------------------------------------
// Response DTOs
// ---------------------------------------------------------------------------

/// Response from KOT generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateKotResponse {
    /// The generated KOTs, one per production unit that received items.
    pub kots: Vec<KotDto>,
    /// Item codes that didn't route to any production unit.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unrouted_items: Vec<String>,
}

/// Single KOT with its items.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KotDto {
    pub id: String,
    pub naming_series: String,
    pub invoice: String,
    pub restaurant_table: Option<String>,
    pub customer_name: Option<String>,
    pub original_kot: Option<String>,
    pub date: NaiveDate,
    pub time: Option<NaiveTime>,
    pub kot_type: String,
    pub order_status: Option<String>,
    pub production: Option<String>,
    pub start_time_prep: Option<NaiveTime>,
    pub items: Vec<KotItemDto>,
    pub pos_profile: Option<String>,
    pub branch: Option<String>,
    pub verified: bool,
    pub verified_by: Option<String>,
    pub table_takeaway: bool,
    pub is_aggregator: bool,
    pub aggregator_id: Option<String>,
    pub comments: Option<String>,
    pub order_no: Option<String>,
}

/// KOT item (child table row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KotItemDto {
    pub item: String,
    pub item_name: String,
    pub quantity: Decimal,
    pub cancelled_qty: Decimal,
    pub comments: Option<String>,
    pub course: Option<String>,
    pub serve_priority: i32,
    pub indicate_course: bool,
}

/// List of pending KOTs for a production unit (kitchen view).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingKotsResponse {
    pub production_unit: String,
    pub kots: Vec<KotDto>,
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

impl From<Kot> for KotDto {
    fn from(kot: Kot) -> Self {
        KotDto {
            id: kot.name.map(|n| n.to_string()).unwrap_or_default(),
            naming_series: kot.naming_series,
            invoice: kot.invoice,
            restaurant_table: kot.restaurant_table.map(|t| t.to_string()),
            customer_name: kot.customer_name.map(|c| c.to_string()),
            original_kot: kot.original_kot,
            date: kot.date,
            time: kot.time,
            kot_type: kot_type_to_string(kot.kot_type),
            order_status: kot.order_status,
            production: kot.production.map(|p| p.to_string()),
            start_time_prep: kot.start_time_prep,
            items: kot.kot_items.into_iter().map(KotItemDto::from).collect(),
            pos_profile: kot.pos_profile.map(|p| p.to_string()),
            branch: kot.branch.map(|b| b.to_string()),
            verified: kot.verified,
            verified_by: kot.verified_by.map(|u| u.to_string()),
            table_takeaway: kot.table_takeaway,
            is_aggregator: kot.is_aggregator,
            aggregator_id: kot.aggregator_id,
            comments: kot.comments,
            order_no: kot.order_no,
        }
    }
}

impl From<KotItem> for KotItemDto {
    fn from(item: KotItem) -> Self {
        KotItemDto {
            item: item.item.to_string(),
            item_name: item.item_name,
            quantity: item.quantity,
            cancelled_qty: item.cancelled_qty,
            comments: item.comments,
            course: item.course.map(|c| c.to_string()),
            serve_priority: item.serve_priority,
            indicate_course: item.indicate_course,
        }
    }
}

fn kot_type_to_string(kt: KotType) -> String {
    match kt {
        KotType::NewOrder => "New Order".to_string(),
        KotType::OrderModified => "Order Modified".to_string(),
        KotType::Cancelled => "Cancelled".to_string(),
        KotType::PartiallyCancelled => "Partially cancelled".to_string(),
    }
}

impl GenerateKotRequest {
    /// Convert to domain `KotContext`.
    pub fn to_context(&self) -> peacock_core::kot::KotContext {
        let mut ctx = peacock_core::kot::KotContext::new(
            self.invoice.clone(),
            BranchName::from(self.branch.as_str()),
            self.naming_series.clone(),
            self.date,
        );

        ctx.time = self.time;
        ctx.restaurant_table = self.restaurant_table.as_ref().map(|t| TableName::from(t.as_str()));
        ctx.room = self.room.as_ref().map(|r| peacock_core::ids::RoomName::from(r.as_str()));
        ctx.customer_name = self.customer_name.as_ref().map(|c| CustomerName::from(c.as_str()));
        ctx.pos_profile = self.pos_profile.as_ref().map(|p| PosProfileName::from(p.as_str()));
        ctx.comments = self.comments.clone();
        ctx.order_no = self.order_no.clone();
        ctx.table_takeaway = self.table_takeaway;
        ctx.is_aggregator = self.is_aggregator;
        ctx.aggregator_id = self.aggregator_id.clone();

        ctx
    }

    /// Convert items to domain `OrderLine`.
    pub fn to_order_lines(&self) -> Vec<peacock_core::model::OrderLine> {
        self.items
            .iter()
            .map(|item| peacock_core::model::OrderLine {
                item_code: ItemCode::from(item.item_code.as_str()),
                item_name: item.item_name.clone(),
                qty: item.qty,
                rate: peacock_core::money::Money::new(Decimal::ZERO), // Not used in routing
                comments: item.comments.clone(),
                serve_priority: item.serve_priority,
                indicate_course: item.indicate_course,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn generate_request_converts_to_context() {
        let req = GenerateKotRequest {
            invoice: "ACC-PSINV-2026-00042".to_string(),
            branch: "Peacock - Main".to_string(),
            naming_series: "KOT-".to_string(),
            date: NaiveDate::from_ymd_opt(2026, 7, 28).unwrap(),
            time: Some(NaiveTime::from_hms_opt(14, 30, 0).unwrap()),
            restaurant_table: Some("T-01".to_string()),
            room: Some("Hall".to_string()),
            customer_name: Some("Walk-in".to_string()),
            pos_profile: Some("Peacock POS".to_string()),
            comments: None,
            order_no: Some("ORD-001".to_string()),
            table_takeaway: false,
            is_aggregator: false,
            aggregator_id: None,
            items: vec![],
        };

        let ctx = req.to_context();
        assert_eq!(ctx.invoice, "ACC-PSINV-2026-00042");
        assert_eq!(ctx.branch.as_str(), "Peacock - Main");
        assert_eq!(ctx.date, NaiveDate::from_ymd_opt(2026, 7, 28).unwrap());
    }

    #[test]
    fn order_line_dto_converts_to_domain() {
        let req = GenerateKotRequest {
            invoice: "INV-001".to_string(),
            branch: "Main".to_string(),
            naming_series: "KOT-".to_string(),
            date: NaiveDate::from_ymd_opt(2026, 7, 28).unwrap(),
            time: None,
            restaurant_table: None,
            room: None,
            customer_name: None,
            pos_profile: None,
            comments: None,
            order_no: None,
            table_takeaway: true,
            is_aggregator: false,
            aggregator_id: None,
            items: vec![OrderLineDto {
                item_code: "CURRY".to_string(),
                item_name: "Chicken Curry".to_string(),
                qty: dec!(2),
                comments: Some("No onions".to_string()),
                serve_priority: 1,
                indicate_course: true,
            }],
        };

        let lines = req.to_order_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].item_code.as_str(), "CURRY");
        assert_eq!(lines[0].qty, dec!(2));
        assert_eq!(lines[0].comments, Some("No onions".to_string()));
    }

    #[test]
    fn kot_type_serializes_correctly() {
        assert_eq!(kot_type_to_string(KotType::NewOrder), "New Order");
        assert_eq!(kot_type_to_string(KotType::OrderModified), "Order Modified");
        assert_eq!(kot_type_to_string(KotType::Cancelled), "Cancelled");
        assert_eq!(
            kot_type_to_string(KotType::PartiallyCancelled),
            "Partially cancelled"
        );
    }
}
