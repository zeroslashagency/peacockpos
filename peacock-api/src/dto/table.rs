//! Table Management DTOs.
//!
//! Request/response types for Lane 3B endpoints. Deliberately separate from
//! `peacock_core::model::Table` so the API boundary can evolve independently.

use peacock_core::model::{Table, TableShape};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TableResponse {
    pub name: String,
    pub no_of_seats: i32,
    pub minimum_seating: i32,
    pub restaurant: String,
    pub restaurant_room: String,
    pub branch: String,
    pub is_take_away: bool,
    pub occupied: bool,
    pub table_shape: Option<String>,
    pub layout_x: f64,
    pub layout_y: f64,
    pub layout_width: f64,
    pub layout_height: f64,
    pub merged_with: Vec<String>,
}

impl From<Table> for TableResponse {
    fn from(t: Table) -> Self {
        Self {
            name: t.name.to_string(),
            no_of_seats: t.no_of_seats,
            minimum_seating: t.minimum_seating,
            restaurant: t.restaurant.to_string(),
            restaurant_room: t.restaurant_room.to_string(),
            branch: t.branch.to_string(),
            is_take_away: t.is_take_away,
            occupied: t.occupied,
            table_shape: t.table_shape.map(|s| match s {
                TableShape::Rectangle => "Rectangle".to_string(),
                TableShape::Square => "Square".to_string(),
                TableShape::Circle => "Circle".to_string(),
            }),
            layout_x: t.layout_x,
            layout_y: t.layout_y,
            layout_width: t.layout_width,
            layout_height: t.layout_height,
            merged_with: t.merged_with.iter().map(|tn| tn.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TableListResponse {
    pub tables: Vec<TableResponse>,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergeResponse {
    pub cluster: Vec<String>,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnmergeResponse {
    pub removed: String,
    pub remaining: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferResponse {
    pub from_table: String,
    pub to_table: String,
    pub success: bool,
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct MergeRequest {
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TransferRequest {
    pub to_table: String,
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TableListQuery {
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub occupied: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use peacock_core::ids::{BranchName, RestaurantName, RoomName, TableName};
    use peacock_core::model::MergedWith;

    fn sample_table() -> Table {
        Table {
            name: TableName::from("T-01"),
            no_of_seats: 4,
            minimum_seating: 2,
            restaurant: RestaurantName::from("Test Restaurant"),
            restaurant_room: RoomName::from("Hall"),
            branch: BranchName::from("Main"),
            is_take_away: false,
            occupied: true,
            latest_invoice_time: None,
            table_shape: Some(TableShape::Rectangle),
            layout_x: 10.0,
            layout_y: 20.0,
            layout_width: 100.0,
            layout_height: 80.0,
            merged_with: MergedWith::parse(Some("T-02,T-03")),
        }
    }

    #[test]
    fn table_response_converts_from_domain_model() {
        let table = sample_table();
        let response = TableResponse::from(table.clone());

        assert_eq!(response.name, "T-01");
        assert_eq!(response.no_of_seats, 4);
        assert_eq!(response.restaurant_room, "Hall");
        assert!(response.occupied);
        assert_eq!(response.table_shape, Some("Rectangle".to_string()));
        assert_eq!(response.merged_with, vec!["T-02", "T-03"]);
    }

    #[test]
    fn table_response_handles_none_shape() {
        let mut table = sample_table();
        table.table_shape = None;

        let response = TableResponse::from(table);
        assert_eq!(response.table_shape, None);
    }

    #[test]
    fn table_response_handles_empty_merged_with() {
        let mut table = sample_table();
        table.merged_with = MergedWith::parse(None);

        let response = TableResponse::from(table);
        assert!(response.merged_with.is_empty());
    }

    #[test]
    fn merge_request_deserializes() {
        let json = r#"{"targets": ["T-02", "T-03"]}"#;
        let req: MergeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.targets, vec!["T-02", "T-03"]);
    }

    #[test]
    fn transfer_request_deserializes() {
        let json = r#"{"to_table": "T-05"}"#;
        let req: TransferRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.to_table, "T-05");
    }
}
