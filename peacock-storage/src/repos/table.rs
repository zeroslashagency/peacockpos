//! PostgreSQL implementation of `TableRepo`.
//!
//! Implements the BFS cluster traversal from `peacock_core::merge` with real storage,
//! handling `merged_with` as a JSONB array per the 001_core_tables.sql schema.

use crate::error::{StorageError, StorageResult};
use crate::repos::blocking::block_on;
use peacock_core::error::{Error, Result};
use peacock_core::ids::{RoomName, TableName};
use peacock_core::model::{MergedWith, Table, TableShape};
use peacock_core::ports::TableRepo;
use sqlx::{PgPool, Row};

/// PostgreSQL implementation of `TableRepo`.
#[derive(Clone)]
pub struct PostgresTableRepo {
    pool: PgPool,
}

impl PostgresTableRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl TableRepo for PostgresTableRepo {
    fn list_all(&self, room: Option<&RoomName>, occupied: Option<bool>) -> Result<Vec<Table>> {
        let rows = block_on(async {
                let mut query = String::from(
                    r#"
                    SELECT
                        name,
                        no_of_seats,
                        minimum_seating,
                        restaurant,
                        restaurant_room,
                        branch,
                        is_take_away,
                        occupied,
                        latest_invoice_time,
                        table_shape,
                        layout_x,
                        layout_y,
                        layout_width,
                        layout_height,
                        merged_with
                    FROM tables
                    WHERE deleted_at IS NULL
                    "#
                );

                let mut conditions: Vec<String> = vec![];
                if room.is_some() {
                    conditions.push("restaurant_room = $1".to_string());
                }
                if occupied.is_some() {
                    let param_idx = if room.is_some() { 2 } else { 1 };
                    conditions.push(format!("occupied = ${}", param_idx));
                }

                if !conditions.is_empty() {
                    query.push_str(" AND ");
                    query.push_str(&conditions.join(" AND "));
                }

                let mut q = sqlx::query(&query);
                if let Some(r) = room {
                    q = q.bind(r.as_str());
                }
                if let Some(o) = occupied {
                    q = q.bind(o);
                }

                q.fetch_all(&self.pool).await
            })
            .map_err(StorageError::from)?;

        rows.into_iter()
            .map(|row| {
                let name: String = row.try_get("name").map_err(StorageError::from)?;
                let no_of_seats: i32 = row.try_get("no_of_seats").map_err(StorageError::from)?;
                let minimum_seating: i32 = row.try_get("minimum_seating").map_err(StorageError::from)?;
                let restaurant: String = row.try_get("restaurant").map_err(StorageError::from)?;
                let restaurant_room: String = row.try_get("restaurant_room").map_err(StorageError::from)?;
                let branch: String = row.try_get("branch").map_err(StorageError::from)?;
                let is_take_away: bool = row.try_get("is_take_away").map_err(StorageError::from)?;
                let occupied: bool = row.try_get("occupied").map_err(StorageError::from)?;
                let latest_invoice_time: Option<chrono::NaiveTime> = row.try_get("latest_invoice_time").map_err(StorageError::from)?;
                let table_shape_str: Option<String> = row.try_get("table_shape").map_err(StorageError::from)?;
                let layout_x: f64 = row.try_get("layout_x").map_err(StorageError::from)?;
                let layout_y: f64 = row.try_get("layout_y").map_err(StorageError::from)?;
                let layout_width: f64 = row.try_get("layout_width").map_err(StorageError::from)?;
                let layout_height: f64 = row.try_get("layout_height").map_err(StorageError::from)?;
                let merged_with_json: serde_json::Value = row.try_get("merged_with").map_err(StorageError::from)?;

                let table_shape = table_shape_str
                    .as_deref()
                    .map(parse_table_shape)
                    .transpose()
                    .map_err(StorageError::from)?;

                let merged_with: Vec<String> = serde_json::from_value(merged_with_json)
                    .map_err(|e| StorageError::Sqlx(sqlx::Error::Decode(Box::new(e))))?;

                Ok(Table {
                    name: TableName::from(name.as_str()),
                    no_of_seats,
                    minimum_seating,
                    restaurant: restaurant.as_str().into(),
                    restaurant_room: restaurant_room.as_str().into(),
                    branch: branch.as_str().into(),
                    is_take_away,
                    occupied,
                    latest_invoice_time,
                    table_shape,
                    layout_x,
                    layout_y,
                    layout_width,
                    layout_height,
                    merged_with: MergedWith::parse(Some(
                        &merged_with.join(","),
                    )),
                })
            })
            .collect::<StorageResult<Vec<Table>>>()
            .map_err(StorageError::into)
    }

