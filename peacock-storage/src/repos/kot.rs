//! KOT (Kitchen Order Ticket) repository — Lane 2E.
//!
//! ## Gapless numbering
//!
//! PostgreSQL sequence + SERIALIZABLE isolation + retry logic (Storage::with_serializable_retry)
//! guarantees no gaps under concurrent inserts. The sequence is global for all naming series;
//! the repository formats `naming_series + sequence_number` into the final KOT name.
//!
//! ## Production unit routing
//!
//! `kots.production` stores which station each KOT routes to. Query:
//! "pending KOTs for this production unit" drives the kitchen display.
//!
//! ## N+1 query fix (bugs 6 and 7)
//!
//! Upstream issued 36 queries for 12 items × 3 stations (ury_kot_generate.py:154, :214).
//! The fix: `list_items_batch` fetches all items for multiple KOTs in one query, ordered by idx.

use crate::error::StorageResult;
use crate::{Storage, StorageError};
use peacock_core::error::{Error as DomainError, Result};
use peacock_core::ids::{ItemCode, KotName, MenuCourseName, ProductionUnitName};
use peacock_core::model::{Kot, KotItem, KotType};
use peacock_core::ports::KotRepo;
use rust_decimal::Decimal;
use sqlx::types::chrono::{NaiveDate, NaiveTime};

/// KOT repository implementation.
#[derive(Clone)]
pub struct PgKotRepo {
    storage: Storage,
}

impl PgKotRepo {
    pub fn new(storage: Storage) -> Self {
        PgKotRepo { storage }
    }

