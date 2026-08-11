"use client";

import { useCallback, useEffect, useState } from "react";
import { motion, AnimatePresence } from "framer-motion";
import {
  CalendarDots,
  Clock,
  Wallet,
  CreditCard,
  ChartBar,
  CheckCircle,
  WarningCircle,
  ArrowsClockwise,
  MagnifyingGlass,
  Receipt,
  Timer,
  ArrowLeft,
  ArrowRight,
  SpinnerGap,
} from "@phosphor-icons/react";
import { shiftsApi, type ShiftResponse, type ZReportResponse } from "@/lib/api";
import { formatMoney } from "@/lib/money";

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

function businessDayRange(label: string, cutoffHour: number): string {
  try {
    const d = new Date(label + "T00:00:00");
    if (Number.isNaN(d.getTime())) return `Business day ${label} cutoff ${cutoffHour}:00 Asia/Kolkata`;
    const next = new Date(d);
    next.setDate(d.getDate() + 1);
    const nextStr = next.toISOString().slice(0, 10);
    return `[${label} ${pad2(cutoffHour)}:00 IST, ${nextStr} ${pad2(cutoffHour)}:00 IST)`;
  } catch {
    return `${label} cutoff ${cutoffHour}:00 IST`;
  }
}

function todayIST(): string {
  try {
    const now = new Date();
    const fmt = new Intl.DateTimeFormat("en-CA", {
      timeZone: "Asia/Kolkata",
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    });
    const parts = fmt.formatToParts(now);
    const y = parts.find((p) => p.type === "year")?.value ?? "2026";
    const m = parts.find((p) => p.type === "month")?.value ?? "01";
    const d = parts.find((p) => p.type === "day")?.value ?? "01";
    return `${y}-${m}-${d}`;
  } catch {
    return new Date().toISOString().slice(0, 10);
  }
}

function inputCls(): string {
  return "w-full rounded-full border border-slate-200/70 bg-white px-4 py-2.5 text-sm tracking-tight outline-none placeholder:text-zinc-400 focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10";
}

// isolated breathing dot for open shift
function OpenDot() {
  return (
    <span className="relative inline-flex h-2.5 w-2.5">
      <motion.span
        aria-hidden
        className="absolute inline-flex h-full w-full rounded-full bg-emerald-500 opacity-40"
        animate={{ scale: [1, 1.9, 1], opacity: [0.5, 0, 0.5] }}
        transition={{ duration: 2.2, repeat: Infinity, ease: "easeInOut" }}
      />
      <span className="relative inline-flex h-2.5 w-2.5 rounded-full bg-emerald-500" />
    </span>
  );
}

// ---------------------------------------------------------------------------
// Panel
// ---------------------------------------------------------------------------