    fn list_by_room(&self, room: &RoomName) -> Result<Vec<Table>> {
        let room_str = room.as_str();

        // Block on the async query. The port trait is deliberately synchronous
        // (peacock-storage/src/lib.rs:9-12), so we handle the async boundary here.
        let rows = block_on(async {
                sqlx::query(
                    r#"
                    SELECT
                        name,
                        no_of_seats,
                        minimum_seating,
                        restaurant,
                        restaurant_room,
                        branch,
                        is_take_away,
                        occupied,
                        latest_invoice_time,
                        table_shape,
                        layout_x,
                        layout_y,
                        layout_width,
                        layout_height,
                        merged_with
                    FROM tables
                    WHERE restaurant_room = $1
                      AND deleted_at IS NULL
                    "#,
                )
                .bind(room_str)
                .fetch_all(&self.pool)
                .await
            })
            .map_err(StorageError::from)?;

        rows.into_iter()
            .map(|row| {
                let name: String = row.try_get("name").map_err(StorageError::from)?;
                let no_of_seats: i32 = row.try_get("no_of_seats").map_err(StorageError::from)?;
                let minimum_seating: i32 = row.try_get("minimum_seating").map_err(StorageError::from)?;
                let restaurant: String = row.try_get("restaurant").map_err(StorageError::from)?;
                let restaurant_room: String = row.try_get("restaurant_room").map_err(StorageError::from)?;
                let branch: String = row.try_get("branch").map_err(StorageError::from)?;
                let is_take_away: bool = row.try_get("is_take_away").map_err(StorageError::from)?;
                let occupied: bool = row.try_get("occupied").map_err(StorageError::from)?;
                let latest_invoice_time: Option<chrono::NaiveTime> = row.try_get("latest_invoice_time").map_err(StorageError::from)?;
                let table_shape_str: Option<String> = row.try_get("table_shape").map_err(StorageError::from)?;
                let layout_x: f64 = row.try_get("layout_x").map_err(StorageError::from)?;
                let layout_y: f64 = row.try_get("layout_y").map_err(StorageError::from)?;
                let layout_width: f64 = row.try_get("layout_width").map_err(StorageError::from)?;
                let layout_height: f64 = row.try_get("layout_height").map_err(StorageError::from)?;
                let merged_with_json: serde_json::Value = row.try_get("merged_with").map_err(StorageError::from)?;

                let table_shape = table_shape_str
                    .as_deref()
                    .map(parse_table_shape)
                    .transpose()
                    .map_err(StorageError::from)?;

                // merged_with is stored as JSONB array ["T-01","T-02"]
                let merged_with: Vec<String> = serde_json::from_value(merged_with_json)
                    .map_err(|e| StorageError::Sqlx(sqlx::Error::Decode(Box::new(e))))?;

                Ok(Table {
                    name: TableName::from(name.as_str()),
                    no_of_seats,
                    minimum_seating,
                    restaurant: restaurant.as_str().into(),
                    restaurant_room: restaurant_room.as_str().into(),
                    branch: branch.as_str().into(),
                    is_take_away,
                    occupied,
                    latest_invoice_time,
                    table_shape,
                    layout_x,
                    layout_y,
                    layout_width,
                    layout_height,
                    merged_with: MergedWith::parse(Some(
                        &merged_with.join(","),
                    )),
                })
            })
            .collect::<StorageResult<Vec<Table>>>()
            .map_err(StorageError::into)
    }

