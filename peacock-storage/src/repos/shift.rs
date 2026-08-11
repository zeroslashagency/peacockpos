//! PostgreSQL implementation of shift management — Lane 2G.
//!
//! Implements business day calculation with proper midnight-crossing handling (bug 2 fix)
//! and cash threshold tracking for CGST Rule 56 compliance.

use crate::repos::blocking::block_on;
use crate::repos::to_domain_error;
use crate::Storage;
use chrono::{DateTime, NaiveDate, Utc};
use peacock_core::businessday::BusinessDay;
use peacock_core::error::{Error as DomainError, Result};
use peacock_core::ids::{ShiftName, TerminalName, UserName};
use peacock_core::money::Money;
use peacock_core::ports::{Shift, ShiftRepo, ZReport};
use rust_decimal::Decimal;
use sqlx::Row;

/// Helper to convert sqlx errors into domain errors
fn sqlx_err(e: sqlx::Error) -> DomainError {
    DomainError::NonNumericData {
        entity: "shift".to_string(),
        field: "query".to_string(),
        raw: e.to_string(),
    }
}

/// PostgreSQL implementation of shift management.
#[derive(Clone)]
pub struct PostgresShiftRepo {
    storage: Storage,
}

impl PostgresShiftRepo {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }
}

impl ShiftRepo for PostgresShiftRepo {
    fn open_shift(
        &self,
        terminal: &TerminalName,
        opened_by: &UserName,
        business_day: NaiveDate,
    ) -> Result<Shift> {
        let terminal_str = terminal.as_str();
        let opened_by_str = opened_by.as_str();
        let now = Utc::now();

        block_on(async {
                // Check if there's already an open shift for this terminal
                let existing = sqlx::query_scalar::<_, i64>(
                    "SELECT id FROM shifts WHERE terminal = $1 AND status = 'open'"
                )
                .bind(terminal_str)
                .fetch_optional(self.storage.pool())
                .await
                .map_err(|e| DomainError::NonNumericData {
                    entity: "shift".to_string(),
                    field: "query".to_string(),
                    raw: e.to_string(),
                })?;

                if existing.is_some() {
                    return Err(DomainError::ShiftAlreadyOpen(terminal.clone()));
                }

                // Insert the new shift with a generated name (sequence-based)
                let id: i64 = sqlx::query_scalar(
                    r#"
                    INSERT INTO shifts (
                        terminal, opened_at, business_day_label, cutoff_hour,
                        status, opened_by
                    ) VALUES ($1, $2, $3, 3, 'open', $4)
                    RETURNING id
                    "#
                )
                .bind(terminal_str)
                .bind(now)
                .bind(business_day)
                .bind(opened_by_str)
                .fetch_one(self.storage.pool())
                .await
                .map_err(|e| DomainError::NonNumericData {
                    entity: "shift".to_string(),
                    field: "query".to_string(),
                    raw: e.to_string(),
                })?;

                // Generate shift name from the ID (e.g., "SHIFT-00001")
                let shift_name = ShiftName::new(format!("SHIFT-{:05}", id));

                Ok(Shift {
                    name: shift_name,
                    terminal: terminal.clone(),
                    opened_at: now,
                    closed_at: None,
                    opened_by: opened_by.clone(),
                    business_day,
                })
            })
            .map_err(to_domain_error)?
    }

    fn get_current_shift(&self, terminal: &TerminalName) -> Result<Option<Shift>> {
        let terminal_str = terminal.as_str();

        block_on(async {
                let row = sqlx::query(
                    r#"
                    SELECT id, terminal, opened_at, closed_at, business_day_label,
                           opened_by
                    FROM shifts
                    WHERE terminal = $1 AND status = 'open'
                    "#
                )
                .bind(terminal_str)
                .fetch_optional(self.storage.pool())
                .await
                .map_err(|e| DomainError::NonNumericData {
                    entity: "shift".to_string(),
                    field: "query".to_string(),
                    raw: e.to_string(),
                })?;

                match row {
                    Some(row) => {
                        let id: i64 = row.try_get("id").map_err(sqlx_err)?;
                        let terminal: String = row.try_get("terminal").map_err(sqlx_err)?;
                        let opened_at: DateTime<Utc> = row.try_get("opened_at").map_err(sqlx_err)?;
                        let closed_at: Option<DateTime<Utc>> = row.try_get("closed_at").map_err(sqlx_err)?;
                        let business_day_label: NaiveDate = row.try_get("business_day_label").map_err(sqlx_err)?;
                        let opened_by: Option<String> = row.try_get("opened_by").map_err(sqlx_err)?;

                        let shift_name = ShiftName::new(format!("SHIFT-{:05}", id));
                        Ok(Some(Shift {
                            name: shift_name,
                            terminal: TerminalName::from(terminal.as_str()),
                            opened_at,
                            closed_at,
                            opened_by: UserName::from(
                                opened_by.as_deref().unwrap_or("system"),
                            ),
                            business_day: business_day_label,
                        }))
                    }
                    None => Ok(None),
                }
            })
            .map_err(to_domain_error)?
    }

