import Link from "next/link";

export default function Home() {
  return (
    <div className="mx-auto flex w-full max-w-6xl flex-1 flex-col px-4 py-10 sm:px-6 sm:py-16">
      {/* Hero */}
      <div className="rounded-2xl border border-zinc-200 bg-white p-8 shadow-sm sm:p-10 dark:border-zinc-800 dark:bg-zinc-900">
        <div className="flex flex-col gap-6">
          <div className="inline-flex w-fit items-center gap-2 rounded-full border border-amber-200 bg-amber-50 px-3 py-1 text-xs font-medium text-amber-800 dark:border-amber-900/50 dark:bg-amber-950/40 dark:text-amber-200">
            <span className="h-2 w-2 rounded-full bg-amber-500" />
            W3-A foundation — scaffold complete, screens in W3-B/C
          </div>

          <div>
            <h1 className="text-3xl font-semibold tracking-tight sm:text-4xl">
              Peacock POS
            </h1>
            <p className="mt-3 max-w-2xl text-sm leading-6 text-zinc-600 sm:text-base dark:text-zinc-400">
              Next.js 15 + TypeScript + Tailwind foundation. Typed API client generated from{" "}
              <code className="rounded bg-zinc-100 px-1 py-0.5 font-mono text-xs dark:bg-zinc-800">
                docs/API.md
              </code>{" "}
              — money as string end-to-end (never JS Number), RFC 7807 errors, Idempotency-Key &
              X-Restaurant headers, and an SSE hook for the live KDS board.
            </p>
          </div>

          <div className="flex flex-wrap gap-3">
            <Link
              href="/pos"
              className="inline-flex items-center justify-center rounded-full bg-zinc-900 px-6 py-2.5 text-sm font-medium text-white transition hover:bg-zinc-800 dark:bg-white dark:text-zinc-900 dark:hover:bg-zinc-100"
            >
              Open POS →
            </Link>
            <Link
              href="/kds"
              className="inline-flex items-center justify-center rounded-full border border-zinc-300 bg-white px-6 py-2.5 text-sm font-medium text-zinc-700 transition hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200 dark:hover:bg-zinc-700"
            >
              KDS Board →
            </Link>
          </div>

          <div className="grid gap-3 pt-2 sm:grid-cols-3">
            <div className="rounded-xl border border-zinc-200 bg-zinc-50 p-4 dark:border-zinc-800 dark:bg-zinc-800/50">
              <div className="text-xs font-semibold uppercase tracking-widest text-zinc-500 dark:text-zinc-400">
                API base
              </div>
              <div className="mt-1 font-mono text-xs text-zinc-900 dark:text-zinc-100">
                {process.env.NEXT_PUBLIC_API_URL ?? "http://100.72.103.1:8080"}
              </div>
              <div className="mt-1 text-xs text-zinc-500">override via NEXT_PUBLIC_API_URL</div>
            </div>
            <div className="rounded-xl border border-zinc-200 bg-zinc-50 p-4 dark:border-zinc-800 dark:bg-zinc-800/50">
              <div className="text-xs font-semibold uppercase tracking-widest text-zinc-500 dark:text-zinc-400">
                Money
              </div>
              <div className="mt-1 font-mono text-xs text-zinc-900 dark:text-zinc-100">
                string only — decimal.js
              </div>
              <div className="mt-1 text-xs text-zinc-500">never Number, paisa-accurate</div>
            </div>
            <div className="rounded-xl border border-zinc-200 bg-zinc-50 p-4 dark:border-zinc-800 dark:bg-zinc-800/50">
              <div className="text-xs font-semibold uppercase tracking-widest text-zinc-500 dark:text-zinc-400">
                SSE
              </div>
              <div className="mt-1 font-mono text-xs text-zinc-900 dark:text-zinc-100">
                /api/events/stream
              </div>
              <div className="mt-1 text-xs text-zinc-500">kot_update · order_update</div>
            </div>
          </div>
        </div>
      </div>

      {/* Module status */}
      <div className="mt-6 grid gap-4 sm:grid-cols-3">
        <div className="rounded-xl border border-zinc-200 bg-white p-5 dark:border-zinc-800 dark:bg-zinc-900">
          <h2 className="text-sm font-semibold">API Client</h2>
          <p className="mt-1 text-xs leading-5 text-zinc-600 dark:text-zinc-400">
            <code className="font-mono">src/lib/api.ts</code> — Table, Menu, Order, KOT, Invoice,
            Shift, Aggregator, Reports, Health. Typed, with Problem JSON and header support.
          </p>
        </div>
        <div className="rounded-xl border border-zinc-200 bg-white p-5 dark:border-zinc-800 dark:bg-zinc-900">
          <h2 className="text-sm font-semibold">Money</h2>
          <p className="mt-1 text-xs leading-5 text-zinc-600 dark:text-zinc-400">
            <code className="font-mono">src/lib/money.ts</code> — formatMoney, parseMoney,
            addMoney, decimal.js, paisa-accurate.
          </p>
        </div>
        <div className="rounded-xl border border-zinc-200 bg-white p-5 dark:border-zinc-800 dark:bg-zinc-900">
          <h2 className="text-sm font-semibold">SSE Hook</h2>
          <p className="mt-1 text-xs leading-5 text-zinc-600 dark:text-zinc-400">
            <code className="font-mono">src/hooks/useSSE.ts</code> — EventSource, reconnect,
            kot_update / order_update, connected + error.
          </p>
        </div>
      </div>

      <div className="mt-6 rounded-xl border border-dashed border-zinc-300 bg-zinc-50 p-4 text-xs leading-5 text-zinc-600 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-400">
        <strong className="font-semibold text-zinc-900 dark:text-zinc-100">Wave 3 lane A note:</strong>{" "}
        POS and KDS screens are built in W3-B/C. This foundation (typed client + money + SSE) is
        auth-less and does not touch <code className="font-mono">peacock-api</code> /{" "}
        <code className="font-mono">peacock-core</code> /{" "}
        <code className="font-mono">peacock-storage</code>.
      </div>
    </div>
  );
}