    fn get(&self, name: &TableName) -> Result<Table> {
        let name_str = name.as_str();

        let row = block_on(async {
                sqlx::query(
                    r#"
                    SELECT
                        name,
                        no_of_seats,
                        minimum_seating,
                        restaurant,
                        restaurant_room,
                        branch,
                        is_take_away,
                        occupied,
                        latest_invoice_time,
                        table_shape,
                        layout_x,
                        layout_y,
                        layout_width,
                        layout_height,
                        merged_with
                    FROM tables
                    WHERE name = $1
                      AND deleted_at IS NULL
                    "#,
                )
                .bind(name_str)
                .fetch_optional(&self.pool)
                .await
            })
            .map_err(StorageError::from)?
            .ok_or_else(|| Error::TableNotFound(name.clone()))?;

        let name_val: String = row.try_get("name").map_err(StorageError::from)?;
        let no_of_seats: i32 = row.try_get("no_of_seats").map_err(StorageError::from)?;
        let minimum_seating: i32 = row.try_get("minimum_seating").map_err(StorageError::from)?;
        let restaurant: String = row.try_get("restaurant").map_err(StorageError::from)?;
        let restaurant_room: String = row.try_get("restaurant_room").map_err(StorageError::from)?;
        let branch: String = row.try_get("branch").map_err(StorageError::from)?;
        let is_take_away: bool = row.try_get("is_take_away").map_err(StorageError::from)?;
        let occupied: bool = row.try_get("occupied").map_err(StorageError::from)?;
        let latest_invoice_time: Option<chrono::NaiveTime> = row.try_get("latest_invoice_time").map_err(StorageError::from)?;
        let table_shape_str: Option<String> = row.try_get("table_shape").map_err(StorageError::from)?;
        let layout_x: f64 = row.try_get("layout_x").map_err(StorageError::from)?;
        let layout_y: f64 = row.try_get("layout_y").map_err(StorageError::from)?;
        let layout_width: f64 = row.try_get("layout_width").map_err(StorageError::from)?;
        let layout_height: f64 = row.try_get("layout_height").map_err(StorageError::from)?;
        let merged_with_json: serde_json::Value = row.try_get("merged_with").map_err(StorageError::from)?;

        let table_shape = table_shape_str
            .as_deref()
            .map(parse_table_shape)
            .transpose()
            .map_err(StorageError::from)?;

        let merged_with: Vec<String> = serde_json::from_value(merged_with_json)
            .map_err(|e| StorageError::Sqlx(sqlx::Error::Decode(Box::new(e))))?;

        Ok(Table {
            name: TableName::from(name_val.as_str()),
            no_of_seats,
            minimum_seating,
            restaurant: restaurant.as_str().into(),
            restaurant_room: restaurant_room.as_str().into(),
            branch: branch.as_str().into(),
            is_take_away,
            occupied,
            latest_invoice_time,
            table_shape,
            layout_x,
            layout_y,
            layout_width,
            layout_height,
            merged_with: MergedWith::parse(Some(&merged_with.join(","))),
        })
    }
}

fn parse_table_shape(s: &str) -> std::result::Result<TableShape, String> {
    match s {
        "Rectangle" => Ok(TableShape::Rectangle),
        "Square" => Ok(TableShape::Square),
        "Circle" => Ok(TableShape::Circle),
        _ => Err(format!("unknown table shape: {}", s)),
    }
}

/// Helper to update `merged_with` for a table.
///
/// Used by merge/unmerge operations. Serializes `Vec<TableName>` into JSONB array.
pub fn update_merged_with(
    pool: &PgPool,
    table: &TableName,
    merged_with: &MergedWith,
) -> Result<()> {
    let table_str = table.as_str();
    let csv = merged_with.to_csv();
    
    // Convert CSV back to Vec<String> for JSONB serialization
    let vec: Vec<String> = if csv.is_empty() {
        vec![]
    } else {
        csv.split(',').map(|s| s.trim().to_owned()).collect()
    };
    
    let json_value = serde_json::to_value(&vec)
        .map_err(|e| StorageError::Sqlx(sqlx::Error::Decode(Box::new(e))))?;

    block_on(async {
            sqlx::query(
                r#"
                UPDATE tables
                SET merged_with = $2
                WHERE name = $1
                  AND deleted_at IS NULL
                "#,
            )
            .bind(table_str)
            .bind(json_value)
            .execute(pool)
            .await
        })
        .map_err(StorageError::from)?;

    Ok(())
}

