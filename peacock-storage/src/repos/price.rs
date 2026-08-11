//! PostgreSQL implementation of [`PriceRepo`].
//!
//! # What a missing price means
//!
//! [`PriceRepo::item_price`] returns `Result<Option<Money>>`, and the `Option` is the
//! whole design. Upstream's COGS walk accumulates `unset_bom_items` so the operator sees
//! which ingredients have no price (`ury_daily_p_and_l.py`, GROUND-TRUTH.md §"BOM / COGS
//! walk"); it does not abort the day's P&L because one ingredient was never priced. So a
//! missing row is `Ok(None)`, never an error, and never `Money::ZERO` — zero is a real
//! price meaning "free", and collapsing the two would silently value an unpriced
//! ingredient at nothing and quietly overstate margin.
//!
//! # Precedence
//!
//! `item_prices` has a dated history per (item, price_list): `valid_from` is nullable and
//! part of the uniqueness key, where NULL is the open-ended base rate
//! (001_core_tables.sql). The lookup therefore has to choose among several live rows, and
//! it does so in this order:
//!
//! 1. **Rows whose validity window excludes the effective date are out.** `valid_from`
//!    in the future or `valid_upto` in the past means the row does not apply today.
//! 2. **The most specific applicable row wins**: latest `valid_from`, NULLs last. A
//!    dated override beats the open-ended base rate, which is what a dated rate is for.
//! 3. **Ties break on `id` descending** — the later-inserted row. The unique index makes
//!    a tie impossible for one (item, price_list, valid_from), so this only guards
//!    against a future schema change and keeps the result deterministic regardless.
//!
//! # Multi-pricelist precedence
//!
//! `item_price` answers for exactly one price list, because the caller knows which one it
//! means: COGS reads the *buying* list (`ury_daily_p_and_l.py:30`), never stock
//! valuation, and the aggregator path reads a *selling* list (api.py:829). Choosing
//! between lists is [`PgPriceRepo::item_price_with_fallback`], where the caller states
//! the order — a repository guessing "specific, else default" would silently price COGS
//! off the wrong basis the first time a list was misconfigured.

use peacock_core::error::Result;
use peacock_core::ids::{ItemCode, PriceListName};
use peacock_core::money::Money;
use peacock_core::ports::PriceRepo;
use rust_decimal::Decimal;
use sqlx::PgPool;

use crate::error::{StorageError, StorageResult};
use crate::repos::blocking::block_on;
use crate::repos::to_domain_error;

/// The columns and precedence shared by every price lookup in this module.
///
/// `$3` is the effective date. Casting it to `date` lets one query serve both "as of
/// today" (`CURRENT_DATE`) and "as of a business day" without a second statement.
const PRICE_PRECEDENCE_SQL: &str = r#"
    SELECT rate
    FROM item_prices
    WHERE item_code = $1
      AND price_list = $2
      AND (valid_from IS NULL OR valid_from <= $3::date)
      AND (valid_upto IS NULL OR valid_upto >= $3::date)
    ORDER BY valid_from DESC NULLS LAST, id DESC
    LIMIT 1
"#;

/// [`PriceRepo`] over a Postgres pool.
#[derive(Clone, Debug)]
pub struct PgPriceRepo {
    pool: PgPool,
}

