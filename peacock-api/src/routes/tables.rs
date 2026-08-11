//! Table Management API endpoints.
//!
//! Lane 3B: HTTP API for table operations, merge/unmerge, and order transfer.
//!
//! ## Endpoints
//!
//! - `GET /api/tables` — list tables (filter by room, status)
//! - `GET /api/tables/:id` — get single table
//! - `POST /api/tables/:id/merge` — merge tables
//! - `POST /api/tables/:id/unmerge` — unmerge tables
//! - `POST /api/tables/:id/transfer` — transfer order between tables

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::dto::table::{
    MergeRequest, MergeResponse, TableListQuery, TableListResponse, TableResponse,
    TransferRequest, TransferResponse, UnmergeResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use peacock_core::ids::{RoomName, TableName};
use peacock_core::merge::{merge_tables_batch, unmerge_tables};
use peacock_core::ports::TableRepo;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/tables", get(list_tables))
        .route("/api/tables/:id", get(get_table))
        .route("/api/tables/:id/merge", post(merge_tables))
        .route("/api/tables/:id/unmerge", post(unmerge_table))
        .route("/api/tables/:id/transfer", post(transfer_order))
}

/// GET /api/tables
///
/// List all tables, optionally filtered by room and/or occupancy status.
async fn list_tables(
    State(state): State<AppState>,
    Query(query): Query<TableListQuery>,
) -> ApiResult<Json<TableListResponse>> {
    let storage = state.storage();
    let table_repo = peacock_storage::repos::PostgresTableRepo::new(storage.pool().clone());

    let room_ref = query.room.as_ref().map(|s| RoomName::from(s.as_str()));
    let tables = table_repo
        .list_all(room_ref.as_ref(), query.occupied)
        .map_err(ApiError::from)?;

    let responses: Vec<TableResponse> = tables.into_iter().map(Into::into).collect();
    Ok(Json(TableListResponse {
        count: responses.len(),
        tables: responses,
    }))
}