    /// Insert a KOT with gapless numbering. The `Kot::name` must be `None` (the sequence
    /// assigns it). Returns the KOT with `name` populated.
    ///
    /// Uses SERIALIZABLE isolation + retry logic to guarantee gapless numbering under
    /// concurrent inserts (PHASE_2_3_PLAN.md Risk 3).
    pub async fn create(&self, kot: Kot) -> StorageResult<Kot> {
        if kot.name.is_some() {
            return Err(StorageError::Constraint {
                table: "kots".to_owned(),
                constraint: "name_must_be_none_for_create".to_owned(),
                message: "KOT name must be None for creation".to_owned(),
            });
        }

        let storage = self.storage.clone();
        let result = storage
            .with_serializable_retry(5, move |tx| {
                let kot = kot.clone();
                Box::pin(async move {
                    // Fetch next sequence value
                    let seq: i64 = sqlx::query_scalar("SELECT nextval('kot_number_seq')")
                        .fetch_one(&mut **tx)
                        .await?;

                    // Format KOT name: naming_series + sequence number
                    let name = format!("{}{:05}", kot.naming_series, seq);
                    let kot_name = KotName::from(name.as_str());

                    // Insert KOT root
                    let kot_type_str = kot_type_to_str(kot.kot_type);
                    sqlx::query(
                        r#"
                        INSERT INTO kots (
                            name, naming_series, invoice, restaurant_table, customer_name,
                            original_kot, date, time, kot_type, order_status, production,
                            start_time_prep, pos_profile, branch, verified, verified_by,
                            table_takeaway, is_aggregator, aggregator_id, comments, order_no
                        ) VALUES (
                            $1, $2, $3, $4, $5, $6, $7, $8, $9::kot_type, $10, $11,
                            $12, $13, $14, $15, $16, $17, $18, $19, $20, $21
                        )
                        "#,
                    )
                    .bind(kot_name.as_str())
                    .bind(&kot.naming_series)
                    .bind(&kot.invoice)
                    .bind(kot.restaurant_table.as_ref().map(|t| t.as_str()))
                    .bind(kot.customer_name.as_ref().map(|c| c.as_str()))
                    .bind(&kot.original_kot)
                    .bind(kot.date)
                    .bind(kot.time)
                    .bind(kot_type_str)
                    .bind(&kot.order_status)
                    .bind(kot.production.as_ref().map(|p| p.as_str()))
                    .bind(kot.start_time_prep)
                    .bind(kot.pos_profile.as_ref().map(|p| p.as_str()))
                    .bind(kot.branch.as_ref().map(|b| b.as_str()))
                    .bind(kot.verified)
                    .bind(kot.verified_by.as_ref().map(|u| u.as_str()))
                    .bind(kot.table_takeaway)
                    .bind(kot.is_aggregator)
                    .bind(&kot.aggregator_id)
                    .bind(&kot.comments)
                    .bind(&kot.order_no)
                    .execute(&mut **tx)
                    .await?;

                    // Insert child items
                    for (idx, item) in kot.kot_items.iter().enumerate() {
                        sqlx::query(
                            r#"
                            INSERT INTO kot_items (
                                kot_name, idx, item, item_name, quantity, cancelled_qty,
                                comments, course, serve_priority, indicate_course
                            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                            "#,
                        )
                        .bind(kot_name.as_str())
                        .bind((idx + 1) as i32) // idx is 1-based
                        .bind(item.item.as_str())
                        .bind(&item.item_name)
                        .bind(item.quantity)
                        .bind(item.cancelled_qty)
                        .bind(&item.comments)
                        .bind(item.course.as_ref().map(|c| c.as_str()))
                        .bind(item.serve_priority)
                        .bind(item.indicate_course)
                        .execute(&mut **tx)
                        .await?;
                    }

                    // Return KOT with name populated
                    let mut created = kot.clone();
                    created.name = Some(kot_name);
                    Ok(created)
                })
            })
            .await?;

        Ok(result)
    }

    /// Fetch a KOT by name with all its items, ordered by idx.
    pub async fn get(&self, name: &KotName) -> StorageResult<Kot> {
        let row = sqlx::query_as::<_, KotRow>(
            r#"
            SELECT
                name, naming_series, invoice, restaurant_table, customer_name,
                original_kot, date, time, kot_type::TEXT as kot_type, order_status,
                production, start_time_prep, pos_profile, branch, verified, verified_by,
                table_takeaway, is_aggregator, aggregator_id, comments, order_no
            FROM kots
            WHERE name = $1
            "#,
        )
        .bind(name.as_str())
        .fetch_optional(self.storage.pool())
        .await?
        .ok_or_else(|| StorageError::Constraint {
            table: "kots".to_owned(),
            constraint: "not_found".to_owned(),
            message: format!("KOT {} not found", name.as_str()),
        })?;

        let items = self.list_items(name).await?;

        Ok(row_to_kot(row, items))
    }

    /// Fetch all items for a single KOT, ordered by idx.
    async fn list_items(&self, kot_name: &KotName) -> StorageResult<Vec<KotItem>> {
        let rows = sqlx::query_as::<_, KotItemRow>(
            r#"
            SELECT item, item_name, quantity, cancelled_qty, comments, course,
                   serve_priority, indicate_course
            FROM kot_items
            WHERE kot_name = $1
            ORDER BY idx
            "#,
        )
        .bind(kot_name.as_str())
        .fetch_all(self.storage.pool())
        .await?;

        Ok(rows.into_iter().map(item_row_to_model).collect())
    }

    /// Fetch all items for multiple KOTs in one query — the N+1 fix.
    ///
    /// Returns a map: KotName → Vec<KotItem> ordered by idx.
    /// This is the batched form that replaces 36 queries with 1 (bugs 6 and 7).
    pub async fn list_items_batch(
        &self,
        kot_names: &[KotName],
    ) -> StorageResult<std::collections::HashMap<KotName, Vec<KotItem>>> {
        if kot_names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let names: Vec<&str> = kot_names.iter().map(|n| n.as_str()).collect();
        let rows = sqlx::query_as::<_, KotItemRowWithName>(
            r#"
            SELECT kot_name, item, item_name, quantity, cancelled_qty, comments, course,
                   serve_priority, indicate_course
            FROM kot_items
            WHERE kot_name = ANY($1)
            ORDER BY kot_name, idx
            "#,
        )
        .bind(&names)
        .fetch_all(self.storage.pool())
        .await?;

        let mut map: std::collections::HashMap<KotName, Vec<KotItem>> =
            std::collections::HashMap::new();
        for row in rows {
            let kot_name = KotName::from(row.kot_name.as_str());
            map.entry(kot_name).or_default().push(KotItemRow {
                item: row.item,
                item_name: row.item_name,
                quantity: row.quantity,
                cancelled_qty: row.cancelled_qty,
                comments: row.comments,
                course: row.course,
                serve_priority: row.serve_priority,
                indicate_course: row.indicate_course,
            }.into());
        }

        Ok(map)
    }

    /// Pending KOTs for a production unit, filtered by date range.
    pub async fn list_pending_for_production(
        &self,
        production: &ProductionUnitName,
        from_date: NaiveDate,
        to_date: NaiveDate,
    ) -> StorageResult<Vec<Kot>> {
        let rows = sqlx::query_as::<_, KotRow>(
            r#"
            SELECT
                name, naming_series, invoice, restaurant_table, customer_name,
                original_kot, date, time, kot_type::TEXT as kot_type, order_status,
                production, start_time_prep, pos_profile, branch, verified, verified_by,
                table_takeaway, is_aggregator, aggregator_id, comments, order_no
            FROM kots
            WHERE production = $1
              AND date >= $2
              AND date <= $3
              AND kot_type IN ('NewOrder', 'OrderModified')
            ORDER BY date, created_at
            "#,
        )
        .bind(production.as_str())
        .bind(from_date)
        .bind(to_date)
        .fetch_all(self.storage.pool())
        .await?;

        // Fetch all items in one batch query (N+1 fix)
        let kot_names: Vec<KotName> = rows.iter().map(|r| KotName::from(r.name.as_str())).collect();
        let items_map = self.list_items_batch(&kot_names).await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let name = KotName::from(row.name.as_str());
                let items = items_map.get(&name).cloned().unwrap_or_default();
                row_to_kot(row, items)
            })
            .collect())
    }

    /// KOTs a station still has work on — the kitchen display query (Lane 4A-3).
    ///
    /// Distinct from [`PgKotRepo::list_pending_for_production`], which filters by
    /// `kot_type` only and therefore keeps returning a ticket after the kitchen has
    /// finished it. "Pending" here means *not yet prepared*: `order_status` is NULL or
    /// anything other than `Prepared`.
    ///
    /// `Cancelled` and `PartiallyCancelled` tickets are excluded — a cancellation slip is
    /// a print instruction, not outstanding work.
    ///
    /// Ordered by `date, created_at`: oldest ticket first, which is the order a kitchen
    /// works in. One batched item query regardless of ticket count (the bug 6/7 fix).
    pub async fn list_unprepared_for_production(
        &self,
        production: &ProductionUnitName,
    ) -> StorageResult<Vec<Kot>> {
        let rows = sqlx::query_as::<_, KotRow>(
            r#"
            SELECT
                name, naming_series, invoice, restaurant_table, customer_name,
                original_kot, date, time, kot_type::TEXT as kot_type, order_status,
                production, start_time_prep, pos_profile, branch, verified, verified_by,
                table_takeaway, is_aggregator, aggregator_id, comments, order_no
            FROM kots
            WHERE production = $1
              AND kot_type IN ('NewOrder', 'OrderModified')
              AND (order_status IS NULL OR order_status <> 'Prepared')
            ORDER BY date, created_at
            "#,
        )
        .bind(production.as_str())
        .fetch_all(self.storage.pool())
        .await?;

        let kot_names: Vec<KotName> = rows.iter().map(|r| KotName::from(r.name.as_str())).collect();
        let items_map = self.list_items_batch(&kot_names).await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let name = KotName::from(row.name.as_str());
                let items = items_map.get(&name).cloned().unwrap_or_default();
                row_to_kot(row, items)
            })
            .collect())
    }

    /// Mark a KOT prepared, stamping the time the kitchen finished it.
    ///
    /// Idempotent: marking an already-prepared ticket returns it unchanged rather than
    /// overwriting `start_time_prep`. A kitchen display that double-taps must not move
    /// the timestamp the service-time report measures against.
    ///
    /// A cancellation slip cannot be prepared — there is nothing to cook.
    pub async fn mark_prepared(
        &self,
        name: &KotName,
        prepared_at: Option<NaiveTime>,
    ) -> StorageResult<Kot> {
        let mut tx = self.storage.begin().await?;

        // FOR UPDATE: read the current status and write it under one lock, so two
        // concurrent marks cannot both see "not prepared" and both stamp a time.
        let current: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT kot_type::TEXT, order_status FROM kots WHERE name = $1 FOR UPDATE",
        )
        .bind(name.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        let Some((kot_type, order_status)) = current else {
            let _ = tx.rollback().await;
            return Err(StorageError::Constraint {
                table: "kots".to_owned(),
                constraint: "not_found".to_owned(),
                message: format!("KOT {} not found", name.as_str()),
            });
        };

        if matches!(kot_type.as_str(), "Cancelled" | "PartiallyCancelled") {
            let _ = tx.rollback().await;
            return Err(StorageError::Domain(DomainError::Conflict {
                expected: "a NewOrder or OrderModified ticket".to_owned(),
                actual: format!("KOT {} is {kot_type}", name.as_str()),
            }));
        }

        let already_prepared = order_status.as_deref() == Some("Prepared");
        if !already_prepared {
            sqlx::query(
                "UPDATE kots
                    SET order_status = 'Prepared',
                        start_time_prep = COALESCE($2, start_time_prep, localtime)
                  WHERE name = $1",
            )
            .bind(name.as_str())
            .bind(prepared_at)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        self.get(name).await
    }
}

