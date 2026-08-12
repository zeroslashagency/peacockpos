"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { motion, AnimatePresence } from "framer-motion";
import {
  ChartBar,
  TrendUp,
  Stack,
  CookingPot,
  Heartbeat,
  Pulse,
  Database,
  UsersThree,
  Clock,
  ArrowClockwise,
  WarningCircle,
  SpinnerGap,
  Lightning,
  Receipt,
  Storefront,
} from "@phosphor-icons/react";
import { apiBase } from "@/lib/api";
import { formatMoney } from "@/lib/money";
import { useSSE } from "@/hooks/useSSE";
import { BreathingDot } from "@/components/LivePulse";

// ---------------------------------------------------------------------------
// Types — mirrors peacock-api/src/routes/dashboard.rs DashboardSummaryResponse
// Serialises Money as string, no Number.
// ---------------------------------------------------------------------------

interface UnsetItems {
  item_prices: string[];
  bundle_items: string[];
  bom_items: string[];
}

interface KotStationCount {
  production: string;
  count: number;
}

interface OpenShiftSummary {
  name: string;
  terminal: string;
  opened_at: string;
  business_day: string;
}

interface DatabaseHealth {
  connected: boolean;
  latency_ms?: number;
  pool_size?: number;
  idle_connections?: number;
  error?: string;
}

interface SystemHealth {
  database: DatabaseHealth;
  sse_subscribers: number;
}

