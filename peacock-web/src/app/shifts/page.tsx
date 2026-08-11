import ShiftPanel from "@/components/ShiftPanel";

export const dynamic = "force-dynamic";

export default function ShiftsPage() {
  return (
    <div className="mx-auto w-full max-w-6xl px-4 py-6 sm:px-6 sm:py-8">
      <div className="mb-4">
        <h1 className="text-xl font-semibold tracking-tight sm:text-2xl">Shifts & Z-Reports</h1>
        <p className="mt-1 max-w-3xl text-sm leading-6 text-zinc-600 dark:text-zinc-400">
          Open / close shifts per terminal, Z-report on close with{" "}
          <code className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-xs dark:bg-zinc-800">cash_total</code>,{" "}
          <code className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-xs dark:bg-zinc-800">card_total</code>,{" "}
          <code className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-xs dark:bg-zinc-800">total_revenue</code>, and{" "}
          <code className="rounded bg-amber-100 px-1 py-0.5 font-mono text-xs text-amber-800 dark:bg-amber-900/40 dark:text-amber-200">
            cashThresholdWarning
          </code>{" "}
          at ₹10,000 (CGST Rule 56). Business days are half-open{" "}
          <code className="font-mono">[start,end)</code> in <code className="font-mono">Asia/Kolkata</code>.
        </p>
      </div>
      <ShiftPanel />
      <p className="mt-6 text-center text-[11px] text-zinc-500 dark:text-zinc-400">
        API: <code className="font-mono">POST /api/shifts/open</code> ·{" "}
        <code className="font-mono">GET /api/shifts/current?terminal=</code> ·{" "}
        <code className="font-mono">POST /api/shifts/:id/close</code> ·{" "}
        <code className="font-mono">GET /api/shifts/:id/report</code> ·{" "}
        <code className="font-mono">GET /api/shifts?terminal=&limit=&offset=</code>
      </p>
    </div>
  );
}
