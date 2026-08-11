//! Aggregator order repository — Lane W1-F.
//!
//! Implements storage for third-party delivery platform (Swiggy/Zomato) webhook orders.
//! Follows the exact patterns from `invoice.rs` and `table.rs`:
//! - `block_on` for the async boundary
//! - `to_domain_error` mapping
//! - `NUMERIC(18,6)` → `rust_decimal::Decimal` for money
//! - Idempotent on aggregator's own order ID

use crate::error::{StorageError, StorageResult};
use crate::repos::blocking::block_on;
use peacock_core::error::{Error as DomainError, Result as DomainResult};
use peacock_core::ids::InvoiceName;
use peacock_core::money::Money;
use rust_decimal::Decimal;
use sqlx::types::chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};

/// Status enum mapping to `aggregator_order_status` in SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregatorOrderStatus {
    Pending,
    Accepted,
    Rejected,
    Completed,
}

impl AggregatorOrderStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AggregatorOrderStatus::Pending => "Pending",
            AggregatorOrderStatus::Accepted => "Accepted",
            AggregatorOrderStatus::Rejected => "Rejected",
            AggregatorOrderStatus::Completed => "Completed",
        }
    }

    pub fn parse(s: &str) -> StorageResult<Self> {
        match s {
            "Pending" => Ok(AggregatorOrderStatus::Pending),
            "Accepted" => Ok(AggregatorOrderStatus::Accepted),
            "Rejected" => Ok(AggregatorOrderStatus::Rejected),
            "Completed" => Ok(AggregatorOrderStatus::Completed),
            other => Err(StorageError::Constraint {
                table: "aggregator_orders".to_owned(),
                constraint: "aggregator_order_status".to_owned(),
                message: format!("unknown aggregator order status {other:?}"),
            }),
        }
    }
}

/// One line item from an aggregator order.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregatorOrderItem {
    pub item_code: String,
    pub item_name: String,
    pub quantity: Decimal,
    pub rate: Money,
    pub special_instructions: Option<String>,
}

/// Input for inserting a new aggregator order.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAggregatorOrder {
    pub aggregator_order_id: String,
    pub platform: String,
    pub customer_name: String,
    pub customer_phone: Option<String>,
    pub total: Money,
    pub ordered_at: DateTime<Utc>,
    pub instructions: Option<String>,
    pub items: Vec<AggregatorOrderItem>,
}

/// A stored aggregator order with its items.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredAggregatorOrder {
    pub id: String,
    pub aggregator_order_id: String,
    pub platform: String,
    pub customer_name: String,
    pub customer_phone: Option<String>,
    pub total: Money,
    pub ordered_at: DateTime<Utc>,
    pub status: AggregatorOrderStatus,
    pub internal_order_id: Option<i64>,
    pub internal_invoice_id: Option<InvoiceName>,
    pub instructions: Option<String>,
    pub reject_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub items: Vec<StoredAggregatorOrderItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredAggregatorOrderItem {
    pub id: i64,
    pub item_code: String,
    pub item_name: String,
    pub quantity: Decimal,
    pub rate: Money,
    pub special_instructions: Option<String>,
}

/// Settlement for reconciliation.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSettlement {
    pub id: String,
    pub platform: String,
    pub settlement_date: NaiveDate,
    pub total_orders: i32,
    pub gross_amount: Money,
    pub commission: Money,
    pub net_amount: Money,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// PostgreSQL aggregator order repository.
#[derive(Clone)]
pub struct PgAggregatorRepo {
    pool: PgPool,
}