impl KotRepo for PgKotRepo {
    fn exists_for(&self, invoice: &str, production: &ProductionUnitName) -> Result<bool> {
        let storage = self.storage.clone();
        let invoice = invoice.to_owned();
        let production = production.clone();

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let exists: bool = sqlx::query_scalar(
                    r#"
                    SELECT EXISTS(
                        SELECT 1 FROM kots
                        WHERE invoice = $1 AND production = $2
                    )
                    "#,
                )
                .bind(&invoice)
                .bind(production.as_str())
                .fetch_one(storage.pool())
                .await?;

                Ok(exists)
            })
        })
        .map_err(|e: StorageError| match e {
            StorageError::Domain(d) => d,
            _ => DomainError::Conflict {
                expected: "query success".to_owned(),
                actual: e.to_string(),
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Row types + conversions
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct KotRow {
    name: String,
    naming_series: String,
    invoice: String,
    restaurant_table: Option<String>,
    customer_name: Option<String>,
    original_kot: Option<String>,
    date: NaiveDate,
    time: Option<NaiveTime>,
    kot_type: String,
    order_status: Option<String>,
    production: Option<String>,
    start_time_prep: Option<NaiveTime>,
    pos_profile: Option<String>,
    branch: Option<String>,
    verified: bool,
    verified_by: Option<String>,
    table_takeaway: bool,
    is_aggregator: bool,
    aggregator_id: Option<String>,
    comments: Option<String>,
    order_no: Option<String>,
}

#[derive(sqlx::FromRow)]
struct KotItemRow {
    item: String,
    item_name: String,
    quantity: Decimal,
    cancelled_qty: Decimal,
    comments: Option<String>,
    course: Option<String>,
    serve_priority: i32,
    indicate_course: bool,
}

#[derive(sqlx::FromRow)]
struct KotItemRowWithName {
    kot_name: String,
    item: String,
    item_name: String,
    quantity: Decimal,
    cancelled_qty: Decimal,
    comments: Option<String>,
    course: Option<String>,
    serve_priority: i32,
    indicate_course: bool,
}

fn row_to_kot(row: KotRow, items: Vec<KotItem>) -> Kot {
    use peacock_core::ids::*;

    Kot {
        name: Some(KotName::from(row.name.as_str())),
        naming_series: row.naming_series,
        invoice: row.invoice,
        restaurant_table: row.restaurant_table.map(|t| TableName::from(t.as_str())),
        customer_name: row.customer_name.map(|c| CustomerName::from(c.as_str())),
        original_kot: row.original_kot,
        date: row.date,
        time: row.time,
        kot_type: str_to_kot_type(&row.kot_type),
        order_status: row.order_status,
        production: row.production.map(|p| ProductionUnitName::from(p.as_str())),
        start_time_prep: row.start_time_prep,
        kot_items: items,
        pos_profile: row.pos_profile.map(|p| PosProfileName::from(p.as_str())),
        branch: row.branch.map(|b| BranchName::from(b.as_str())),
        verified: row.verified,
        verified_by: row.verified_by.map(|u| UserName::from(u.as_str())),
        table_takeaway: row.table_takeaway,
        is_aggregator: row.is_aggregator,
        aggregator_id: row.aggregator_id,
        comments: row.comments,
        order_no: row.order_no,
    }
}

fn item_row_to_model(row: KotItemRow) -> KotItem {
    KotItem {
        item: ItemCode::from(row.item.as_str()),
        item_name: row.item_name,
        quantity: row.quantity,
        cancelled_qty: row.cancelled_qty,
        comments: row.comments,
        course: row.course.map(|c| MenuCourseName::from(c.as_str())),
        serve_priority: row.serve_priority,
        indicate_course: row.indicate_course,
    }
}

impl From<KotItemRow> for KotItem {
    fn from(row: KotItemRow) -> Self {
        item_row_to_model(row)
    }
}

fn kot_type_to_str(t: KotType) -> &'static str {
    match t {
        KotType::NewOrder => "NewOrder",
        KotType::OrderModified => "OrderModified",
        KotType::Cancelled => "Cancelled",
        KotType::PartiallyCancelled => "PartiallyCancelled",
    }
}

fn str_to_kot_type(s: &str) -> KotType {
    match s {
        "NewOrder" => KotType::NewOrder,
        "OrderModified" => KotType::OrderModified,
        "Cancelled" => KotType::Cancelled,
        "PartiallyCancelled" => KotType::PartiallyCancelled,
        _ => KotType::NewOrder, // defensive fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peacock_core::ids::*;
    use rust_decimal_macros::dec;

    fn sample_kot() -> Kot {
        Kot {
            name: None,
            naming_series: "KOT-".to_owned(),
            invoice: "ACC-PSINV-2026-00042".to_owned(),
            restaurant_table: Some(TableName::from("T-01")),
            customer_name: Some(CustomerName::from("Walk-in")),
            original_kot: None,
            date: NaiveDate::from_ymd_opt(2026, 7, 28).unwrap(),
            time: Some(NaiveTime::from_hms_opt(12, 30, 0).unwrap()),
            kot_type: KotType::NewOrder,
            order_status: None,
            production: Some(ProductionUnitName::from("Hot Kitchen")),
            start_time_prep: None,
            kot_items: vec![
                KotItem {
                    item: ItemCode::from("CURRY"),
                    item_name: "Chicken Curry".to_owned(),
                    quantity: dec!(2),
                    cancelled_qty: dec!(0),
                    comments: None,
                    course: Some(MenuCourseName::from("Main Course")),
                    serve_priority: 0,
                    indicate_course: false,
                },
                KotItem {
                    item: ItemCode::from("NAAN"),
                    item_name: "Butter Naan".to_owned(),
                    quantity: dec!(4),
                    cancelled_qty: dec!(0),
                    comments: Some("Extra butter".to_owned()),
                    course: Some(MenuCourseName::from("Breads")),
                    serve_priority: 1,
                    indicate_course: true,
                },
            ],
            pos_profile: Some(PosProfileName::from("Peacock POS")),
            branch: Some(BranchName::from("Peacock - Main")),
            verified: false,
            verified_by: None,
            table_takeaway: false,
            is_aggregator: false,
            aggregator_id: None,
            comments: None,
            order_no: Some("ORD-001".to_owned()),
        }
    }

    #[test]
    fn kot_type_round_trips() {
        assert_eq!(str_to_kot_type(kot_type_to_str(KotType::NewOrder)), KotType::NewOrder);
        assert_eq!(
            str_to_kot_type(kot_type_to_str(KotType::OrderModified)),
            KotType::OrderModified
        );
        assert_eq!(str_to_kot_type(kot_type_to_str(KotType::Cancelled)), KotType::Cancelled);
        assert_eq!(
            str_to_kot_type(kot_type_to_str(KotType::PartiallyCancelled)),
            KotType::PartiallyCancelled
        );
    }

    #[test]
    fn sample_kot_has_no_name_before_creation() {
        let kot = sample_kot();
        assert!(kot.name.is_none(), "name must be None for create()");
    }

    #[test]
    fn sample_kot_has_two_items() {
        let kot = sample_kot();
        assert_eq!(kot.kot_items.len(), 2);
        assert_eq!(kot.kot_items[0].item.as_str(), "CURRY");
        assert_eq!(kot.kot_items[1].item.as_str(), "NAAN");
    }
}