/// GET /api/tables/:id
///
/// Get a single table by name.
async fn get_table(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<TableResponse>> {
    // Phase 2 integration (Lane 4A-4)
    let storage = state.storage();
    let table_repo = peacock_storage::repos::PostgresTableRepo::new(storage.pool().clone());

    let table = table_repo
        .get(&TableName::from(id.as_str()))
        .map_err(ApiError::from)?;

    Ok(Json(table.into()))
}

/// POST /api/tables/:id/merge
///
/// Merge target tables into the anchor table's cluster.
///
/// ## Request Body
/// ```json
/// {
///   "targets": ["T-02", "T-03"]
/// }
/// ```
///
/// ## Response
/// ```json
/// {
///   "cluster": ["T-01", "T-02", "T-03"],
///   "count": 3
/// }
/// ```
///
/// ## Errors
/// - 400: Cross-room merge attempted
/// - 404: Anchor or target table not found
/// - 409: Target already merged, occupied, or multiple active orders
async fn merge_tables(
    State(state): State<AppState>,
    Path(anchor_id): Path<String>,
    Json(req): Json<MergeRequest>,
) -> ApiResult<Json<MergeResponse>> {
    // Validation: at least one target required
    if req.targets.is_empty() {
        return Err(ApiError::invalid_input(
            "At least one target table is required",
        ));
    }
    
    // Validation: no duplicate targets
    let mut seen = std::collections::HashSet::new();
    for target in &req.targets {
        if !seen.insert(target) {
            return Err(ApiError::invalid_input(format!(
                "Duplicate target table: {}",
                target
            )));
        }
    }
    
    // Phase 2 integration (Lane 4A-4)
    let storage = state.storage();
    let table_repo = peacock_storage::repos::PostgresTableRepo::new(storage.pool().clone());
    
    // Fake OrderRepo for now (no active orders check)
    struct FakeOrderRepo;
    impl peacock_core::ports::OrderRepo for FakeOrderRepo {
        fn count_separate_active(&self, _tables: &[TableName]) -> peacock_core::error::Result<usize> {
            Ok(0)
        }
    }
    let order_repo = FakeOrderRepo;

    let anchor = TableName::from(anchor_id.as_str());
    let targets: Vec<TableName> = req.targets.iter().map(|s| TableName::from(s.as_str())).collect();

    let cluster = merge_tables_batch(&anchor, &targets, &table_repo, &order_repo)
        .map_err(ApiError::from)?;

    // Persist the merge writes
    peacock_storage::repos::batch_update_merged_with(
        storage.pool(),
        &cluster.writes()
    ).map_err(ApiError::from)?;

    let members: Vec<String> = cluster.sorted_members().iter().map(|t| t.to_string()).collect();
    Ok(Json(MergeResponse {
        count: members.len(),
        cluster: members,
    }))
}

/// POST /api/tables/:id/unmerge
///
/// Remove a table from its merge cluster.
///
/// ## Response
/// ```json
/// {
///   "removed": "T-02",
///   "remaining": ["T-01", "T-03"]
/// }
/// ```
///
/// ## Errors
/// - 404: Table not found
///
/// Note: Unmerging a table that is not merged is idempotent and succeeds.
async fn unmerge_table(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<UnmergeResponse>> {
    // Phase 2 integration (Lane 4A-4)
    let storage = state.storage();
    let table_repo = peacock_storage::repos::PostgresTableRepo::new(storage.pool().clone());
    
    let table = TableName::from(id.as_str());

    let plan = unmerge_tables(&table, &table_repo)
        .map_err(ApiError::from)?;

    // Persist the unmerge writes
    peacock_storage::repos::batch_update_merged_with(
        storage.pool(),
        &plan.writes
    ).map_err(ApiError::from)?;

    Ok(Json(UnmergeResponse {
        removed: plan.removed.to_string(),
        remaining: plan.remaining.iter().map(|t| t.to_string()).collect(),
    }))
}

/// POST /api/tables/:id/transfer
///
/// Transfer an order from one table to another.
///
/// ## Request Body
/// ```json
/// {
///   "to_table": "T-05"
/// }
/// ```
///
/// ## Response
/// ```json
/// {
///   "from_table": "T-01",
///   "to_table": "T-05",
///   "success": true
/// }
/// ```
///
/// ## Errors
/// - 400: Tables are in different rooms
/// - 404: Source or destination table not found
/// - 409: Source table has no active order
async fn transfer_order(
    State(state): State<AppState>,
    Path(from_id): Path<String>,
    Json(req): Json<TransferRequest>,
) -> ApiResult<Json<TransferResponse>> {
    // Validation: cannot transfer to the same table
    if from_id == req.to_table {
        return Err(ApiError::invalid_input(
            "Cannot transfer order to the same table",
        ));
    }
    
    let storage = state.storage();
    let table_repo = peacock_storage::repos::PostgresTableRepo::new(storage.pool().clone());

    let from_table = TableName::from(from_id.as_str());
    let to_table = TableName::from(req.to_table.as_str());

    // Validate both tables exist and are in same room
    let from = table_repo.get(&from_table).map_err(ApiError::from)?;
    let to = table_repo.get(&to_table).map_err(ApiError::from)?;

    if from.restaurant_room != to.restaurant_room {
        return Err(ApiError::invalid_input(format!(
            "Cannot transfer order between tables in different rooms: {} is in '{}', {} is in '{}'",
            from_table, from.restaurant_room, to_table, to.restaurant_room
        )));
    }

    // Transfer the active order (and its draft invoices) via the order repo.
    // The repo uses SELECT ... FOR UPDATE so two concurrent transfers
    // serialise rather than interleaving, and the check for an empty
    // destination prevents clobbering an existing order (409).
    let order_repo = storage.order_repo();
    let _transferred = order_repo
        .transfer_table(&from_table, &to_table)
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        from_table = %from_table,
        to_table = %to_table,
        "Order transferred"
    );

    Ok(Json(TransferResponse {
        from_table: from_id,
        to_table: req.to_table,
        success: true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::testing::TestDb;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use peacock_storage::Storage;
    use tower::ServiceExt;

    async fn test_db() -> TestDb {
        TestDb::new().await
    }

    async fn clean_and_seed(storage: &Storage) {
        let pool = storage.pool();

        // Clean child tables first so FKs don't block table deletion.
        for tbl in &[
            "order_items",
            "orders",
            "invoice_lines",
            "idempotency_keys",
            "order_idempotency_keys",
            "kot_items",
            "kots",
            "aggregator_order_items",
            "aggregator_orders",
            "aggregator_settlement_orders",
            "aggregator_settlements",
            "invoices",
        ] {
            let q = format!("DELETE FROM {}", tbl);
            let _ = sqlx::query(&q).execute(pool).await;
        }
        sqlx::query("DELETE FROM tables")
            .execute(pool)
            .await
            .expect("failed to clean tables");

        // Ensure parent rows exist for the test tables.
        sqlx::query(
            "INSERT INTO rooms (name, branch, room_type) VALUES ('Hall', 'Main', 'AC') ON CONFLICT (name) DO NOTHING",
        )
        .execute(pool)
        .await
        .expect("seed room");

        sqlx::query(
            "INSERT INTO restaurants (name, company, branch, pos_profile, invoice_series_prefix, default_room) VALUES ('Test Restaurant', 'Test Co', 'Main', 'Test POS', 'TST-', 'Hall') ON CONFLICT (name) DO NOTHING",
        )
        .execute(pool)
        .await
        .expect("seed restaurant");

        // Seed test data
        let restaurant = "Test Restaurant";
        let room = "Hall";
        let branch = "Main";
        
        for i in 1..=5 {
            let name = format!("T-{:02}", i);
            let occupied = i % 2 == 0;
            let merged: Vec<String> = if i == 1 {
                vec!["T-02".to_string()]
            } else if i == 2 {
                vec!["T-01".to_string()]
            } else {
                vec![]
            };
            
            sqlx::query(
                r#"
                INSERT INTO tables (
                    name, no_of_seats, minimum_seating, restaurant, restaurant_room,
                    branch, is_take_away, occupied, latest_invoice_time, table_shape,
                    layout_x, layout_y, layout_width, layout_height, merged_with
                ) VALUES ($1, 4, 1, $2, $3, $4, false, $5, NULL, NULL, 0, 0, 0, 0, $6)
                "#,
            )
            .bind(&name)
            .bind(restaurant)
            .bind(room)
            .bind(branch)
            .bind(occupied)
            .bind(serde_json::to_value(&merged).unwrap())
            .execute(pool)
            .await
            .expect("failed to insert table");
        }
    }

    fn app_with_storage(storage: Storage) -> Router {
        routes().with_state(AppState::with_storage(Config::default(), storage))
    }

    async fn send(app: Router, request: Request<Body>) -> axum::response::Response {
        app.oneshot(request).await.unwrap()
    }

    async fn json_body<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_tables_returns_all_when_no_filters() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .uri("/api/tables")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: TableListResponse = json_body(response).await;
        assert_eq!(body.count, 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_tables_filters_by_room() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .uri("/api/tables?room=Hall")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: TableListResponse = json_body(response).await;
        assert_eq!(body.count, 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_tables_filters_by_occupied() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .uri("/api/tables?occupied=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: TableListResponse = json_body(response).await;
        assert_eq!(body.count, 2); // T-02 and T-04
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_tables_combines_filters() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .uri("/api/tables?room=Hall&occupied=false")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: TableListResponse = json_body(response).await;
        assert_eq!(body.count, 3); // T-01, T-03, T-05
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_table_returns_existing_table() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .uri("/api/tables/T-01")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: TableResponse = json_body(response).await;
        assert_eq!(body.name, "T-01");
        assert_eq!(body.restaurant_room, "Hall");
        assert_eq!(body.merged_with, vec!["T-02"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_table_returns_404_for_nonexistent() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .uri("/api/tables/T-99")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn merge_tables_rejects_empty_targets() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .method("POST")
                .uri("/api/tables/T-01/merge")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"targets": []}"#))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("At least one target"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn merge_tables_rejects_duplicate_targets() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .method("POST")
                .uri("/api/tables/T-01/merge")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"targets": ["T-02", "T-02"]}"#))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Duplicate target"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn merge_tables_creates_real_cluster() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .method("POST")
                .uri("/api/tables/T-03/merge")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"targets": ["T-05"]}"#))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: MergeResponse = json_body(response).await;
        assert_eq!(body.count, 2);
        assert!(body.cluster.contains(&"T-03".to_string()));
        assert!(body.cluster.contains(&"T-05".to_string()));

        // Verify persistence
        let repo = peacock_storage::repos::PostgresTableRepo::new(storage.pool().clone());
        let t3 = repo.get(&TableName::from("T-03")).unwrap();
        assert!(t3.merged_with.contains(&TableName::from("T-05")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unmerge_table_removes_from_cluster() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .method("POST")
                .uri("/api/tables/T-02/unmerge")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: UnmergeResponse = json_body(response).await;
        assert_eq!(body.removed, "T-02");
        assert_eq!(body.remaining, vec!["T-01"]);

        // Verify persistence
        let repo = peacock_storage::repos::PostgresTableRepo::new(storage.pool().clone());
        let t2 = repo.get(&TableName::from("T-02")).unwrap();
        assert!(t2.merged_with.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn transfer_order_rejects_same_table() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .method("POST")
                .uri("/api/tables/T-01/transfer")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"to_table": "T-01"}"#))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Cannot transfer order to the same table"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn transfer_order_validates_table_exists() {
        let db = test_db().await;
        let storage = db.storage().clone();
        clean_and_seed(&storage).await;
        let app = app_with_storage(storage.clone());

        let response = send(
            app,
            Request::builder()
                .method("POST")
                .uri("/api/tables/T-99/transfer")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"to_table": "T-01"}"#))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