impl PgAggregatorRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert an aggregator order with its items in one transaction.
    /// Idempotent on `aggregator_order_id`: replaying the same webhook returns the existing order.
    pub fn insert_order(&self, order: &NewAggregatorOrder) -> DomainResult<String> {
        block_on(async { self.insert_order_async(order).await })
            .map_err(crate::repos::to_domain_error)
            .map(|stored| stored.id)
    }

    async fn insert_order_async(
        &self,
        order: &NewAggregatorOrder,
    ) -> StorageResult<StoredAggregatorOrder> {
        let mut tx = self.pool.begin().await?;

        // Check for existing order (idempotency)
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM aggregator_orders WHERE aggregator_order_id = $1",
        )
        .bind(&order.aggregator_order_id)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(existing_id) = existing {
            // Replay: load and return the existing order
            let stored = load_order(&mut tx, &existing_id).await?;
            tx.commit().await?;
            return Ok(stored);
        }

        // Generate internal ID
        let internal_id = format!(
            "AGG-{}-{}",
            order.platform.to_uppercase(),
            order.aggregator_order_id
        );

        // Insert the order
        sqlx::query(
            r#"
            INSERT INTO aggregator_orders (
                id, aggregator_order_id, platform, customer_name, customer_phone,
                total, ordered_at, status, instructions
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, 'Pending', $8)
            "#,
        )
        .bind(&internal_id)
        .bind(&order.aggregator_order_id)
        .bind(&order.platform)
        .bind(&order.customer_name)
        .bind(&order.customer_phone)
        .bind(order.total.inner())
        .bind(order.ordered_at)
        .bind(&order.instructions)
        .execute(&mut *tx)
        .await?;

        // Insert items
        for item in &order.items {
            sqlx::query(
                r#"
                INSERT INTO aggregator_order_items (
                    aggregator_order_id, item_code, item_name, quantity, rate, special_instructions
                ) VALUES ($1, $2, $3, $4, $5, $6)
                "#,
            )
            .bind(&internal_id)
            .bind(&item.item_code)
            .bind(&item.item_name)
            .bind(item.quantity)
            .bind(item.rate.inner())
            .bind(&item.special_instructions)
            .execute(&mut *tx)
            .await?;
        }

        let stored = load_order(&mut tx, &internal_id).await?;
        tx.commit().await?;
        Ok(stored)
    }

    /// Find an aggregator order by internal ID with its items.
    pub fn find_order(&self, id: &str) -> DomainResult<Option<StoredAggregatorOrder>> {
        block_on(async { self.find_order_async(id).await })
            .map_err(crate::repos::to_domain_error)
    }

    async fn find_order_async(&self, id: &str) -> StorageResult<Option<StoredAggregatorOrder>> {
        let mut tx = self.pool.begin().await?;
        let exists: Option<String> =
            sqlx::query_scalar("SELECT id FROM aggregator_orders WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;

        let result = match exists {
            Some(_) => Some(load_order(&mut tx, id).await?),
            None => None,
        };

        tx.commit().await?;
        Ok(result)
    }

    /// Accept an aggregator order: create internal order + invoice, link them back.
    /// Returns error if the order is not in Pending status.
    pub fn accept_order(
        &self,
        id: &str,
        internal_order_id: i64,
        internal_invoice_id: &InvoiceName,
    ) -> DomainResult<StoredAggregatorOrder> {
        block_on(async {
                self.accept_order_async(id, internal_order_id, internal_invoice_id)
                    .await
            })
            .map_err(crate::repos::to_domain_error)
    }

    async fn accept_order_async(
        &self,
        id: &str,
        internal_order_id: i64,
        internal_invoice_id: &InvoiceName,
    ) -> StorageResult<StoredAggregatorOrder> {
        let mut tx = self.pool.begin().await?;

        // Lock and check current status
        let current_status: Option<String> = sqlx::query_scalar(
            "SELECT status::TEXT FROM aggregator_orders WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(status_str) = current_status else {
            let _ = tx.rollback().await;
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: format!("aggregator order {id} to exist"),
                actual: "not found".to_owned(),
            }));
        };

        let status = AggregatorOrderStatus::parse(&status_str)?;

        match status {
            AggregatorOrderStatus::Pending => {
                // Allowed transition
            }
            AggregatorOrderStatus::Accepted => {
                // Already accepted - return conflict
                let _ = tx.rollback().await;
                return Err(StorageError::Domain(DomainError::Conflict {
                    expected: "order in Pending status".to_owned(),
                    actual: "already Accepted".to_owned(),
                }));
            }
            _ => {
                let _ = tx.rollback().await;
                return Err(StorageError::Domain(DomainError::Conflict {
                    expected: "order in Pending status".to_owned(),
                    actual: format!("{status:?}"),
                }));
            }
        }

        // Update to Accepted with links
        sqlx::query(
            r#"
            UPDATE aggregator_orders
            SET status = 'Accepted',
                internal_order_id = $2,
                internal_invoice_id = $3
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(internal_order_id)
        .bind(internal_invoice_id.as_str())
        .execute(&mut *tx)
        .await?;

        let stored = load_order(&mut tx, id).await?;
        tx.commit().await?;
        Ok(stored)
    }

    /// Reject an aggregator order with a reason.
    pub fn reject_order(&self, id: &str, reason: &str) -> DomainResult<StoredAggregatorOrder> {
        block_on(async { self.reject_order_async(id, reason).await })
            .map_err(crate::repos::to_domain_error)
    }

    async fn reject_order_async(
        &self,
        id: &str,
        reason: &str,
    ) -> StorageResult<StoredAggregatorOrder> {
        if reason.trim().is_empty() {
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: "non-empty rejection reason".to_owned(),
                actual: "blank".to_owned(),
            }));
        }

        let mut tx = self.pool.begin().await?;

        // Lock and check current status
        let current_status: Option<String> = sqlx::query_scalar(
            "SELECT status::TEXT FROM aggregator_orders WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(status_str) = current_status else {
            let _ = tx.rollback().await;
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: format!("aggregator order {id} to exist"),
                actual: "not found".to_owned(),
            }));
        };

        let status = AggregatorOrderStatus::parse(&status_str)?;

        if status != AggregatorOrderStatus::Pending {
            let _ = tx.rollback().await;
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: "order in Pending status".to_owned(),
                actual: format!("{status:?}"),
            }));
        }

        // Update to Rejected
        sqlx::query(
            r#"
            UPDATE aggregator_orders
            SET status = 'Rejected', reject_reason = $2
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;

        let stored = load_order(&mut tx, id).await?;
        tx.commit().await?;
        Ok(stored)
    }

    /// List settlements for reconciliation, filtered by date range and optional platform.
    pub fn list_settlements(
        &self,
        start_date: NaiveDate,
        end_date: NaiveDate,
        platform: Option<&str>,
    ) -> DomainResult<Vec<StoredSettlement>> {
        block_on(async { self.list_settlements_async(start_date, end_date, platform).await })
            .map_err(crate::repos::to_domain_error)
    }

    /// Ensure every item code in the aggregator order exists in the catalog.
    /// Inserts missing items as `Aggregator` group stubs so downstream FKs (order_items,
    /// invoice_lines, kot_items) don't fail. Real menu sync would enrich them later.
    pub fn ensure_items_exist(
        &self,
        items: &[StoredAggregatorOrderItem],
    ) -> DomainResult<()> {
        block_on(async { self.ensure_items_exist_async(items).await })
            .map_err(crate::repos::to_domain_error)
    }

    async fn ensure_items_exist_async(
        &self,
        items: &[StoredAggregatorOrderItem],
    ) -> StorageResult<()> {
        for item in items {
            sqlx::query(
                "INSERT INTO items (code, name, item_group) VALUES ($1, $2, 'Aggregator') ON CONFLICT (code) DO NOTHING",
            )
            .bind(&item.item_code)
            .bind(&item.item_name)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Fetch linked aggregator order ids for a settlement (junction table).
    pub fn settlement_order_ids(&self, settlement_id: &str) -> DomainResult<Vec<String>> {
        block_on(async { self.settlement_order_ids_async(settlement_id).await })
            .map_err(crate::repos::to_domain_error)
    }

    async fn settlement_order_ids_async(
        &self,
        settlement_id: &str,
    ) -> StorageResult<Vec<String>> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT aggregator_order_id FROM aggregator_settlement_orders WHERE settlement_id = $1",
        )
        .bind(settlement_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids)
    }

    async fn list_settlements_async(
        &self,
        start_date: NaiveDate,
        end_date: NaiveDate,
        platform: Option<&str>,
    ) -> StorageResult<Vec<StoredSettlement>> {
        let rows = sqlx::query(
            r#"
            SELECT id, platform, settlement_date, total_orders,
                   gross_amount, commission, net_amount, created_at, updated_at
            FROM aggregator_settlements
            WHERE settlement_date >= $1 AND settlement_date <= $2
              AND ($3::TEXT IS NULL OR platform = $3)
            ORDER BY settlement_date DESC, platform, id
            "#,
        )
        .bind(start_date)
        .bind(end_date)
        .bind(platform)
        .fetch_all(&self.pool)
        .await?;

        let mut settlements = Vec::new();
        for row in rows {
            settlements.push(StoredSettlement {
                id: row.try_get("id")?,
                platform: row.try_get("platform")?,
                settlement_date: row.try_get("settlement_date")?,
                total_orders: row.try_get("total_orders")?,
                gross_amount: Money::new(row.try_get("gross_amount")?),
                commission: Money::new(row.try_get("commission")?),
                net_amount: Money::new(row.try_get("net_amount")?),
                created_at: row.try_get("created_at")?,
                updated_at: row.try_get("updated_at")?,
            });
        }

        Ok(settlements)
    }
}