/// Batch update for merge cluster writes.
///
/// Performs all writes in a single transaction to ensure atomicity.
pub fn batch_update_merged_with(
    pool: &PgPool,
    writes: &[(TableName, MergedWith)],
) -> Result<()> {
    block_on(async {
        let mut tx = pool
            .begin()
            .await
            .map_err(StorageError::from)?;

        for (table, merged_with) in writes {
            let table_str = table.as_str();
            let csv = merged_with.to_csv();

            let vec: Vec<String> = if csv.is_empty() {
                vec![]
            } else {
                csv.split(',').map(|s| s.trim().to_owned()).collect()
            };

            let json_value = serde_json::to_value(&vec)
                .map_err(|e| StorageError::Sqlx(sqlx::Error::Decode(Box::new(e))))?;

            sqlx::query(
                r#"
                UPDATE tables
                SET merged_with = $2
                WHERE name = $1
                  AND deleted_at IS NULL
                "#,
            )
            .bind(table_str)
            .bind(json_value)
            .execute(&mut *tx)
            .await
            .map_err(StorageError::from)?;
        }

        tx.commit()
            .await
            .map_err(StorageError::from)?;

        Ok(())
    })
}

impl From<String> for StorageError {
    fn from(s: String) -> Self {
        StorageError::Sqlx(sqlx::Error::Decode(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            s,
        ))))
    }
}

