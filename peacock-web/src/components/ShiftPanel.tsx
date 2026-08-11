"use client";

import { useCallback, useEffect, useState } from "react";
import { shiftsApi, type ShiftResponse, type ZReportResponse } from "@/lib/api";
import { formatMoney } from "@/lib/money";

// ---------------------------------------------------------------------------
// BusinessDay helper — half-open [start,end) Asia/Kolkata
// ---------------------------------------------------------------------------

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/**
 * Display half-open range [start,end) for a business_day label with cutoff_hour in Asia/Kolkata.
 * Example: label 2026-07-28, cutoff 3 → [2026-07-28 03:00 IST, 2026-07-29 03:00 IST)
 */
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
    // Use Asia/Kolkata via Intl
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

// ---------------------------------------------------------------------------
// Small UI atoms
// ---------------------------------------------------------------------------

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500 dark:text-zinc-400">
        {label}
      </span>
      {children}
    </label>
  );
}

function inputCls(): string {
  return "w-full rounded-xl border border-zinc-300 bg-white px-3 py-2 text-sm outline-none placeholder:text-zinc-400 focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-100 dark:placeholder:text-zinc-500 dark:focus:border-zinc-300";
}

// ---------------------------------------------------------------------------
// ShiftPanel
// ---------------------------------------------------------------------------