/// Load an order with its items inside a transaction.
async fn load_order(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> StorageResult<StoredAggregatorOrder> {
    let row = sqlx::query(
        r#"
        SELECT id, aggregator_order_id, platform, customer_name, customer_phone,
               total, ordered_at, status::TEXT as status, internal_order_id, internal_invoice_id,
               instructions, reject_reason, created_at, updated_at
        FROM aggregator_orders
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| {
        StorageError::Domain(DomainError::Conflict {
            expected: format!("aggregator order {id} to exist"),
            actual: "not found".to_owned(),
        })
    })?;

    let status_str: String = row.try_get("status")?;
    let status = AggregatorOrderStatus::parse(&status_str)?;

    let internal_invoice_id: Option<String> = row.try_get("internal_invoice_id")?;

    // Load items
    let item_rows = sqlx::query(
        r#"
        SELECT id, item_code, item_name, quantity, rate, special_instructions
        FROM aggregator_order_items
        WHERE aggregator_order_id = $1
        ORDER BY id
        "#,
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await?;

    let mut items = Vec::new();
    for item_row in item_rows {
        items.push(StoredAggregatorOrderItem {
            id: item_row.try_get("id")?,
            item_code: item_row.try_get("item_code")?,
            item_name: item_row.try_get("item_name")?,
            quantity: item_row.try_get("quantity")?,
            rate: Money::new(item_row.try_get("rate")?),
            special_instructions: item_row.try_get("special_instructions")?,
        });
    }

    Ok(StoredAggregatorOrder {
        id: row.try_get("id")?,
        aggregator_order_id: row.try_get("aggregator_order_id")?,
        platform: row.try_get("platform")?,
        customer_name: row.try_get("customer_name")?,
        customer_phone: row.try_get("customer_phone")?,
        total: Money::new(row.try_get("total")?),
        ordered_at: row.try_get("ordered_at")?,
        status,
        internal_order_id: row.try_get("internal_order_id")?,
        internal_invoice_id: internal_invoice_id.map(|s| InvoiceName::from(s.as_str())),
        instructions: row.try_get("instructions")?,
        reject_reason: row.try_get("reject_reason")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    async fn setup_test_pool() -> PgPool {
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://peacock:peacock@localhost/peacock_test".to_string());

        PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("failed to connect to test database")
    }

    async fn clean_aggregator_tables(pool: &PgPool) {
        sqlx::query("DELETE FROM aggregator_order_items")
            .execute(pool)
            .await
            .expect("failed to clean aggregator_order_items");
        sqlx::query("DELETE FROM aggregator_orders")
            .execute(pool)
            .await
            .expect("failed to clean aggregator_orders");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_insert_order_creates_new() {
        let pool = setup_test_pool().await;
        clean_aggregator_tables(&pool).await;

        let repo = PgAggregatorRepo::new(pool.clone());
        let order = NewAggregatorOrder {
            aggregator_order_id: "SWGY-123".to_string(),
            platform: "swiggy".to_string(),
            customer_name: "John Doe".to_string(),
            customer_phone: Some("+919876543210".to_string()),
            total: Money::new(Decimal::from(250)),
            ordered_at: Utc::now(),
            instructions: Some("Ring bell".to_string()),
            items: vec![AggregatorOrderItem {
                item_code: "ITEM-001".to_string(),
                item_name: "Masala Dosa".to_string(),
                quantity: Decimal::from(2),
                rate: Money::new(Decimal::from(125)),
                special_instructions: None,
            }],
        };

        let result_id = repo.insert_order(&order).unwrap();
        let result = repo.find_order(&result_id).unwrap().unwrap();
        
        assert_eq!(result.aggregator_order_id, "SWGY-123");
        assert_eq!(result.platform, "swiggy");
        assert_eq!(result.status, AggregatorOrderStatus::Pending);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].item_code, "ITEM-001");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_insert_order_idempotent() {
        let pool = setup_test_pool().await;
        clean_aggregator_tables(&pool).await;

        let repo = PgAggregatorRepo::new(pool.clone());
        let order = NewAggregatorOrder {
            aggregator_order_id: "SWGY-456".to_string(),
            platform: "swiggy".to_string(),
            customer_name: "Jane Doe".to_string(),
            customer_phone: None,
            total: Money::new(Decimal::from(100)),
            ordered_at: Utc::now(),
            instructions: None,
            items: vec![],
        };

        let first_id = repo.insert_order(&order).unwrap();
        let second_id = repo.insert_order(&order).unwrap();

        assert_eq!(first_id, second_id);
        
        let first = repo.find_order(&first_id).unwrap().unwrap();
        let second = repo.find_order(&second_id).unwrap().unwrap();
        assert_eq!(first.aggregator_order_id, second.aggregator_order_id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_find_order() {
        let pool = setup_test_pool().await;
        clean_aggregator_tables(&pool).await;

        let repo = PgAggregatorRepo::new(pool.clone());
        let order = NewAggregatorOrder {
            aggregator_order_id: "ZOM-789".to_string(),
            platform: "zomato".to_string(),
            customer_name: "Test User".to_string(),
            customer_phone: None,
            total: Money::new(Decimal::from(150)),
            ordered_at: Utc::now(),
            instructions: None,
            items: vec![],
        };

        let created_id = repo.insert_order(&order).unwrap();
        let found = repo.find_order(&created_id).unwrap();

        assert!(found.is_some());
        assert_eq!(found.unwrap().id, created_id);

        let missing = repo.find_order("NONEXISTENT").unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_accept_order() {
        let pool = setup_test_pool().await;
        clean_aggregator_tables(&pool).await;

        let repo = PgAggregatorRepo::new(pool.clone());
        let order = NewAggregatorOrder {
            aggregator_order_id: "ACC-001".to_string(),
            platform: "swiggy".to_string(),
            customer_name: "Accept Test".to_string(),
            customer_phone: None,
            total: Money::new(Decimal::from(200)),
            ordered_at: Utc::now(),
            instructions: None,
            items: vec![],
        };

        let created_id = repo.insert_order(&order).unwrap();
        let created = repo.find_order(&created_id).unwrap().unwrap();
        assert_eq!(created.status, AggregatorOrderStatus::Pending);

        repo.accept_order(&created_id, 123, &InvoiceName::from("INV-001"))
            .unwrap();

        let accepted = repo.find_order(&created_id).unwrap().unwrap();
        assert_eq!(accepted.status, AggregatorOrderStatus::Accepted);
        assert_eq!(accepted.internal_order_id, Some(123));
        assert_eq!(
            accepted.internal_invoice_id,
            Some(InvoiceName::from("INV-001"))
        );

        // Accepting again should fail
        let result = repo.accept_order(&created_id, 124, &InvoiceName::from("INV-002"));
        assert!(result.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_reject_order() {
        let pool = setup_test_pool().await;
        clean_aggregator_tables(&pool).await;

        let repo = PgAggregatorRepo::new(pool.clone());
        let order = NewAggregatorOrder {
            aggregator_order_id: "REJ-001".to_string(),
            platform: "zomato".to_string(),
            customer_name: "Reject Test".to_string(),
            customer_phone: None,
            total: Money::new(Decimal::from(180)),
            ordered_at: Utc::now(),
            instructions: None,
            items: vec![],
        };

        let created_id = repo.insert_order(&order).unwrap();
        repo.reject_order(&created_id, "Item unavailable").unwrap();

        let rejected = repo.find_order(&created_id).unwrap().unwrap();
        assert_eq!(rejected.status, AggregatorOrderStatus::Rejected);
        assert_eq!(rejected.reject_reason, Some("Item unavailable".to_string()));

        // Rejecting with blank reason should fail
        let order2 = NewAggregatorOrder {
            aggregator_order_id: "REJ-002".to_string(),
            platform: "swiggy".to_string(),
            customer_name: "Test".to_string(),
            customer_phone: None,
            total: Money::new(Decimal::from(100)),
            ordered_at: Utc::now(),
            instructions: None,
            items: vec![],
        };
        let created2_id = repo.insert_order(&order2).unwrap();
        let result = repo.reject_order(&created2_id, "   ");
        assert!(result.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_list_settlements() {
        let pool = setup_test_pool().await;

        // Clean settlements
        sqlx::query("DELETE FROM aggregator_settlements")
            .execute(&pool)
            .await
            .expect("failed to clean settlements");

        // Insert test settlements
        let today = Utc::now().date_naive();
        sqlx::query(
            r#"
            INSERT INTO aggregator_settlements
                (id, platform, settlement_date, total_orders, gross_amount, commission, net_amount)
            VALUES
                ('SET-001', 'swiggy', $1, 10, 5000.00, 500.00, 4500.00),
                ('SET-002', 'zomato', $1, 8, 4000.00, 400.00, 3600.00)
            "#,
        )
        .bind(today)
        .execute(&pool)
        .await
        .expect("failed to insert test settlements");

        let repo = PgAggregatorRepo::new(pool.clone());
        let settlements = repo.list_settlements(today, today, None).unwrap();

        assert_eq!(settlements.len(), 2);

        let swiggy_only = repo
            .list_settlements(today, today, Some("swiggy"))
            .unwrap();
        assert_eq!(swiggy_only.len(), 1);
        assert_eq!(swiggy_only[0].platform, "swiggy");
    }
}
