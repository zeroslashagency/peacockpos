//! Shift Management DTOs.
//!
//! Request/response types for Lane 3G endpoints. Deliberately separate from
//! `peacock_core::ports::{Shift, ZReport}` so the API boundary can evolve independently.

use chrono::{DateTime, Utc};
use peacock_core::ports::{Shift, ZReport};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShiftResponse {
    pub name: String,
    pub terminal: String,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub opened_by: String,
    pub business_day: String, // ISO 8601 date string (YYYY-MM-DD)
}

impl From<Shift> for ShiftResponse {
    fn from(s: Shift) -> Self {
        Self {
            name: s.name.to_string(),
            terminal: s.terminal.to_string(),
            opened_at: s.opened_at,
            closed_at: s.closed_at,
            opened_by: s.opened_by.to_string(),
            business_day: s.business_day.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZReportResponse {
    pub shift_name: String,
    pub terminal: String,
    pub business_day: String,
    pub opened_at: DateTime<Utc>,
    pub closed_at: DateTime<Utc>,
    pub invoice_count: i64,
    pub cash_total: String,
    pub card_total: String,
    pub total_revenue: String,
    pub cash_threshold_warning: bool,
}

impl From<ZReport> for ZReportResponse {
    fn from(z: ZReport) -> Self {
        Self {
            shift_name: z.shift_name.to_string(),
            terminal: z.terminal.to_string(),
            business_day: z.business_day.to_string(),
            opened_at: z.opened_at,
            closed_at: z.closed_at,
            invoice_count: z.invoice_count,
            cash_total: z.cash_total.to_string(),
            card_total: z.card_total.to_string(),
            total_revenue: z.total_revenue.to_string(),
            cash_threshold_warning: z.cash_threshold_warning,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShiftListResponse {
    pub shifts: Vec<ShiftResponse>,
    pub count: usize,
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct OpenShiftRequest {
    pub terminal: String,
    pub opened_by: String,
    #[serde(default = "default_business_day")]
    pub business_day: Option<String>, // ISO 8601 date, defaults to today
}

fn default_business_day() -> Option<String> {
    None
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CloseShiftRequest {
    #[serde(default = "default_cutoff_hour")]
    pub cutoff_hour: u32, // Hour (0–23) in IST when business day rolls over
}

fn default_cutoff_hour() -> u32 {
    3 // Default: 03:00 IST
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ShiftListQuery {
    #[serde(default)]
    pub terminal: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default = "default_offset")]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

fn default_offset() -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use peacock_core::ids::{ShiftName, TerminalName, UserName};
    use peacock_core::money::Money;
    use rust_decimal_macros::dec;

    fn sample_shift() -> Shift {
        Shift {
            name: ShiftName::new("SHIFT-001"),
            terminal: TerminalName::new("POS-01"),
            opened_at: Utc.with_ymd_and_hms(2026, 7, 28, 10, 0, 0).unwrap(),
            closed_at: None,
            opened_by: UserName::new("waiter@test.com"),
            business_day: NaiveDate::from_ymd_opt(2026, 7, 28).unwrap(),
        }
    }

    fn sample_z_report() -> ZReport {
        ZReport {
            shift_name: ShiftName::new("SHIFT-001"),
            terminal: TerminalName::new("POS-01"),
            business_day: NaiveDate::from_ymd_opt(2026, 7, 28).unwrap(),
            opened_at: Utc.with_ymd_and_hms(2026, 7, 28, 10, 0, 0).unwrap(),
            closed_at: Utc.with_ymd_and_hms(2026, 7, 28, 22, 0, 0).unwrap(),
            invoice_count: 42,
            cash_total: Money::new(dec!(8500.00)),
            card_total: Money::new(dec!(2500.00)),
            total_revenue: Money::new(dec!(11000.00)),
            cash_threshold_warning: false,
        }
    }

    #[test]
    fn shift_response_converts_from_domain() {
        let shift = sample_shift();
        let response = ShiftResponse::from(shift.clone());

        assert_eq!(response.name, "SHIFT-001");
        assert_eq!(response.terminal, "POS-01");
        assert_eq!(response.opened_by, "waiter@test.com");
        assert_eq!(response.business_day, "2026-07-28");
        assert_eq!(response.closed_at, None);
    }

    #[test]
    fn z_report_response_converts_from_domain() {
        let report = sample_z_report();
        let response = ZReportResponse::from(report.clone());

        assert_eq!(response.shift_name, "SHIFT-001");
        assert_eq!(response.terminal, "POS-01");
        assert_eq!(response.business_day, "2026-07-28");
        assert_eq!(response.invoice_count, 42);
        assert_eq!(response.cash_total, "8500.00");
        assert_eq!(response.card_total, "2500.00");
        assert_eq!(response.total_revenue, "11000.00");
        assert!(!response.cash_threshold_warning);
    }

    #[test]
    fn z_report_serializes_money_as_strings() {
        let report = sample_z_report();
        let response = ZReportResponse::from(report);
        let json = serde_json::to_value(&response).unwrap();

        assert_eq!(json["cash_total"], "8500.00");
        assert_eq!(json["card_total"], "2500.00");
        assert_eq!(json["total_revenue"], "11000.00");
    }

    #[test]
    fn open_shift_request_deserializes() {
        let json = r#"{"terminal": "POS-01", "opened_by": "waiter@test.com"}"#;
        let req: OpenShiftRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.terminal, "POS-01");
        assert_eq!(req.opened_by, "waiter@test.com");
        assert_eq!(req.business_day, None);
    }

    #[test]
    fn open_shift_request_with_explicit_business_day() {
        let json = r#"{"terminal": "POS-01", "opened_by": "waiter@test.com", "business_day": "2026-07-28"}"#;
        let req: OpenShiftRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.business_day, Some("2026-07-28".to_string()));
    }

    #[test]
    fn close_shift_request_uses_default_cutoff() {
        let json = r#"{}"#;
        let req: CloseShiftRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.cutoff_hour, 3);
    }

    #[test]
    fn close_shift_request_with_explicit_cutoff() {
        let json = r#"{"cutoff_hour": 4}"#;
        let req: CloseShiftRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.cutoff_hour, 4);
    }

    #[test]
    fn shift_list_query_defaults() {
        let query = ShiftListQuery {
            terminal: None,
            limit: default_limit(),
            offset: default_offset(),
        };

        assert_eq!(query.limit, 50);
        assert_eq!(query.offset, 0);
    }

    #[test]
    fn shift_response_handles_closed_shift() {
        let mut shift = sample_shift();
        shift.closed_at = Some(Utc.with_ymd_and_hms(2026, 7, 28, 22, 0, 0).unwrap());

        let response = ShiftResponse::from(shift);
        assert!(response.closed_at.is_some());
    }

    #[test]
    fn cash_threshold_warning_serializes_correctly() {
        let mut report = sample_z_report();
        report.cash_threshold_warning = true;

        let response = ZReportResponse::from(report);
        let json = serde_json::to_value(&response).unwrap();

        assert_eq!(json["cash_threshold_warning"], true);
    }
}