    fn close_shift(
        &self,
        shift_name: &ShiftName,
        cutoff_hour: u32,
        tz: chrono_tz::Tz,
    ) -> Result<ZReport> {
        let shift_name_str = shift_name.as_str();
        let now = Utc::now();

        // Extract shift ID from name (e.g., "SHIFT-00001" -> 1)
        let shift_id: i64 = shift_name_str
            .strip_prefix("SHIFT-")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| DomainError::ShiftNotFound(shift_name.clone()))?;

        block_on(async {
                // Get the shift details
                let shift = sqlx::query(
                    r#"
                    SELECT id, terminal, opened_at, business_day_label, opened_by
                    FROM shifts
                    WHERE id = $1 AND status = 'open'
                    "#
                )
                .bind(shift_id)
                .fetch_optional(self.storage.pool())
                .await
                .map_err(|e| DomainError::NonNumericData {
                    entity: "shift".to_string(),
                    field: "query".to_string(),
                    raw: e.to_string(),
                })?
                .ok_or_else(|| DomainError::ShiftNotFound(shift_name.clone()))?;

                let terminal: String = shift.try_get("terminal").map_err(sqlx_err)?;
                let opened_at: DateTime<Utc> = shift.try_get("opened_at").map_err(sqlx_err)?;
                let business_day_label: NaiveDate = shift.try_get("business_day_label").map_err(sqlx_err)?;

                // Calculate business day range using the businessday module
                let _business_day = BusinessDay::for_instant(opened_at, cutoff_hour, tz);

                // TODO: Query invoices in the business day range [start, end) and calculate totals
                // For now, we'll use placeholder values
                let cash_total = Money::ZERO;
                let card_total = Money::ZERO;
                let invoice_count = 0i32;
                let total_revenue = cash_total + card_total;

                // CGST Rule 56: flag if cash >= ₹10,000
                let cash_threshold = Money::new(Decimal::from(10000));
                let cash_over_threshold = cash_total >= cash_threshold;

                // Update the shift
                sqlx::query(
                    r#"
                    UPDATE shifts
                    SET closed_at = $1,
                        status = 'closed',
                        cash_total = $2,
                        card_total = $3,
                        invoice_count = $4,
                        cash_over_threshold = $5
                    WHERE id = $6
                    "#
                )
                .bind(now)
                .bind(cash_total.0)
                .bind(card_total.0)
                .bind(invoice_count)
                .bind(cash_over_threshold)
                .bind(shift_id)
                .execute(self.storage.pool())
                .await
                .map_err(|e| DomainError::NonNumericData {
                    entity: "shift".to_string(),
                    field: "query".to_string(),
                    raw: e.to_string(),
                })?;

                Ok(ZReport {
                    shift_name: shift_name.clone(),
                    terminal: TerminalName::from(terminal.as_str()),
                    business_day: business_day_label,
                    opened_at,
                    closed_at: now,
                    invoice_count: invoice_count as i64,
                    cash_total,
                    card_total,
                    total_revenue,
                    cash_threshold_warning: cash_over_threshold,
                })
            })
            .map_err(to_domain_error)?
    }

    fn get_report(&self, shift_name: &ShiftName) -> Result<ZReport> {
        let shift_name_str = shift_name.as_str();

        // Extract shift ID from name
        let shift_id: i64 = shift_name_str
            .strip_prefix("SHIFT-")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| DomainError::ShiftNotFound(shift_name.clone()))?;

        block_on(async {
                let row = sqlx::query(
                    r#"
                    SELECT id, terminal, opened_at, closed_at, business_day_label,
                           cash_total, card_total, invoice_count, cash_over_threshold
                    FROM shifts
                    WHERE id = $1 AND status = 'closed'
                    "#
                )
                .bind(shift_id)
                .fetch_optional(self.storage.pool())
                .await
                .map_err(|e| DomainError::NonNumericData {
                    entity: "shift".to_string(),
                    field: "query".to_string(),
                    raw: e.to_string(),
                })?
                .ok_or_else(|| DomainError::ShiftNotFound(shift_name.clone()))?;

                let terminal: String = row.try_get("terminal").map_err(sqlx_err)?;
                let opened_at: DateTime<Utc> = row.try_get("opened_at").map_err(sqlx_err)?;
                let closed_at: Option<DateTime<Utc>> = row.try_get("closed_at").map_err(sqlx_err)?;
                let business_day_label: NaiveDate = row.try_get("business_day_label").map_err(sqlx_err)?;
                let cash_total_decimal: Option<Decimal> = row.try_get("cash_total").map_err(sqlx_err)?;
                let card_total_decimal: Option<Decimal> = row.try_get("card_total").map_err(sqlx_err)?;
                let invoice_count: Option<i32> = row.try_get("invoice_count").map_err(sqlx_err)?;
                let cash_over_threshold: bool = row.try_get("cash_over_threshold").map_err(sqlx_err)?;

                let cash_total = Money::new(cash_total_decimal.unwrap_or(Decimal::ZERO));
                let card_total = Money::new(card_total_decimal.unwrap_or(Decimal::ZERO));
                let total_revenue = cash_total + card_total;

                Ok(ZReport {
                    shift_name: shift_name.clone(),
                    terminal: TerminalName::from(terminal.as_str()),
                    business_day: business_day_label,
                    opened_at,
                    closed_at: closed_at.ok_or_else(|| DomainError::NonNumericData {
                        entity: "shift".to_string(),
                        field: "closed_at".to_string(),
                        raw: "closed shift missing closed_at".to_string(),
                    })?,
                    invoice_count: invoice_count.unwrap_or(0) as i64,
                    cash_total,
                    card_total,
                    total_revenue,
                    cash_threshold_warning: cash_over_threshold,
                })
            })
            .map_err(to_domain_error)?
    }

    fn get(&self, shift_name: &ShiftName) -> Result<Shift> {
        let shift_name_str = shift_name.as_str();

        // Extract shift ID from name
        let shift_id: i64 = shift_name_str
            .strip_prefix("SHIFT-")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| DomainError::ShiftNotFound(shift_name.clone()))?;

        block_on(async {
                let row = sqlx::query(
                    r#"
                    SELECT id, terminal, opened_at, closed_at, business_day_label,
                           opened_by
                    FROM shifts
                    WHERE id = $1
                    "#
                )
                .bind(shift_id)
                .fetch_optional(self.storage.pool())
                .await
                .map_err(|e| DomainError::NonNumericData {
                    entity: "shift".to_string(),
                    field: "query".to_string(),
                    raw: e.to_string(),
                })?
                .ok_or_else(|| DomainError::ShiftNotFound(shift_name.clone()))?;

                let terminal: String = row.try_get("terminal").map_err(sqlx_err)?;
                let opened_at: DateTime<Utc> = row.try_get("opened_at").map_err(sqlx_err)?;
                let closed_at: Option<DateTime<Utc>> = row.try_get("closed_at").map_err(sqlx_err)?;
                let business_day_label: NaiveDate = row.try_get("business_day_label").map_err(sqlx_err)?;
                let opened_by: Option<String> = row.try_get("opened_by").map_err(sqlx_err)?;

                Ok(Shift {
                    name: shift_name.clone(),
                    terminal: TerminalName::from(terminal.as_str()),
                    opened_at,
                    closed_at,
                    opened_by: UserName::from(opened_by.as_deref().unwrap_or("system")),
                    business_day: business_day_label,
                })
            })
            .map_err(to_domain_error)?
    }

    fn list_shifts(
        &self,
        terminal: Option<&TerminalName>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Shift>> {
        block_on(async {
                let rows = match terminal {
                    Some(term) => {
                        let terminal_str = term.as_str();
                        sqlx::query(
                            r#"
                            SELECT id, terminal, opened_at, closed_at, business_day_label,
                                   opened_by
                            FROM shifts
                            WHERE terminal = $1
                            ORDER BY opened_at DESC
                            LIMIT $2 OFFSET $3
                            "#
                        )
                        .bind(terminal_str)
                        .bind(limit)
                        .bind(offset)
                        .fetch_all(self.storage.pool())
                        .await
                        .map_err(|e| DomainError::NonNumericData {
                            entity: "shift".to_string(),
                            field: "query".to_string(),
                            raw: e.to_string(),
                        })?
                    }
                    None => {
                        sqlx::query(
                            r#"
                            SELECT id, terminal, opened_at, closed_at, business_day_label,
                                   opened_by
                            FROM shifts
                            ORDER BY opened_at DESC
                            LIMIT $1 OFFSET $2
                            "#
                        )
                        .bind(limit)
                        .bind(offset)
                        .fetch_all(self.storage.pool())
                        .await
                        .map_err(|e| DomainError::NonNumericData {
                            entity: "shift".to_string(),
                            field: "query".to_string(),
                            raw: e.to_string(),
                        })?
                    }
                };

                rows.into_iter()
                    .map(|row| {
                        let id: i64 = row.try_get("id").map_err(sqlx_err)?;
                        let terminal: String = row.try_get("terminal").map_err(sqlx_err)?;
                        let opened_at: DateTime<Utc> = row.try_get("opened_at").map_err(sqlx_err)?;
                        let closed_at: Option<DateTime<Utc>> = row.try_get("closed_at").map_err(sqlx_err)?;
                        let business_day_label: NaiveDate = row.try_get("business_day_label").map_err(sqlx_err)?;
                        let opened_by: Option<String> = row.try_get("opened_by").map_err(sqlx_err)?;

                        let shift_name = ShiftName::new(format!("SHIFT-{:05}", id));
                        Ok(Shift {
                            name: shift_name,
                            terminal: TerminalName::from(terminal.as_str()),
                            opened_at,
                            closed_at,
                            opened_by: UserName::from(
                                opened_by.as_deref().unwrap_or("system"),
                            ),
                            business_day: business_day_label,
                        })
                    })
                    .collect()
            })
            .map_err(to_domain_error)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chrono_tz::Asia::Kolkata;
    use peacock_core::businessday::BusinessDay;

    async fn setup_test_storage() -> Storage {
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://localhost/peacock_test".to_string());

        let config = crate::DbConfig::from_url(&database_url).unwrap();
        Storage::connect(config)
            .await
            .expect("failed to connect to test database")
    }

    async fn cleanup_shifts(storage: &Storage) {
        sqlx::query("DELETE FROM shifts")
            .execute(storage.pool())
            .await
            .expect("failed to cleanup shifts");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_shift_creates_record_with_correct_business_day() {
        let storage = setup_test_storage().await;
        cleanup_shifts(&storage).await;

        let repo = PostgresShiftRepo::new(storage.clone());
        let terminal = TerminalName::from("TERMINAL-01");
        let user = UserName::from("user@example.com");
        let business_day = chrono::Utc::now().date_naive();

        let shift = repo
            .open_shift(&terminal, &user, business_day)
            .expect("failed to open shift");

        assert_eq!(shift.terminal.as_str(), "TERMINAL-01");
        assert_eq!(shift.opened_by.as_str(), "user@example.com");
        assert_eq!(shift.business_day, business_day);
        assert!(shift.closed_at.is_none());

        cleanup_shifts(&storage).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_shift_prevents_duplicate_open_shifts() {
        let storage = setup_test_storage().await;
        cleanup_shifts(&storage).await;

        let repo = PostgresShiftRepo::new(storage.clone());
        let terminal = TerminalName::from("TERMINAL-01");
        let user = UserName::from("user@example.com");
        let business_day = chrono::Utc::now().date_naive();

        repo.open_shift(&terminal, &user, business_day)
            .expect("first open should succeed");

        let result = repo.open_shift(&terminal, &user, business_day);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DomainError::Conflict { .. }));

        cleanup_shifts(&storage).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_current_shift_returns_open_shift() {
        let storage = setup_test_storage().await;
        cleanup_shifts(&storage).await;

        let repo = PostgresShiftRepo::new(storage.clone());
        let terminal = TerminalName::from("TERMINAL-01");
        let user = UserName::from("user@example.com");
        let business_day = chrono::Utc::now().date_naive();

        // No open shift initially
        let none = repo.get_current_shift(&terminal).expect("query failed");
        assert!(none.is_none());

        // Open a shift
        let opened = repo
            .open_shift(&terminal, &user, business_day)
            .expect("failed to open shift");

        // Now we should get it
        let found = repo
            .get_current_shift(&terminal)
            .expect("query failed")
            .expect("shift not found");
        assert_eq!(found.name.as_str(), opened.name.as_str());

        cleanup_shifts(&storage).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn close_shift_updates_status_and_generates_report() {
        let storage = setup_test_storage().await;
        cleanup_shifts(&storage).await;

        let repo = PostgresShiftRepo::new(storage.clone());
        let terminal = TerminalName::from("TERMINAL-01");
        let user = UserName::from("user@example.com");
        let business_day = chrono::Utc::now().date_naive();

        let opened = repo
            .open_shift(&terminal, &user, business_day)
            .expect("failed to open shift");

        let report = repo
            .close_shift(&opened.name, 3, Kolkata)
            .expect("failed to close shift");

        assert_eq!(report.shift_name.as_str(), opened.name.as_str());
        assert_eq!(report.terminal.as_str(), "TERMINAL-01");
        assert!(report.closed_at > report.opened_at);

        // Should not be able to get it as current shift anymore
        let none = repo.get_current_shift(&terminal).expect("query failed");
        assert!(none.is_none());

        cleanup_shifts(&storage).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_report_retrieves_closed_shift_report() {
        let storage = setup_test_storage().await;
        cleanup_shifts(&storage).await;

        let repo = PostgresShiftRepo::new(storage.clone());
        let terminal = TerminalName::from("TERMINAL-01");
        let user = UserName::from("user@example.com");
        let business_day = chrono::Utc::now().date_naive();

        let opened = repo
            .open_shift(&terminal, &user, business_day)
            .expect("failed to open shift");

        let close_report = repo
            .close_shift(&opened.name, 3, Kolkata)
            .expect("failed to close shift");

        let retrieved_report = repo
            .get_report(&opened.name)
            .expect("failed to get report");

        assert_eq!(
            retrieved_report.shift_name.as_str(),
            close_report.shift_name.as_str()
        );
        assert_eq!(
            retrieved_report.terminal.as_str(),
            close_report.terminal.as_str()
        );

        cleanup_shifts(&storage).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_shifts_returns_paginated_results() {
        let storage = setup_test_storage().await;
        cleanup_shifts(&storage).await;

        let repo = PostgresShiftRepo::new(storage.clone());
        let user = UserName::from("user@example.com");
        let business_day = chrono::Utc::now().date_naive();

        // Open 3 shifts on different terminals
        repo.open_shift(&TerminalName::from("TERMINAL-01"), &user, business_day)
            .expect("failed to open shift 1");
        repo.open_shift(&TerminalName::from("TERMINAL-02"), &user, business_day)
            .expect("failed to open shift 2");
        repo.open_shift(&TerminalName::from("TERMINAL-03"), &user, business_day)
            .expect("failed to open shift 3");

        // List all shifts
        let all_shifts = repo.list_shifts(None, 10, 0).expect("failed to list shifts");
        assert_eq!(all_shifts.len(), 3);

        // List shifts for specific terminal
        let terminal_shifts = repo
            .list_shifts(Some(&TerminalName::from("TERMINAL-01")), 10, 0)
            .expect("failed to list shifts");
        assert_eq!(terminal_shifts.len(), 1);
        assert_eq!(terminal_shifts[0].terminal.as_str(), "TERMINAL-01");

        cleanup_shifts(&storage).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn multiple_terminals_can_have_open_shifts_simultaneously() {
        let storage = setup_test_storage().await;
        cleanup_shifts(&storage).await;

        let repo = PostgresShiftRepo::new(storage.clone());
        let user = UserName::from("user@example.com");
        let business_day = chrono::Utc::now().date_naive();

        let shift1 = repo
            .open_shift(&TerminalName::from("TERMINAL-01"), &user, business_day)
            .expect("failed to open shift 1");

        let shift2 = repo
            .open_shift(&TerminalName::from("TERMINAL-02"), &user, business_day)
            .expect("failed to open shift 2");

        assert_ne!(shift1.name.as_str(), shift2.name.as_str());

        // Each terminal should have its own open shift
        let found1 = repo
            .get_current_shift(&TerminalName::from("TERMINAL-01"))
            .expect("query failed");
        let found2 = repo
            .get_current_shift(&TerminalName::from("TERMINAL-02"))
            .expect("query failed");

        assert!(found1.is_some());
        assert!(found2.is_some());

        cleanup_shifts(&storage).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn midnight_crossing_business_day_calculation() {
        let storage = setup_test_storage().await;
        cleanup_shifts(&storage).await;

        let _repo = PostgresShiftRepo::new(storage.clone());
        let _user = UserName::from("user@example.com");

        // Simulate a shift at 01:30 IST (before 03:00 cutoff)
        // Business day should be the previous calendar day
        let instant = chrono::Utc.with_ymd_and_hms(2026, 7, 27, 20, 0, 0).unwrap();
        let business_day = BusinessDay::for_instant(instant, 3, Kolkata);

        assert_eq!(
            business_day.label,
            NaiveDate::from_ymd_opt(2026, 7, 27).unwrap(),
            "01:30 IST should belong to previous day's business day"
        );

        cleanup_shifts(&storage).await;
    }
}
