//! DTOs for aggregator integration (Swiggy/Zomato webhooks).

use peacock_core::money::Money;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Webhook payload from aggregator platform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregatorWebhook {
    /// Unique order ID from aggregator
    pub order_id: String,
    /// Platform identifier (swiggy, zomato, etc)
    pub platform: String,
    /// Customer details
    pub customer_name: String,
    pub customer_phone: Option<String>,
    /// Order items
    pub items: Vec<AggregatorItem>,
    /// Total amount charged to customer
    pub total: Money,
    /// Order timestamp (ISO 8601)
    pub ordered_at: String,
    /// Delivery instructions
    pub instructions: Option<String>,
}

/// Single item in an aggregator order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregatorItem {
    /// Item code (must map to internal menu)
    pub item_code: String,
    pub item_name: String,
    pub quantity: Decimal,
    pub rate: Money,
    pub special_instructions: Option<String>,
}

/// Request to create an aggregator order in the system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateAggregatorOrderRequest {
    pub order_id: String,
    pub platform: String,
    pub customer_name: String,
    pub customer_phone: Option<String>,
    pub items: Vec<AggregatorItem>,
    pub total: Money,
    pub ordered_at: String,
    pub instructions: Option<String>,
}

/// Response after receiving webhook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookResponse {
    pub status: String,
    pub order_id: String,
    pub internal_order_id: Option<String>,
}

/// Aggregator order stored in our system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregatorOrder {
    pub id: String,
    pub aggregator_order_id: String,
    pub platform: String,
    pub customer_name: String,
    pub customer_phone: Option<String>,
    pub items: Vec<AggregatorItem>,
    pub total: Money,
    pub ordered_at: String,
    pub status: AggregatorOrderStatus,
    pub internal_order_id: Option<String>,
    pub internal_invoice_id: Option<String>,
    pub instructions: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Status of an aggregator order in our system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AggregatorOrderStatus {
    Pending,
    Accepted,
    Rejected,
    Completed,
}

/// Request to accept an aggregator order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptOrderRequest {
    /// Estimated preparation time in minutes
    pub prep_time_minutes: Option<u32>,
}

/// Response after accepting an order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptOrderResponse {
    pub status: String,
    pub internal_order_id: String,
    pub message: String,
}

/// Request to reject an aggregator order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejectOrderRequest {
    /// Reason for rejection
    pub reason: String,
}

/// Response after rejecting an order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejectOrderResponse {
    pub status: String,
    pub message: String,
}

/// Settlement/payout record from aggregator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settlement {
    pub id: String,
    pub platform: String,
    pub settlement_date: String,
    pub total_orders: u32,
    pub gross_amount: Money,
    pub commission: Money,
    pub net_amount: Money,
    pub order_ids: Vec<String>,
}

/// Settlement reconciliation result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettlementReconciliation {
    pub settlement_id: String,
    pub matched_orders: u32,
    pub unmatched_orders: Vec<String>,
    pub amount_mismatch: Vec<AmountMismatch>,
    pub total_matched_amount: Money,
    pub total_unmatched_amount: Money,
}

/// Represents an amount mismatch between settlement and our records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmountMismatch {
    pub order_id: String,
    pub expected: Money,
    pub actual: Money,
    pub difference: Money,
}
