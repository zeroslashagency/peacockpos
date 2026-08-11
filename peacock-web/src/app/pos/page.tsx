"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { motion, AnimatePresence } from "framer-motion";
import {
  MagnifyingGlass,
  Chair,
  CookingPot,
  Receipt,
  Minus,
  Plus,
  X,
  Trash,
  ShoppingCart,
  Storefront,
  Users,
  ArrowsClockwise,
  Star,
  Check,
  House,
  CircleNotch,
} from "@phosphor-icons/react";
import {
  tablesApi,
  menuApi,
  itemsApi,
  ordersApi,
  kotApi,
  invoicesApi,
  newIdempotencyKey,
  type TableResponse,
  type MenuItemResponse,
  type OrderResponse,
} from "@/lib/api";
import { formatMoney, sumMoney, mulMoney } from "@/lib/money";

type CartLine = { code: string; name: string; qty: number; rate: string; course: string | null };

function todayISO(): string {
  return new Date().toISOString().slice(0, 10);
}

const spring = { type: "spring" as const, stiffness: 100, damping: 20 };
const containerVariants = {
  hidden: {},
  show: { transition: { staggerChildren: 0.08 } },
};
const itemVariants = {
  hidden: { opacity: 0, y: 8 },
  show: { opacity: 1, y: 0, transition: spring },
};

// ---------------------------------------------------------------------------
// Skeletons
// ---------------------------------------------------------------------------

function FloorSkeleton() {
  return (
    <div className="grid grid-cols-2 gap-2">
      {Array.from({ length: 6 }).map((_, i) => (
        <div
          key={i}
          className="shimmer animate-pulse rounded-2xl border border-slate-200/50 bg-white p-4"
          style={{ animationDelay: `${i * 80}ms` }}
        >
          <div className="flex items-center justify-between">
            <div className="h-4 w-12 rounded-full bg-slate-100" />
            <div className="h-2 w-2 rounded-full bg-slate-200" />
          </div>
          <div className="mt-3 h-3 w-20 rounded-full bg-slate-100" />
          <div className="mt-2 h-2 w-16 rounded-full bg-slate-50" />
        </div>
      ))}
    </div>
  );
}

