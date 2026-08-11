"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Link from "next/link";
import { motion, AnimatePresence } from "framer-motion";
import {
  CookingPot,
  Timer,
  CheckCircle,
  Fire,
  WarningCircle,
  ArrowsClockwise,
  ChefHat,
  Clock,
  Receipt,
  SpinnerGap,
} from "@phosphor-icons/react";
import { kotApi, type KotDto } from "@/lib/api";
import { useSSE } from "@/hooks/useSSE";
import KotCarousel from "@/components/KotCarousel";
import { BreathingDot, PopBadge } from "@/components/LivePulse";

// ---------------------------------------------------------------------------
const STATION_PRESETS = [
  "Hot Kitchen",
  "Tandoor",
  "Cold Kitchen",
  "Bar",
  "Grill",
  "Pantry",
  "Main Kitchen",
] as const;

function elapsedLabel(timeStr: string | null, dateStr: string): string {
  if (!timeStr) return "—";
  try {
    const parts = timeStr.split(":");
    if (parts.length < 2) return timeStr;
    const hh = Number(parts[0]);
    const mm = Number(parts[1]);
    const ss = Number(parts[2] ?? 0);
    const kotDate = new Date(`${dateStr}T00:00:00`);
    kotDate.setHours(hh, mm, ss, 0);
    const diffMs = Date.now() - kotDate.getTime();
    if (diffMs < 0) return "now";
    const mins = Math.floor(diffMs / 60000);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m ago`;
    const hrs = Math.floor(mins / 60);
    const rem = mins % 60;
    return `${hrs}h ${rem}m ago`;
  } catch {
    return timeStr;
  }
}

function timeDisplay(k: KotDto): string {
  if (k.time) return k.time.slice(0, 5);
  if (k.date) return k.date;
  return "—";
}

// ---------------------------------------------------------------------------
// Kot card — tactile, layout + stagger
// ---------------------------------------------------------------------------

function TicketCard({
  kot,
  onBump,
  bumping,
  index,
}: {
  kot: KotDto;
  onBump: (id: string) => void;
  bumping: boolean;
  index: number;
}) {
  const qtyTotal = useMemo(() => {
    try {
      return kot.items.reduce((acc, it) => acc + Number(it.quantity), 0);
    } catch {
      return kot.items.length;
    }
  }, [kot.items]);

  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: 8 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: 6 }}
      transition={{ type: "spring", stiffness: 100, damping: 20, delay: index * 0.04 }}
      className="group flex min-h-[320px] w-[340px] shrink-0 flex-col rounded-[2rem] border border-slate-200/50 bg-white shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] transition will-change-transform hover:-translate-y-[1px] hover:shadow-[0_20px_40px_-12px_rgba(0,0,0,0.08)]"
    >
      {/* header */}
      <div className="flex items-start justify-between gap-2 border-b border-zinc-100 px-5 py-4">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="truncate font-mono text-sm font-semibold tracking-tight text-zinc-900">{kot.id}</span>
            {kot.kot_type && (
              <span
                className={`shrink-0 rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-widest ${
                  kot.kot_type === "New Order"
                    ? "bg-emerald-50 text-emerald-700 border border-emerald-200/50"
                    : kot.kot_type === "Order Modified"
                      ? "bg-sky-50 text-sky-700 border border-sky-200/50"
                      : kot.kot_type === "Cancelled"
                        ? "bg-red-50 text-red-600 border border-red-200/50"
                        : "bg-zinc-50 text-zinc-500 border border-zinc-200/60"
                }`}
              >
                {kot.kot_type}
              </span>
            )}
          </div>
          <div className="mt-1.5 flex flex-wrap items-center gap-1.5 text-xs text-zinc-500">
            <span className="font-mono tracking-tight text-zinc-600">{kot.invoice}</span>
            <span className="text-zinc-300">·</span>
            <span className="inline-flex items-center gap-1">
              <Clock size={12} weight="light" className="text-zinc-400" />
              {timeDisplay(kot)}
            </span>
            <span className="text-zinc-300">·</span>
            <span className="text-[11px] text-zinc-400">{elapsedLabel(kot.time ?? null, kot.date)}</span>
          </div>
        </div>
        <span
          className={`shrink-0 rounded-full px-2.5 py-1 text-[11px] font-medium tracking-tight ${
            kot.is_aggregator
              ? "bg-zinc-900 text-white"
              : "border border-slate-200/60 bg-zinc-50 text-zinc-600"
          }`}
        >
          {kot.is_aggregator ? (kot.aggregator_id ?? "AGG") : kot.table_takeaway ? "Takeaway" : "Dine-in"}
        </span>
      </div>

      {/* meta */}
      <div className="grid grid-cols-2 gap-2 px-5 py-3">
        <div className="rounded-2xl bg-[#f9fafb] px-3 py-2.5">
          <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-400">Table</div>
          <div className="mt-0.5 truncate text-sm font-medium tracking-tight text-zinc-900">
            {kot.restaurant_table ?? "—"}
            {kot.order_no ? <span className="ml-1 font-mono text-xs text-zinc-500">· {kot.order_no}</span> : null}
          </div>
        </div>
        <div className="rounded-2xl bg-[#f9fafb] px-3 py-2.5">
          <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-400">Production</div>
          <div className="mt-0.5 flex items-center gap-1.5 truncate text-sm font-medium tracking-tight text-zinc-900">
            <span className="truncate">{kot.production ?? "—"}</span>
            <span className="inline-flex shrink-0 items-center rounded-full bg-white px-2 py-0.5 font-mono text-[11px] font-medium text-zinc-600 shadow-sm ring-1 ring-slate-200/50">
              {qtyTotal} pcs
            </span>
          </div>
        </div>
        {kot.customer_name && (
          <div className="col-span-2 truncate pt-1 text-xs leading-5 text-zinc-500">
            <span className="font-medium tracking-tight text-zinc-900">{kot.customer_name}</span>
            {kot.branch ? <span className="text-zinc-400"> · {kot.branch}</span> : null}
            {kot.comments ? <span className="text-zinc-500"> — {kot.comments}</span> : null}
          </div>
        )}
      </div>

      {/* items */}
      <div className="flex-1 px-5 pb-2">
        <div className="space-y-2">
          {kot.items.map((it, idx) => (
            <div
              key={`${it.item}-${idx}`}
              className="flex items-start justify-between gap-2 rounded-2xl border border-slate-200/50 bg-white px-3 py-2.5 shadow-sm"
            >
              <div className="min-w-0">
                <div className="flex items-center gap-1.5">
                  <span className="truncate text-sm font-medium leading-none tracking-tight text-zinc-900">{it.item_name}</span>
                  {it.course && (
                    <span className="shrink-0 rounded-full bg-[#f9fafb] px-1.5 py-0.5 text-[10px] font-medium tracking-wide text-zinc-500 ring-1 ring-slate-200/50">
                      {it.course}
                    </span>
                  )}
                  {it.indicate_course && <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-emerald-500" />}
                </div>
                <div className="mt-1 flex flex-wrap items-center gap-1.5 text-xs text-zinc-500">
                  <span className="font-mono tracking-tight text-zinc-600">{it.item}</span>
                  {it.comments && <span className="italic text-zinc-500">“{it.comments}”</span>}
                  {it.serve_priority ? (
                    <span className="rounded-full bg-amber-50 px-1.5 py-0.5 text-[10px] font-semibold tracking-wide text-amber-700 ring-1 ring-amber-200/50">
                      P{it.serve_priority}
                    </span>
                  ) : null}
                </div>
              </div>
              <span className="shrink-0 rounded-full bg-zinc-900 px-2.5 py-1 font-mono text-xs font-semibold tracking-tight text-white">
                ×{it.quantity}
              </span>
            </div>
          ))}
        </div>
      </div>

      {/* footer tactile */}
      <div className="flex items-center justify-between gap-2 border-t border-zinc-100 px-5 py-4">
        <div className="min-w-0 text-[11px] leading-4 text-zinc-400">
          <span className="truncate">{kot.pos_profile ?? "—"}</span>
          {kot.verified && kot.verified_by ? (
            <span className="ml-1.5 inline-flex items-center gap-1 font-medium text-emerald-600">
              <CheckCircle size={12} weight="light" /> {kot.verified_by}
            </span>
          ) : null}
        </div>
        <motion.button
          onClick={() => onBump(kot.id)}
          disabled={bumping}
          whileTap={{ scale: 0.98 }}
          transition={{ type: "spring", stiffness: 100, damping: 20 }}
          className="inline-flex shrink-0 items-center justify-center gap-1.5 rounded-full bg-emerald-600 px-5 py-2 text-xs font-semibold tracking-tight text-white shadow-sm transition hover:bg-emerald-700 hover:shadow-md active:translate-y-px disabled:cursor-not-allowed disabled:opacity-50"
        >
          {bumping ? (
            <>
              <SpinnerGap size={14} weight="light" className="animate-spin" /> Bumping…
            </>
          ) : (
            <>
              <CheckCircle size={14} weight="light" /> Mark prepared
            </>
          )}
        </motion.button>
      </div>
    </motion.div>
  );
}

// Skeleton matching card
function TicketSkeleton({ index }: { index: number }) {
  return (
    <div
      className="flex h-[320px] w-[340px] shrink-0 flex-col rounded-[2rem] border border-slate-200/50 bg-white p-5 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]"
      style={{ animationDelay: `calc(${index}*60ms)` } as React.CSSProperties}
    >
      <div className="shimmer h-4 w-24 rounded-full bg-zinc-100" />
      <div className="mt-3 h-3 w-40 rounded-full bg-zinc-50" />
      <div className="mt-6 grid grid-cols-2 gap-2">
        <div className="h-16 rounded-2xl bg-zinc-50" />
        <div className="h-16 rounded-2xl bg-zinc-50" />
      </div>
      <div className="mt-4 space-y-2">
        <div className="h-12 rounded-2xl bg-zinc-50" />
        <div className="h-12 rounded-2xl bg-zinc-50" />
      </div>
      <div className="mt-auto h-9 rounded-full bg-zinc-100" />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export default function KdsPage() {
  const [station, setStation] = useState<string>("Hot Kitchen");
  const [stationInput, setStationInput] = useState<string>("Hot Kitchen");
  const [tickets, setTickets] = useState<KotDto[]>([]);
  const [preparedLocal, setPreparedLocal] = useState<KotDto[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [bumpingId, setBumpingId] = useState<string | null>(null);
  const [lastRefresh, setLastRefresh] = useState<string | null>(null);
  const [popCount, setPopCount] = useState(0);
  const [showPop, setShowPop] = useState(false);
  const prevLen = useRef(0);

  const { events, connected, error: sseError } = useSSE({
    events: ["kot.generated", "kot.prepared", "kot_update", "kot.submitted", "kot.modified"],
    maxEvents: 80,
  });

  const fetchTickets = useCallback(async () => {
    const s = station.trim();
    if (!s) return;
    setLoading(true);
    setError(null);
    try {
      const res = await kotApi.pending(s);
      const next = res.kots;
      // pop detection
      if (next.length > prevLen.current && prevLen.current !== 0) {
        const delta = next.length - prevLen.current;
        setPopCount(delta);
        setShowPop(true);
        setTimeout(() => setShowPop(false), 3000);
      }
      prevLen.current = next.length;
      setTickets(next);
      setLastRefresh(new Date().toLocaleTimeString("en-IN", { hour: "2-digit", minute: "2-digit" }));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [station]);

  useEffect(() => {
    fetchTickets();
  }, [fetchTickets]);

  useEffect(() => {
    if (events.length === 0) return;
    const last = events[events.length - 1];
    const kind = String(last.event).toLowerCase();
    if (kind.includes("kot") || kind.includes("generated") || kind.includes("prepared") || kind === "kot_update") {
      fetchTickets();
    }
  }, [events, fetchTickets]);

  useEffect(() => {
    if (connected) return;
    const id = setInterval(fetchTickets, 10_000);
    return () => clearInterval(id);
  }, [connected, fetchTickets]);

  const handleStationLoad = () => {
    const trimmed = stationInput.trim();
    if (!trimmed) return;
    setStation(trimmed);
    setPreparedLocal([]);
    prevLen.current = 0;
  };

  const handleBump = async (kotId: string) => {
    setBumpingId(kotId);
    try {
      const updated = await kotApi.markPrepared(kotId, {});
      const bumped = tickets.find((t) => t.id === kotId) ?? updated;
      setTickets((prev) => prev.filter((t) => t.id !== kotId));
      setPreparedLocal((prev) => [
        { ...bumped, start_time_prep: updated.start_time_prep ?? new Date().toTimeString().slice(0, 8) },
        ...prev,
      ].slice(0, 12));
      // adjust prevLen
      prevLen.current = Math.max(0, prevLen.current - 1);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBumpingId(null);
      fetchTickets();
    }
  };

  const preparing = useMemo(
    () => tickets.filter((t) => t.items.some((it) => it.serve_priority > 0 || it.indicate_course)),
    [tickets]
  );
  const pendingOnly = useMemo(() => {
    const ids = new Set(preparing.map((p) => p.id));
    return tickets.filter((k) => !ids.has(k.id));
  }, [tickets, preparing]);

  const totalPending = tickets.length;

  return (
    <div className="mx-auto flex min-h-[100dvh] w-full max-w-[1400px] flex-col gap-6 bg-[#f9fafb] px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      {/* Page heading — left aligned, anti-center */}
      <div className="flex flex-col gap-2">
        <div className="flex flex-wrap items-end justify-between gap-4">
          <div>
            <div className="flex items-center gap-2.5">
              <span className="inline-flex h-8 w-8 items-center justify-center rounded-xl bg-zinc-900 text-white">
                <CookingPot size={16} weight="light" />
              </span>
              <h1 className="text-3xl font-semibold tracking-tighter leading-none text-zinc-900">Kitchen</h1>
              <span className="hidden text-sm font-light tracking-tight text-zinc-400 sm:inline">· Fire &amp; bump</span>
            </div>
            <p className="mt-2 max-w-[65ch] text-sm leading-6 text-zinc-600">
              Station-filtered live board. Bump is idempotent — double-tap is safe. Polls every 10s when SSE is off.
            </p>
          </div>
          <div className="flex items-center gap-2 text-xs">
            <span className="inline-flex items-center gap-1.5 rounded-full border border-slate-200/60 bg-white px-3 py-1.5 font-mono tracking-tight text-zinc-600">
              <span className="relative flex h-2 w-2">
                <span className={`absolute inline-flex h-full w-full rounded-full ${connected ? "bg-emerald-500 opacity-30" : "bg-amber-400 opacity-30"} animate-ping`} />
                <span className={`relative inline-flex h-2 w-2 rounded-full ${connected ? "bg-emerald-500" : "bg-amber-500"}`} />
              </span>
              {connected ? "SSE live" : "Polling"}
            </span>
            {lastRefresh && <span className="font-mono text-zinc-400">updated {lastRefresh}</span>}
          </div>
        </div>
        {(sseError || error) && (
          <div className="flex flex-wrap gap-2">
            {sseError && (
              <span className="inline-flex items-center gap-1.5 rounded-full bg-amber-50 px-3 py-1.5 text-xs font-medium tracking-tight text-amber-700 ring-1 ring-amber-200/50">
                <WarningCircle size={14} weight="light" /> SSE: {sseError}
              </span>
            )}
            {error && (
              <span className="inline-flex max-w-full items-center gap-1.5 rounded-full bg-red-50 px-3 py-1.5 text-xs font-medium tracking-tight text-red-700 ring-1 ring-red-200/60">
                <WarningCircle size={14} weight="light" /> {error}
              </span>
            )}
          </div>
        )}
      </div>

      {/* Asymmetric 1+2 */}
      <div className="grid grid-cols-12 gap-6">
        {/* LEFT — Live Status */}
        <section className="col-span-12 flex flex-col gap-4 lg:col-span-4">
          <div className="rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
            <div className="flex items-start justify-between gap-3">
              <div className="flex items-center gap-2.5">
                <span className="inline-flex h-9 w-9 items-center justify-center rounded-full bg-zinc-900 text-white">
                  <Fire size={16} weight="light" />
                </span>
                <div>
                  <h2 className="text-sm font-semibold tracking-tighter leading-none text-zinc-900">Live Status</h2>
                  <p className="mt-0.5 text-xs leading-4 text-zinc-500">Station · {station}</p>
                </div>
              </div>
              <BreathingDot active={totalPending > 0} />
            </div>

            {/* Hero number */}
            <div className="mt-6 flex items-baseline gap-3">
              <span className="text-5xl font-semibold tracking-tighter leading-none text-zinc-900 md:text-6xl">
                {loading && totalPending === 0 ? "—" : totalPending}
              </span>
              <span className="text-sm font-medium tracking-tight text-zinc-500">pending</span>
              <span className="ml-1">
                <PopBadge count={popCount} show={showPop} />
              </span>
            </div>
            <div className="mt-1 flex items-center gap-1.5 text-xs text-zinc-400">
              <Clock size={12} weight="light" /> fire queue · {pendingOnly.length} awaiting · {preparing.length} firing
            </div>

            {/* stats — airy, not boxed triad */}
            <div className="mt-6 grid grid-cols-3 gap-3">
              <div className="rounded-2xl bg-[#f9fafb] p-4">
                <div className="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-widest text-zinc-400">
                  <Timer size={12} weight="light" /> Pending
                </div>
                <div className="mt-1 font-mono text-xl font-semibold tracking-tighter text-zinc-900">{pendingOnly.length}</div>
                <div className="text-[11px] leading-4 text-zinc-400">awaiting fire</div>
              </div>
              <div className="rounded-2xl bg-[#f9fafb] p-4">
                <div className="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-widest text-zinc-400">
                  <Fire size={12} weight="light" /> Firing
                </div>
                <div className="mt-1 font-mono text-xl font-semibold tracking-tighter text-zinc-900">{preparing.length}</div>
                <div className="text-[11px] leading-4 text-zinc-400">priority / course</div>
              </div>
              <div className="rounded-2xl bg-emerald-50 p-4 ring-1 ring-emerald-200/40">
                <div className="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-widest text-emerald-700">
                  <CheckCircle size={12} weight="light" /> Done
                </div>
                <div className="mt-1 font-mono text-xl font-semibold tracking-tighter text-emerald-900">{preparedLocal.length}</div>
                <div className="text-[11px] leading-4 text-emerald-700/60">this session</div>
              </div>
            </div>

            {/* station picker */}
            <div className="mt-6 flex flex-col gap-2">
              <label className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Station — production unit</label>
              <div className="flex gap-2">
                <input
                  value={stationInput}
                  onChange={(e) => setStationInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") handleStationLoad();
                  }}
                  placeholder="Hot Kitchen"
                  list="kds-stations"
                  className="w-full rounded-full border border-slate-200/60 bg-white px-4 py-2.5 text-sm tracking-tight outline-none placeholder:text-zinc-400 focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10"
                />
                <datalist id="kds-stations">
                  {STATION_PRESETS.map((s) => (
                    <option key={s} value={s} />
                  ))}
                </datalist>
                <motion.button
                  whileTap={{ scale: 0.98 }}
                  onClick={handleStationLoad}
                  className="shrink-0 rounded-full bg-zinc-900 px-5 py-2.5 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:bg-zinc-800"
                >
                  Load
                </motion.button>
              </div>
              <div className="flex flex-wrap gap-1.5">
                {STATION_PRESETS.map((s) => (
                  <button
                    key={s}
                    onClick={() => {
                      setStationInput(s);
                      setStation(s);
                      setPreparedLocal([]);
                      prevLen.current = 0;
                    }}
                    className={`rounded-full border px-2.5 py-1 text-xs font-medium tracking-tight transition ${
                      station === s
                        ? "border-zinc-900 bg-zinc-900 text-white"
                        : "border-slate-200/60 bg-white text-zinc-600 hover:bg-zinc-50"
                    }`}
                  >
                    {s}
                  </button>
                ))}
              </div>
            </div>

            <div className="mt-6 flex items-center gap-2">
              <motion.button
                whileTap={{ scale: 0.98 }}
                onClick={fetchTickets}
                disabled={loading}
                className="inline-flex items-center gap-1.5 rounded-full border border-slate-200/60 bg-white px-4 py-2 text-sm font-medium tracking-tight text-zinc-700 transition hover:bg-zinc-50 disabled:opacity-50"
              >
                {loading ? <SpinnerGap size={14} weight="light" className="animate-spin" /> : <ArrowsClockwise size={14} weight="light" />}
                Refresh
              </motion.button>
              <span className="font-mono text-xs tracking-tight text-zinc-400">{totalPending} tickets · {station}</span>
            </div>

            <p className="mt-4 text-xs leading-5 text-zinc-400">
              Filter <span className="font-mono tracking-tight text-zinc-600">production = {JSON.stringify(station)}</span> via{" "}
              <span className="font-mono tracking-tight">GET /api/production-units/:id/pending-kots</span>
            </p>
          </div>

          {/* Prepared mini timeline — border-t not boxed */}
          <div className="rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
            <h3 className="flex items-center gap-2 text-sm font-semibold tracking-tighter text-zinc-900">
              <CheckCircle size={16} weight="light" className="text-emerald-500" /> Bumped this session
              <span className="rounded-full bg-zinc-900 px-2 py-0.5 font-mono text-xs font-semibold tracking-tight text-white">
                {preparedLocal.length}
              </span>
            </h3>
            <p className="mt-1 text-xs leading-5 text-zinc-500">Clears on station switch or refresh.</p>

            <div className="mt-4">
              {preparedLocal.length === 0 ? (
                <div className="rounded-2xl border border-dashed border-slate-200/70 bg-[#f9fafb] p-6 text-center">
                  <div className="mx-auto flex h-9 w-9 items-center justify-center rounded-full bg-white text-zinc-400 ring-1 ring-slate-200/50">
                    <Receipt size={16} weight="light" />
                  </div>
                  <div className="mt-2 text-sm font-medium tracking-tight text-zinc-700">No bumps yet</div>
                  <div className="mx-auto mt-1 max-w-[28ch] text-xs leading-5 text-zinc-500">
                    Bumped tickets appear here and are filtered server-side as <span className="font-mono">order_status = Prepared</span>.
                  </div>
                </div>
              ) : (
                <div className="divide-y divide-zinc-100 border-t border-zinc-100">
                  <AnimatePresence initial={false}>
                    {preparedLocal.map((kot) => (
                      <motion.div
                        key={kot.id}
                        initial={{ opacity: 0, y: 6 }}
                        animate={{ opacity: 1, y: 0 }}
                        exit={{ opacity: 0, y: -6 }}
                        transition={{ type: "spring", stiffness: 100, damping: 20 }}
                        className="flex items-center justify-between gap-3 py-3"
                      >
                        <div className="min-w-0">
                          <div className="flex items-center gap-2">
                            <span className="font-mono text-sm font-semibold tracking-tight text-zinc-900">{kot.id}</span>
                            <span className="rounded-full bg-emerald-50 px-2 py-0.5 text-[11px] font-medium tracking-tight text-emerald-700 ring-1 ring-emerald-200/40">
                              Prepared {kot.start_time_prep ? `· ${kot.start_time_prep}` : ""}
                            </span>
                          </div>
                          <div className="mt-0.5 truncate text-xs text-zinc-500">
                            {kot.invoice} · {kot.restaurant_table ?? "—"} · {kot.items.length} items
                          </div>
                        </div>
                        <span className="hidden shrink-0 font-mono text-xs tracking-tight text-zinc-400 sm:inline">
                          {kot.items.slice(0, 2).map((i) => i.item_name).join(" · ")}
                        </span>
                      </motion.div>
                    ))}
                  </AnimatePresence>
                </div>
              )}
            </div>
          </div>
        </section>

        {/* RIGHT — Wide Data Stream */}
        <section className="col-span-12 flex min-h-[520px] flex-col lg:col-span-8">
          <div className="flex-1 rounded-[2.5rem] border border-slate-200/50 bg-white p-6 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] sm:p-8">
            <div className="flex flex-wrap items-start justify-between gap-4">
              <div>
                <h2 className="flex items-center gap-2 text-lg font-semibold tracking-tighter leading-none text-zinc-900">
                  <ChefHat size={18} weight="light" className="text-zinc-900" /> Fire queue
                  <span className="rounded-full bg-zinc-900 px-2.5 py-1 font-mono text-xs font-semibold tracking-tight text-white">
                    {totalPending}
                  </span>
                </h2>
                <p className="mt-1.5 max-w-[60ch] text-sm leading-5 text-zinc-500">
                  <span className="font-mono tracking-tight text-zinc-900">{station}</span> · infinite stream{" "}
                  <span className="font-mono text-xs tracking-tight">x 0% → -100%</span> — hover to read, bump to clear.
                </p>
              </div>
              <div className="flex items-center gap-2">
                <span className="hidden items-center gap-1.5 rounded-full bg-[#f9fafb] px-3 py-1.5 text-xs font-medium tracking-tight text-zinc-600 ring-1 ring-slate-200/50 sm:inline-flex">
                  <Timer size={14} weight="light" /> {pendingOnly.length} pending · {preparing.length} firing
                </span>
                <Link
                  href="/pos"
                  className="inline-flex items-center gap-1 rounded-full border border-slate-200/60 bg-white px-3 py-1.5 text-xs font-medium tracking-tight text-zinc-700 hover:bg-zinc-50"
                >
                  Open POS
                </Link>
              </div>
            </div>

            {/* stream */}
            <div className="mt-6">
              {loading && totalPending === 0 ? (
                <div className="flex gap-4 overflow-hidden">
                  {[0, 1, 2].map((i) => (
                    <TicketSkeleton key={i} index={i} />
                  ))}
                </div>
              ) : totalPending === 0 ? (
                <motion.div
                  initial={{ opacity: 0, y: 8 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ type: "spring", stiffness: 100, damping: 20 }}
                  className="flex flex-col items-center justify-center rounded-[2rem] border border-dashed border-slate-200/70 bg-[#f9fafb] px-6 py-14 text-center"
                >
                  <span className="flex h-14 w-14 items-center justify-center rounded-full bg-white text-zinc-400 shadow-sm ring-1 ring-slate-200/50">
                    <CookingPot size={24} weight="light" />
                  </span>
                  <div className="mt-4 text-base font-semibold tracking-tighter text-zinc-900">Kitchen clear</div>
                  <div className="mx-auto mt-1 max-w-[32ch] text-sm leading-6 text-zinc-500">
                    No pending KOTs for <span className="font-mono font-medium tracking-tight text-zinc-700">{station}</span>. Fire from POS to see tickets stream here.
                  </div>
                  <div className="mt-5 flex gap-2">
                    <motion.button
                      whileTap={{ scale: 0.98 }}
                      onClick={fetchTickets}
                      className="rounded-full bg-zinc-900 px-5 py-2 text-sm font-semibold tracking-tight text-white hover:bg-zinc-800"
                    >
                      Refresh
                    </motion.button>
                    <Link
                      href="/pos"
                      className="rounded-full border border-slate-200/60 bg-white px-5 py-2 text-sm font-medium tracking-tight text-zinc-700 hover:bg-zinc-50"
                    >
                      Go to POS
                    </Link>
                  </div>
                </motion.div>
              ) : totalPending > 2 ? (
                // Wide Data Stream — perpetual carousel
                <KotCarousel enabled>
                  {tickets.map((kot, i) => (
                    <TicketCard key={kot.id} kot={kot} onBump={handleBump} bumping={bumpingId === kot.id} index={i} />
                  ))}
                </KotCarousel>
              ) : (
                // Few tickets — static grid with stagger, not carousel
                <motion.div
                  className="flex flex-wrap gap-4"
                  initial="hidden"
                  animate="show"
                  variants={{ hidden: {}, show: { transition: { staggerChildren: 0.08 } } }}
                >
                  {tickets.map((kot, i) => (
                    <TicketCard key={kot.id} kot={kot} onBump={handleBump} bumping={bumpingId === kot.id} index={i} />
                  ))}
                </motion.div>
              )}
            </div>

            {/* helper footer */}
            <div className="mt-6 flex flex-wrap items-center justify-between gap-2 border-t border-zinc-100 pt-4 text-xs">
              <span className="inline-flex items-center gap-1.5 tracking-tight text-zinc-400">
                <Fire size={12} weight="light" /> Stagger 80ms · spring 100/20 · layout · tactile bump
              </span>
              <span className="font-mono tracking-tight text-zinc-400">stream pauses on hover (scroll)</span>
            </div>
          </div>

          {/* legibility strip — not a card: negative space */}
          <p className="px-2 pt-4 text-center text-xs leading-5 tracking-tight text-zinc-400">
            KOTs filtered by <span className="font-mono text-zinc-600">production = {JSON.stringify(station)}</span> ·{" "}
            <span className="font-mono">GET /api/production-units/:id/pending-kots</span> · SSE{" "}
            <span className="font-mono">kot.generated</span> → burst deduped · Poll fallback 10s
          </p>
        </section>
      </div>
    </div>
  );
}