impl From<StorageError> for Error {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::Domain(domain_err) => domain_err,
            other => Error::NonNumericData {
                entity: "storage".to_string(),
                field: "unknown".to_string(),
                raw: other.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peacock_core::merge::{get_merge_cluster, merge_tables_batch, unmerge_tables};
    use peacock_core::ports::OrderRepo;
    use sqlx::postgres::PgPoolOptions;

    // Fake OrderRepo for testing merge operations
    struct FakeOrders {
        active: Vec<TableName>,
    }

    impl FakeOrders {
        fn none() -> Self {
            FakeOrders { active: vec![] }
        }

        #[allow(dead_code)]
        fn on(names: &[&str]) -> Self {
            FakeOrders {
                active: names.iter().map(|n| TableName::from(*n)).collect(),
            }
        }
    }

    impl OrderRepo for FakeOrders {
        fn count_separate_active(&self, tables: &[TableName]) -> Result<usize> {
            Ok(tables.iter().filter(|t| self.active.contains(t)).count())
        }
    }

    async fn setup_test_pool() -> PgPool {
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://peacock:peacock@localhost/peacock_test".to_string());

        PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("failed to connect to test database")
    }

    async fn clean_tables(pool: &PgPool) {
        sqlx::query("DELETE FROM tables")
            .execute(pool)
            .await
            .expect("failed to clean tables");
    }

    async fn insert_table(
        pool: &PgPool,
        name: &str,
        room: &str,
        occupied: bool,
        merged_with: &[&str],
    ) {
        let restaurant = "Test Restaurant";
        let branch = "Test Branch";
        let merged_json = serde_json::to_value(merged_with).unwrap();

        // Ensure FK parents exist (rooms + restaurants) — storage tests run isolated
        let _ = sqlx::query(
            "INSERT INTO restaurants (name, company, branch, invoice_series_prefix) VALUES ($1, $2, $3, 'INV-') ON CONFLICT (name) DO NOTHING",
        )
        .bind(restaurant)
        .bind("Test Company")
        .bind(branch)
        .execute(pool)
        .await;
        let _ = sqlx::query(
            "INSERT INTO rooms (name, branch) VALUES ($1, $2) ON CONFLICT (name) DO NOTHING",
        )
        .bind(room)
        .bind(branch)
        .execute(pool)
        .await;

        sqlx::query(
            r#"
            INSERT INTO tables (
                name, no_of_seats, minimum_seating, restaurant,
                restaurant_room, branch, is_take_away, occupied,
                latest_invoice_time, table_shape, layout_x, layout_y,
                layout_width, layout_height, merged_with
            ) VALUES ($1, 4, 1, $2, $3, $4, false, $5, NULL, NULL, 0, 0, 0, 0, $6)
            "#,
        )
        .bind(name)
        .bind(restaurant)
        .bind(room)
        .bind(branch)
        .bind(occupied)
        .bind(merged_json)
        .execute(pool)
        .await
        .expect("failed to insert table");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_get_table() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        insert_table(&pool, "T-01", "Hall", false, &[]).await;

        let repo = PostgresTableRepo::new(pool.clone());
        let table = repo.get(&TableName::from("T-01")).unwrap();

        assert_eq!(table.name.as_str(), "T-01");
        assert_eq!(table.restaurant_room.as_str(), "Hall");
        assert!(!table.occupied);
        assert!(table.merged_with.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_get_nonexistent_table() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        let repo = PostgresTableRepo::new(pool.clone());
        let result = repo.get(&TableName::from("T-99"));

        assert!(matches!(result, Err(Error::TableNotFound(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_list_by_room() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        insert_table(&pool, "T-01", "Hall", false, &[]).await;
        insert_table(&pool, "T-02", "Hall", false, &[]).await;
        insert_table(&pool, "T-50", "Patio", false, &[]).await;

        let repo = PostgresTableRepo::new(pool.clone());
        let tables = repo.list_by_room(&RoomName::from("Hall")).unwrap();

        assert_eq!(tables.len(), 2);
        let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"T-01"));
        assert!(names.contains(&"T-02"));
        assert!(!names.contains(&"T-50"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_merged_with_round_trip() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        insert_table(&pool, "T-01", "Hall", false, &["T-02", "T-03"]).await;

        let repo = PostgresTableRepo::new(pool.clone());
        let table = repo.get(&TableName::from("T-01")).unwrap();

        assert_eq!(table.merged_with.to_csv(), "T-02,T-03");
        assert!(table.merged_with.contains(&TableName::from("T-02")));
        assert!(table.merged_with.contains(&TableName::from("T-03")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_update_merged_with() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        insert_table(&pool, "T-01", "Hall", false, &[]).await;

        let repo = PostgresTableRepo::new(pool.clone());
        let merged = MergedWith::parse(Some("T-02,T-03"));

        update_merged_with(&pool, &TableName::from("T-01"), &merged).unwrap();

        let table = repo.get(&TableName::from("T-01")).unwrap();
        assert_eq!(table.merged_with.to_csv(), "T-02,T-03");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bfs_cluster_single_table() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        insert_table(&pool, "T-01", "Hall", false, &[]).await;
        insert_table(&pool, "T-02", "Hall", false, &[]).await;

        let repo = PostgresTableRepo::new(pool.clone());
        let cluster = get_merge_cluster(
            &TableName::from("T-01"),
            &RoomName::from("Hall"),
            &repo,
        )
        .unwrap();

        assert_eq!(cluster.members().len(), 1);
        assert!(cluster.contains(&TableName::from("T-01")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bfs_cluster_two_tables() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        insert_table(&pool, "T-01", "Hall", false, &["T-02"]).await;
        insert_table(&pool, "T-02", "Hall", false, &["T-01"]).await;

        let repo = PostgresTableRepo::new(pool.clone());
        let cluster = get_merge_cluster(
            &TableName::from("T-01"),
            &RoomName::from("Hall"),
            &repo,
        )
        .unwrap();

        assert_eq!(cluster.members().len(), 2);
        assert!(cluster.contains(&TableName::from("T-01")));
        assert!(cluster.contains(&TableName::from("T-02")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_bfs_cluster_transitive() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        insert_table(&pool, "T-01", "Hall", false, &["T-02"]).await;
        insert_table(&pool, "T-02", "Hall", false, &["T-01", "T-03"]).await;
        insert_table(&pool, "T-03", "Hall", false, &["T-02"]).await;

        let repo = PostgresTableRepo::new(pool.clone());
        let cluster = get_merge_cluster(
            &TableName::from("T-01"),
            &RoomName::from("Hall"),
            &repo,
        )
        .unwrap();

        let mut sorted = cluster.sorted_members();
        sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));

        assert_eq!(sorted.len(), 3);
        assert!(cluster.contains(&TableName::from("T-01")));
        assert!(cluster.contains(&TableName::from("T-02")));
        assert!(cluster.contains(&TableName::from("T-03")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_cross_room_isolation() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        insert_table(&pool, "T-01", "Hall", false, &["T-02", "T-50"]).await;
        insert_table(&pool, "T-02", "Hall", false, &["T-01"]).await;
        insert_table(&pool, "T-50", "Patio", false, &["T-01"]).await;

        let repo = PostgresTableRepo::new(pool.clone());
        let cluster = get_merge_cluster(
            &TableName::from("T-01"),
            &RoomName::from("Hall"),
            &repo,
        )
        .unwrap();

        // T-50 is in Patio, not included in Hall cluster
        assert_eq!(cluster.members().len(), 2);
        assert!(cluster.contains(&TableName::from("T-01")));
        assert!(cluster.contains(&TableName::from("T-02")));
        assert!(!cluster.contains(&TableName::from("T-50")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_merge_tables_batch() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        insert_table(&pool, "T-01", "Hall", false, &[]).await;
        insert_table(&pool, "T-02", "Hall", false, &[]).await;

        let repo = PostgresTableRepo::new(pool.clone());
        let cluster = merge_tables_batch(
            &TableName::from("T-01"),
            &[TableName::from("T-02")],
            &repo,
            &FakeOrders::none(),
        )
        .unwrap();

        assert_eq!(cluster.members().len(), 2);

        // Persist the writes
        batch_update_merged_with(&pool, &cluster.writes()).unwrap();

        // Verify
        let t1 = repo.get(&TableName::from("T-01")).unwrap();
        let t2 = repo.get(&TableName::from("T-02")).unwrap();

        assert_eq!(t1.merged_with.to_csv(), "T-02");
        assert_eq!(t2.merged_with.to_csv(), "T-01");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_unmerge_tables() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        insert_table(&pool, "T-01", "Hall", false, &["T-02", "T-03"]).await;
        insert_table(&pool, "T-02", "Hall", false, &["T-01", "T-03"]).await;
        insert_table(&pool, "T-03", "Hall", false, &["T-01", "T-02"]).await;

        let repo = PostgresTableRepo::new(pool.clone());
        let plan = unmerge_tables(&TableName::from("T-02"), &repo).unwrap();

        assert_eq!(plan.removed.as_str(), "T-02");
        assert_eq!(plan.remaining.len(), 2);

        // Persist the writes
        batch_update_merged_with(&pool, &plan.writes).unwrap();

        // Verify
        let t1 = repo.get(&TableName::from("T-01")).unwrap();
        let t2 = repo.get(&TableName::from("T-02")).unwrap();
        let t3 = repo.get(&TableName::from("T-03")).unwrap();

        assert_eq!(t1.merged_with.to_csv(), "T-03");
        assert_eq!(t2.merged_with.to_csv(), "");
        assert_eq!(t3.merged_with.to_csv(), "T-01");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_concurrent_updates() {
        let pool = setup_test_pool().await;
        clean_tables(&pool).await;

        // Insert 10 tables
        for i in 1..=10 {
            insert_table(&pool, &format!("T-{:02}", i), "Hall", false, &[]).await;
        }

        // Spawn 5 concurrent merge operations (pairing 10 tables)
        let handles: Vec<_> = (1..=5)
            .map(|i| {
                let pool_clone = pool.clone();
                tokio::spawn(async move {
                    let t1 = format!("T-{:02}", i * 2 - 1);
                    let t2 = format!("T-{:02}", i * 2);

                    let writes = vec![
                        (TableName::from(t1.as_str()), MergedWith::parse(Some(&t2))),
                        (TableName::from(t2.as_str()), MergedWith::parse(Some(&t1))),
                    ];

                    batch_update_merged_with(&pool_clone, &writes).unwrap();
                })
            })
            .collect();

        // Wait for all to complete
        for handle in handles {
            handle.await.unwrap();
        }

        // Verify all merges succeeded
        let repo = PostgresTableRepo::new(pool.clone());
        for i in 1..=5 {
            let t1 = repo
                .get(&TableName::from(format!("T-{:02}", i * 2 - 1).as_str()))
                .unwrap();
            let t2 = repo
                .get(&TableName::from(format!("T-{:02}", i * 2).as_str()))
                .unwrap();

            assert!(!t1.merged_with.is_empty());
            assert!(!t2.merged_with.is_empty());
        }
    }
}
