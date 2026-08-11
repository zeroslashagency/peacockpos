import ShiftPanel from "@/components/ShiftPanel";

export const dynamic = "force-dynamic";

export default function ShiftsPage() {
  return (
    <div className="mx-auto flex min-h-[100dvh] w-full max-w-[1400px] flex-col bg-[#f9fafb] px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      <div className="flex flex-col gap-2">
        <h1 className="text-3xl font-semibold tracking-tighter leading-none text-zinc-900">
          Shifts &amp; Z-Reports
        </h1>
        <p className="max-w-[65ch] text-sm leading-6 text-zinc-600">
          Open and close per terminal · Z-report is sum of{" "}
          <span className="font-mono tracking-tight text-zinc-900">rounded_total</span> for{" "}
          <span className="font-mono tracking-tight text-zinc-900">REVENUE</span> statuses in the half-open{" "}
          <span className="font-mono tracking-tight text-zinc-900">[start,end)</span> day —{" "}
          <span className="font-mono tracking-tight text-zinc-900">Asia/Kolkata</span>.
        </p>
      </div>

      <div className="mt-6">
        <ShiftPanel />
      </div>

      <p className="mt-6 text-center text-xs tracking-tight text-zinc-400">
        Range is half-open <span className="font-mono">[start,end)</span> in{" "}
        <span className="font-mono">Asia/Kolkata</span> — invoices at{" "}
        <span className="font-mono">end</span> belong to the next day. Reconciled with Daily P&amp;L.
      </p>
    </div>
  );
}
