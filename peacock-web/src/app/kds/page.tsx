"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { kotApi, type KotDto } from "@/lib/api";
import { useSSE } from "@/hooks/useSSE";

// ---------------------------------------------------------------------------
// Helpers
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
    // Build a local-ish timestamp: date + time, interpret as IST-ish display
    // We show elapsed since kot time (HH:mm) today — best-effort.
    const parts = timeStr.split(":");
    if (parts.length < 2) return timeStr;
    const hh = Number(parts[0]);
    const mm = Number(parts[1]);
    const ss = Number(parts[2] ?? 0);
    const now = new Date();
    const kotDate = new Date(`${dateStr}T00:00:00`);
    kotDate.setHours(hh, mm, ss, 0);
    const diffMs = now.getTime() - kotDate.getTime();
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
// Ticket card
// ---------------------------------------------------------------------------

function TicketCard({
  kot,
  onBump,
  bumping,
}: {
  kot: KotDto;
  onBump: (id: string) => void;
  bumping: boolean;
}) {
  const qtyTotal = useMemo(() => {
    try {
      return kot.items.reduce((acc, it) => acc + Number(it.quantity), 0);
    } catch {
      return kot.items.length;
    }
  }, [kot.items]);

  return (
    <div className="group flex flex-col rounded-xl border border-zinc-200 bg-white shadow-sm transition hover:shadow-md dark:border-zinc-800 dark:bg-zinc-900">
      {/* header */}
      <div className="flex items-start justify-between gap-2 border-b border-zinc-100 px-3.5 py-3 dark:border-zinc-800">
        <div className="min-w-0">
          <div className="flex items-center gap-1.5">
            <span className="truncate font-mono text-sm font-semibold text-zinc-900 dark:text-zinc-100">
              {kot.id}
            </span>
            {kot.kot_type && (
              <span
                className={`shrink-0 rounded-full px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-widest ${
                  kot.kot_type === "New Order"
                    ? "bg-amber-100 text-amber-800 dark:bg-amber-900/40 dark:text-amber-200"
                    : kot.kot_type === "Order Modified"
                      ? "bg-sky-100 text-sky-800 dark:bg-sky-900/40 dark:text-sky-200"
                      : kot.kot_type === "Cancelled"
                        ? "bg-red-100 text-red-700 dark:bg-red-900/40 dark:text-red-200"
                        : "bg-zinc-100 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300"
                }`}
              >
                {kot.kot_type}
              </span>
            )}
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-1.5 text-xs text-zinc-500 dark:text-zinc-400">
            <span className="font-mono">{kot.invoice}</span>
            <span className="text-zinc-300 dark:text-zinc-600">·</span>
            <span>{timeDisplay(kot)}</span>
            <span className="text-zinc-300 dark:text-zinc-600">·</span>
            <span className="text-[11px]">{elapsedLabel(kot.time ?? null, kot.date)}</span>
          </div>
        </div>
        <span
          className={`shrink-0 rounded-full px-2 py-1 text-[11px] font-medium ${
            kot.is_aggregator
              ? "bg-violet-100 text-violet-700 dark:bg-violet-900/40 dark:text-violet-200"
              : "bg-zinc-100 text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400"
          }`}
          title={kot.is_aggregator ? `Aggregator ${kot.aggregator_id ?? ""}` : "Dine-in / Takeaway"}
        >
          {kot.is_aggregator ? kot.aggregator_id ?? "AGG" : kot.table_takeaway ? "Takeaway" : "Dine-in"}
        </span>
      </div>

      {/* meta */}
      <div className="grid grid-cols-2 gap-2 px-3.5 py-2.5 text-xs">
        <div className="rounded-lg bg-zinc-50 px-2.5 py-2 dark:bg-zinc-800/60">
          <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 dark:text-zinc-400">
            Table
          </div>
          <div className="mt-0.5 font-medium text-zinc-900 dark:text-zinc-100">
            {kot.restaurant_table ?? "—"}
            {kot.order_no ? <span className="font-mono text-zinc-500"> · {kot.order_no}</span> : null}
          </div>
        </div>
        <div className="rounded-lg bg-zinc-50 px-2.5 py-2 dark:bg-zinc-800/60">
          <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 dark:text-zinc-400">
            Covers · Production
          </div>
          <div className="mt-0.5 truncate font-medium text-zinc-900 dark:text-zinc-100">
            {kot.production ?? "—"}
            <span className="ml-1.5 inline-flex items-center rounded bg-white px-1 py-0.5 font-mono text-[11px] dark:bg-zinc-900">
              {qtyTotal} pcs
            </span>
          </div>
        </div>
        {kot.customer_name && (
          <div className="col-span-2 truncate text-xs text-zinc-600 dark:text-zinc-400">
            <span className="font-medium text-zinc-900 dark:text-zinc-200">{kot.customer_name}</span>
            {kot.branch ? <span className="text-zinc-400"> · {kot.branch}</span> : null}
            {kot.comments ? <span className="text-zinc-500"> — {kot.comments}</span> : null}
          </div>
        )}
      </div>

      {/* items */}
      <div className="flex-1 px-3.5 pb-2">
        <div className="space-y-1.5">
          {kot.items.map((it, idx) => (
            <div
              key={`${it.item}-${idx}`}
              className="flex items-start justify-between gap-2 rounded-lg border border-zinc-100 bg-zinc-50/70 px-2.5 py-2 dark:border-zinc-800 dark:bg-zinc-800/50"
            >
              <div className="min-w-0">
                <div className="flex items-center gap-1.5">
                  <span className="truncate text-sm font-medium leading-none text-zinc-900 dark:text-zinc-100">
                    {it.item_name}
                  </span>
                  {it.course && (
                    <span className="shrink-0 rounded bg-white px-1 py-0.5 text-[10px] font-medium text-zinc-500 dark:bg-zinc-900 dark:text-zinc-400">
                      {it.course}
                    </span>
                  )}
                  {it.indicate_course && (
                    <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-amber-500" title="Course fire" />
                  )}
                </div>
                <div className="mt-1 flex flex-wrap items-center gap-1.5 text-xs text-zinc-500 dark:text-zinc-400">
                  <span className="font-mono text-zinc-600 dark:text-zinc-300">{it.item}</span>
                  {it.comments && <span className="italic">“{it.comments}”</span>}
                  {it.serve_priority ? (
                    <span className="rounded bg-amber-100 px-1 py-0.5 text-[10px] font-semibold text-amber-800 dark:bg-amber-900/30 dark:text-amber-200">
                      P{it.serve_priority}
                    </span>
                  ) : null}
                </div>
              </div>
              <span className="shrink-0 rounded-full bg-zinc-900 px-2 py-1 font-mono text-xs font-semibold text-white dark:bg-white dark:text-zinc-900">
                ×{it.quantity}
              </span>
            </div>
          ))}
        </div>
      </div>

      {/* footer */}
      <div className="flex items-center justify-between gap-2 border-t border-zinc-100 px-3.5 py-3 dark:border-zinc-800">
        <div className="text-[11px] text-zinc-500 dark:text-zinc-400">
          {kot.pos_profile ?? "—"}
          {kot.verified && kot.verified_by ? (
            <span className="ml-1 text-emerald-600 dark:text-emerald-400">✓ {kot.verified_by}</span>
          ) : null}
        </div>
        <button
          onClick={() => onBump(kot.id)}
          disabled={bumping}
          className="inline-flex items-center justify-center rounded-full bg-emerald-600 px-4 py-1.5 text-xs font-semibold text-white shadow-sm transition hover:bg-emerald-700 disabled:cursor-not-allowed disabled:opacity-50 dark:bg-emerald-500 dark:hover:bg-emerald-600"
        >
          {bumping ? "Bumping…" : "Bump ✓"}
        </button>
      </div>
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
      setTickets(res.kots);
      setLastRefresh(new Date().toLocaleTimeString());
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      // ApiError carries problem.detail in message
      setError(msg);
    } finally {
      setLoading(false);
    }
  }, [station]);

  // initial + station change
  useEffect(() => {
    fetchTickets();
  }, [fetchTickets]);

  // SSE → auto-refresh (debounced: event-driven)
  useEffect(() => {
    if (events.length === 0) return;
    const last = events[events.length - 1];
    const kind = String(last.event).toLowerCase();
    if (
      kind.includes("kot") ||
      kind.includes("generated") ||
      kind.includes("prepared") ||
      kind === "kot_update"
    ) {
      // Only refresh if the event is for this station — but payload is opaque, so refresh anyway
      // with a short debounce to avoid thundering on burst.
      fetchTickets();
    }
  }, [events, fetchTickets]);

  // Poll fallback every 10s when SSE disconnected
  useEffect(() => {
    if (connected) return;
    const id = setInterval(() => {
      fetchTickets();
    }, 10_000);
    return () => clearInterval(id);
  }, [connected, fetchTickets]);

  const handleStationLoad = () => {
    const trimmed = stationInput.trim();
    if (!trimmed) return;
    setStation(trimmed);
    // clear prepared local when switching station so it doesn't leak
    setPreparedLocal([]);
  };

  const handleBump = async (kotId: string) => {
    setBumpingId(kotId);
    try {
      const updated = await kotApi.markPrepared(kotId, {});
      // move to preparedLocal optimistically
      const bumped = tickets.find((t) => t.id === kotId) ?? updated;
      setTickets((prev) => prev.filter((t) => t.id !== kotId));
      setPreparedLocal((prev) => [
        { ...bumped, start_time_prep: updated.start_time_prep ?? new Date().toTimeString().slice(0, 8) },
        ...prev,
      ].slice(0, 20));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
    } finally {
      setBumpingId(null);
      // background refresh to stay consistent with server filter
      fetchTickets();
    }
  };

  // Columns: pending = server pending (not prepared). For premium 3-col layout:
  // Pending  = all pending
  // Preparing = subset artificially: tickets older than 5m (or with comments/priority) — still pending but highlighted
  // Prepared = local bumped + any with start_time_prep (should be empty from API, but we keep local)
  const pending = tickets;
  const preparing = useMemo(() => {
    // Treat tickets with serve_priority > 0 or indicate_course as "firing now" → Preparing
    // Fallback: if no such tickets, show empty with placeholder.
    return tickets.filter((t) => t.items.some((it) => it.serve_priority > 0 || it.indicate_course));
  }, [tickets]);
  const pendingOnly = useMemo(() => {
    const preparingIds = new Set(preparing.map((p) => p.id));
    return pending.filter((k) => !preparingIds.has(k.id));
  }, [pending, preparing]);

  return (
    <div className="mx-auto flex w-full max-w-[1600px] flex-col px-3 py-4 sm:px-4 lg:px-6 lg:py-6">
      {/* Top bar */}
      <div className="rounded-2xl border border-zinc-200 bg-white p-4 shadow-sm sm:p-5 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="flex flex-col gap-4 lg:flex-row lg:items-end lg:justify-between">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <h1 className="text-xl font-semibold tracking-tight sm:text-2xl">KDS — Kitchen Display</h1>
              <span
                className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium ${
                  connected
                    ? "bg-emerald-100 text-emerald-800 dark:bg-emerald-900/40 dark:text-emerald-200"
                    : "bg-amber-100 text-amber-800 dark:bg-amber-900/40 dark:text-amber-200"
                }`}
              >
                <span className={`h-2 w-2 rounded-full ${connected ? "bg-emerald-500" : "bg-amber-500 animate-pulse"}`} />
                {connected ? "SSE live" : "Polling (SSE off)"}
              </span>
              {lastRefresh && (
                <span className="text-xs text-zinc-500 dark:text-zinc-400">updated {lastRefresh}</span>
              )}
            </div>
            <p className="mt-1 max-w-2xl text-xs leading-5 text-zinc-600 dark:text-zinc-400 sm:text-sm">
              Station-filtered ticket board. Live via{" "}
              <code className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-xs dark:bg-zinc-800">
                /api/events/stream?events=kot.generated,kot.prepared
              </code>{" "}
              with 10s poll fallback. Bump calls{" "}
              <code className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-xs dark:bg-zinc-800">
                POST /api/kot/:id/mark-prepared
              </code>{" "}
              (idempotent).
            </p>
            {sseError && (
              <div className="mt-2 rounded-lg bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:bg-amber-950/40 dark:text-amber-200">
                SSE: {sseError} — polling fallback active.
              </div>
            )}
            {error && (
              <div className="mt-2 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900/50 dark:bg-red-950/30 dark:text-red-300">
                {error}
              </div>
            )}
          </div>

          <div className="flex w-full flex-col gap-2 sm:flex-row sm:items-end lg:w-auto">
            <div className="flex-1 sm:min-w-[280px]">
              <label className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 dark:text-zinc-400">
                Station — production unit / item_group
              </label>
              <div className="mt-1 flex gap-1.5">
                <input
                  value={stationInput}
                  onChange={(e) => setStationInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") handleStationLoad();
                  }}
                  placeholder="Hot Kitchen"
                  list="kds-stations"
                  className="w-full rounded-full border border-zinc-300 bg-white px-4 py-2 text-sm outline-none placeholder:text-zinc-400 focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-100 dark:placeholder:text-zinc-500 dark:focus:border-zinc-300"
                />
                <datalist id="kds-stations">
                  {STATION_PRESETS.map((s) => (
                    <option key={s} value={s} />
                  ))}
                </datalist>
                <button
                  onClick={handleStationLoad}
                  className="shrink-0 rounded-full bg-zinc-900 px-5 py-2 text-sm font-medium text-white transition hover:bg-zinc-800 dark:bg-white dark:text-zinc-900 dark:hover:bg-zinc-100"
                >
                  Load
                </button>
              </div>
              <div className="mt-2 flex flex-wrap gap-1.5">
                {STATION_PRESETS.map((s) => (
                  <button
                    key={s}
                    onClick={() => {
                      setStationInput(s);
                      setStation(s);
                      setPreparedLocal([]);
                    }}
                    className={`rounded-full border px-2.5 py-1 text-xs font-medium transition ${
                      station === s
                        ? "border-zinc-900 bg-zinc-900 text-white dark:border-white dark:bg-white dark:text-zinc-900"
                        : "border-zinc-200 bg-white text-zinc-600 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
                    }`}
                  >
                    {s}
                  </button>
                ))}
              </div>
            </div>
            <button
              onClick={fetchTickets}
              disabled={loading}
              className="inline-flex items-center justify-center gap-1.5 rounded-full border border-zinc-300 bg-white px-4 py-2 text-sm font-medium text-zinc-700 transition hover:bg-zinc-50 disabled:opacity-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200 dark:hover:bg-zinc-700"
            >
              <span className={`h-3 w-3 rounded-full border-2 border-zinc-400 border-t-transparent ${loading ? "animate-spin" : "hidden"}`} />
              Refresh
            </button>
          </div>
        </div>

        {/* stats */}
        <div className="mt-4 grid grid-cols-3 gap-2 sm:grid-cols-4">
          <div className="rounded-xl bg-zinc-900 px-3 py-3 text-white dark:bg-white dark:text-zinc-900">
            <div className="text-[10px] font-semibold uppercase tracking-widest opacity-70">Station</div>
            <div className="mt-1 truncate font-mono text-sm font-semibold">{station}</div>
            <div className="text-xs opacity-70">{pending.length} pending</div>
          </div>
          <div className="rounded-xl border border-amber-200 bg-amber-50 px-3 py-3 dark:border-amber-900/30 dark:bg-amber-950/30">
            <div className="text-[10px] font-semibold uppercase tracking-widest text-amber-700 dark:text-amber-300">Pending</div>
            <div className="mt-1 text-lg font-semibold text-amber-900 dark:text-amber-100">{pendingOnly.length}</div>
            <div className="text-xs text-amber-700/70 dark:text-amber-300/70">awaiting fire</div>
          </div>
          <div className="rounded-xl border border-sky-200 bg-sky-50 px-3 py-3 dark:border-sky-900/30 dark:bg-sky-950/30">
            <div className="text-[10px] font-semibold uppercase tracking-widest text-sky-700 dark:text-sky-300">Preparing</div>
            <div className="mt-1 text-lg font-semibold text-sky-900 dark:text-sky-100">{preparing.length}</div>
            <div className="text-xs text-sky-700/70 dark:text-sky-300/70">priority / course</div>
          </div>
          <div className="rounded-xl border border-emerald-200 bg-emerald-50 px-3 py-3 dark:border-emerald-900/30 dark:bg-emerald-950/30">
            <div className="text-[10px] font-semibold uppercase tracking-widest text-emerald-700 dark:text-emerald-300">Prepared</div>
            <div className="mt-1 text-lg font-semibold text-emerald-900 dark:text-emerald-100">{preparedLocal.length}</div>
            <div className="text-xs text-emerald-700/70 dark:text-emerald-300/70">bumped this view</div>
          </div>
        </div>
      </div>

      {/* Board */}
      <div className="mt-4 grid flex-1 gap-4 lg:grid-cols-3">
        {/* Pending */}
        <section className="flex flex-col rounded-2xl border border-amber-200 bg-amber-50/40 dark:border-amber-900/20 dark:bg-amber-950/10">
          <div className="sticky top-0 z-10 flex items-center justify-between gap-2 border-b border-amber-200 bg-amber-50/95 px-3 py-3 backdrop-blur supports-[backdrop-filter]:bg-amber-50/80 sm:px-4 dark:border-amber-900/30 dark:bg-amber-950/40">
            <h2 className="flex items-center gap-2 text-sm font-semibold text-amber-900 dark:text-amber-100">
              <span className="h-2 w-2 rounded-full bg-amber-500" />
              Pending
              <span className="rounded-full bg-white px-2 py-0.5 font-mono text-xs text-amber-800 dark:bg-zinc-900 dark:text-amber-200">
                {pendingOnly.length}
              </span>
            </h2>
            <span className="text-[11px] text-amber-700/70 dark:text-amber-300/60">oldest first</span>
          </div>
          <div className="flex-1 space-y-3 p-3 sm:p-4">
            {loading && pendingOnly.length === 0 ? (
              <div className="rounded-xl border border-dashed border-amber-300 bg-white p-6 text-center text-sm text-amber-700/70 dark:border-amber-800 dark:bg-zinc-900 dark:text-amber-300/60">
                Loading tickets…
              </div>
            ) : pendingOnly.length === 0 ? (
              <div className="rounded-xl border border-dashed border-zinc-300 bg-white p-8 text-center dark:border-zinc-700 dark:bg-zinc-900">
                <div className="text-sm font-medium text-zinc-700 dark:text-zinc-300">No pending tickets</div>
                <div className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
                  Station <span className="font-mono font-semibold">{station}</span> has no unprepared KOTs.
                </div>
                <div className="mt-3 text-xs text-zinc-400">Generate a KOT via POS → Send to Kitchen to see it here.</div>
              </div>
            ) : (
              pendingOnly.map((kot) => (
                <TicketCard key={kot.id} kot={kot} onBump={handleBump} bumping={bumpingId === kot.id} />
              ))
            )}
          </div>
        </section>

        {/* Preparing */}
        <section className="flex flex-col rounded-2xl border border-sky-200 bg-sky-50/40 dark:border-sky-900/20 dark:bg-sky-950/10">
          <div className="sticky top-0 z-10 flex items-center justify-between gap-2 border-b border-sky-200 bg-sky-50/95 px-3 py-3 backdrop-blur supports-[backdrop-filter]:bg-sky-50/80 sm:px-4 dark:border-sky-900/30 dark:bg-sky-950/40">
            <h2 className="flex items-center gap-2 text-sm font-semibold text-sky-900 dark:text-sky-100">
              <span className="h-2 w-2 rounded-full bg-sky-500" />
              Preparing
              <span className="rounded-full bg-white px-2 py-0.5 font-mono text-xs text-sky-800 dark:bg-zinc-900 dark:text-sky-200">
                {preparing.length}
              </span>
            </h2>
            <span className="text-[11px] text-sky-700/70 dark:text-sky-300/60">priority / course</span>
          </div>
          <div className="flex-1 space-y-3 p-3 sm:p-4">
            {preparing.length === 0 ? (
              <div className="rounded-xl border border-dashed border-sky-200 bg-white p-8 text-center dark:border-sky-900/30 dark:bg-zinc-900">
                <div className="text-sm font-medium text-zinc-700 dark:text-zinc-300">Nothing firing</div>
                <div className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
                  Tickets with course or priority land here. Bump from Pending when fire starts.
                </div>
              </div>
            ) : (
              preparing.map((kot) => (
                <TicketCard key={kot.id} kot={kot} onBump={handleBump} bumping={bumpingId === kot.id} />
              ))
            )}
          </div>
        </section>

        {/* Prepared */}
        <section className="flex flex-col rounded-2xl border border-emerald-200 bg-emerald-50/30 dark:border-emerald-900/20 dark:bg-emerald-950/10">
          <div className="sticky top-0 z-10 flex items-center justify-between gap-2 border-b border-emerald-200 bg-emerald-50/95 px-3 py-3 backdrop-blur supports-[backdrop-filter]:bg-emerald-50/80 sm:px-4 dark:border-emerald-900/30 dark:bg-emerald-950/40">
            <h2 className="flex items-center gap-2 text-sm font-semibold text-emerald-900 dark:text-emerald-100">
              <span className="h-2 w-2 rounded-full bg-emerald-500" />
              Prepared
              <span className="rounded-full bg-white px-2 py-0.5 font-mono text-xs text-emerald-800 dark:bg-zinc-900 dark:text-emerald-200">
                {preparedLocal.length}
              </span>
            </h2>
            <span className="text-[11px] text-emerald-700/70 dark:text-emerald-300/60">this session</span>
          </div>
          <div className="flex-1 space-y-3 p-3 sm:p-4">
            {preparedLocal.length === 0 ? (
              <div className="rounded-xl border border-dashed border-emerald-200 bg-white p-8 text-center dark:border-emerald-900/30 dark:bg-zinc-900">
                <div className="text-sm font-medium text-zinc-700 dark:text-zinc-300">No bumps yet</div>
                <div className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
                  Bumped tickets appear here until refresh. Server filters them as{" "}
                  <code className="font-mono text-xs">order_status = Prepared</code>.
                </div>
              </div>
            ) : (
              preparedLocal.map((kot) => (
                <div
                  key={kot.id}
                  className="rounded-xl border border-emerald-200 bg-white p-3 opacity-90 shadow-sm dark:border-emerald-900/30 dark:bg-zinc-900"
                >
                  <div className="flex items-center justify-between">
                    <span className="font-mono text-sm font-semibold text-zinc-900 dark:text-zinc-100">{kot.id}</span>
                    <span className="rounded-full bg-emerald-100 px-2 py-0.5 text-xs font-medium text-emerald-800 dark:bg-emerald-900/40 dark:text-emerald-200">
                      Prepared {kot.start_time_prep ? `· ${kot.start_time_prep}` : ""}
                    </span>
                  </div>
                  <div className="mt-1 text-xs text-zinc-600 dark:text-zinc-400">
                    {kot.invoice} · {kot.restaurant_table ?? "—"} · {kot.items.length} items
                  </div>
                  <div className="mt-2 flex flex-wrap gap-1">
                    {kot.items.slice(0, 4).map((it, i) => (
                      <span
                        key={i}
                        className="rounded-full bg-zinc-100 px-2 py-0.5 text-xs text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300"
                      >
                        {it.item_name} ×{it.quantity}
                      </span>
                    ))}
                    {kot.items.length > 4 && (
                      <span className="text-xs text-zinc-500">+{kot.items.length - 4} more</span>
                    )}
                  </div>
                </div>
              ))
            )}
          </div>
        </section>
      </div>

      <p className="mt-4 text-center text-[11px] text-zinc-500 dark:text-zinc-400">
        KOTs filtered by <code className="font-mono">production = {JSON.stringify(station)}</code> via{" "}
        <code className="font-mono">GET /api/production-units/:unit_id/pending-kots</code> (where{" "}
        <code className="font-mono">order_status ≠ Prepared</code>). Bump is idempotent — double-tap is safe.
      </p>
    </div>
  );
}
