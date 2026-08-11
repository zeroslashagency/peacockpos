import Link from "next/link";

export default function Home() {
  const apiBase =
    process.env.NEXT_PUBLIC_API_URL ?? "http://100.72.103.1:8080";
  return (
    <div className="mx-auto w-full max-w-[1400px] px-4 py-8 sm:px-6 sm:py-10">
      {/* Hero — split 60/40 left-aligned, not centered */}
      <div className="grid gap-6 lg:grid-cols-12">
        {/* Left — primary hero */}
        <div className="lg:col-span-7">
          <div className="flex h-full flex-col rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] sm:p-10">
            <div className="inline-flex w-fit items-center gap-2 rounded-full border border-slate-200/50 bg-[#f9fafb] px-3 py-1 text-[11px] font-semibold uppercase tracking-widest text-zinc-500">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-500 animate-pulse" />
              Service · Floor → Menu → Fire
            </div>

            <h1 className="mt-5 text-3xl font-semibold tracking-tighter leading-none text-zinc-900 sm:text-4xl lg:text-[42px]">
              Peacock POS
            </h1>
            <p className="mt-3 max-w-[65ch] text-sm leading-6 text-zinc-600">
              Next.js 15 + TypeScript + Tailwind foundation. Typed API client
              generated from{" "}
              <code className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-xs">
                docs/API.md
              </code>{" "}
              — money as string end-to-end (never JS Number), RFC 7807 errors,
              Idempotency-Key &amp; X-Restaurant headers, and an SSE hook for
              the live KDS board.
            </p>

            <div className="mt-4 flex flex-wrap items-center gap-3 font-mono text-xs">
              <span className="inline-flex items-center gap-2 rounded-full bg-zinc-900 px-3.5 py-1.5 font-medium text-white">
                <span className="h-1.5 w-1.5 rounded-full bg-emerald-400 animate-pulse" />
                ₹0.00 · 0 items · No table
              </span>
              <span className="hidden h-4 w-px bg-slate-200 sm:block" />
              <span className="text-zinc-500">takeaway ready · paisa-accurate</span>
            </div>

            <div className="mt-7 flex flex-wrap gap-3">
              <Link
                href="/pos"
                className="inline-flex items-center justify-center gap-2 rounded-full bg-zinc-900 px-6 py-2.5 text-sm font-medium text-white transition-all duration-300 ease-[cubic-bezier(0.16,1,0.3,1)] hover:-translate-y-[1px] hover:bg-zinc-800 hover:shadow-md active:scale-[0.98]"
              >
                Open POS
                <svg
                  width="14"
                  height="14"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.5"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden
                >
                  <path d="M5 12h14" />
                  <path d="M13 5l7 7-7 7" />
                </svg>
              </Link>
              <Link
                href="/kds"
                className="inline-flex items-center justify-center gap-2 rounded-full border border-slate-200 bg-white px-6 py-2.5 text-sm font-medium text-zinc-700 shadow-sm transition-all duration-300 ease-[cubic-bezier(0.16,1,0.3,1)] hover:-translate-y-[1px] hover:bg-zinc-50 hover:shadow-md active:scale-[0.98]"
              >
                KDS Board
                <svg
                  width="14"
                  height="14"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.5"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden
                >
                  <path d="M3 7h18" />
                  <path d="M8 7V5a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
                  <path d="M12 12v8" />
                  <path d="M8 16h8" />
                </svg>
              </Link>
            </div>

            <div className="mt-8 flex items-center gap-4 border-t border-slate-100 pt-6">
              <div className="flex -space-x-2">
                {/* eslint-disable-next-line @next/next/no-img-element */}
                <img
                  src="https://picsum.photos/seed/peacock-arjun/64/64"
                  alt=""
                  width={32}
                  height={32}
                  className="h-8 w-8 rounded-full border-2 border-white object-cover"
                />
                {/* eslint-disable-next-line @next/next/no-img-element */}
                <img
                  src="https://picsum.photos/seed/peacock-meera/64/64"
                  alt=""
                  width={32}
                  height={32}
                  className="h-8 w-8 rounded-full border-2 border-white object-cover"
                />
                <span className="inline-flex h-8 w-8 items-center justify-center rounded-full border-2 border-white bg-zinc-900 text-[10px] font-semibold tracking-wide text-white">
                  +4
                </span>
              </div>
              <div className="text-xs leading-5 text-zinc-500">
                <span className="font-medium text-zinc-900">
                  Arjun Patel · 4 pax · Table T7 Hall
                </span>
                <br />
                <span className="font-mono text-zinc-600">
                  ₹1,240.00 — verified via decimal.js
                </span>
              </div>
            </div>
          </div>
        </div>

        {/* Right — contextual order preview */}
        <div className="lg:col-span-5">
          <div className="flex h-full flex-col rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
            <div className="flex items-center justify-between">
              <div className="text-xs font-semibold uppercase tracking-widest text-zinc-400">
                Live order
              </div>
              <span className="inline-flex items-center gap-1.5 rounded-full border border-emerald-200 bg-emerald-50 px-2.5 py-1 text-xs font-medium text-emerald-700">
                <span className="h-2 w-2 rounded-full bg-emerald-500 animate-pulse" />
                Preparing
              </span>
            </div>

            <div className="mt-6 rounded-2xl border border-slate-200/50 bg-[#f9fafb] p-4">
              <div className="flex items-start justify-between gap-3">
                <div>
                  <div className="font-mono text-sm font-semibold tracking-tight text-zinc-900">
                    T-07 · Hall
                  </div>
                  <div className="text-xs text-zinc-500">
                    Arjun Patel · 4 pax · POS-01
                  </div>
                </div>
                <span className="rounded-full border border-slate-200/50 bg-white px-2.5 py-1 font-mono text-xs font-medium text-zinc-700 shadow-sm">
                  INV-0041
                </span>
              </div>

              <div className="mt-4 divide-y divide-slate-200/50 overflow-hidden rounded-xl border border-slate-200/50 bg-white">
                <div className="flex items-center justify-between px-3 py-2.5 text-sm">
                  <span className="font-medium text-zinc-900">
                    Paneer Tikka{" "}
                    <span className="font-mono text-xs font-normal text-zinc-500">
                      ×2
                    </span>
                  </span>
                  <span className="font-mono text-sm font-medium tracking-tight text-zinc-900">
                    ₹620.00
                  </span>
                </div>
                <div className="flex items-center justify-between px-3 py-2.5 text-sm">
                  <span className="font-medium text-zinc-900">
                    Butter Naan{" "}
                    <span className="font-mono text-xs font-normal text-zinc-500">
                      ×4
                    </span>
                  </span>
                  <span className="font-mono text-sm font-medium tracking-tight text-zinc-900">
                    ₹320.00
                  </span>
                </div>
                <div className="flex items-center justify-between px-3 py-2.5 text-sm">
                  <span className="font-medium text-zinc-900">
                    Gulab Jamun{" "}
                    <span className="font-mono text-xs font-normal text-zinc-500">
                      ×1
                    </span>
                  </span>
                  <span className="font-mono text-sm font-medium tracking-tight text-zinc-900">
                    ₹180.00
                  </span>
                </div>
              </div>

              <div className="mt-4 flex items-baseline justify-between border-t border-dashed border-slate-200 pt-4">
                <span className="text-xs font-semibold uppercase tracking-widest text-zinc-400">
                  Total
                </span>
                <span className="font-mono text-lg font-semibold tracking-tighter text-zinc-900">
                  ₹1,240.00
                </span>
              </div>
              <div className="text-right font-mono text-xs text-zinc-500">
                4 pcs · string via decimal.js
              </div>

              <div className="mt-4 grid grid-cols-2 gap-2">
                <span className="inline-flex items-center justify-center rounded-full bg-zinc-900 px-4 py-2.5 text-xs font-semibold text-white">
                  Fired to KDS
                </span>
                <span className="inline-flex items-center justify-center rounded-full bg-emerald-600 px-4 py-2.5 text-xs font-semibold text-white">
                  Paid
                </span>
              </div>
            </div>

            <div className="mt-auto flex items-center gap-2 pt-6 text-xs text-zinc-500">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-500 animate-pulse" />
              Kitchen live — updates via SSE
            </div>
          </div>
        </div>
      </div>

      {/* Bento — 3 tiles asymmetric, not 3 equal */}
      <div className="mt-6 grid grid-cols-12 gap-6">
        {/* API base — large */}
        <div className="col-span-12 lg:col-span-7 rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] transition-all duration-300 ease-[cubic-bezier(0.16,1,0.3,1)] hover:-translate-y-[1px] hover:shadow-md">
          <div className="flex items-start justify-between gap-4">
            <div className="min-w-0">
              <div className="text-xs font-semibold uppercase tracking-widest text-zinc-400">
                API base
              </div>
              <div className="mt-3 break-all font-mono text-sm font-medium tracking-tight text-zinc-900">
                {apiBase}
              </div>
              <div className="mt-1 text-xs leading-5 text-zinc-500">
                override via NEXT_PUBLIC_API_URL
              </div>
            </div>
            <span className="hidden shrink-0 items-center gap-1.5 rounded-full bg-zinc-900 px-3 py-1.5 font-mono text-xs font-medium text-white sm:inline-flex">
              <svg
                width="12"
                height="12"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden
              >
                <path d="M12 12a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z" />
                <path d="M12 2v3" />
                <path d="M12 19v3" />
                <path d="M4.93 4.93l2.12 2.12" />
                <path d="M16.95 16.95l2.12 2.12" />
                <path d="M2 12h3" />
                <path d="M19 12h3" />
              </svg>
              /health
            </span>
          </div>
          <div className="mt-6 inline-flex items-center gap-2 rounded-full border border-slate-200/50 bg-[#f9fafb] px-3 py-2 font-mono text-xs text-zinc-600">
            <span className="h-2 w-2 rounded-full bg-emerald-500 animate-pulse" />
            GET /api/health · <span className="font-medium text-zinc-900">200 OK</span>
          </div>
          <p className="mt-6 max-w-[65ch] text-xs leading-5 text-zinc-500">
            Typed client in{" "}
            <code className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-xs">
              src/lib/api.ts
            </code>{" "}
            — Table, Menu, Order, KOT, Invoice, Shift, Aggregator, Reports. RFC
            7807 Problem JSON, Idempotency-Key &amp; X-Restaurant headers.
          </p>
        </div>

        {/* Money — narrow, tall */}
        <div className="col-span-12 sm:col-span-6 lg:col-span-5 rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] transition-all duration-300 ease-[cubic-bezier(0.16,1,0.3,1)] hover:-translate-y-[1px] hover:shadow-md">
          <div className="text-xs font-semibold uppercase tracking-widest text-zinc-400">
            Money
          </div>
          <div className="mt-3 font-mono text-sm font-medium tracking-tight text-zinc-900">
            string only — decimal.js
          </div>
          <div className="mt-1 text-xs text-zinc-500">
            never Number, paisa-accurate
          </div>
          <div className="mt-6 rounded-2xl bg-zinc-900 p-5 text-white">
            <div className="text-[11px] font-semibold uppercase tracking-widest text-zinc-400">
              Paisa-accurate
            </div>
            <div className="mt-2 font-mono text-2xl font-semibold tracking-tighter">
              ₹1,240.00
            </div>
            <div className="mt-1 font-mono text-xs text-zinc-400">
              addMoney(&quot;620.00&quot;,&quot;620.00&quot;) → &quot;1240.00&quot;
            </div>
            <div className="mt-3 flex items-center gap-2 text-xs text-zinc-400">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
              Half-away-from-zero · en-IN
            </div>
          </div>
          <p className="mt-4 text-xs leading-5 text-zinc-500">
            Format via{" "}
            <code className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-xs">
              src/lib/money.ts
            </code>{" "}
            — en-IN grouping, 2dp fixed. No IEEE-754 drift.
          </p>
        </div>

        {/* SSE — full-width third tile, asymmetric vs 7+5 above */}
        <div className="col-span-12 sm:col-span-6 lg:col-span-12 rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] transition-all duration-300 ease-[cubic-bezier(0.16,1,0.3,1)] hover:-translate-y-[1px] hover:shadow-md">
          <div className="flex flex-col gap-6 lg:flex-row lg:items-start lg:justify-between">
            <div className="min-w-0">
              <div className="text-xs font-semibold uppercase tracking-widest text-zinc-400">
                SSE
              </div>
              <div className="mt-3 font-mono text-sm font-medium tracking-tight text-zinc-900">
                /api/events/stream
              </div>
              <div className="mt-1 text-xs text-zinc-500">
                kot_update · order_update · idempotent mark-prepared
              </div>
              <div className="mt-4 inline-flex items-center gap-2 rounded-full border border-emerald-200 bg-emerald-50 px-3 py-1.5 text-xs font-medium text-emerald-700">
                <span className="relative flex h-2 w-2">
                  <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-75" />
                  <span className="relative inline-flex h-2 w-2 rounded-full bg-emerald-500" />
                </span>
                Live · EventSource with 10s poll fallback
              </div>
            </div>
            <div className="w-full max-w-[520px] shrink-0 rounded-2xl border border-slate-200/50 bg-[#f9fafb] p-4 font-mono text-xs leading-5">
              <div className="flex items-center gap-2 text-[11px] font-semibold uppercase tracking-widest text-zinc-500">
                <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
                useSSE.ts — kot.generated → refresh
              </div>
              <div className="mt-3 space-y-1.5 text-zinc-600">
                <div className="flex gap-2">
                  <span className="text-zinc-400">→</span>
                  <span>event: kot.generated</span>
                  <span className="ml-auto text-emerald-600">
                    data: {"{id:'KOT-0041'}"}
                  </span>
                </div>
                <div className="flex gap-2">
                  <span className="text-zinc-400">→</span>
                  <span>event: order_update</span>
                  <span className="ml-auto text-zinc-900">
                    grand_total: &quot;1240.00&quot;
                  </span>
                </div>
              </div>
              <div className="mt-3 rounded-lg border border-slate-200/50 bg-white px-3 py-2 font-mono text-xs text-zinc-500">
                hook:{" "}
                <code className="font-mono">src/hooks/useSSE.ts</code> — reconnect,
                kot_update / order_update, connected + error
              </div>
            </div>
          </div>
        </div>
      </div>

      {/* Foundations — border-t / divide-y, not cards */}
      <div className="mt-10 border-t border-slate-200/50 pt-8">
        <div className="flex flex-wrap items-baseline justify-between gap-3">
          <h2 className="text-sm font-semibold tracking-tight text-zinc-900">
            Foundations
          </h2>
          <span className="text-xs text-zinc-500">
            auth-less · string money · decimal.js
          </span>
        </div>

        <div className="mt-6 overflow-hidden rounded-[2.5rem] border border-slate-200/50 bg-white shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
          <div className="grid gap-4 p-6 sm:grid-cols-12 sm:items-center sm:gap-6 sm:p-8">
            <div className="sm:col-span-3">
              <div className="font-mono text-xs font-semibold tracking-wide text-zinc-900">
                src/lib/api.ts
              </div>
              <div className="mt-1 text-xs text-zinc-500">Typed client</div>
            </div>
            <div className="max-w-[65ch] text-sm leading-6 text-zinc-600 sm:col-span-9">
              Table, Menu, Order, KOT, Invoice, Shift, Aggregator, Reports,
              Health — RFC 7807 Problem JSON, Idempotency-Key &amp; X-Restaurant
              headers.
            </div>
          </div>
          <div className="grid gap-4 border-t border-slate-200/50 p-6 sm:grid-cols-12 sm:items-center sm:gap-6 sm:p-8">
            <div className="sm:col-span-3">
              <div className="font-mono text-xs font-semibold tracking-wide text-zinc-900">
                src/lib/money.ts
              </div>
              <div className="mt-1 text-xs text-zinc-500">Paisa-accurate</div>
            </div>
            <div className="max-w-[65ch] text-sm leading-6 text-zinc-600 sm:col-span-9">
              formatMoney, parseMoney, addMoney, mulMoney — decimal.js,
              MidpointAwayFromZero, never JS Number, 2-decimal paisa string
              end-to-end.
            </div>
          </div>
          <div className="grid gap-4 border-t border-slate-200/50 p-6 sm:grid-cols-12 sm:items-center sm:gap-6 sm:p-8">
            <div className="sm:col-span-3">
              <div className="font-mono text-xs font-semibold tracking-wide text-zinc-900">
                src/hooks/useSSE.ts
              </div>
              <div className="mt-1 text-xs text-zinc-500">Live KDS</div>
            </div>
            <div className="max-w-[65ch] text-sm leading-6 text-zinc-600 sm:col-span-9">
              EventSource, reconnect, kot_update / order_update, connected +
              error — live ticket board without polling, with 10s fallback when
              offline.
            </div>
          </div>
        </div>

        <div className="mt-6 flex items-center gap-2 text-xs text-zinc-500">
          <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
          <span>
            Wave 3 lane A — typed client + money + SSE is auth-less and does
            not touch{" "}
            <code className="font-mono">peacock-api</code> /{" "}
            <code className="font-mono">peacock-core</code> /{" "}
            <code className="font-mono">peacock-storage</code>.
          </span>
        </div>
      </div>
    </div>
  );
}
