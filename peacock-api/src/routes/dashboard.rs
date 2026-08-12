//! Dashboard summary — owner live view.
//!
//! Single round-trip for `peacock-web/src/app/dashboard/page.tsx`:
//! revenue (REVENUE = rounded_total where status in PosInvoiceStatus::REVENUE),
//! COGS (BOM/bundle parity), active orders, KOT backlog, open shifts, system.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;

use peacock_core::businessday::BusinessDay;
use peacock_core::ids::ItemCode;

use crate::dto::reports::UnsetItems;
use crate::error::ApiResult;
use crate::middleware::auth::{CallerContext, Role};
use crate::state::AppState;
use crate::routes::reports::REPORT_TZ;
use crate::routes::cogs::{aggregate_cogs, CogsAggregate};
use crate::routes::reports::summarise_revenue;

use peacock_core::money::Money;
use peacock_storage::repos::{BomSnapshot, BundleSnapshot};

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/dashboard/summary", get(summary))
}

#[derive(Debug, serde::Serialize)]
pub struct DashboardSummary {
    pub business_day: String,
    pub start: String,
    pub end: String,
    pub cutoff_hour: u32,
    pub invoice_count: usize,
    pub excluded_invoice_count: usize,
    pub revenue: String,
    pub cogs: String,
    pub gross_profit: String,
    pub gross_margin_pct: Option<String>,
    pub round_off_total: String,
    pub has_unset_items: bool,
    pub unset: UnsetItems,
    pub active_orders: usize,
    pub kot_pending: usize,
    pub kot_by_station: Vec<KotStationCount>,
    pub shifts_open: usize,
    pub open_shifts: Vec<OpenShiftSummary>,
    pub system: SystemHealth,
}

#[derive(Debug, serde::Serialize)]
pub struct KotStationCount {
    pub production: String,
    pub count: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct OpenShiftSummary {
    pub name: String,
    pub terminal: String,
    pub opened_at: String,
    pub business_day: String,
}

#[derive(Debug, serde::Serialize)]
pub struct SystemHealth {
    pub database: DatabaseHealth,
    pub sse_subscribers: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct DatabaseHealth {
    pub connected: bool,
    pub latency_ms: Option<u64>,
    pub pool_size: Option<u32>,
    pub idle_connections: Option<usize>,
    pub error: Option<String>,
}

async fn summary(
    caller: CallerContext,
    State(state): State<AppState>,
) -> ApiResult<Json<DashboardSummary>> {
    // owner/dev only
    if !caller.has_role(Role::Owner) && !caller.has_role(Role::Dev) {
        // has_role is hierarchical, Owner includes Dev? Actually Dev=4 > Owner=3, so Owner has_role Owner true, Dev has_role Owner true via level
        // But to be explicit, check level
        if caller.role.level() < Role::Owner.level() {
            return Err(crate::error::ApiError::forbidden("dashboard requires owner role"));
        }
    }

    let cutoff_hour = 3u32;
    let now = Utc::now();
    let day = BusinessDay::for_instant(now, cutoff_hour, REPORT_TZ);

    let storage = state.storage();
    let invoice_repo = storage.invoice_repo();
    let invoices = invoice_repo.summaries_between(day.start, day.end).await.unwrap_or_default();
    let lines = invoice_repo.revenue_lines_between(day.start, day.end).await.unwrap_or_default();

    // revenue
    let rev = summarise_revenue(&invoices, &day);

    // COGS
    let distinct: Vec<ItemCode> = {
        let mut seen = std::collections::HashSet::new();
        lines.iter().filter_map(|l| if seen.insert(l.item_code.clone()) { Some(l.item_code.clone()) } else { None }).collect()
    };
    let bundle_snapshot = storage.bundle_repo().snapshot(&distinct).await.unwrap_or_else(|_| BundleSnapshot::from_map(Default::default()));
    let mut bom_seed = distinct.clone();
    bom_seed.extend(bundle_snapshot.child_items());
    {
        let mut seen = std::collections::HashSet::new();
        bom_seed.retain(|c| seen.insert(c.clone()));
    }
    let bom_snapshot = storage.bom_repo().snapshot_for_items(&bom_seed).await.unwrap_or_else(|_| BomSnapshot::from_map(Default::default()));
    let price_repo = storage.price_repo();
    let cogs_agg = aggregate_cogs(&lines, &state.config().buying_price_list, &bundle_snapshot, &bom_snapshot, &price_repo).unwrap_or_else(|_| CogsAggregate { total: Money::ZERO, items: vec![], unset: UnsetItems::default() });

    let gross_profit = rev.revenue - cogs_agg.total;
    let gross_margin_pct = {
        if rev.revenue.is_zero() { None } else {
            let pct = (gross_profit.inner() / rev.revenue.inner()) * rust_decimal::Decimal::from(100);
            Some(pct.round_dp_with_strategy(2, peacock_core::money::ROUNDING).to_string())
        }
    };

    // active orders
    let active_orders = {
        let pool = storage.pool();
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM orders WHERE status = 'open'")
            .fetch_one(pool)
            .await
            .unwrap_or(0) as usize
    };

    // kot pending
    let kot_pending = {
        let pool = storage.pool();
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM kots WHERE status = 'pending' OR status IS NULL")
            .fetch_one(pool)
            .await
            .unwrap_or(0) as usize
    };
    let kot_by_station: Vec<KotStationCount> = {
        let pool = storage.pool();
        let rows = sqlx::query_as::<_, (String, i64)>("SELECT production, count(*) FROM kots WHERE status = 'pending' GROUP BY production")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
        rows.into_iter().map(|(p,c)| KotStationCount { production: p, count: c as usize }).collect()
    };

    // shifts open
    let open_shifts = {
        let pool = storage.pool();
        let rows = sqlx::query_as::<_, (String, String, String, String)>("SELECT name, terminal, opened_at::text, business_day FROM shifts WHERE closed_at IS NULL LIMIT 5")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
        rows.into_iter().map(|(n,t,o,b)| OpenShiftSummary { name: n, terminal: t, opened_at: o, business_day: b }).collect::<Vec<_>>()
    };
    let shifts_open = open_shifts.len();

    // system health
    let db_health = match storage.health_check().await {
        Ok(h) => DatabaseHealth { connected: true, latency_ms: Some(h.latency.as_millis() as u64), pool_size: Some(h.pool_size), idle_connections: Some(h.idle_connections), error: None },
        Err(e) => DatabaseHealth { connected: false, latency_ms: None, pool_size: None, idle_connections: None, error: Some(e.to_string()) },
    };
    let sse_subscribers = state.events().subscriber_count();

    Ok(Json(DashboardSummary {
        business_day: day.label.to_string(),
        start: day.start.to_rfc3339(),
        end: day.end.to_rfc3339(),
        cutoff_hour,
        invoice_count: rev.invoice_count,
        excluded_invoice_count: rev.excluded_invoice_count,
        revenue: rev.revenue.to_string(),
        cogs: cogs_agg.total.to_string(),
        gross_profit: gross_profit.to_string(),
        gross_margin_pct,
        round_off_total: rev.round_off_total.to_string(),
        has_unset_items: cogs_agg.has_unset_items(),
        unset: cogs_agg.unset,
        active_orders,
        kot_pending,
        kot_by_station,
        shifts_open,
        open_shifts,
        system: SystemHealth { database: db_health, sse_subscribers },
    }))
}