export default function ShiftPanel() {
  const [terminal, setTerminal] = useState("POS-01");
  const [openedBy, setOpenedBy] = useState("cashier@peacock.local");
  const [businessDay, setBusinessDay] = useState<string>(() => todayIST());
  const [cutoffHour, setCutoffHour] = useState<number>(3);

  const [current, setCurrent] = useState<ShiftResponse | null>(null);
  const [currentError, setCurrentError] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState<"open" | "close" | "refresh" | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [lastZ, setLastZ] = useState<ZReportResponse | null>(null);

  const [shifts, setShifts] = useState<ShiftResponse[]>([]);
  const [listTotal, setListTotal] = useState(0);
  const [listLimit] = useState(20);
  const [listOffset, setListOffset] = useState(0);
  const [listLoading, setListLoading] = useState(false);
  const [listError, setListError] = useState<string | null>(null);

  const [reportShiftId, setReportShiftId] = useState<string>("");
  const [report, setReport] = useState<ZReportResponse | null>(null);
  const [reportLoading, setReportLoading] = useState(false);
  const [reportError, setReportError] = useState<string | null>(null);

  const fetchCurrent = useCallback(async () => {
    const t = terminal.trim();
    if (!t) return;
    setCurrentError(null);
    try {
      const s = await shiftsApi.current(t);
      setCurrent(s);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (
        msg.toLowerCase().includes("404") ||
        msg.toLowerCase().includes("not found") ||
        msg.toLowerCase().includes("no open shift")
      ) {
        setCurrent(null);
        setCurrentError(null);
      } else {
        setCurrentError(msg);
      }
    }
  }, [terminal]);

  const fetchList = useCallback(async () => {
    setListLoading(true);
    setListError(null);
    try {
      const res = await shiftsApi.list({
        terminal: terminal.trim() || undefined,
        limit: listLimit,
        offset: listOffset,
      });
      setShifts(res.shifts);
      setListTotal(res.count);
    } catch (e) {
      setListError(e instanceof Error ? e.message : String(e));
    } finally {
      setListLoading(false);
    }
  }, [terminal, listLimit, listOffset]);

  useEffect(() => {
    fetchCurrent();
  }, [fetchCurrent]);

  useEffect(() => {
    fetchList();
  }, [fetchList]);

  const handleOpen = async () => {
    setActionBusy("open");
    setActionError(null);
    try {
      const s = await shiftsApi.open({
        terminal: terminal.trim(),
        opened_by: openedBy.trim(),
        business_day: businessDay || null,
      });
      setCurrent(s);
      setLastZ(null);
      setListOffset(0);
      await fetchList();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    } finally {
      setActionBusy(null);
    }
  };

  const handleClose = async () => {
    if (!current) return;
    setActionBusy("close");
    setActionError(null);
    try {
      const z = await shiftsApi.close(current.name, { cutoff_hour: cutoffHour });
      setLastZ(z);
      setCurrent(null);
      setReport(z);
      setReportShiftId(current.name);
      await fetchList();
      await fetchCurrent();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    } finally {
      setActionBusy(null);
    }
  };

  const handleFetchReport = async () => {
    const id = reportShiftId.trim();
    if (!id) return;
    setReportLoading(true);
    setReportError(null);
    try {
      const z = await shiftsApi.report(id);
      setReport(z);
      setLastZ(z);
    } catch (e) {
      setReportError(e instanceof Error ? e.message : String(e));
      setReport(null);
    } finally {
      setReportLoading(false);
    }
  };

  const totalPages = Math.max(1, Math.ceil(listTotal / listLimit));
  const page = Math.floor(listOffset / listLimit) + 1;

  // motion variants
  const listVariants = {
    hidden: {},
    show: { transition: { staggerChildren: 0.05 } },
  };
  const itemVariants = {
    hidden: { opacity: 0, y: 8 },
    show: { opacity: 1, y: 0, transition: { type: "spring" as const, stiffness: 100, damping: 20 } },
  };

  return (
    <div className="flex flex-col gap-6">
      {/* Controls — diffusion card, airy p-8, labels above inputs gap-2 */}
      <section className="rounded-[2.5rem] border border-slate-200/50 bg-white p-6 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] sm:p-8">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h2 className="flex items-center gap-2 text-sm font-semibold tracking-tighter text-zinc-900">
              <span className="inline-flex h-7 w-7 items-center justify-center rounded-full bg-zinc-900 text-white">
                <Timer size={14} weight="light" />
              </span>
              Shift control
            </h2>
            <p className="mt-1 text-xs leading-5 text-zinc-500">
              Half-open <span className="font-mono tracking-tight text-zinc-700">[start,end)</span> in{" "}
              <span className="font-mono tracking-tight text-zinc-700">Asia/Kolkata</span> · cutoff 03:00 IST rollover
            </p>
          </div>
          <span className="inline-flex items-center gap-1.5 rounded-full border border-slate-200/60 bg-[#f9fafb] px-3 py-1.5 font-mono text-xs tracking-tight text-zinc-600">
            <CalendarDots size={14} weight="light" className="text-zinc-400" />
            {businessDayRange(businessDay || todayIST(), cutoffHour)}
          </span>
        </div>

        <div className="mt-6 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          <label className="flex flex-col gap-2">
            <span className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Terminal</span>
            <input value={terminal} onChange={(e) => setTerminal(e.target.value)} placeholder="POS-01" className={inputCls()} />
          </label>
          <label className="flex flex-col gap-2">
            <span className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Opened by</span>
            <input
              value={openedBy}
              onChange={(e) => setOpenedBy(e.target.value)}
              placeholder="cashier@peacock.local"
              className={inputCls()}
            />
          </label>
          <label className="flex flex-col gap-2">
            <span className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Business day</span>
            <input type="date" value={businessDay} onChange={(e) => setBusinessDay(e.target.value)} className={inputCls()} />
            <span className="text-[11px] leading-4 text-zinc-400">Empty → today (IST).</span>
          </label>
          <label className="flex flex-col gap-2">
            <span className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Cutoff hour (0–23 IST)</span>
            <input
              type="number"
              min={0}
              max={23}
              value={cutoffHour}
              onChange={(e) => setCutoffHour(Math.min(23, Math.max(0, Number(e.target.value) || 0)))}
              className={inputCls()}
            />
            <span className="text-[11px] leading-4 text-zinc-400">Default 3 = 03:00 IST rollover.</span>
          </label>
        </div>

        <div className="mt-6 flex flex-wrap gap-2">
          <motion.button
            whileTap={{ scale: 0.98 }}
            transition={{ type: "spring", stiffness: 100, damping: 20 }}
            onClick={fetchCurrent}
            disabled={actionBusy !== null}
            className="inline-flex items-center gap-1.5 rounded-full border border-slate-200/60 bg-white px-4 py-2 text-sm font-medium tracking-tight text-zinc-700 transition hover:bg-zinc-50 hover:-translate-y-px active:translate-y-px disabled:opacity-50"
          >
            <ArrowsClockwise size={14} weight="light" /> Refresh current
          </motion.button>
          <motion.button
            whileTap={{ scale: 0.98 }}
            transition={{ type: "spring", stiffness: 100, damping: 20 }}
            onClick={handleOpen}
            disabled={actionBusy !== null || !terminal.trim() || !openedBy.trim()}
            className="inline-flex items-center justify-center rounded-full bg-zinc-900 px-5 py-2 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:bg-zinc-800 hover:-translate-y-px active:translate-y-px disabled:opacity-50"
          >
            {actionBusy === "open" ? (
              <>
                <SpinnerGap size={14} weight="light" className="animate-spin" /> Opening…
              </>
            ) : (
              "Open shift"
            )}
          </motion.button>
          <motion.button
            whileTap={{ scale: 0.98 }}
            transition={{ type: "spring", stiffness: 100, damping: 20 }}
            onClick={handleClose}
            disabled={actionBusy !== null || !current}
            title={!current ? "No open shift on this terminal" : `Close ${current.name}`}
            className="inline-flex items-center justify-center rounded-full bg-zinc-900 px-5 py-2 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:bg-zinc-800 hover:-translate-y-px active:translate-y-px disabled:cursor-not-allowed disabled:opacity-45"
          >
            {actionBusy === "close" ? (
              <>
                <SpinnerGap size={14} weight="light" className="animate-spin" /> Closing…
              </>
            ) : current ? (
              `Close ${current.name}`
            ) : (
              "Close — no open shift"
            )}
          </motion.button>
        </div>

        {actionError && (
          <motion.div
            initial={{ opacity: 0, y: 4 }}
            animate={{ opacity: 1, y: 0 }}
            className="mt-4 inline-flex max-w-full items-center gap-2 rounded-full bg-red-50 px-3 py-2 text-sm font-medium tracking-tight text-red-700 ring-1 ring-red-200/60"
          >
            <WarningCircle size={16} weight="light" /> {actionError}
          </motion.div>
        )}

        {/* Current shift — diffusion if open, else beautiful empty */}
        <div className="mt-6">
          <AnimatePresence mode="wait">
            {currentError ? (
              <motion.div
                key="err"
                initial={{ opacity: 0, y: 6 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0 }}
                className="rounded-2xl bg-amber-50 px-4 py-3 text-sm tracking-tight text-amber-800 ring-1 ring-amber-200/50"
              >
                {currentError}
              </motion.div>
            ) : current ? (
              <motion.div
                key={current.name}
                layout
                initial={{ opacity: 0, y: 8 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: -6 }}
                transition={{ type: "spring", stiffness: 100, damping: 20 }}
                className="overflow-hidden rounded-[2rem] border border-emerald-200/50 bg-emerald-50/40 shadow-sm"
              >
                <div className="flex flex-wrap items-center justify-between gap-3 px-5 py-4 sm:px-6">
                  <div className="flex items-center gap-2.5">
                    <OpenDot />
                    <span className="font-mono text-sm font-semibold tracking-tight text-emerald-900">{current.name}</span>
                    <span className="rounded-full bg-emerald-600 px-2 py-0.5 text-xs font-semibold tracking-tight text-white">OPEN</span>
                  </div>
                  <span className="inline-flex items-center gap-1.5 font-mono text-xs tracking-tight text-emerald-700/70">
                    <Clock size={12} weight="light" />
                    {new Date(current.opened_at).toLocaleString("en-IN", { timeZone: "Asia/Kolkata" })} IST
                  </span>
                </div>
                <div className="grid grid-cols-2 gap-2 px-5 pb-5 sm:grid-cols-4 sm:px-6">
                  <div className="rounded-2xl bg-white p-3 shadow-sm ring-1 ring-slate-200/50">
                    <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-400">Terminal</div>
                    <div className="mt-1 font-medium tracking-tight text-zinc-900">{current.terminal}</div>
                  </div>
                  <div className="rounded-2xl bg-white p-3 shadow-sm ring-1 ring-slate-200/50">
                    <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-400">Business day</div>
                    <div className="mt-1 font-mono text-sm font-medium tracking-tight text-zinc-900">{current.business_day}</div>
                  </div>
                  <div className="rounded-2xl bg-white p-3 shadow-sm ring-1 ring-slate-200/50">
                    <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-400">Opened by</div>
                    <div className="mt-1 truncate text-sm tracking-tight text-zinc-900">{current.opened_by}</div>
                  </div>
                  <div className="rounded-2xl bg-white p-3 shadow-sm ring-1 ring-slate-200/50">
                    <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-400">Range</div>
                    <div className="mt-1 truncate font-mono text-[11px] tracking-tight text-zinc-600">
                      {businessDayRange(current.business_day, cutoffHour)}
                    </div>
                  </div>
                </div>
              </motion.div>
            ) : (
              <motion.div
                key="empty-current"
                initial={{ opacity: 0, y: 6 }}
                animate={{ opacity: 1, y: 0 }}
                className="flex flex-col items-center justify-center rounded-[2rem] border border-dashed border-slate-200/70 bg-[#f9fafb] px-6 py-10 text-center"
              >
                <span className="flex h-10 w-10 items-center justify-center rounded-full bg-white text-zinc-400 shadow-sm ring-1 ring-slate-200/50">
                  <Receipt size={18} weight="light" />
                </span>
                <div className="mt-3 text-sm font-semibold tracking-tighter text-zinc-900">No open shift</div>
                <div className="mt-1 max-w-[38ch] text-xs leading-5 text-zinc-500">
                  Terminal <span className="font-mono font-semibold tracking-tight text-zinc-700">{terminal || "—"}</span> has no open
                  shift. Open one above — business day is half-open{" "}
                  <span className="font-mono tracking-tight">[start,end)</span>.
                </div>
              </motion.div>
            )}
          </AnimatePresence>
        </div>

        {/* Last Z — diffusion shadow, emerald inline */}
        <AnimatePresence>
          {lastZ && (
            <motion.div
              key={lastZ.shift_name}
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -6 }}
              transition={{ type: "spring", stiffness: 100, damping: 20 }}
              className="mt-6 overflow-hidden rounded-[2.5rem] border border-slate-200/50 bg-white shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]"
            >
              <div className="bg-zinc-900 px-6 py-5 text-white sm:px-8">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <span className="flex items-center gap-2 text-sm font-semibold tracking-tighter">
                    <ChartBar size={16} weight="light" /> Z-Report — {lastZ.shift_name}
                  </span>
                  <span className="font-mono text-xs tracking-tight opacity-60">
                    {lastZ.business_day} · {lastZ.terminal}
                  </span>
                </div>
                <div className="mt-1 font-mono text-xs tracking-tight opacity-50">
                  {new Date(lastZ.opened_at).toLocaleString("en-IN", { timeZone: "Asia/Kolkata" })} →{" "}
                  {new Date(lastZ.closed_at).toLocaleString("en-IN", { timeZone: "Asia/Kolkata" })} IST
                </div>
              </div>
              <div className="grid grid-cols-2 gap-px bg-slate-200/50 p-px sm:grid-cols-4">
                <div className="bg-white px-5 py-4">
                  <div className="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-widest text-zinc-400">
                    <Receipt size={12} weight="light" /> Invoices
                  </div>
                  <div className="mt-1 font-mono text-lg font-semibold tracking-tighter text-zinc-900">{lastZ.invoice_count}</div>
                </div>
                <div className="bg-white px-5 py-4">
                  <div className="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-widest text-zinc-400">
                    <Wallet size={12} weight="light" /> Cash
                  </div>
                  <div className="mt-1 font-mono text-lg font-semibold tracking-tighter text-zinc-900">{formatMoney(lastZ.cash_total)}</div>
                </div>
                <div className="bg-white px-5 py-4">
                  <div className="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-widest text-zinc-400">
                    <CreditCard size={12} weight="light" /> Card
                  </div>
                  <div className="mt-1 font-mono text-lg font-semibold tracking-tighter text-zinc-900">{formatMoney(lastZ.card_total)}</div>
                </div>
                <div className="bg-white px-5 py-4">
                  <div className="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-widest text-zinc-400">
                    <ChartBar size={12} weight="light" /> Total revenue
                  </div>
                  <div className="mt-1 font-mono text-lg font-semibold tracking-tighter text-zinc-900">{formatMoney(lastZ.total_revenue)}</div>
                  <div className="text-[11px] tracking-tight text-zinc-400">rounded_total · REVENUE</div>
                </div>
              </div>
              {/* emerald inline — single accent, not amber card soup */}
              {lastZ.cash_threshold_warning ? (
                <div className="flex items-start gap-2.5 border-t border-emerald-200/50 bg-emerald-50 px-6 py-3 text-sm font-medium tracking-tight text-emerald-800 sm:px-8">
                  <CheckCircle size={18} weight="light" className="mt-0.5 shrink-0 text-emerald-600" />
                  <span>
                    Cash <span className="font-mono tracking-tight">{formatMoney(lastZ.cash_total)}</span> ≥ ₹10,000 — CGST Rule 56 threshold
                    breached. Deposit verification required before next shift.
                  </span>
                </div>
              ) : (
                <div className="flex items-center gap-2 border-t border-emerald-200/30 bg-emerald-50/60 px-6 py-2.5 text-xs tracking-tight text-emerald-700 sm:px-8">
                  <CheckCircle size={14} weight="light" className="text-emerald-600" />
                  Cash below ₹10,000 — no CGST Rule 56 action required.
                </div>
              )}
            </motion.div>
          )}
        </AnimatePresence>
      </section>

      {/* Bottom: timeline + lookup — grid not flex-math */}
      <div className="grid grid-cols-12 gap-6">
        {/* Timeline — border-t not boxed */}
        <section className="col-span-12 rounded-[2.5rem] border border-slate-200/50 bg-white p-6 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] sm:p-8 lg:col-span-7">
          <div className="flex items-center justify-between gap-3">
            <h3 className="flex items-center gap-2 text-sm font-semibold tracking-tighter text-zinc-900">
              <Clock size={16} weight="light" /> Shifts
              <span className="rounded-full bg-zinc-900 px-2 py-0.5 font-mono text-xs font-semibold tracking-tight text-white">
                {listTotal}
              </span>
            </h3>
            <div className="flex items-center gap-2">
              <span className="hidden font-mono text-xs tracking-tight text-zinc-400 sm:inline">
                page {page}/{totalPages}
              </span>
              <motion.button
                whileTap={{ scale: 0.98 }}
                onClick={fetchList}
                disabled={listLoading}
                className="inline-flex items-center gap-1.5 rounded-full border border-slate-200/60 bg-white px-3 py-1.5 text-xs font-medium tracking-tight text-zinc-700 hover:bg-zinc-50 disabled:opacity-50"
              >
                {listLoading ? <SpinnerGap size={12} weight="light" className="animate-spin" /> : <ArrowsClockwise size={12} weight="light" />}{" "}
                {listLoading ? "Loading…" : "Refresh"}
              </motion.button>
            </div>
          </div>

          {listError && (
            <div className="mt-4 rounded-2xl bg-red-50 px-4 py-3 text-sm tracking-tight text-red-700 ring-1 ring-red-200/50">
              {listError}
            </div>
          )}

          {/* Timeline — border-t */}
          <div className="mt-4 border-t border-slate-200/60">
            {listLoading && shifts.length === 0 ? (
              <div className="divide-y divide-slate-100">
                {[0, 1, 2, 3].map((i) => (
                  <div key={i} className="flex items-center justify-between py-4">
                    <div className="flex items-center gap-3">
                      <div className="h-2.5 w-2.5 rounded-full bg-zinc-100" />
                      <div className="h-3 w-28 rounded-full bg-zinc-100" />
                    </div>
                    <div className="h-6 w-16 rounded-full bg-zinc-100" />
                  </div>
                ))}
              </div>
            ) : shifts.length === 0 ? (
              <div className="flex flex-col items-center justify-center py-12 text-center">
                <span className="flex h-12 w-12 items-center justify-center rounded-full bg-[#f9fafb] text-zinc-400 ring-1 ring-slate-200/50">
                  <CalendarDots size={20} weight="light" />
                </span>
                <div className="mt-3 text-sm font-semibold tracking-tighter text-zinc-900">No shifts yet</div>
                <div className="mt-1 max-w-[32ch] text-xs leading-5 text-zinc-500">
                  {terminal ? `No shifts for ${terminal} — open one above.` : "Set a terminal filter to see history."}
                </div>
              </div>
            ) : (
              <motion.div
                variants={listVariants}
                initial="hidden"
                animate="show"
                className="relative"
              >
                {/* vertical line */}
                <div className="pointer-events-none absolute left-[5px] top-2 hidden h-[calc(100%-16px)] w-px bg-slate-200/60 sm:block" />
                {shifts.map((s) => {
                  const isOpen = !s.closed_at;
                  return (
                    <motion.div
                      key={s.name}
                      variants={itemVariants}
                      layout
                      className="relative flex flex-col gap-2 border-b border-slate-200/40 py-4 last:border-b-0 sm:flex-row sm:items-center sm:justify-between sm:gap-3 sm:pl-6"
                    >
                      <span
                        className={`absolute left-0 top-6 hidden h-2.5 w-2.5 rounded-full ring-4 ring-white sm:block ${
                          isOpen ? "bg-emerald-500" : "bg-zinc-300"
                        }`}
                      />
                      <div className="flex min-w-0 items-center gap-2.5">
                        <span className="sm:hidden">
                          <span className={`inline-block h-2 w-2 rounded-full ${isOpen ? "bg-emerald-500" : "bg-zinc-300"}`} />
                        </span>
                        <span className="truncate font-mono text-sm font-semibold tracking-tight text-zinc-900">{s.name}</span>
                        <span className="hidden text-xs tracking-tight text-zinc-400 sm:inline">· {s.terminal}</span>
                        <span className="font-mono text-xs tracking-tight text-zinc-500 sm:hidden">{s.terminal}</span>
                        <span className="font-mono text-xs tracking-tight text-zinc-400">{s.business_day}</span>
                        {isOpen ? (
                          <span className="inline-flex items-center gap-1 rounded-full bg-emerald-50 px-2 py-0.5 text-xs font-medium tracking-tight text-emerald-700 ring-1 ring-emerald-200/40">
                            <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" /> Open
                          </span>
                        ) : (
                          <span className="rounded-full bg-zinc-50 px-2 py-0.5 text-xs font-medium tracking-tight text-zinc-500 ring-1 ring-slate-200/50">
                            Closed
                          </span>
                        )}
                      </div>
                      <div className="flex items-center gap-2 pl-0 sm:pl-0">
                        {!isOpen && (
                          <motion.button
                            whileTap={{ scale: 0.98 }}
                            whileHover={{ y: -1 }}
                            onClick={() => {
                              setReportShiftId(s.name);
                              setReport(null);
                              setReportError(null);
                              shiftsApi
                                .report(s.name)
                                .then((z) => {
                                  setReport(z);
                                  setLastZ(z);
                                })
                                .catch((e) => setReportError(e instanceof Error ? e.message : String(e)));
                            }}
                            className="inline-flex items-center gap-1 rounded-full bg-zinc-900 px-3.5 py-1.5 text-xs font-semibold tracking-tight text-white shadow-sm transition hover:bg-zinc-800"
                          >
                            <Receipt size={12} weight="light" /> Report
                          </motion.button>
                        )}
                      </div>
                    </motion.div>
                  );
                })}
              </motion.div>
            )}
          </div>

          <div className="mt-6 flex items-center justify-between gap-2 border-t border-slate-200/60 pt-4">
            <motion.button
              whileTap={{ scale: 0.98 }}
              disabled={listOffset === 0}
              onClick={() => setListOffset((o) => Math.max(0, o - listLimit))}
              className="inline-flex items-center gap-1 rounded-full border border-slate-200/60 bg-white px-4 py-1.5 text-xs font-medium tracking-tight text-zinc-700 hover:bg-zinc-50 disabled:opacity-50"
            >
              <ArrowLeft size={12} weight="light" /> Prev
            </motion.button>
            <span className="font-mono text-xs tracking-tight text-zinc-400">
              offset {listOffset} · limit {listLimit}
            </span>
            <motion.button
              whileTap={{ scale: 0.98 }}
              disabled={shifts.length < listLimit}
              onClick={() => setListOffset((o) => o + listLimit)}
              className="inline-flex items-center gap-1 rounded-full border border-slate-200/60 bg-white px-4 py-1.5 text-xs font-medium tracking-tight text-zinc-700 hover:bg-zinc-50 disabled:opacity-50"
            >
              Next <ArrowRight size={12} weight="light" />
            </motion.button>
          </div>
        </section>

        {/* Z-Report lookup — diffusion shadow */}
        <section className="col-span-12 rounded-[2.5rem] border border-slate-200/50 bg-white p-6 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] sm:p-8 lg:col-span-5">
          <h3 className="flex items-center gap-2 text-sm font-semibold tracking-tighter text-zinc-900">
            <MagnifyingGlass size={16} weight="light" /> Z-Report lookup
          </h3>
          <p className="mt-1 text-xs leading-5 text-zinc-500">
            Fetch <span className="font-mono tracking-tight text-zinc-700">GET /api/shifts/:id/report</span> for any closed shift.
          </p>

          <div className="mt-4 flex gap-2">
            <input value={reportShiftId} onChange={(e) => setReportShiftId(e.target.value)} placeholder="SHIFT-00001" className={inputCls()} />
            <motion.button
              whileTap={{ scale: 0.98 }}
              onClick={handleFetchReport}
              disabled={reportLoading || !reportShiftId.trim()}
              className="inline-flex shrink-0 items-center gap-1.5 rounded-full bg-zinc-900 px-4 py-2 text-sm font-semibold tracking-tight text-white shadow-sm hover:bg-zinc-800 disabled:opacity-50"
            >
              {reportLoading ? <SpinnerGap size={14} weight="light" className="animate-spin" /> : <MagnifyingGlass size={14} weight="light" />}
              {reportLoading ? "…" : "Fetch"}
            </motion.button>
          </div>

          {reportError && (
            <motion.div
              initial={{ opacity: 0, y: 4 }}
              animate={{ opacity: 1, y: 0 }}
              className="mt-3 rounded-2xl bg-red-50 px-3 py-2.5 text-sm tracking-tight text-red-700 ring-1 ring-red-200/50"
            >
              {reportError}
            </motion.div>
          )}

          <AnimatePresence mode="wait">
            {report ? (
              <motion.div
                key={report.shift_name}
                initial={{ opacity: 0, y: 8 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: -6 }}
                transition={{ type: "spring", stiffness: 100, damping: 20 }}
                className="mt-5 overflow-hidden rounded-[2rem] border border-slate-200/50 bg-white shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]"
              >
                <div className="bg-zinc-900 px-5 py-4 text-white">
                  <div className="font-mono text-sm font-semibold tracking-tight">{report.shift_name}</div>
                  <div className="mt-0.5 flex flex-wrap items-center gap-1.5 font-mono text-xs tracking-tight opacity-60">
                    <span>{report.terminal}</span>
                    <span>·</span>
                    <span>{report.business_day}</span>
                    <span>·</span>
                    <span>cutoff {pad2(cutoffHour)}:00 IST</span>
                  </div>
                  <div className="mt-1 font-mono text-xs tracking-tight opacity-50">
                    {new Date(report.opened_at).toLocaleString("en-IN", { timeZone: "Asia/Kolkata" })} →{" "}
                    {new Date(report.closed_at).toLocaleString("en-IN", { timeZone: "Asia/Kolkata" })} IST
                  </div>
                  <div className="mt-1 font-mono text-xs tracking-tight opacity-40">{businessDayRange(report.business_day, cutoffHour)}</div>
                </div>
                <div className="divide-y divide-zinc-100">
                  <div className="flex justify-between bg-white px-5 py-3 text-sm">
                    <span className="flex items-center gap-1.5 tracking-tight text-zinc-500">
                      <Receipt size={14} weight="light" /> Invoices
                    </span>
                    <span className="font-mono font-semibold tracking-tighter text-zinc-900">{report.invoice_count}</span>
                  </div>
                  <div className="flex justify-between bg-white px-5 py-3 text-sm">
                    <span className="flex items-center gap-1.5 tracking-tight text-zinc-500">
                      <Wallet size={14} weight="light" /> Cash
                    </span>
                    <span className="font-mono font-semibold tracking-tighter text-zinc-900">{formatMoney(report.cash_total)}</span>
                  </div>
                  <div className="flex justify-between bg-white px-5 py-3 text-sm">
                    <span className="flex items-center gap-1.5 tracking-tight text-zinc-500">
                      <CreditCard size={14} weight="light" /> Card
                    </span>
                    <span className="font-mono font-semibold tracking-tighter text-zinc-900">{formatMoney(report.card_total)}</span>
                  </div>
                  <div className="flex justify-between bg-[#f9fafb] px-5 py-4 text-sm font-semibold">
                    <span className="flex items-center gap-1.5 tracking-tight text-zinc-700">
                      <ChartBar size={14} weight="light" /> Total revenue
                    </span>
                    <span className="font-mono tracking-tighter text-zinc-900">{formatMoney(report.total_revenue)}</span>
                  </div>
                </div>
                {report.cash_threshold_warning ? (
                  <div className="flex items-start gap-2 border-t border-emerald-200/50 bg-emerald-50 px-5 py-3 text-sm font-medium tracking-tight text-emerald-800">
                    <CheckCircle size={18} weight="light" className="mt-0.5 shrink-0 text-emerald-600" />
                    <span>
                      Cash <span className="font-mono tracking-tight">{formatMoney(report.cash_total)}</span> ≥ ₹10,000 — CGST Rule 56.
                      Deposit verification required.
                    </span>
                  </div>
                ) : (
                  <div className="flex items-center gap-2 border-t border-emerald-200/30 bg-emerald-50/60 px-5 py-2.5 text-xs tracking-tight text-emerald-700">
                    <CheckCircle size={14} weight="light" className="text-emerald-600" />
                    Cash below ₹10,000 — no CGST Rule 56 action required.
                  </div>
                )}
              </motion.div>
            ) : (
              <motion.div
                key="empty-report"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                className="mt-5 flex flex-col items-center justify-center rounded-[2rem] border border-dashed border-slate-200/70 bg-[#f9fafb] p-8 text-center"
              >
                <span className="flex h-10 w-10 items-center justify-center rounded-full bg-white text-zinc-400 shadow-sm ring-1 ring-slate-200/50">
                  <ChartBar size={18} weight="light" />
                </span>
                <div className="mt-3 text-sm font-medium tracking-tight text-zinc-700">No report loaded</div>
                <div className="mt-1 max-w-[28ch] text-xs leading-5 text-zinc-500">
                  Enter a shift ID like <span className="font-mono tracking-tight text-zinc-700">SHIFT-00001</span> and fetch — or close a
                  shift above to auto-populate.
                </div>
              </motion.div>
            )}
          </AnimatePresence>

          <div className="mt-6 rounded-2xl bg-[#f9fafb] p-4 text-xs leading-5 tracking-tight text-zinc-600 ring-1 ring-slate-200/40">
            <span className="font-semibold tracking-tighter text-zinc-900">BusinessDay note:</span> Revenue is sum of{" "}
            <span className="font-mono tracking-tight">rounded_total</span> for invoices with{" "}
            <span className="font-mono tracking-tight">PosInvoiceStatus::REVENUE</span> in the half-open range{" "}
            <span className="font-mono tracking-tight">[start,end)</span>. Reconciled with Daily P&amp;L — bugs 3+4 fixed.
          </div>
        </section>
      </div>
    </div>
  );
}