impl PgPriceRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The rate for `item` on `price_list`, effective today.
    ///
    /// `Ok(None)` when no row applies — see the module docs on why that is not an error.
    pub async fn item_price_async(
        &self,
        item: &ItemCode,
        price_list: &PriceListName,
    ) -> StorageResult<Option<Money>> {
        self.item_price_on_async(item, price_list, None).await
    }

    /// Same lookup, as of `on` rather than today.
    ///
    /// `None` means today (`CURRENT_DATE`), evaluated by the server so a client clock
    /// cannot move a price.
    pub async fn item_price_on_async(
        &self,
        item: &ItemCode,
        price_list: &PriceListName,
        on: Option<chrono::NaiveDate>,
    ) -> StorageResult<Option<Money>> {
        let effective = match on {
            Some(d) => d,
            // `CURRENT_DATE` would be cleaner, but binding keeps one SQL string for both
            // paths. The server's date is what a NULL resolves to below.
            None => self.server_date().await?,
        };

        let rate: Option<Decimal> = sqlx::query_scalar(PRICE_PRECEDENCE_SQL)
            .bind(item.as_str())
            .bind(price_list.as_str())
            .bind(effective)
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?;

        Ok(rate.map(Money::new))
    }

    /// Walk `price_lists` in order and return the first price found, with the list that
    /// supplied it.
    ///
    /// This is the multi-pricelist precedence rule, and the caller owns the order:
    /// `&[branch_list, default_list]` prefers the branch's own price and falls back to
    /// the default. `Ok(None)` when no list in the chain prices the item.
    ///
    /// One query per list, short-circuiting on the first hit. The chain is a handful of
    /// entries (branch, then default), so this is bounded and does not grow with the
    /// number of items.
    pub async fn item_price_with_fallback_async(
        &self,
        item: &ItemCode,
        price_lists: &[PriceListName],
        on: Option<chrono::NaiveDate>,
    ) -> StorageResult<Option<(PriceListName, Money)>> {
        // Resolve the date once so every list in the chain is priced as of the same day.
        // Re-reading CURRENT_DATE per list could straddle midnight and mix two days.
        let effective = match on {
            Some(d) => d,
            None => self.server_date().await?,
        };

        for list in price_lists {
            if let Some(money) = self.item_price_on_async(item, list, Some(effective)).await? {
                return Ok(Some((list.clone(), money)));
            }
        }
        Ok(None)
    }

    /// Batched lookup: one query for many items on one price list.
    ///
    /// The per-item version inside a loop is the N+1 shape that bugs 6 and 7 are
    /// (GROUND-TRUTH.md). A COGS run prices every ingredient of every BOM, so it uses
    /// this. Items with no applicable row are simply absent from the map — the caller
    /// treats a missing key exactly as it treats `Ok(None)`.
    pub async fn item_prices_batch_async(
        &self,
        items: &[ItemCode],
        price_list: &PriceListName,
        on: Option<chrono::NaiveDate>,
    ) -> StorageResult<std::collections::HashMap<ItemCode, Money>> {
        if items.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let effective = match on {
            Some(d) => d,
            None => self.server_date().await?,
        };

        let codes: Vec<String> = items.iter().map(|i| i.as_str().to_owned()).collect();

        // DISTINCT ON collapses each item's dated history to its single winning row using
        // the same precedence as the scalar lookup. The ORDER BY must lead with the
        // DISTINCT ON key, which is why item_code repeats there.
        let rows: Vec<(String, Decimal)> = sqlx::query_as(
            r#"
            SELECT DISTINCT ON (item_code) item_code, rate
            FROM item_prices
            WHERE item_code = ANY($1)
              AND price_list = $2
              AND (valid_from IS NULL OR valid_from <= $3::date)
              AND (valid_upto IS NULL OR valid_upto >= $3::date)
            ORDER BY item_code, valid_from DESC NULLS LAST, id DESC
            "#,
        )
        .bind(&codes)
        .bind(price_list.as_str())
        .bind(effective)
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(rows
            .into_iter()
            .map(|(code, rate)| (ItemCode::new(code), Money::new(rate)))
            .collect())
    }

    /// The database server's current date.
    ///
    /// Read from the server, not from the process clock: a POS terminal with a skewed
    /// clock must not be able to select yesterday's price.
    async fn server_date(&self) -> StorageResult<chrono::NaiveDate> {
        sqlx::query_scalar("SELECT CURRENT_DATE")
            .fetch_one(&self.pool)
            .await
            .map_err(StorageError::from)
    }

    /// Blocking [`item_price_with_fallback_async`](Self::item_price_with_fallback_async).
    ///
    /// See [`crate::repos::blocking`] for the calling contexts this is legal from.
    pub fn item_price_with_fallback(
        &self,
        item: &ItemCode,
        price_lists: &[PriceListName],
    ) -> Result<Option<(PriceListName, Money)>> {
        block_on(self.item_price_with_fallback_async(item, price_lists, None))
            .map_err(to_domain_error)
    }
}

impl PriceRepo for PgPriceRepo {
    fn item_price(&self, item: &ItemCode, price_list: &PriceListName) -> Result<Option<Money>> {
        block_on(self.item_price_async(item, price_list)).map_err(to_domain_error)
    }
}