export default function ShiftPanel() {
  // form state
  const [terminal, setTerminal] = useState("POS-01");
  const [openedBy, setOpenedBy] = useState("cashier@peacock.local");
  const [businessDay, setBusinessDay] = useState<string>(() => todayIST());
  const [cutoffHour, setCutoffHour] = useState<number>(3);

  // data state
  const [current, setCurrent] = useState<ShiftResponse | null>(null);
  const [currentError, setCurrentError] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState<"open" | "close" | "refresh" | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [lastZ, setLastZ] = useState<ZReportResponse | null>(null);

  // list state
  const [shifts, setShifts] = useState<ShiftResponse[]>([]);
  const [listTotal, setListTotal] = useState(0);
  const [listLimit] = useState(20);
  const [listOffset, setListOffset] = useState(0);
  const [listLoading, setListLoading] = useState(false);
  const [listError, setListError] = useState<string | null>(null);

  // report detail
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
      // 404 = no open shift — not an error, show empty
      if (msg.toLowerCase().includes("404") || msg.toLowerCase().includes("not found") || msg.toLowerCase().includes("no open shift")) {
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

  return (
    <div className="space-y-4">
      {/* Controls + current shift */}
      <div className="rounded-2xl border border-zinc-200 bg-white p-4 shadow-sm dark:border-zinc-800 dark:bg-zinc-900 sm:p-5">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <h2 className="text-sm font-semibold tracking-tight">Shift control</h2>
          <span className="rounded-full bg-zinc-100 px-2.5 py-1 font-mono text-xs text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300">
            Asia/Kolkata · cutoff {pad2(cutoffHour)}:00 · {businessDayRange(businessDay || todayIST(), cutoffHour)}
          </span>
        </div>

        <div className="mt-4 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          <Field label="Terminal">
            <input value={terminal} onChange={(e) => setTerminal(e.target.value)} placeholder="POS-01" className={inputCls()} />
          </Field>
          <Field label="Opened by (user)">
            <input value={openedBy} onChange={(e) => setOpenedBy(e.target.value)} placeholder="cashier@peacock.local" className={inputCls()} />
          </Field>
          <Field label="Business day (YYYY-MM-DD)">
            <input
              type="date"
              value={businessDay}
              onChange={(e) => setBusinessDay(e.target.value)}
              className={inputCls()}
            />
            <span className="text-[11px] text-zinc-500 dark:text-zinc-400">
              Half-open <code className="font-mono">[start,end)</code> in <code className="font-mono">Asia/Kolkata</code>. Empty → today.
            </span>
          </Field>
          <Field label="Cutoff hour (0–23 IST)">
            <input
              type="number"
              min={0}
              max={23}
              value={cutoffHour}
              onChange={(e) => setCutoffHour(Math.min(23, Math.max(0, Number(e.target.value) || 0)))}
              className={inputCls()}
            />
            <span className="text-[11px] text-zinc-500 dark:text-zinc-400">Default 3 = 03:00 IST rollover.</span>
          </Field>
        </div>

        <div className="mt-4 flex flex-wrap gap-2">
          <button
            onClick={fetchCurrent}
            disabled={actionBusy !== null}
            className="rounded-full border border-zinc-300 bg-white px-4 py-2 text-sm font-medium text-zinc-700 hover:bg-zinc-50 disabled:opacity-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200 dark:hover:bg-zinc-700"
          >
            Refresh current
          </button>
          <button
            onClick={handleOpen}
            disabled={actionBusy !== null || !terminal.trim() || !openedBy.trim()}
            className="rounded-full bg-zinc-900 px-5 py-2 text-sm font-semibold text-white hover:bg-zinc-800 disabled:opacity-50 dark:bg-white dark:text-zinc-900 dark:hover:bg-zinc-100"
          >
            {actionBusy === "open" ? "Opening…" : "Open shift"}
          </button>
          <button
            onClick={handleClose}
            disabled={actionBusy !== null || !current}
            className="rounded-full bg-amber-600 px-5 py-2 text-sm font-semibold text-white hover:bg-amber-700 disabled:cursor-not-allowed disabled:opacity-50 dark:bg-amber-500 dark:hover:bg-amber-600"
            title={!current ? "No open shift on this terminal" : `Close ${current.name}`}
          >
            {actionBusy === "close" ? "Closing…" : current ? `Close ${current.name}` : "Close (no open shift)"}
          </button>
        </div>

        {actionError && (
          <div className="mt-3 rounded-xl border border-red-200 bg-red-50 px-3 py-2.5 text-sm text-red-700 dark:border-red-900/40 dark:bg-red-950/30 dark:text-red-300">
            {actionError}
          </div>
        )}

        {/* Current shift card */}
        <div className="mt-4">
          {currentError ? (
            <div className="rounded-xl border border-amber-200 bg-amber-50 px-3 py-2.5 text-sm text-amber-800 dark:border-amber-900/40 dark:bg-amber-950/30 dark:text-amber-200">
              {currentError}
            </div>
          ) : current ? (
            <div className="rounded-xl border border-emerald-200 bg-emerald-50 px-4 py-3 dark:border-emerald-900/30 dark:bg-emerald-950/20">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="flex items-center gap-2">
                  <span className="h-2 w-2 rounded-full bg-emerald-500" />
                  <span className="font-mono text-sm font-semibold text-emerald-900 dark:text-emerald-100">{current.name}</span>
                  <span className="rounded-full bg-white px-2 py-0.5 text-xs font-medium text-emerald-700 dark:bg-zinc-900 dark:text-emerald-200">OPEN</span>
                </div>
                <span className="font-mono text-xs text-emerald-700/70 dark:text-emerald-300/70">
                  {new Date(current.opened_at).toLocaleString("en-IN", { timeZone: "Asia/Kolkata" })} IST
                </span>
              </div>
              <div className="mt-2 grid grid-cols-2 gap-2 text-xs sm:grid-cols-4">
                <div className="rounded-lg bg-white px-2.5 py-2 dark:bg-zinc-900">
                  <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Terminal</div>
                  <div className="font-medium text-zinc-900 dark:text-zinc-100">{current.terminal}</div>
                </div>
                <div className="rounded-lg bg-white px-2.5 py-2 dark:bg-zinc-900">
                  <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Business day</div>
                  <div className="font-mono font-medium text-zinc-900 dark:text-zinc-100">{current.business_day}</div>
                </div>
                <div className="rounded-lg bg-white px-2.5 py-2 dark:bg-zinc-900">
                  <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Opened by</div>
                  <div className="truncate font-medium text-zinc-900 dark:text-zinc-100">{current.opened_by}</div>
                </div>
                <div className="rounded-lg bg-white px-2.5 py-2 dark:bg-zinc-900">
                  <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Range</div>
                  <div className="truncate font-mono text-[11px] text-zinc-600 dark:text-zinc-300">
                    {businessDayRange(current.business_day, cutoffHour)}
                  </div>
                </div>
              </div>
              <p className="mt-2 text-[11px] text-emerald-700/70 dark:text-emerald-300/60">
                Business day is half-open <code className="font-mono">[start,end)</code> in <code className="font-mono">Asia/Kolkata</code> — invoices at exactly{" "}
                <code className="font-mono">end</code> belong to the next day (bug-2 invariant).
              </p>
            </div>
          ) : (
            <div className="rounded-xl border border-dashed border-zinc-300 bg-zinc-50 px-4 py-6 text-center dark:border-zinc-700 dark:bg-zinc-800/50">
              <div className="text-sm font-medium text-zinc-700 dark:text-zinc-300">No open shift</div>
              <div className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
                Terminal <span className="font-mono font-semibold">{terminal || "—"}</span> has no open shift. Open one above.
              </div>
            </div>
          )}
        </div>

        {/* Last Z inline */}
        {lastZ && (
          <div className="mt-4 overflow-hidden rounded-xl border border-zinc-200 dark:border-zinc-700">
            <div className="bg-zinc-900 px-4 py-3 text-white dark:bg-zinc-800">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="text-sm font-semibold">Z-Report — {lastZ.shift_name}</span>
                <span className="font-mono text-xs opacity-70">
                  {lastZ.business_day} · {lastZ.terminal}
                </span>
              </div>
              <div className="mt-1 font-mono text-xs opacity-60">
                {new Date(lastZ.opened_at).toLocaleString("en-IN", { timeZone: "Asia/Kolkata" })} →{" "}
                {new Date(lastZ.closed_at).toLocaleString("en-IN", { timeZone: "Asia/Kolkata" })} IST
              </div>
            </div>
            <div className="grid grid-cols-2 gap-px bg-zinc-200 p-px dark:bg-zinc-800 sm:grid-cols-4">
              <div className="bg-white px-4 py-3 dark:bg-zinc-900">
                <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Invoices</div>
                <div className="mt-1 font-mono text-lg font-semibold text-zinc-900 dark:text-zinc-100">{lastZ.invoice_count}</div>
              </div>
              <div className="bg-white px-4 py-3 dark:bg-zinc-900">
                <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Cash</div>
                <div className="mt-1 font-mono text-lg font-semibold text-zinc-900 dark:text-zinc-100">
                  {formatMoney(lastZ.cash_total)}
                </div>
              </div>
              <div className="bg-white px-4 py-3 dark:bg-zinc-900">
                <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Card</div>
                <div className="mt-1 font-mono text-lg font-semibold text-zinc-900 dark:text-zinc-100">
                  {formatMoney(lastZ.card_total)}
                </div>
              </div>
              <div className="bg-white px-4 py-3 dark:bg-zinc-900">
                <div className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Total revenue</div>
                <div className="mt-1 font-mono text-lg font-semibold text-zinc-900 dark:text-zinc-100">
                  {formatMoney(lastZ.total_revenue)}
                </div>
                <div className="text-[11px] text-zinc-500">rounded_total, REVENUE statuses</div>
              </div>
            </div>
            {lastZ.cash_threshold_warning && (
              <div className="flex items-start gap-2 bg-amber-50 px-4 py-3 text-sm font-medium text-amber-900 dark:bg-amber-950/40 dark:text-amber-200">
                <span className="mt-0.5">⚠️</span>
                <span>
                  Cash {formatMoney(lastZ.cash_total)} ≥ ₹10,000 — CGST Rule 56 threshold breached. Deposit verification required
                  before next shift.
                </span>
              </div>
            )}
            {!lastZ.cash_threshold_warning && (
              <div className="bg-emerald-50 px-4 py-2 text-xs text-emerald-700 dark:bg-emerald-950/20 dark:text-emerald-300">
                Cash below ₹10,000 — no CGST Rule 56 action required.
              </div>
            )}
          </div>
        )}
      </div>

      {/* Shifts list + report */}
      <div className="grid gap-4 lg:grid-cols-5">
        <div className="rounded-2xl border border-zinc-200 bg-white p-4 shadow-sm dark:border-zinc-800 dark:bg-zinc-900 sm:p-5 lg:col-span-3">
          <div className="flex items-center justify-between">
            <h3 className="text-sm font-semibold">Shifts</h3>
            <div className="flex items-center gap-2">
              <span className="font-mono text-xs text-zinc-500 dark:text-zinc-400">
                {listTotal} total · page {page}/{totalPages}
              </span>
              <button
                onClick={fetchList}
                disabled={listLoading}
                className="rounded-full border border-zinc-200 bg-white px-3 py-1 text-xs font-medium hover:bg-zinc-50 disabled:opacity-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200"
              >
                {listLoading ? "Loading…" : "Refresh"}
              </button>
            </div>
          </div>

          {listError && (
            <div className="mt-3 rounded-xl border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700 dark:border-red-900/40 dark:bg-red-950/30 dark:text-red-300">
              {listError}
            </div>
          )}

          <div className="mt-3 overflow-hidden rounded-xl border border-zinc-200 dark:border-zinc-700">
            <div className="max-h-[420px] overflow-auto">
              <table className="w-full text-left text-sm">
                <thead className="sticky top-0 bg-zinc-50 text-xs uppercase tracking-widest text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400">
                  <tr>
                    <th className="px-3 py-2 font-semibold">Shift</th>
                    <th className="px-3 py-2 font-semibold">Terminal</th>
                    <th className="px-3 py-2 font-semibold">Business day</th>
                    <th className="px-3 py-2 font-semibold">Status</th>
                    <th className="px-3 py-2 font-semibold"></th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-zinc-100 dark:divide-zinc-800">
                  {shifts.length === 0 ? (
                    <tr>
                      <td colSpan={5} className="px-3 py-8 text-center text-sm text-zinc-500 dark:text-zinc-400">
                        {listLoading ? "Loading…" : "No shifts yet for this terminal filter."}
                      </td>
                    </tr>
                  ) : (
                    shifts.map((s) => {
                      const isOpen = !s.closed_at;
                      return (
                        <tr key={s.name} className="hover:bg-zinc-50 dark:hover:bg-zinc-800/50">
                          <td className="px-3 py-2.5 font-mono text-xs font-semibold text-zinc-900 dark:text-zinc-100">{s.name}</td>
                          <td className="px-3 py-2.5 text-xs text-zinc-600 dark:text-zinc-300">{s.terminal}</td>
                          <td className="px-3 py-2.5 font-mono text-xs text-zinc-600 dark:text-zinc-300">{s.business_day}</td>
                          <td className="px-3 py-2.5">
                            {isOpen ? (
                              <span className="rounded-full bg-emerald-100 px-2 py-0.5 text-xs font-medium text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-200">
                                Open
                              </span>
                            ) : (
                              <span className="rounded-full bg-zinc-100 px-2 py-0.5 text-xs font-medium text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300">
                                Closed
                              </span>
                            )}
                          </td>
                          <td className="px-3 py-2.5 text-right">
                            {!isOpen && (
                              <button
                                onClick={() => {
                                  setReportShiftId(s.name);
                                  setReport(null);
                                  setReportError(null);
                                  // auto-fetch
                                  shiftsApi
                                    .report(s.name)
                                    .then((z) => {
                                      setReport(z);
                                      setLastZ(z);
                                    })
                                    .catch((e) => setReportError(e instanceof Error ? e.message : String(e)));
                                }}
                                className="rounded-full bg-zinc-900 px-3 py-1 text-xs font-medium text-white hover:bg-zinc-800 dark:bg-white dark:text-zinc-900"
                              >
                                Report
                              </button>
                            )}
                          </td>
                        </tr>
                      );
                    })
                  )}
                </tbody>
              </table>
            </div>
          </div>

          <div className="mt-3 flex items-center justify-between">
            <button
              disabled={listOffset === 0}
              onClick={() => setListOffset((o) => Math.max(0, o - listLimit))}
              className="rounded-full border border-zinc-300 bg-white px-4 py-1.5 text-xs font-medium hover:bg-zinc-50 disabled:opacity-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200"
            >
              ← Prev
            </button>
            <span className="font-mono text-xs text-zinc-500">
              offset {listOffset} · limit {listLimit}
            </span>
            <button
              disabled={shifts.length < listLimit}
              onClick={() => setListOffset((o) => o + listLimit)}
              className="rounded-full border border-zinc-300 bg-white px-4 py-1.5 text-xs font-medium hover:bg-zinc-50 disabled:opacity-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200"
            >
              Next →
            </button>
          </div>
        </div>

        <div className="rounded-2xl border border-zinc-200 bg-white p-4 shadow-sm dark:border-zinc-800 dark:bg-zinc-900 sm:p-5 lg:col-span-2">
          <h3 className="text-sm font-semibold">Z-Report lookup</h3>
          <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
            Fetch <code className="font-mono">GET /api/shifts/:id/report</code> for any closed shift.
          </p>

          <div className="mt-3 flex gap-2">
            <input
              value={reportShiftId}
              onChange={(e) => setReportShiftId(e.target.value)}
              placeholder="SHIFT-00001"
              className={inputCls()}
            />
            <button
              onClick={handleFetchReport}
              disabled={reportLoading || !reportShiftId.trim()}
              className="shrink-0 rounded-full bg-zinc-900 px-4 py-2 text-sm font-medium text-white hover:bg-zinc-800 disabled:opacity-50 dark:bg-white dark:text-zinc-900"
            >
              {reportLoading ? "…" : "Fetch"}
            </button>
          </div>

          {reportError && (
            <div className="mt-3 rounded-xl border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700 dark:border-red-900/40 dark:bg-red-950/30 dark:text-red-300">
              {reportError}
            </div>
          )}

          {report ? (
            <div className="mt-4 overflow-hidden rounded-xl border border-zinc-200 dark:border-zinc-700">
              <div className="bg-zinc-900 px-4 py-3 text-white dark:bg-zinc-800">
                <div className="font-mono text-sm font-semibold">{report.shift_name}</div>
                <div className="font-mono text-xs opacity-60">
                  {report.terminal} · {report.business_day} · cutoff {pad2(cutoffHour)}:00 IST
                </div>
                <div className="mt-1 font-mono text-xs opacity-60">
                  {new Date(report.opened_at).toLocaleString("en-IN", { timeZone: "Asia/Kolkata" })} →{" "}
                  {new Date(report.closed_at).toLocaleString("en-IN", { timeZone: "Asia/Kolkata" })} IST
                </div>
                <div className="mt-1 text-xs opacity-60">{businessDayRange(report.business_day, cutoffHour)}</div>
              </div>
              <div className="divide-y divide-zinc-100 dark:divide-zinc-800">
                <div className="flex justify-between bg-white px-4 py-2.5 text-sm dark:bg-zinc-900">
                  <span className="text-zinc-500">Invoices</span>
                  <span className="font-mono font-semibold text-zinc-900 dark:text-zinc-100">{report.invoice_count}</span>
                </div>
                <div className="flex justify-between bg-white px-4 py-2.5 text-sm dark:bg-zinc-900">
                  <span className="text-zinc-500">Cash</span>
                  <span className="font-mono font-semibold text-zinc-900 dark:text-zinc-100">{formatMoney(report.cash_total)}</span>
                </div>
                <div className="flex justify-between bg-white px-4 py-2.5 text-sm dark:bg-zinc-900">
                  <span className="text-zinc-500">Card</span>
                  <span className="font-mono font-semibold text-zinc-900 dark:text-zinc-100">{formatMoney(report.card_total)}</span>
                </div>
                <div className="flex justify-between bg-zinc-50 px-4 py-3 text-sm font-semibold dark:bg-zinc-800">
                  <span className="text-zinc-700 dark:text-zinc-200">Total revenue</span>
                  <span className="font-mono text-zinc-900 dark:text-zinc-100">{formatMoney(report.total_revenue)}</span>
                </div>
              </div>
              {report.cash_threshold_warning ? (
                <div className="flex gap-2 bg-amber-50 px-4 py-3 text-sm font-medium text-amber-900 dark:bg-amber-950/40 dark:text-amber-200">
                  <span>⚠️</span>
                  <span>Cash {formatMoney(report.cash_total)} ≥ ₹10,000 — CGST Rule 56. Deposit verification required.</span>
                </div>
              ) : (
                <div className="bg-emerald-50 px-4 py-2 text-xs text-emerald-700 dark:bg-emerald-950/20 dark:text-emerald-300">
                  Cash below ₹10,000 — no CGST Rule 56 action required.
                </div>
              )}
            </div>
          ) : (
            <div className="mt-4 rounded-xl border border-dashed border-zinc-300 bg-zinc-50 p-6 text-center text-sm text-zinc-500 dark:border-zinc-700 dark:bg-zinc-800/50 dark:text-zinc-400">
              Enter a shift ID (e.g. <code className="font-mono">SHIFT-00001</code>) and fetch its Z-report. Close a shift above to auto-populate.
            </div>
          )}

          <div className="mt-4 rounded-xl bg-zinc-50 p-3 text-xs leading-5 text-zinc-600 dark:bg-zinc-800/50 dark:text-zinc-400">
            <strong className="font-semibold text-zinc-900 dark:text-zinc-100">BusinessDay note:</strong> Revenue is sum of{" "}
            <code className="font-mono">rounded_total</code> for invoices with{" "}
            <code className="font-mono">PosInvoiceStatus::REVENUE</code> in the half-open range{" "}
            <code className="font-mono">[start,end)</code>. Reconciled with Daily P&L (bugs 3+4 fixed).
          </div>
        </div>
      </div>
    </div>
  );
}