function MenuSkeleton() {
  return (
    <div className="grid gap-4 sm:grid-cols-2">
      {Array.from({ length: 6 }).map((_, i) => (
        <div
          key={i}
          className="shimmer overflow-hidden rounded-2xl border border-slate-200/50 bg-white"
        >
          <div
            className={`m-3 mb-0 animate-pulse bg-slate-100 ${i % 2 === 0 ? "aspect-[4/3]" : "aspect-[16/9]"} rounded-2xl`}
          />
          <div className="p-4">
            <div className="h-4 w-3/4 rounded-full bg-slate-100" />
            <div className="mt-2 flex gap-2">
              <div className="h-3 w-12 rounded-full bg-slate-50" />
              <div className="h-3 w-16 rounded-full bg-slate-50" />
            </div>
            <div className="mt-3 h-5 w-20 rounded-full bg-slate-100" />
            <div className="mt-3 h-8 w-full rounded-full bg-slate-100" />
          </div>
        </div>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Empty states
// ---------------------------------------------------------------------------

function FloorEmpty({ filter, onRetry }: { filter: string; onRetry: () => void }) {
  return (
    <div className="flex flex-col items-center rounded-2xl border border-slate-200/50 bg-zinc-50/60 px-6 py-10 text-center">
      <span className="flex h-12 w-12 items-center justify-center rounded-2xl bg-white shadow-sm ring-1 ring-slate-200/50">
        <Chair size={20} weight="regular" className="text-zinc-400" />
      </span>
      <h3 className="mt-4 text-sm font-semibold tracking-tighter text-zinc-900">No tables yet</h3>
      <p className="mt-1 max-w-[32ch] text-sm leading-6 text-zinc-500">
        {filter ? (
          <>
            No match for <span className="font-mono font-medium text-zinc-700">“{filter}”</span> — try another room or clear the filter.
          </>
        ) : (
          <>Sync from ERP or set a room to load the floor plan.</>
        )}
      </p>
      <button
        onClick={onRetry}
        className="mt-4 inline-flex items-center gap-1.5 rounded-full bg-zinc-900 px-4 py-2 text-xs font-semibold tracking-tight text-white transition hover:bg-zinc-800 active:scale-[0.98]"
      >
        <ArrowsClockwise size={14} weight="regular" />
        Sync floor
      </button>
    </div>
  );
}

function MenuEmpty({
  q,
  course,
  hasRestaurant,
}: {
  q: string;
  course: string;
  hasRestaurant: boolean;
}) {
  if (!hasRestaurant) {
    return (
      <div className="flex flex-col items-center rounded-2xl border border-slate-200/50 bg-zinc-50/60 px-6 py-12 text-center">
        <span className="flex h-12 w-12 items-center justify-center rounded-2xl bg-white shadow-sm ring-1 ring-slate-200/50">
          <CookingPot size={20} weight="regular" className="text-zinc-400" />
        </span>
        <h3 className="mt-4 text-sm font-semibold tracking-tighter text-zinc-900">Set restaurant to load menu</h3>
        <p className="mt-1 max-w-[32ch] text-sm leading-6 text-zinc-500">
          Choose a restaurant and pick a table room — we resolve the menu for that room.
        </p>
      </div>
    );
  }
  return (
    <div className="flex flex-col items-center rounded-2xl border border-slate-200/50 bg-zinc-50/60 px-6 py-10 text-center">
      <span className="flex h-12 w-12 items-center justify-center rounded-2xl bg-white shadow-sm ring-1 ring-slate-200/50">
        <MagnifyingGlass size={20} weight="regular" className="text-zinc-400" />
      </span>
      <h3 className="mt-4 text-sm font-semibold tracking-tighter text-zinc-900">No dishes found</h3>
      <p className="mt-1 text-sm leading-6 text-zinc-500">
        {q ? (
          <>
            No match for <span className="font-mono font-medium text-zinc-700">“{q}”</span>
          </>
        ) : null}
        {q && course !== "All" ? " · " : null}
        {course !== "All" ? (
          <>
            in <span className="font-medium text-zinc-700">{course}</span>
          </>
        ) : !q ? (
          <>Try a different course or search.</>
        ) : null}
      </p>
    </div>
  );
}

function CartEmpty() {
  return (
    <div className="flex flex-col items-center px-6 py-10 text-center">
      <span className="flex h-12 w-12 items-center justify-center rounded-2xl bg-zinc-50 ring-1 ring-slate-200/50">
        <ShoppingCart size={20} weight="regular" className="text-zinc-400" />
      </span>
      <h3 className="mt-4 text-sm font-semibold tracking-tighter text-zinc-900">Your cart is empty</h3>
      <p className="mt-1 max-w-[28ch] text-sm leading-6 text-zinc-500">Tap a dish to fire — it will appear here ready for the kitchen.</p>
      <span className="mt-3 inline-flex items-center gap-1 text-xs font-medium tracking-tight text-zinc-400">
        <span className="h-px w-6 bg-slate-200" /> Add from menu
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export default function PosPage() {
  // restaurant / customer header
  const [restaurant, setRestaurant] = useState("Peacock Restaurant");
  const [customer, setCustomer] = useState("Walk-in");
  const [pax, setPax] = useState(2);

  // tables
  const [tables, setTables] = useState<TableResponse[]>([]);
  const [tLoading, setTLoading] = useState(false);
  const [tErr, setTErr] = useState<string | null>(null);
  const [selTable, setSelTable] = useState<TableResponse | null>(null);
  const [tFilter, setTFilter] = useState("");

  // menu
  const [menuItems, setMenuItems] = useState<MenuItemResponse[]>([]);
  const [menuMeta, setMenuMeta] = useState<{ menu: string; strategy: string; fellBack: boolean } | null>(null);
  const [mLoading, setMLoading] = useState(false);
  const [mErr, setMErr] = useState<string | null>(null);
  const [q, setQ] = useState("");
  const [course, setCourse] = useState<string>("All");
  const [priceInfo, setPriceInfo] = useState<Record<string, string>>({});
  const [priceBusy, setPriceBusy] = useState<string | null>(null);

  // cart / order / invoice
  const [cart, setCart] = useState<CartLine[]>([]);
  const [order, setOrder] = useState<OrderResponse | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [ok, setOk] = useState<string | null>(null);
  const [invoiceId, setInvoiceId] = useState<string | null>(null);
  const [kotRes, setKotRes] = useState<string | null>(null);

  const fetchTables = useCallback(async () => {
    setTLoading(true);
    setTErr(null);
    try {
      const r = await tablesApi.list();
      setTables(r.tables);
      if (r.tables.length && restaurant === "Peacock Restaurant") {
        const rc = r.tables[0]?.restaurant;
        if (rc) setRestaurant(rc);
      }
    } catch (e) {
      setTErr(e instanceof Error ? e.message : String(e));
    } finally {
      setTLoading(false);
    }
  }, [restaurant]);

  const fetchMenu = useCallback(async () => {
    if (!restaurant) return;
    setMLoading(true);
    setMErr(null);
    try {
      const room = selTable?.restaurant_room || undefined;
      const resolved = await menuApi.resolve({ room }, { restaurant });
      setMenuMeta({ menu: resolved.menu, strategy: resolved.strategy, fellBack: resolved.fell_back });
      try {
        const det = await menuApi.getItems(resolved.menu, { restaurant });
        setMenuItems(det.items);
      } catch {
        setMenuItems(resolved.items);
      }
    } catch (e) {
      setMErr(e instanceof Error ? e.message : String(e));
      setMenuItems([]);
    } finally {
      setMLoading(false);
    }
  }, [restaurant, selTable?.restaurant_room]);

  useEffect(() => {
    fetchTables();
  }, [fetchTables]);
  useEffect(() => {
    fetchMenu();
  }, [fetchMenu]);

  const courses = useMemo(() => {
    const s = new Set<string>();
    menuItems.forEach((i) => {
      if (i.course) s.add(i.course);
    });
    return ["All", ...Array.from(s).sort()];
  }, [menuItems]);

  const filteredMenu = useMemo(() => {
    let a = menuItems;
    if (course !== "All") a = a.filter((i) => i.course === course);
    if (q.trim()) {
      const needle = q.toLowerCase();
      a = a.filter((i) => i.item_name.toLowerCase().includes(needle) || i.item_code.toLowerCase().includes(needle));
    }
    return a;
  }, [menuItems, course, q]);

  const filteredTables = useMemo(() => {
    if (!tFilter.trim()) return tables;
    const n = tFilter.toLowerCase();
    return tables.filter((t) => t.name.toLowerCase().includes(n) || t.restaurant_room.toLowerCase().includes(n));
  }, [tables, tFilter]);

  const cartTotal = useMemo(() => {
    if (!cart.length) return "0.00";
    return sumMoney(cart.map((l) => mulMoney(l.rate, String(l.qty))));
  }, [cart]);

  function addToCart(m: MenuItemResponse) {
    setCart((prev) => {
      const idx = prev.findIndex((p) => p.code === m.item_code);
      if (idx >= 0) {
        const cp = [...prev];
        cp[idx] = { ...cp[idx], qty: cp[idx].qty + 1 };
        return cp;
      }
      return [...prev, { code: m.item_code, name: m.item_name, qty: 1, rate: m.rate, course: m.course }];
    });
    setOk(null);
    setErr(null);
  }
  function changeQty(code: string, d: number) {
    setCart((p) => p.map((l) => (l.code === code ? { ...l, qty: Math.max(1, l.qty + d) } : l)));
  }
  function removeLine(code: string) {
    setCart((p) => p.filter((l) => l.code !== code));
  }

  async function checkPrice(code: string) {
    setPriceBusy(code);
    try {
      const r = await itemsApi.getPrice(code);
      setPriceInfo((s) => ({ ...s, [code]: r.price }));
    } catch (e) {
      setPriceInfo((s) => ({ ...s, [code]: e instanceof Error ? e.message : String(e) }));
    } finally {
      setPriceBusy(null);
    }
  }

  // orders: create on first, patch append thereafter
  async function handleSaveOrder() {
    if (!cart.length) {
      setErr("Cart is empty");
      return;
    }
    setBusy("order");
    setErr(null);
    setOk(null);
    const items = cart.map((l) => ({ item: l.code, item_name: l.name, qty: l.qty, rate: l.rate }));
    try {
      if (!order) {
        const o = await ordersApi.create(
          {
            restaurant_table: selTable?.name ?? null,
            customer_name: customer || "Walk-in",
            no_of_pax: pax,
            items,
          },
          { idempotencyKey: newIdempotencyKey(), restaurant }
        );
        setOrder(o);
        setOk(`Order ${o.id} created · ${formatMoney(o.grand_total)}`);
      } else {
        const o = await ordersApi.patch(order.id, { append_items: items }, { restaurant });
        setOrder(o);
        setOk(`Order ${o.id} updated · ${formatMoney(o.grand_total)}`);
      }
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  async function handleKot() {
    if (!cart.length) {
      setErr("Add items first");
      return;
    }
    setBusy("kot");
    setErr(null);
    setKotRes(null);
    try {
      let inv = invoiceId;
      if (!inv && order) {
        try {
          const invRes = await ordersApi.createInvoice(
            order.id,
            { series: "INV-", date: todayISO(), branch: selTable?.branch ?? "Main Branch" },
            { idempotencyKey: newIdempotencyKey(), restaurant }
          );
          inv = invRes.invoice_name;
          setInvoiceId(inv);
        } catch {
          inv = `INV-${Date.now()}`;
        }
      }
      if (!inv) inv = `INV-POS-${Date.now()}`;
      const res = await kotApi.generate(
        {
          invoice: inv,
          branch: selTable?.branch ?? "Main Branch",
          naming_series: "KOT-",
          date: todayISO(),
          restaurant_table: selTable?.name ?? null,
          room: selTable?.restaurant_room ?? null,
          customer_name: customer,
          items: cart.map((l) => ({ item_code: l.code, item_name: l.name, qty: String(l.qty), comments: null })),
        },
        { restaurant }
      );
      setKotRes(`${res.kots.length} KOT(s) · ${res.unrouted_items.length} unrouted`);
      setOk(`Kitchen sent · invoice ${inv}`);
      setInvoiceId(inv);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  async function handleInvoice() {
    if (!order) {
      setErr("Save order first");
      return;
    }
    setBusy("invoice");
    setErr(null);
    try {
      const req = {
        order_id: order.id,
        table: selTable?.name ?? null,
        customer_name: customer,
        lines: cart.map((l) => ({ item_code: l.code, item_name: l.name, quantity: String(l.qty), rate: l.rate })),
        tax_rate: "0.05",
        series: "INV-",
      };
      const inv = await invoicesApi.create(req as never, { idempotencyKey: newIdempotencyKey(), restaurant });
      setInvoiceId(inv.invoice_id);
      setOk(`Invoice ${inv.invoice_id} · ${formatMoney(inv.grand_total)} (round ${formatMoney(inv.rounded_total)})`);
    } catch (e) {
      try {
        const r = await ordersApi.createInvoice(
          order.id,
          { series: "INV-", date: todayISO(), branch: selTable?.branch ?? "Main Branch" },
          { idempotencyKey: newIdempotencyKey(), restaurant }
        );
        setInvoiceId(r.invoice_name);
        setOk(`Invoice ${r.invoice_name} · ${formatMoney(r.grand_total)}`);
      } catch (e2) {
        setErr(e2 instanceof Error ? e2.message : String(e2));
      }
    } finally {
      setBusy(null);
    }
  }

  async function handlePay() {
    if (!invoiceId) {
      setErr("Create invoice first");
      return;
    }
    setBusy("pay");
    setErr(null);
    try {
      const paid = await invoicesApi.pay(invoiceId, { method: "Cash", amount: cartTotal }, { restaurant });
      setOk(`Paid ${formatMoney(paid.paid_amount)} · outstanding ${formatMoney(paid.outstanding_amount)} · ${paid.status}`);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  const totalPcs = cart.reduce((a, b) => a + b.qty, 0);

  return (
    <div className="mx-auto flex w-full max-w-[1400px] flex-col gap-6 bg-[#f9fafb] px-4 py-6 sm:px-6 lg:px-8 lg:py-10">
      {/* Hero */}
      <div className="flex flex-col gap-3 lg:flex-row lg:items-end lg:justify-between">
        <div>
          <h1 className="text-3xl font-semibold tracking-tighter leading-none text-zinc-900">Service</h1>
          <p className="mt-2 max-w-[65ch] text-sm leading-6 text-zinc-600">
            Floor → Menu → Fire · Pick a table, build the cart, send to kitchen and invoice.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <div className="inline-flex items-center gap-2 rounded-full border border-slate-200/60 bg-white px-4 py-2 shadow-sm">
            <span className="text-xs tracking-tight text-zinc-500">{selTable ? `Table ${selTable.name}` : "Takeaway"}</span>
            <span className="h-1 w-1 rounded-full bg-zinc-300" />
            <span className="font-mono text-sm font-semibold tracking-tighter text-zinc-900">{formatMoney(cartTotal)}</span>
            <span className="text-xs tracking-tight text-zinc-400">· {totalPcs} pcs · {cart.length} lines</span>
          </div>
          {order && (
            <span className="inline-flex items-center rounded-full border border-slate-200/60 bg-white px-3 py-1.5 font-mono text-xs font-medium tracking-tight text-zinc-700 shadow-sm">
              {order.id.slice(0, 8)} · {formatMoney(order.grand_total)}
            </span>
          )}
          {invoiceId && (
            <span className="inline-flex items-center rounded-full bg-zinc-900 px-3 py-1.5 font-mono text-xs font-medium tracking-tight text-white">
              {invoiceId.slice(0, 14)}
            </span>
          )}
        </div>
      </div>

      {/* Header filters — label above input gap-2 */}
      <section className="rounded-[2.5rem] border border-slate-200/50 bg-white p-6 sm:p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
        <div className="grid gap-6 sm:grid-cols-3">
          {/* Restaurant */}
          <label className="flex flex-col gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500">Restaurant — X-Restaurant</span>
            <div className="relative">
              <Storefront size={16} weight="regular" className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-zinc-400" />
              <input
                value={restaurant}
                onChange={(e) => setRestaurant(e.target.value)}
                placeholder="Peacock Restaurant"
                className="w-full rounded-2xl border border-slate-200 bg-white py-2.5 pl-9 pr-3 text-sm tracking-tight text-zinc-900 placeholder:text-zinc-400 outline-none transition focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10"
              />
            </div>
            <span className="text-xs leading-5 text-zinc-400">
              Auto-filled from first table · sent as <span className="font-mono text-zinc-600">X-Restaurant</span>
            </span>
            {mErr && (
              <span className="text-xs font-medium leading-5 text-red-600">
                Couldn’t resolve menu — {mErr}
              </span>
            )}
          </label>

          {/* Customer */}
          <label className="flex flex-col gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500">Customer</span>
            <div className="relative">
              <Users size={16} weight="regular" className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-zinc-400" />
              <input
                value={customer}
                onChange={(e) => setCustomer(e.target.value)}
                placeholder="Walk-in"
                className="w-full rounded-2xl border border-slate-200 bg-white py-2.5 pl-9 pr-3 text-sm tracking-tight text-zinc-900 placeholder:text-zinc-400 outline-none transition focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10"
              />
            </div>
            <span className="text-xs leading-5 text-zinc-400">Shown on KOT & invoice · defaults to Walk-in.</span>
          </label>

          {/* Pax stepper — not number input */}
          <div className="flex flex-col gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500">Guests · Pax</span>
            <div className="flex items-center gap-2 rounded-2xl border border-slate-200 bg-white p-1.5">
              <button
                type="button"
                onClick={() => setPax((p) => Math.max(1, p - 1))}
                className="flex h-8 w-8 items-center justify-center rounded-full bg-zinc-900 text-white transition hover:bg-zinc-800 active:scale-[0.98] active:-translate-y-[1px]"
                aria-label="Decrease pax"
              >
                <Minus size={14} weight="regular" />
              </button>
              <span className="flex flex-1 justify-center font-mono text-sm font-semibold tracking-tighter text-zinc-900">{pax}</span>
              <button
                type="button"
                onClick={() => setPax((p) => p + 1)}
                className="flex h-8 w-8 items-center justify-center rounded-full bg-white ring-1 ring-slate-200 transition hover:bg-zinc-50 active:scale-[0.98] active:-translate-y-[1px]"
                aria-label="Increase pax"
              >
                <Plus size={14} weight="regular" className="text-zinc-700" />
              </button>
            </div>
            <span className="text-xs leading-5 text-zinc-400">
              Covers for <span className="font-mono font-medium text-zinc-600">{selTable?.name ?? "takeaway"}</span> · min {selTable?.minimum_seating ?? 1}
            </span>
          </div>
        </div>

        <div className="mt-6 flex flex-wrap gap-2">
          <button
            onClick={fetchTables}
            className="inline-flex items-center gap-1.5 rounded-full border border-slate-200 bg-white px-4 py-2 text-sm font-medium tracking-tight text-zinc-700 transition hover:bg-zinc-50 active:scale-[0.98]"
          >
            <ArrowsClockwise size={14} weight="regular" />
            Refresh floor
          </button>
          <button
            onClick={fetchMenu}
            className="inline-flex items-center gap-1.5 rounded-full bg-zinc-900 px-5 py-2 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:bg-zinc-800 active:scale-[0.98] active:-translate-y-[1px]"
          >
            <CookingPot size={14} weight="regular" />
            Reload menu
          </button>
        </div>

        {/* Inline status — not red slab hero */}
        <AnimatePresence>
          {(err || ok || kotRes) && (
            <motion.div
              initial={{ opacity: 0, y: 4 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: 4 }}
              transition={spring}
              className="mt-4 flex flex-col gap-2"
            >
              {err && (
                <div className="rounded-2xl border border-red-200/60 bg-red-50 px-4 py-3 text-sm leading-6 text-red-700">
                  {err}
                </div>
              )}
              {ok && (
                <div className="rounded-2xl border border-emerald-200/60 bg-emerald-50 px-4 py-3 text-sm leading-6 text-emerald-800">
                  <span className="inline-flex items-center gap-1.5">
                    <Check size={14} weight="bold" className="text-emerald-600" />
                    {ok}
                  </span>
                </div>
              )}
              {kotRes && (
                <div className="rounded-2xl border border-slate-200/60 bg-zinc-50 px-4 py-3 text-sm leading-6 text-zinc-700">
                  {kotRes}
                </div>
              )}
            </motion.div>
          )}
        </AnimatePresence>
      </section>

      {/* Bento: asymmetric 7 / 5 — grid grid-cols-12 gap-6 col-span-7 Menu col-span-5 stack Floor+Cart */}
      <div className="grid grid-cols-12 gap-6 max-[768px]:grid-cols-1">
        {/* Menu — col-span-7 */}
        <div className="flex flex-col gap-3 col-span-7 max-[768px]:col-span-1">
          <div className="flex items-baseline justify-between px-1">
            <h2 className="text-lg font-semibold tracking-tighter text-zinc-900">Menu</h2>
            <span className="font-mono text-xs tracking-tight text-zinc-500">
              {mLoading ? "loading…" : menuItems.length === 0 ? "—" : `${filteredMenu.length} / ${menuItems.length}`} {menuMeta ? `· ${menuMeta.menu}` : ""}{menuMeta?.fellBack ? " · fallback" : ""}{menuMeta?.strategy ? ` · ${menuMeta.strategy}` : ""}
            </span>
          </div>

          <div className="flex flex-col overflow-hidden rounded-[2.5rem] border border-slate-200/50 bg-white shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
            {/* Search + filter pills */}
            <div className="border-b border-slate-100 p-6">
              <div className="relative">
                <MagnifyingGlass
                  size={16}
                  weight="regular"
                  className="pointer-events-none absolute left-3.5 top-1/2 -translate-y-1/2 text-zinc-400"
                />
                <input
                  value={q}
                  onChange={(e) => setQ(e.target.value)}
                  placeholder="Search dish or code…"
                  className="w-full rounded-full border border-slate-200 bg-zinc-50 py-2.5 pl-10 pr-4 text-sm tracking-tight text-zinc-900 placeholder:text-zinc-400 outline-none transition focus:border-zinc-900 focus:bg-white focus:ring-2 focus:ring-zinc-900/10"
                />
              </div>

              {/* filter pills horizontal scroll */}
              <div className="mt-4 -mx-1 flex gap-2 overflow-x-auto px-1 pb-1 scrollbar-none">
                {courses.map((c) => (
                  <button
                    key={c}
                    onClick={() => setCourse(c)}
                    className={`shrink-0 rounded-full px-4 py-1.5 text-xs font-medium tracking-tight transition active:scale-[0.98] ${
                      course === c
                        ? "bg-zinc-900 text-white shadow-sm"
                        : "border border-slate-200 bg-white text-zinc-600 hover:bg-zinc-50"
                    }`}
                    style={{ transition: "all 0.3s cubic-bezier(0.16,1,0.3,1)" }}
                  >
                    {c}
                  </button>
                ))}
              </div>

              {menuMeta?.fellBack && (
                <div className="mt-3 rounded-2xl bg-amber-50 px-3 py-2 text-xs leading-5 text-amber-800">
                  Fallback menu — no exact match for room{" "}
                  <span className="font-mono font-medium">{selTable?.restaurant_room ?? "—"}</span>
                </div>
              )}
            </div>

            {/* Menu grid */}
            <div className="p-4 sm:p-6">
              {mLoading ? (
                <MenuSkeleton />
              ) : mErr ? (
                <div className="rounded-2xl border border-slate-200/50 bg-zinc-50 px-6 py-8 text-center">
                  <p className="text-sm font-medium tracking-tight text-zinc-900">Couldn’t load menu</p>
                  <p className="mx-auto mt-1 max-w-[42ch] text-sm leading-6 text-zinc-500">{mErr}</p>
                  <button
                    onClick={fetchMenu}
                    className="mt-4 inline-flex items-center gap-1.5 text-xs font-semibold tracking-tight text-zinc-900 underline decoration-slate-300 underline-offset-4 hover:decoration-zinc-900"
                  >
                    Retry
                  </button>
                </div>
              ) : filteredMenu.length === 0 ? (
                <MenuEmpty q={q} course={course} hasRestaurant={!!restaurant} />
              ) : (
                <motion.div
                  variants={containerVariants}
                  initial="hidden"
                  animate="show"
                  className="grid gap-4 sm:grid-cols-2"
                >
                  {filteredMenu.map((it, idx) => (
                    <motion.div
                      key={it.item_code}
                      variants={itemVariants}
                      layout
                      transition={spring}
                      className="group flex flex-col overflow-hidden rounded-2xl border border-slate-200/50 bg-white transition-all duration-300 hover:-translate-y-[1px] hover:shadow-md"
                      style={{ transitionTimingFunction: "cubic-bezier(0.16,1,0.3,1)" }}
                    >
                      {/* Image */}
                      <div className="relative m-3 mb-0 overflow-hidden rounded-2xl bg-zinc-100">
                        {/* eslint-disable-next-line @next/next/no-img-element */}
                        <img
                          src={`https://picsum.photos/seed/${encodeURIComponent(it.item_code)}/400/300`}
                          alt=""
                          loading="lazy"
                          className={`w-full object-cover transition duration-500 group-hover:scale-[1.02] ${idx % 2 === 0 ? "aspect-[4/3]" : "aspect-[16/9]"}`}
                        />
                        {it.special_dish && (
                          <span className="absolute right-2 top-2 inline-flex items-center gap-1 rounded-full bg-emerald-600 px-2.5 py-1 text-[10px] font-bold uppercase tracking-widest text-white shadow-sm">
                            <Star size={10} weight="fill" />
                            Special
                          </span>
                        )}
                      </div>

                      {/* Content below image — rate outside */}
                      <div className="flex flex-1 flex-col p-4">
                        <div className="line-clamp-2 text-sm font-medium leading-tight tracking-tight text-zinc-900">{it.item_name}</div>
                        <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                          <span className="font-mono text-xs tracking-tight text-zinc-500">{it.item_code}</span>
                          {it.course && (
                            <span className="rounded-full bg-zinc-100 px-2 py-0.5 text-[11px] font-medium tracking-tight text-zinc-600">
                              {it.course}
                              {it.course_sequence ? ` #${it.course_sequence}` : ""}
                            </span>
                          )}
                        </div>
                        {/* Rate font-mono text-lg outside image */}
                        <div className="mt-2 font-mono text-lg font-semibold tracking-tighter text-zinc-900">{formatMoney(it.rate)}</div>

                        {priceInfo[it.item_code] && (
                          <div className="mt-2 rounded-xl bg-emerald-50 px-2.5 py-1.5 font-mono text-xs tracking-tight text-emerald-700">
                            Pricelist:{" "}
                            {priceInfo[it.item_code].startsWith("₹") ||
                            priceInfo[it.item_code].includes("HTTP") ||
                            priceInfo[it.item_code].length > 20
                              ? priceInfo[it.item_code]
                              : formatMoney(priceInfo[it.item_code])}
                          </div>
                        )}

                        <div className="mt-3 flex gap-2">
                          <motion.button
                            whileTap={{ scale: 0.98, y: 1 }}
                            transition={spring}
                            onClick={() => addToCart(it)}
                            className="flex flex-1 items-center justify-center gap-1 rounded-full bg-zinc-900 px-4 py-2 text-xs font-semibold tracking-tight text-white shadow-sm transition hover:bg-zinc-800 hover:shadow"
                          >
                            Add <Plus size={12} weight="bold" />
                          </motion.button>
                          <button
                            onClick={() => checkPrice(it.item_code)}
                            disabled={priceBusy === it.item_code}
                            className="rounded-full border border-slate-200 bg-white px-3 py-2 text-xs font-medium tracking-tight text-zinc-700 transition hover:bg-zinc-50 disabled:opacity-50 active:scale-[0.98]"
                          >
                            {priceBusy === it.item_code ? (
                              <CircleNotch size={12} weight="regular" className="animate-spin" />
                            ) : (
                              "Price"
                            )}
                          </button>
                        </div>
                      </div>
                    </motion.div>
                  ))}
                </motion.div>
              )}
            </div>
          </div>

          {/* Menu footnote — helper link */}
          <p className="px-1 text-xs leading-5 text-zinc-400">
            Prices as string via <span className="font-mono text-zinc-600">formatMoney</span> · Pricelist defaults to Standard Selling.
          </p>
        </div>

        {/* Right stack — col-span-5 — Floor + Cart */}
        <div className="flex flex-col gap-6 col-span-5 max-[768px]:col-span-1">
          {/* Floor plan */}
          <div className="flex flex-col gap-3">
            <div className="flex items-baseline justify-between px-1">
              <h2 className="text-lg font-semibold tracking-tighter text-zinc-900">Floor plan</h2>
              <span className="font-mono text-xs tracking-tight text-zinc-500">
                {tLoading ? "…" : tables.length === 0 ? "—" : `${filteredTables.length} / ${tables.length}`}
              </span>
            </div>

            <div className="flex flex-col overflow-hidden rounded-[2.5rem] border border-slate-200/50 bg-white shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
              <div className="p-6 pb-4">
                <div className="relative">
                  <MagnifyingGlass
                    size={14}
                    weight="regular"
                    className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-zinc-400"
                  />
                  <input
                    value={tFilter}
                    onChange={(e) => setTFilter(e.target.value)}
                    placeholder="Filter T1, Hall…"
                    className="w-full rounded-full border border-slate-200 bg-zinc-50 py-2 pl-9 pr-3 text-sm tracking-tight placeholder:text-zinc-400 outline-none transition focus:border-zinc-900 focus:bg-white focus:ring-2 focus:ring-zinc-900/10"
                  />
                </div>
                {tErr && (
                  <div className="mt-3 flex items-center gap-2 text-xs leading-5">
                    <span className="font-medium tracking-tight text-red-600">Couldn’t load floor — {tErr}</span>
                    <button
                      onClick={fetchTables}
                      className="font-semibold tracking-tight text-zinc-900 underline decoration-slate-300 underline-offset-4 hover:decoration-zinc-900"
                    >
                      Retry
                    </button>
                  </div>
                )}
              </div>

              <div className="h-px bg-slate-100" />

              <div className="p-3 sm:p-4">
                {tLoading ? (
                  <FloorSkeleton />
                ) : filteredTables.length === 0 ? (
                  <FloorEmpty filter={tFilter} onRetry={fetchTables} />
                ) : (
                  <motion.div
                    variants={containerVariants}
                    initial="hidden"
                    animate="show"
                    className="grid grid-cols-2 gap-2"
                  >
                    {filteredTables.map((t) => {
                      const sel = selTable?.name === t.name;
                      const merged = t.merged_with.length > 0;
                      return (
                        <motion.button
                          key={t.name}
                          layout
                          layoutId={`table-${t.name}`}
                          variants={itemVariants}
                          transition={spring}
                          onClick={() => setSelTable(t)}
                          className={`group relative flex flex-col items-start gap-1 rounded-2xl border p-3.5 text-left transition-all duration-300 hover:-translate-y-[1px] hover:shadow-md active:scale-[0.98] ${
                            sel
                              ? "border-zinc-900 bg-zinc-900 text-white shadow-md"
                              : t.occupied
                                ? "border-amber-200/60 bg-amber-50/60 hover:bg-amber-50"
                                : "border-slate-200/60 bg-white hover:bg-zinc-50"
                          }`}
                          style={{ transitionTimingFunction: "cubic-bezier(0.16,1,0.3,1)" }}
                        >
                          <span className="flex w-full items-center justify-between">
                            <span className={`font-mono text-sm font-semibold tracking-tighter ${sel ? "text-white" : "text-zinc-900"}`}>
                              {t.name}
                            </span>
                            <span
                              className={`h-2 w-2 rounded-full ${t.occupied ? "bg-amber-500" : "bg-emerald-500"} ${t.occupied ? "animate-pulse" : ""}`}
                              style={t.occupied ? { animation: "breathe 1.8s ease-in-out infinite" } : undefined}
                              title={t.occupied ? "Occupied" : "Free"}
                            />
                          </span>
                          <span className={`line-clamp-1 text-xs tracking-tight ${sel ? "text-white/70" : "text-zinc-500"}`}>
                            {t.restaurant_room} · {t.no_of_seats} seats{t.is_take_away ? " · Takeaway" : ""}
                          </span>
                          {merged && (
                            <span
                              className={`mt-1 inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium tracking-tight ${
                                sel ? "bg-white/15 text-white" : "bg-zinc-900 text-white"
                              }`}
                            >
                              {t.merged_with.length + 1} merged
                            </span>
                          )}
                          {t.table_shape && (
                            <span className={`text-[10px] uppercase tracking-widest ${sel ? "text-white/50" : "text-zinc-400"}`}>{t.table_shape}</span>
                          )}
                        </motion.button>
                      );
                    })}
                  </motion.div>
                )}
              </div>

              {selTable && (
                <div className="border-t border-slate-100 bg-zinc-50/70 p-4">
                  <div className="rounded-2xl bg-white p-4 ring-1 ring-slate-200/50">
                    <div className="flex items-start justify-between gap-2">
                      <div>
                        <div className="font-mono text-sm font-semibold tracking-tighter text-zinc-900">
                          {selTable.name}{" "}
                          <span className="font-sans text-xs font-normal tracking-tight text-zinc-500">
                            · {selTable.restaurant_room} · {selTable.branch}
                          </span>
                        </div>
                        <div className="mt-1.5 flex flex-wrap gap-1.5">
                          <span className="rounded-full bg-zinc-100 px-2 py-0.5 text-xs tracking-tight text-zinc-600">
                            {selTable.no_of_seats} seats (min {selTable.minimum_seating})
                          </span>
                          <span
                            className={`rounded-full px-2 py-0.5 text-xs font-medium tracking-tight ${
                              selTable.occupied
                                ? "bg-amber-100 text-amber-800"
                                : "bg-emerald-100 text-emerald-800"
                            }`}
                          >
                            {selTable.occupied ? "Occupied" : "Free"}
                          </span>
                          {selTable.merged_with.length > 0 && (
                            <span className="rounded-full bg-zinc-900 px-2 py-0.5 text-xs font-medium tracking-tight text-white">
                              Cluster: {[selTable.name, ...selTable.merged_with].join(", ")}
                            </span>
                          )}
                        </div>
                      </div>
                      <button
                        onClick={() => setSelTable(null)}
                        className="rounded-full bg-zinc-100 p-1.5 text-zinc-500 transition hover:bg-zinc-200 hover:text-zinc-700 active:scale-[0.98]"
                        aria-label="Clear selection"
                      >
                        <X size={12} weight="bold" />
                      </button>
                    </div>
                  </div>
                </div>
              )}
            </div>

            <div className="flex items-center gap-3 px-1 text-xs tracking-tight text-zinc-400">
              <span className="inline-flex items-center gap-1.5">
                <span className="h-2 w-2 rounded-full bg-emerald-500" /> Free
              </span>
              <span className="inline-flex items-center gap-1.5">
                <span className="h-2 w-2 rounded-full bg-amber-500 animate-pulse" style={{ animation: "breathe 1.8s ease-in-out infinite" }} /> Occupied
              </span>
              <span className="ml-auto font-mono text-[11px] text-zinc-400">{selTable ? `T ${selTable.name}` : "No selection"}</span>
            </div>
          </div>

          {/* Cart — sticky top-20 */}
          <div className="flex flex-col gap-3 lg:sticky lg:top-20">
            <div className="flex items-baseline justify-between px-1">
              <h2 className="text-lg font-semibold tracking-tighter text-zinc-900">Cart</h2>
              <span className="font-mono text-xs tracking-tight text-zinc-500">{cart.length} lines · {totalPcs} pcs</span>
            </div>

            <div className="flex flex-col overflow-hidden rounded-[2.5rem] border border-slate-200/50 bg-white shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
              <div className="flex items-center justify-between border-b border-slate-100 px-6 py-4">
                <span className="text-sm font-medium tracking-tight text-zinc-900">
                  {selTable ? (
                    <>
                      Table <span className="font-mono font-semibold tracking-tighter">{selTable.name}</span>{" "}
                      <span className="text-zinc-500">· {selTable.restaurant_room}</span>
                    </>
                  ) : (
                    "No table — takeaway"
                  )}
                </span>
                <span className="font-mono text-sm font-semibold tracking-tighter text-zinc-900">{formatMoney(cartTotal)}</span>
              </div>

              <div className="max-h-[380px] overflow-auto">
                {cart.length === 0 ? (
                  <CartEmpty />
                ) : (
                  <div className="divide-y divide-zinc-100">
                    <AnimatePresence initial={false}>
                      {cart.map((l) => (
                        <motion.div
                          key={l.code}
                          layout
                          initial={{ opacity: 0, y: 4 }}
                          animate={{ opacity: 1, y: 0 }}
                          exit={{ opacity: 0, y: -4 }}
                          transition={spring}
                          className="flex gap-3 px-6 py-4"
                        >
                          <div className="min-w-0 flex-1">
                            <div className="truncate text-sm font-medium tracking-tight text-zinc-900">{l.name}</div>
                            <div className="mt-1 flex flex-wrap items-center gap-1.5 text-xs tracking-tight text-zinc-500">
                              <span className="font-mono text-zinc-600">{l.code}</span>
                              {l.course && <span className="rounded bg-zinc-100 px-1.5 py-0.5">{l.course}</span>}
                              <span className="font-mono font-medium tracking-tight text-zinc-700">
                                {formatMoney(l.rate)} × {l.qty} = {formatMoney(mulMoney(l.rate, String(l.qty)))}
                              </span>
                            </div>
                          </div>
                          <div className="flex shrink-0 items-center gap-1">
                            <motion.button
                              whileTap={{ scale: 0.92, y: 1 }}
                              transition={spring}
                              onClick={() => changeQty(l.code, -1)}
                              className="flex h-7 w-7 items-center justify-center rounded-full border border-slate-200 bg-white text-zinc-700 transition hover:bg-zinc-50 active:-translate-y-[1px]"
                              aria-label="Decrease"
                            >
                              <Minus size={12} weight="regular" />
                            </motion.button>
                            <span className="w-6 text-center font-mono text-sm font-semibold tracking-tighter text-zinc-900">{l.qty}</span>
                            <motion.button
                              whileTap={{ scale: 0.92, y: 1 }}
                              transition={spring}
                              onClick={() => changeQty(l.code, 1)}
                              className="flex h-7 w-7 items-center justify-center rounded-full border border-slate-200 bg-white text-zinc-700 transition hover:bg-zinc-50 active:-translate-y-[1px]"
                              aria-label="Increase"
                            >
                              <Plus size={12} weight="regular" />
                            </motion.button>
                            <button
                              onClick={() => removeLine(l.code)}
                              className="ml-1 rounded-full bg-zinc-100 p-1.5 text-zinc-500 transition hover:bg-red-50 hover:text-red-600 active:scale-[0.98]"
                              aria-label="Remove"
                            >
                              <Trash size={12} weight="regular" />
                            </button>
                          </div>
                        </motion.div>
                      ))}
                    </AnimatePresence>
                  </div>
                )}
              </div>
            </div>

            {/* Totals outside card */}
            {cart.length > 0 && (
              <div className="space-y-1.5 px-2">
                <div className="flex justify-between text-sm">
                  <span className="tracking-tight text-zinc-500">Subtotal · {totalPcs} pcs</span>
                  <span className="font-mono text-sm font-semibold tracking-tighter text-zinc-900">{formatMoney(cartTotal)}</span>
                </div>
                {order && (
                  <div className="flex justify-between text-xs tracking-tight text-zinc-500">
                    <span>
                      Order <span className="font-mono font-medium text-zinc-700">{order.id.slice(0, 8)}</span> · v{order.version} · {order.status}
                    </span>
                    <span className="font-mono font-medium text-zinc-700">{formatMoney(order.grand_total)}</span>
                  </div>
                )}
                {invoiceId && (
                  <div className="flex justify-between text-xs tracking-tight text-zinc-500">
                    <span>Invoice</span>
                    <span className="font-mono font-medium tracking-tight text-zinc-900">{invoiceId}</span>
                  </div>
                )}
              </div>
            )}

            {/* Actions */}
            <div className="grid grid-cols-2 gap-2 px-1">
              <motion.button
                whileTap={{ scale: 0.98, y: 1 }}
                transition={spring}
                onClick={handleSaveOrder}
                disabled={!!busy || !cart.length}
                className="rounded-full bg-zinc-900 px-4 py-2.5 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:bg-zinc-800 hover:shadow disabled:opacity-50 active:-translate-y-[1px]"
              >
                {busy === "order" ? "Saving…" : order ? "Add to order" : "Create order"}
              </motion.button>
              <motion.button
                whileTap={{ scale: 0.98, y: 1 }}
                transition={spring}
                onClick={handleKot}
                disabled={!!busy || !cart.length}
                className="rounded-full bg-emerald-600 px-4 py-2.5 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:bg-emerald-700 hover:shadow disabled:opacity-50 active:-translate-y-[1px]"
              >
                {busy === "kot" ? "Sending…" : "Send to kitchen"}
              </motion.button>
              <motion.button
                whileTap={{ scale: 0.98 }}
                transition={spring}
                onClick={handleInvoice}
                disabled={!!busy || !order}
                className="rounded-full border border-slate-200 bg-white px-4 py-2.5 text-sm font-semibold tracking-tight text-zinc-700 shadow-sm transition hover:bg-zinc-50 disabled:opacity-50 active:-translate-y-[1px]"
              >
                {busy === "invoice" ? "…" : "Create invoice"}
              </motion.button>
              <motion.button
                whileTap={{ scale: 0.98, y: 1 }}
                transition={spring}
                onClick={handlePay}
                disabled={!!busy || !invoiceId}
                className="rounded-full bg-emerald-600 px-4 py-2.5 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:bg-emerald-700 hover:shadow disabled:opacity-50 active:-translate-y-[1px]"
              >
                {busy === "pay" ? "Paying…" : `Pay ${formatMoney(cartTotal)}`}
              </motion.button>
            </div>

            <button
              onClick={() => {
                setCart([]);
                setOrder(null);
                setInvoiceId(null);
                setKotRes(null);
                setOk(null);
                setErr(null);
              }}
              className="self-center text-xs font-medium tracking-tight text-zinc-500 underline decoration-slate-300 underline-offset-4 transition hover:text-zinc-900 hover:decoration-zinc-900"
            >
              Clear cart
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