interface DashboardSummary {
  business_day: string;
  start: string;
  end: string;
  cutoff_hour: number;
  invoice_count: number;
  excluded_invoice_count: number;
  revenue: string;
  cogs: string;
  gross_profit: string;
  gross_margin_pct: string | null;
  round_off_total: string;
  has_unset_items: boolean;
  unset: UnsetItems;
  active_orders: number;
  kot_pending: number;
  kot_by_station: KotStationCount[];
  shifts_open: number;
  open_shifts: OpenShiftSummary[];
  system: SystemHealth;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const spring = { type: "spring" as const, stiffness: 100, damping: 20 };

function KpiSkeleton() {
  return (
    <div className="rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
      <div className="h-3 w-24 animate-pulse rounded-full bg-zinc-100" />
      <div className="mt-4 h-10 w-40 animate-pulse rounded-xl bg-zinc-100" />
      <div className="mt-3 h-3 w-56 animate-pulse rounded-full bg-zinc-50" />
      <div className="mt-6 grid grid-cols-2 gap-3">
        <div className="h-20 animate-pulse rounded-2xl bg-zinc-50" />
        <div className="h-20 animate-pulse rounded-2xl bg-zinc-50" />
      </div>
    </div>
  );
}

function LiveSkeleton() {
  return (
    <div className="space-y-2">
      {[0, 1, 2].map((i) => (
        <div
          key={i}
          className="h-16 animate-pulse rounded-2xl border border-slate-200/50 bg-white"
          style={{ animationDelay: `${i * 80}ms` }}
        />
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export default function DashboardPage() {
  const [summary, setSummary] = useState<DashboardSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [lastRefresh, setLastRefresh] = useState<string | null>(null);
  const [refreshTick, setRefreshTick] = useState(0);

  const {
    events: sseEvents,
    connected,
    error: sseError,
  } = useSSE({
    events: [
      "order.created",
      "order.updated",
      "kot.generated",
      "kot.prepared",
      "invoice.paid",
      "order_update",
      "kot_update",
    ],
    maxEvents: 32,
  });

  const fetchSummary = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const url = `${apiBase()}/api/dashboard/summary`;
      const res = await fetch(url, {
        headers: { Accept: "application/json" },
        credentials: "include",
        cache: "no-store",
      });
      if (!res.ok) {
        const text = await res.text();
        let detail = text;
        try {
          const j = JSON.parse(text);
          detail = j.detail || j.title || text;
        } catch {}
        throw new Error(detail.slice(0, 400) || `HTTP ${res.status}`);
      }
      const data = (await res.json()) as DashboardSummary;
      setSummary(data);
      setLastRefresh(new Date().toLocaleTimeString("en-IN", { hour: "2-digit", minute: "2-digit" }));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  // initial + manual tick
  useEffect(() => {
    fetchSummary();
  }, [fetchSummary, refreshTick]);

  // live refresh on SSE: debounce 800ms so a burst doesn't hammer the DB
  useEffect(() => {
    if (sseEvents.length === 0) return;
    const last = sseEvents[sseEvents.length - 1];
    const k = String(last.event).toLowerCase();
    if (
      k.includes("order") ||
      k.includes("kot") ||
      k.includes("invoice") ||
      k.includes("paid") ||
      k.includes("generated")
    ) {
      const id = setTimeout(() => {
        // silent refresh without skeleton flicker
        fetchSummary();
      }, 900);
      return () => clearTimeout(id);
    }
  }, [sseEvents, fetchSummary]);

  // polling fallback when SSE disconnected
  useEffect(() => {
    if (connected) return;
    const id = setInterval(() => fetchSummary(), 12_000);
    return () => clearInterval(id);
  }, [connected, fetchSummary]);

  const isEmptyDay = useMemo(() => {
    if (!summary) return false;
    return summary.invoice_count === 0 && summary.active_orders === 0 && summary.kot_pending === 0;
  }, [summary]);

  // recent live events (newest first, max 8)
  const liveList = useMemo(() => {
    const arr = [...sseEvents].reverse().slice(0, 8);
    return arr;
  }, [sseEvents]);

  return (
    <div className="mx-auto flex min-h-[100dvh] w-full max-w-[1400px] flex-col gap-6 bg-[#f9fafb] px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      {/* Heading — anti-center, left aligned, Geist tracking-tighter */}
      <div className="flex flex-col gap-2">
        <div className="flex flex-wrap items-end justify-between gap-4">
          <div>
            <div className="flex items-center gap-2.5">
              <span className="inline-flex h-8 w-8 items-center justify-center rounded-xl bg-zinc-900 text-white shadow-sm">
                <ChartBar size={16} weight="light" />
              </span>
              <h1 className="text-3xl font-semibold tracking-tighter leading-none text-zinc-900">Dashboard</h1>
              <span className="hidden text-sm font-light tracking-tight text-zinc-400 sm:inline">· Today · live</span>
              <BreathingDot active={!!summary && summary.kot_pending > 0} />
            </div>
            <p className="mt-2 max-w-[65ch] text-sm leading-6 text-zinc-600">
              One round-trip ·{" "}
              <span className="font-mono tracking-tight text-zinc-900">[start,end)</span> in{" "}
              <span className="font-mono tracking-tight text-zinc-900">Asia/Kolkata</span> ·{" "}
              <span className="font-mono tracking-tight text-zinc-900">REVENUE</span> ={" "}
              <span className="font-mono tracking-tight text-zinc-900">rounded_total</span> + COGS by BOM/bundle parity. Live via SSE.
            </p>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <span
              className={`inline-flex items-center gap-1.5 rounded-full border px-3 py-1.5 font-mono text-xs tracking-tight shadow-sm ${
                connected
                  ? "border-emerald-200 bg-emerald-50 text-emerald-700"
                  : "border-amber-200 bg-amber-50 text-amber-700"
              }`}
            >
              <span className="relative flex h-2 w-2">
                <span
                  className={`absolute inline-flex h-full w-full rounded-full opacity-30 animate-ping ${
                    connected ? "bg-emerald-500" : "bg-amber-400"
                  }`}
                />
                <span
                  className={`relative inline-flex h-2 w-2 rounded-full ${
                    connected ? "bg-emerald-500" : "bg-amber-500"
                  }`}
                />
              </span>
              {connected ? "SSE live" : "Polling 12s"}
            </span>
            {lastRefresh && <span className="font-mono text-xs tracking-tight text-zinc-400">updated {lastRefresh}</span>}
            <motion.button
              whileTap={{ scale: 0.98 }}
              onClick={() => setRefreshTick((v) => v + 1)}
              disabled={loading}
              className="inline-flex items-center gap-1.5 rounded-full bg-zinc-900 px-4 py-2 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:bg-zinc-800 hover:shadow disabled:opacity-50"
            >
              {loading ? <SpinnerGap size={14} weight="light" className="animate-spin" /> : <ArrowClockwise size={14} weight="light" />}
              Refresh
            </motion.button>
          </div>
        </div>

        {/* inline errors — not a red slab hero */}
        <AnimatePresence>
          {(error || sseError) && (
            <motion.div
              initial={{ opacity: 0, y: 4 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: 4 }}
              transition={spring}
              className="flex flex-col gap-2"
            >
              {error && (
                <div className="inline-flex max-w-full items-center gap-2 rounded-2xl border border-red-200/60 bg-red-50 px-4 py-2.5 text-sm leading-5 text-red-700">
                  <WarningCircle size={16} weight="light" className="shrink-0" />
                  <span className="truncate font-medium tracking-tight">{error}</span>
                  <button
                    onClick={() => setRefreshTick((v) => v + 1)}
                    className="ml-2 shrink-0 rounded-full bg-white px-3 py-1 text-xs font-semibold tracking-tight text-red-700 ring-1 ring-red-200 transition hover:bg-red-50"
                  >
                    Retry
                  </button>
                </div>
              )}
              {sseError && (
                <div className="inline-flex items-center gap-2 rounded-full bg-amber-50 px-3 py-1.5 text-xs font-medium tracking-tight text-amber-700 ring-1 ring-amber-200/50">
                  <WarningCircle size={14} weight="light" /> SSE: {sseError}
                </div>
              )}
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      {/* KPI bento — asymmetric 7 / 5, 4 Daily App density */}
      {loading && !summary ? (
        <div className="grid grid-cols-12 gap-6">
          <div className="col-span-12 lg:col-span-7">
            <KpiSkeleton />
          </div>
          <div className="col-span-12 lg:col-span-5 space-y-4">
            <div className="h-[140px] animate-pulse rounded-[2.5rem] border border-slate-200/50 bg-white shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]" />
            <div className="h-[140px] animate-pulse rounded-[2.5rem] border border-slate-200/50 bg-white shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]" />
          </div>
        </div>
      ) : summary ? (
        <>
          <div className="grid grid-cols-12 gap-6">
            {/* LEFT — Today hero 7 cols */}
            <div className="col-span-12 flex flex-col gap-4 lg:col-span-7">
              {/* Revenue hero */}
              <motion.div
                initial={{ opacity: 0, y: 8 }}
                animate={{ opacity: 1, y: 0 }}
                transition={spring}
                className="flex flex-col rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] transition hover:-translate-y-[1px] hover:shadow-[0_20px_40px_-12px_rgba(0,0,0,0.08)]"
              >
                <div className="flex items-start justify-between gap-3">
                  <div>
                    <div className="text-[11px] font-semibold uppercase tracking-widest text-zinc-400">Today · revenue</div>
                    <div className="mt-1 flex items-baseline gap-2">
                      <span className="font-mono text-5xl font-semibold tracking-tighter leading-none text-zinc-900 md:text-6xl">
                        {formatMoney(summary.revenue)}
                      </span>
                      {summary.gross_margin_pct && (
                        <span className="rounded-full bg-emerald-50 px-2.5 py-1 text-xs font-medium tracking-tight text-emerald-700 ring-1 ring-emerald-200/40">
                          {summary.gross_margin_pct}% margin
                        </span>
                      )}
                    </div>
                    <div className="mt-2 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
                      <span className="inline-flex items-center gap-1.5 rounded-full bg-[#f9fafb] px-2.5 py-1 font-mono tracking-tight text-zinc-700 ring-1 ring-slate-200/50">
                        <Receipt size={12} weight="light" /> {summary.invoice_count} invoices
                      </span>
                      <span className="text-zinc-300">·</span>
                      <span className="font-mono tracking-tight">{summary.business_day}</span>
                      <span className="text-zinc-300">cutoff</span>
                      <span className="font-mono tracking-tight">{summary.cutoff_hour}:00 IST</span>
                    </div>
                    {summary.has_unset_items && (
                      <div className="mt-3 inline-flex items-center gap-1.5 rounded-2xl bg-amber-50 px-3 py-2 text-xs leading-5 text-amber-800 ring-1 ring-amber-200/50">
                        <WarningCircle size={14} weight="light" /> COGS understates — missing buying prices for{" "}
                        <span className="font-mono font-medium">
                          {[...summary.unset.item_prices, ...summary.unset.bundle_items, ...summary.unset.bom_items]
                            .slice(0, 4)
                            .join(", ")}
                        </span>
                      </div>
                    )}
                  </div>
                  <span className="hidden items-center gap-1.5 rounded-full bg-zinc-900 px-3 py-1.5 text-xs font-medium tracking-tight text-white shadow-sm sm:inline-flex">
                    <ChartBar size={12} weight="light" /> REVENUE
                  </span>
                </div>

                {/* airy stats — not boxed triad */}
                <div className="mt-6 grid grid-cols-3 gap-3">
                  <div className="rounded-2xl bg-[#f9fafb] p-4 ring-1 ring-slate-200/40">
                    <div className="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-widest text-zinc-400">
                      <ChartBar size={12} weight="light" /> Revenue
                    </div>
                    <div className="mt-1 font-mono text-lg font-semibold tracking-tighter text-zinc-900">{formatMoney(summary.revenue)}</div>
                    <div className="text-[11px] leading-4 text-zinc-400">{summary.invoice_count} counted · {summary.excluded_invoice_count} excluded</div>
                  </div>
                  <div className="rounded-2xl bg-[#f9fafb] p-4 ring-1 ring-slate-200/40">
                    <div className="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-widest text-zinc-400">
                      <Stack size={12} weight="light" /> COGS
                    </div>
                    <div className="mt-1 font-mono text-lg font-semibold tracking-tighter text-zinc-900">{formatMoney(summary.cogs)}</div>
                    <div className="text-[11px] leading-4 text-zinc-400">bundle → BOM → plain</div>
                  </div>
                  <div className="rounded-2xl bg-emerald-50 p-4 ring-1 ring-emerald-200/40">
                    <div className="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-widest text-emerald-700">
                      <TrendUp size={12} weight="light" /> Gross
                    </div>
                    <div className="mt-1 font-mono text-lg font-semibold tracking-tighter text-emerald-900">{formatMoney(summary.gross_profit)}</div>
                    <div className="text-[11px] leading-4 text-emerald-700/60">{summary.gross_margin_pct ? `${summary.gross_margin_pct}% margin` : "no margin — no revenue"}</div>
                  </div>
                </div>

                <div className="mt-4 flex flex-wrap items-center gap-2 text-xs text-zinc-400">
                  <span className="font-mono tracking-tight">round_off {formatMoney(summary.round_off_total)}</span>
                  <span className="h-1 w-1 rounded-full bg-slate-300" />
                  <span className="font-mono tracking-tight">
                    {summary.start.slice(0, 10)} → {summary.end.slice(0, 10)} half-open
                  </span>
                </div>
              </motion.div>

              {/* Secondary KPI — active orders + KOT */}
              <div className="grid grid-cols-2 gap-4">
                <motion.div
                  initial={{ opacity: 0, y: 8 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ ...spring, delay: 0.06 }}
                  className="rounded-[2.5rem] border border-slate-200/50 bg-white p-6 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]"
                >
                  <div className="flex items-center gap-2 text-[11px] font-semibold uppercase tracking-widest text-zinc-400">
                    <Storefront size={14} weight="light" /> Active orders
                  </div>
                  <div className="mt-3 flex items-baseline gap-2">
                    <span className="text-4xl font-semibold tracking-tighter leading-none text-zinc-900">{summary.active_orders}</span>
                    <span className="text-sm font-medium tracking-tight text-zinc-500">open</span>
                  </div>
                  <div className="mt-1 text-xs leading-4 text-zinc-400">cancelled × &amp; last_invoice × excluded</div>
                  <Link
                    href="/pos"
                    className="mt-4 inline-flex items-center gap-1 rounded-full border border-slate-200 bg-white px-3 py-1.5 text-xs font-medium tracking-tight text-zinc-700 hover:bg-zinc-50"
                  >
                    Open POS
                  </Link>
                </motion.div>

                <motion.div
                  initial={{ opacity: 0, y: 8 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ ...spring, delay: 0.12 }}
                  className="rounded-[2.5rem] border border-slate-200/50 bg-white p-6 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]"
                >
                  <div className="flex items-center justify-between gap-2">
                    <div className="flex items-center gap-2 text-[11px] font-semibold uppercase tracking-widest text-zinc-400">
                      <CookingPot size={14} weight="light" /> KOT backlog
                    </div>
                    <BreathingDot active={summary.kot_pending > 0} />
                  </div>
                  <div className="mt-3 flex items-baseline gap-2">
                    <span className="text-4xl font-semibold tracking-tighter leading-none text-zinc-900">{summary.kot_pending}</span>
                    <span className="text-sm font-medium tracking-tight text-zinc-500">pending</span>
                  </div>
                  <div className="mt-1 text-xs leading-4 text-zinc-400">
                    {summary.kot_by_station.length ? summary.kot_by_station.slice(0, 2).map((s) => `${s.production} ${s.count}`).join(" · ") : "no pending KOTs"}
                  </div>
                  <Link
                    href="/kds"
                    className="mt-4 inline-flex items-center gap-1 rounded-full bg-zinc-900 px-3 py-1.5 text-xs font-semibold tracking-tight text-white hover:bg-zinc-800"
                  >
                    Open KDS
                  </Link>
                </motion.div>
              </div>
            </div>

            {/* RIGHT stack — 5 cols · Shifts + round_off + kot by station */}
            <div className="col-span-12 flex flex-col gap-4 lg:col-span-5">
              {/* Gross detail — border-t not boxed */}
              <motion.div
                initial={{ opacity: 0, y: 8 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ ...spring, delay: 0.08 }}
                className="rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]"
              >
                <h2 className="flex items-center gap-2 text-sm font-semibold tracking-tighter text-zinc-900">
                  <TrendUp size={16} weight="light" className="text-emerald-600" /> Gross today
                </h2>
                <p className="mt-1 text-xs leading-5 text-zinc-500">Revenue − COGS · money as string via decimal.js</p>

                <div className="mt-4 overflow-hidden rounded-2xl border border-slate-200/50">
                  <div className="grid grid-cols-2 divide-x divide-slate-200/50">
                    <div className="bg-[#f9fafb] p-4">
                      <div className="text-[11px] font-semibold uppercase tracking-widest text-zinc-400">Revenue</div>
                      <div className="mt-1 font-mono text-sm font-semibold tracking-tighter text-zinc-900">{formatMoney(summary.revenue)}</div>
                      <div className="text-xs text-zinc-400">{summary.invoice_count} invoices</div>
                    </div>
                    <div className="bg-[#f9fafb] p-4">
                      <div className="text-[11px] font-semibold uppercase tracking-widest text-zinc-400">COGS</div>
                      <div className="mt-1 font-mono text-sm font-semibold tracking-tighter text-zinc-900">{formatMoney(summary.cogs)}</div>
                      <div className="text-xs text-zinc-400">{summary.has_unset_items ? "⚠ unset prices" : "all priced"}</div>
                    </div>
                  </div>
                  <div className="flex items-center justify-between bg-white px-4 py-3">
                    <span className="text-xs font-semibold uppercase tracking-widest text-zinc-400">Gross profit</span>
                    <span className="font-mono text-lg font-semibold tracking-tighter text-zinc-900">{formatMoney(summary.gross_profit)}</span>
                  </div>
                </div>

                {summary.kot_by_station.length > 0 && (
                  <div className="mt-4">
                    <div className="text-[11px] font-semibold uppercase tracking-widest text-zinc-400">Pending by station</div>
                    <div className="mt-2 flex flex-wrap gap-1.5">
                      {summary.kot_by_station.map((s) => (
                        <span
                          key={s.production}
                          className="inline-flex items-center gap-1.5 rounded-full border border-slate-200/60 bg-[#f9fafb] px-2.5 py-1 text-xs font-medium tracking-tight text-zinc-700"
                        >
                          <span className="h-1.5 w-1.5 rounded-full bg-amber-500" />
                          {s.production} <span className="font-mono font-semibold text-zinc-900">{s.count}</span>
                        </span>
                      ))}
                    </div>
                  </div>
                )}

                {summary.open_shifts.length > 0 && (
                  <div className="mt-5 border-t border-slate-100 pt-4">
                    <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-widest text-zinc-400">
                      <UsersThree size={14} weight="light" /> Open shifts
                      <span className="rounded-full bg-zinc-900 px-2 py-0.5 font-mono text-[11px] font-semibold tracking-tight text-white">
                        {summary.shifts_open}
                      </span>
                    </div>
                    <div className="mt-3 space-y-2">
                      {summary.open_shifts.slice(0, 3).map((sh) => (
                        <div key={sh.name} className="flex items-center justify-between rounded-2xl bg-[#f9fafb] px-3 py-2.5">
                          <div>
                            <div className="font-mono text-sm font-semibold tracking-tight text-zinc-900">{sh.name}</div>
                            <div className="text-xs tracking-tight text-zinc-500">
                              {sh.terminal} · {sh.business_day}
                            </div>
                          </div>
                          <span className="font-mono text-xs tracking-tight text-zinc-400">{new Date(sh.opened_at).toLocaleTimeString("en-IN", { hour: "2-digit", minute: "2-digit" })}</span>
                        </div>
                      ))}
                    </div>
                    <Link
                      href="/shifts"
                      className="mt-3 inline-flex text-xs font-semibold tracking-tight text-zinc-900 underline decoration-slate-300 underline-offset-4 hover:decoration-zinc-900"
                    >
                      View shifts →
                    </Link>
                  </div>
                )}
                {summary.open_shifts.length === 0 && (
                  <div className="mt-5 rounded-2xl border border-dashed border-slate-200/70 bg-[#f9fafb] px-4 py-3 text-center">
                    <div className="text-xs font-medium tracking-tight text-zinc-700">No open shift</div>
                    <div className="text-xs leading-5 text-zinc-500">Open a shift on a terminal to see Z-report here.</div>
                  </div>
                )}
              </motion.div>

              {/* Compact SSE status */}
              <div className="rounded-[2.5rem] border border-slate-200/50 bg-white p-6 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
                <h3 className="flex items-center gap-2 text-sm font-semibold tracking-tighter text-zinc-900">
                  <Lightning size={14} weight="light" className="text-amber-500" /> SSE bus
                </h3>
                <div className="mt-3 flex items-center gap-2 text-xs">
                  <span className="inline-flex items-center gap-1.5 rounded-full bg-zinc-900 px-2.5 py-1 font-mono tracking-tight text-white">
                    <Pulse size={12} weight="light" /> {summary.system.sse_subscribers} subscribers
                  </span>
                  <span className="font-mono tracking-tight text-zinc-400">{connected ? "connected" : "reconnecting…"}</span>
                </div>
                <p className="mt-2 text-xs leading-5 text-zinc-500">
                  Branch updates · <span className="font-mono tracking-tight">order.created kot.generated invoice.paid</span> ·{" "}
                  <span className="font-mono tracking-tight">GET /api/events/stream</span>
                </p>
              </div>
            </div>
          </div>

          {/* Second row — Live SSE + System health asymmetric 7 / 5 */}
          <div className="grid grid-cols-12 gap-6">
            {/* LIVE — 7 cols */}
            <section className="col-span-12 lg:col-span-7">
              <div className="flex h-full flex-col rounded-[2.5rem] border border-slate-200/50 bg-white p-6 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] sm:p-8">
                <div className="flex items-start justify-between gap-4">
                  <div>
                    <h2 className="flex items-center gap-2 text-lg font-semibold tracking-tighter leading-none text-zinc-900">
                      <Heartbeat size={18} weight="light" className="text-emerald-600" /> Live
                      <span className="rounded-full bg-zinc-900 px-2.5 py-1 font-mono text-xs font-semibold tracking-tight text-white">
                        {sseEvents.length}
                      </span>
                    </h2>
                    <p className="mt-1.5 max-w-[55ch] text-sm leading-5 text-zinc-500">
                      <span className="font-mono tracking-tight text-zinc-900">order.created · kot.generated · invoice.paid</span> — newest first.
                    </p>
                  </div>
                  <span className="hidden items-center gap-1.5 rounded-full bg-[#f9fafb] px-3 py-1.5 text-xs font-medium tracking-tight text-zinc-600 ring-1 ring-slate-200/50 sm:inline-flex">
                    <Pulse size={12} weight="light" /> {connected ? "connected" : "polling"}
                  </span>
                </div>

                <div className="mt-6 min-h-[180px]">
                  {liveList.length === 0 ? (
                    <div className="flex flex-col items-center rounded-2xl border border-dashed border-slate-200/70 bg-[#f9fafb] px-6 py-10 text-center">
                      <span className="flex h-12 w-12 items-center justify-center rounded-2xl bg-white text-zinc-400 ring-1 ring-slate-200/50">
                        <Pulse size={20} weight="light" />
                      </span>
                      <div className="mt-3 text-sm font-semibold tracking-tighter text-zinc-900">Waiting for live events</div>
                      <div className="mx-auto mt-1 max-w-[34ch] text-sm leading-6 text-zinc-500">
                        Create an order or fire a KOT from <Link href="/pos" className="font-medium text-zinc-900 underline decoration-slate-300 underline-offset-4 hover:decoration-zinc-900">POS</Link> to see it stream here. Falls back to 12s polling when SSE is off.
                      </div>
                    </div>
                  ) : (
                    <div className="space-y-2">
                      <AnimatePresence initial={false}>
                        {liveList.map((evt) => {
                          const kind = String(evt.event);
                          const isOrder = kind.includes("order");
                          const isKot = kind.includes("kot");
                          const isPaid = kind.includes("paid") || kind.includes("invoice");
                          return (
                            <motion.div
                              key={evt.id + kind + evt.raw.slice(0, 16)}
                              layout
                              initial={{ opacity: 0, y: 6 }}
                              animate={{ opacity: 1, y: 0 }}
                              exit={{ opacity: 0, y: -6 }}
                              transition={spring}
                              className="flex items-start justify-between gap-3 rounded-2xl border border-slate-200/50 bg-[#f9fafb] px-4 py-3"
                            >
                              <div className="min-w-0">
                                <div className="flex items-center gap-2">
                                  <span
                                    className={`inline-flex h-2 w-2 rounded-full ${
                                      isPaid ? "bg-emerald-500" : isKot ? "bg-amber-500" : "bg-sky-500"
                                    }`}
                                  />
                                  <span className="font-mono text-xs font-semibold tracking-tight text-zinc-900">{kind}</span>
                                  <span className="font-mono text-[11px] tracking-tight text-zinc-400">#{evt.id}</span>
                                  {isOrder && <span className="rounded-full bg-white px-2 py-0.5 text-[11px] font-medium tracking-tight text-zinc-600 ring-1 ring-slate-200/50">order</span>}
                                  {isKot && <span className="rounded-full bg-white px-2 py-0.5 text-[11px] font-medium tracking-tight text-zinc-600 ring-1 ring-slate-200/50">kot</span>}
                                  {isPaid && <span className="rounded-full bg-emerald-600 px-2 py-0.5 text-[11px] font-semibold tracking-tight text-white">paid</span>}
                                </div>
                                <div className="mt-1 truncate font-mono text-xs leading-5 text-zinc-600">{evt.raw.slice(0, 120)}</div>
                              </div>
                              <span className="shrink-0 font-mono text-[11px] tracking-tight text-zinc-400">live</span>
                            </motion.div>
                          );
                        })}
                      </AnimatePresence>
                    </div>
                  )}
                </div>

                <div className="mt-6 flex flex-wrap items-center justify-between gap-2 border-t border-zinc-100 pt-4 text-xs">
                  <span className="inline-flex items-center gap-1.5 tracking-tight text-zinc-400">
                    <Lightning size={12} weight="light" /> EventSource + retry 3s · keep-alive 15s · replays missed on reconnect
                  </span>
                  <span className="font-mono tracking-tight text-zinc-400">stream pauses on gap → refetch</span>
                </div>
              </div>
            </section>

            {/* SYSTEM HEALTH — 5 cols */}
            <section className="col-span-12 flex flex-col gap-4 lg:col-span-5">
              <div className="rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
                <h2 className="flex items-center gap-2 text-sm font-semibold tracking-tighter text-zinc-900">
                  <Database size={16} weight="light" className="text-zinc-900" /> System health
                </h2>
                <p className="mt-1 text-xs leading-5 text-zinc-500">
                  Hetzner <span className="font-mono tracking-tight text-zinc-700">cpx12 · 2.28.30.22:8080</span> · Postgres 16 · Vercel rewrites
                </p>

                {/* DB row */}
                <div className="mt-5 rounded-2xl border border-slate-200/50 bg-[#f9fafb] p-4">
                  <div className="flex items-center justify-between gap-2">
                    <span className="text-xs font-semibold uppercase tracking-widest text-zinc-400">Database</span>
                    <span
                      className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium tracking-tight ${
                        summary.system.database.connected
                          ? "bg-emerald-50 text-emerald-700 ring-1 ring-emerald-200/40"
                          : "bg-red-50 text-red-700 ring-1 ring-red-200/40"
                      }`}
                    >
                      <span className={`h-1.5 w-1.5 rounded-full ${summary.system.database.connected ? "bg-emerald-500" : "bg-red-500"}`} />
                      {summary.system.database.connected ? "connected" : "unavailable"}
                    </span>
                  </div>
                  <div className="mt-3 grid grid-cols-3 gap-2 text-center">
                    <div className="rounded-xl bg-white p-3 ring-1 ring-slate-200/50">
                      <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-400">Latency</div>
                      <div className="mt-1 font-mono text-sm font-semibold tracking-tighter text-zinc-900">
                        {summary.system.database.latency_ms != null ? `${summary.system.database.latency_ms}ms` : "—"}
                      </div>
                    </div>
                    <div className="rounded-xl bg-white p-3 ring-1 ring-slate-200/50">
                      <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-400">Pool</div>
                      <div className="mt-1 font-mono text-sm font-semibold tracking-tighter text-zinc-900">
                        {summary.system.database.pool_size ?? "—"}
                      </div>
                    </div>
                    <div className="rounded-xl bg-white p-3 ring-1 ring-slate-200/50">
                      <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-400">Idle</div>
                      <div className="mt-1 font-mono text-sm font-semibold tracking-tighter text-zinc-900">
                        {summary.system.database.idle_connections ?? "—"}
                      </div>
                    </div>
                  </div>
                  {summary.system.database.error && (
                    <div className="mt-3 rounded-xl bg-red-50 px-3 py-2 font-mono text-xs leading-5 text-red-700 ring-1 ring-red-200/40">
                      {summary.system.database.error}
                    </div>
                  )}
                </div>

                {/* SSE + API */}
                <div className="mt-4 grid grid-cols-2 gap-3">
                  <div className="rounded-2xl bg-zinc-900 p-4 text-white">
                    <div className="text-[11px] font-semibold uppercase tracking-widest text-zinc-400">SSE bus</div>
                    <div className="mt-1 flex items-baseline gap-1">
                      <span className="text-2xl font-semibold tracking-tighter">{summary.system.sse_subscribers}</span>
                      <span className="text-xs tracking-tight text-zinc-400">clients</span>
                    </div>
                    <div className="mt-1 flex items-center gap-1.5 text-xs text-zinc-400">
                      <span className={`h-1.5 w-1.5 rounded-full ${connected ? "bg-emerald-400 animate-pulse" : "bg-amber-400"}`} />
                      {connected ? "EventSource live" : "polling fallback"}
                    </div>
                  </div>
                  <div className="rounded-2xl bg-[#f9fafb] p-4 ring-1 ring-slate-200/50">
                    <div className="text-[11px] font-semibold uppercase tracking-widest text-zinc-400">API</div>
                    <div className="mt-1 font-mono text-xs font-medium tracking-tight text-zinc-900">/api/dashboard/summary</div>
                    <div className="mt-1 font-mono text-xs tracking-tight text-zinc-500">/health · /health/ready</div>
                    <div className="mt-2 flex gap-1.5">
                      <a href={`${apiBase()}/health`} target="_blank" rel="noreferrer" className="rounded-full bg-white px-2.5 py-1 text-xs font-medium tracking-tight text-zinc-700 ring-1 ring-slate-200/50 hover:bg-zinc-50">
                        /health
                      </a>
                      <a href={`${apiBase()}/health/ready`} target="_blank" rel="noreferrer" className="rounded-full bg-white px-2.5 py-1 text-xs font-medium tracking-tight text-zinc-700 ring-1 ring-slate-200/50 hover:bg-zinc-50">
                        /ready
                      </a>
                    </div>
                  </div>
                </div>

                <div className="mt-4 flex items-center gap-2 text-xs text-zinc-400">
                  <Clock size={12} weight="light" />
                  <span className="font-mono tracking-tight">Business day {summary.business_day} · {summary.start.slice(11, 19)}Z → {summary.end.slice(11, 19)}Z · cutoff {summary.cutoff_hour}:00 IST</span>
                </div>
              </div>

              {/* isEmpty day CTA */}
              {isEmptyDay && (
                <div className="rounded-[2.5rem] border border-dashed border-slate-200/70 bg-white p-6 text-center shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
                  <div className="mx-auto flex h-10 w-10 items-center justify-center rounded-full bg-[#f9fafb] text-zinc-400 ring-1 ring-slate-200/50">
                    <Receipt size={18} weight="light" />
                  </div>
                  <div className="mt-3 text-sm font-semibold tracking-tighter text-zinc-900">No invoices today</div>
                  <div className="mx-auto mt-1 max-w-[28ch] text-xs leading-5 text-zinc-500">
                    Today’s window <span className="font-mono tracking-tight text-zinc-700">{summary.business_day}</span> has no revenue yet — fire an order from POS to seed it.
                  </div>
                  <Link
                    href="/pos"
                    className="mt-4 inline-flex items-center justify-center rounded-full bg-zinc-900 px-5 py-2 text-sm font-semibold tracking-tight text-white hover:bg-zinc-800"
                  >
                    Go to POS
                  </Link>
                </div>
              )}
            </section>
          </div>

          {/* negative space helper — not a card */}
          <p className="px-1 pt-2 text-center text-xs leading-5 tracking-tight text-zinc-400">
            Range is half-open <span className="font-mono tracking-tight text-zinc-600">[start,end)</span> in{" "}
            <span className="font-mono tracking-tight text-zinc-600">Asia/Kolkata</span> — invoices at{" "}
            <span className="font-mono tracking-tight text-zinc-600">end</span> belong to the next day ·{" "}
            <span className="font-mono tracking-tight text-zinc-600">GET /api/dashboard/summary</span> reuses POS Invoice + BOM/bundle parity · SSE{" "}
            <span className="font-mono tracking-tight text-zinc-600">kot.generated invoice.paid</span>
          </p>
        </>
      ) : null}

      {loading && summary && (
        <div className="flex items-center gap-2 text-xs text-zinc-400">
          <SpinnerGap size={14} weight="light" className="animate-spin" /> Refreshing…
        </div>
      )}
    </div>
  );
}
