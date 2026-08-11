"use client";
import { useCallback, useEffect, useMemo, useState } from "react";
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

function todayISO(): string { return new Date().toISOString().slice(0, 10); }

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
    setTLoading(true); setTErr(null);
    try {
      const r = await tablesApi.list();
      setTables(r.tables);
      if (r.tables.length && restaurant === "Peacock Restaurant") {
        const rc = r.tables[0]?.restaurant;
        if (rc) setRestaurant(rc);
      }
    } catch (e) { setTErr(e instanceof Error ? e.message : String(e)); }
    finally { setTLoading(false); }
  }, [restaurant]);

  const fetchMenu = useCallback(async () => {
    if (!restaurant) return;
    setMLoading(true); setMErr(null);
    try {
      const room = selTable?.restaurant_room || undefined;
      const resolved = await menuApi.resolve({ room }, { restaurant });
      setMenuMeta({ menu: resolved.menu, strategy: resolved.strategy, fellBack: resolved.fell_back });
      try {
        const det = await menuApi.getItems(resolved.menu, { restaurant });
        setMenuItems(det.items);
      } catch { setMenuItems(resolved.items); }
    } catch (e) { setMErr(e instanceof Error ? e.message : String(e)); setMenuItems([]); }
    finally { setMLoading(false); }
  }, [restaurant, selTable?.restaurant_room]);

  useEffect(() => { fetchTables(); }, [fetchTables]);
  useEffect(() => { fetchMenu(); }, [fetchMenu]);

  const courses = useMemo(() => {
    const s = new Set<string>();
    menuItems.forEach((i) => { if (i.course) s.add(i.course); });
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
      if (idx >= 0) { const cp = [...prev]; cp[idx] = { ...cp[idx], qty: cp[idx].qty + 1 }; return cp; }
      return [...prev, { code: m.item_code, name: m.item_name, qty: 1, rate: m.rate, course: m.course }];
    });
    setOk(null); setErr(null);
  }
  function changeQty(code: string, d: number) {
    setCart((p) => p.map((l) => l.code === code ? { ...l, qty: Math.max(1, l.qty + d) } : l));
  }
  function removeLine(code: string) { setCart((p) => p.filter((l) => l.code !== code)); }

  async function checkPrice(code: string) {
    setPriceBusy(code);
    try {
      const r = await itemsApi.getPrice(code);
      setPriceInfo((s) => ({ ...s, [code]: r.price }));
    } catch (e) { setPriceInfo((s) => ({ ...s, [code]: e instanceof Error ? e.message : String(e) })); }
    finally { setPriceBusy(null); }
  }

  // orders: create on first, patch append thereafter
  async function handleSaveOrder() {
    if (!cart.length) { setErr("Cart is empty"); return; }
    setBusy("order"); setErr(null); setOk(null);
    const items = cart.map((l) => ({ item: l.code, item_name: l.name, qty: l.qty, rate: l.rate }));
    try {
      if (!order) {
        const o = await ordersApi.create({
          restaurant_table: selTable?.name ?? null,
          customer_name: customer || "Walk-in",
          no_of_pax: pax,
          items,
        }, { idempotencyKey: newIdempotencyKey(), restaurant });
        setOrder(o); setOk(`Order ${o.id} created · ${formatMoney(o.grand_total)}`);
      } else {
        const o = await ordersApi.patch(order.id, { append_items: items }, { restaurant });
        setOrder(o); setOk(`Order ${o.id} updated · ${formatMoney(o.grand_total)}`);
      }
    } catch (e) { setErr(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(null); }
  }

  async function handleKot() {
    if (!cart.length) { setErr("Add items first"); return; }
    setBusy("kot"); setErr(null); setKotRes(null);
    try {
      // ensure invoice/order context
      let inv = invoiceId;
      if (!inv && order) {
        try {
          const invRes = await ordersApi.createInvoice(order.id, { series: "INV-", date: todayISO(), branch: selTable?.branch ?? "Main Branch" }, { idempotencyKey: newIdempotencyKey(), restaurant });
          inv = invRes.invoice_name; setInvoiceId(inv);
        } catch { inv = `INV-${Date.now()}`; }
      }
      if (!inv) inv = `INV-POS-${Date.now()}`;
      const res = await kotApi.generate({
        invoice: inv,
        branch: selTable?.branch ?? "Main Branch",
        naming_series: "KOT-",
        date: todayISO(),
        restaurant_table: selTable?.name ?? null,
        room: selTable?.restaurant_room ?? null,
        customer_name: customer,
        items: cart.map((l) => ({ item_code: l.code, item_name: l.name, qty: String(l.qty), comments: null })),
      }, { restaurant });
      setKotRes(`${res.kots.length} KOT(s) · ${res.unrouted_items.length} unrouted`);
      setOk(`Kitchen sent · invoice ${inv}`);
      setInvoiceId(inv);
    } catch (e) { setErr(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(null); }
  }

  async function handleInvoice() {
    if (!order) { setErr("Save order first"); return; }
    setBusy("invoice"); setErr(null);
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
      setInvoiceId(inv.invoice_id); setOk(`Invoice ${inv.invoice_id} · ${formatMoney(inv.grand_total)} (round ${formatMoney(inv.rounded_total)})`);
    } catch (e) {
      // fallback to order-invoice
      try {
        const r = await ordersApi.createInvoice(order.id, { series: "INV-", date: todayISO(), branch: selTable?.branch ?? "Main Branch" }, { idempotencyKey: newIdempotencyKey(), restaurant });
        setInvoiceId(r.invoice_name); setOk(`Invoice ${r.invoice_name} · ${formatMoney(r.grand_total)}`);
      } catch (e2) { setErr(e2 instanceof Error ? e2.message : String(e2)); }
    } finally { setBusy(null); }
  }

  async function handlePay() {
    if (!invoiceId) { setErr("Create invoice first"); return; }
    setBusy("pay"); setErr(null);
    try {
      const paid = await invoicesApi.pay(invoiceId, { method: "Cash", amount: cartTotal }, { restaurant });
      setOk(`Paid ${formatMoney(paid.paid_amount)} · outstanding ${formatMoney(paid.outstanding_amount)} · ${paid.status}`);
    } catch (e) { setErr(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(null); }
  }

  return (
    <div className="mx-auto flex w-full max-w-[1600px] flex-col gap-4 px-3 py-4 sm:px-4 lg:px-6">
      {/* header */}
      <div className="rounded-2xl border border-zinc-200 bg-white p-4 shadow-sm dark:border-zinc-800 dark:bg-zinc-900">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h1 className="text-xl font-semibold tracking-tight">POS</h1>
            <p className="text-xs text-zinc-500 dark:text-zinc-400">Floor → Menu → Cart → KOT → Invoice · money as string · {formatMoney("0.00")} </p>
          </div>
          <div className="flex flex-wrap gap-2 text-xs">
            <span className={`rounded-full px-2.5 py-1 font-medium ${selTable ? "bg-emerald-100 text-emerald-800 dark:bg-emerald-900/30 dark:text-emerald-200" : "bg-zinc-100 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300"}`}>{selTable ? `Table ${selTable.name}` : "No table"}</span>
            {order && <span className="rounded-full bg-sky-100 px-2.5 py-1 font-mono text-sky-800 dark:bg-sky-900/30 dark:text-sky-200">{order.id.slice(0, 8)} · {formatMoney(order.grand_total)}</span>}
            {invoiceId && <span className="rounded-full bg-violet-100 px-2.5 py-1 font-mono text-violet-800 dark:bg-violet-900/30 dark:text-violet-200">{invoiceId}</span>}
          </div>
        </div>
        <div className="mt-3 grid gap-2 sm:grid-cols-4">
          <label className="flex flex-col gap-1"><span className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Restaurant (X-Restaurant)</span><input value={restaurant} onChange={(e) => setRestaurant(e.target.value)} placeholder="Peacock Restaurant" className="rounded-xl border border-zinc-300 bg-white px-3 py-2 text-sm outline-none focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10 dark:border-zinc-700 dark:bg-zinc-800" /></label>
          <label className="flex flex-col gap-1"><span className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Customer</span><input value={customer} onChange={(e) => setCustomer(e.target.value)} className="rounded-xl border border-zinc-300 bg-white px-3 py-2 text-sm outline-none focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10 dark:border-zinc-700 dark:bg-zinc-800" /></label>
          <label className="flex flex-col gap-1"><span className="text-[10px] font-semibold uppercase tracking-widest text-zinc-500">Pax</span><input type="number" min={1} value={pax} onChange={(e) => setPax(Math.max(1, Number(e.target.value) || 1))} className="rounded-xl border border-zinc-300 bg-white px-3 py-2 text-sm outline-none focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10 dark:border-zinc-700 dark:bg-zinc-800" /></label>
          <div className="flex items-end gap-1.5"><button onClick={fetchTables} className="w-full rounded-xl border border-zinc-300 bg-white px-3 py-2 text-sm font-medium hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800">Refresh floor</button><button onClick={fetchMenu} className="w-full rounded-xl bg-zinc-900 px-3 py-2 text-sm font-semibold text-white hover:bg-zinc-800 dark:bg-white dark:text-zinc-900">Reload menu</button></div>
        </div>
        {(err || ok || kotRes) && (
          <div className="mt-3 space-y-1.5">
            {err && <div className="rounded-xl border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700 dark:border-red-900/40 dark:bg-red-950/30 dark:text-red-300">{err}</div>}
            {ok && <div className="rounded-xl border border-emerald-200 bg-emerald-50 px-3 py-2 text-sm text-emerald-800 dark:border-emerald-900/30 dark:bg-emerald-950/20 dark:text-emerald-200">{ok}</div>}
            {kotRes && <div className="rounded-xl border border-sky-200 bg-sky-50 px-3 py-2 text-sm text-sky-800 dark:border-sky-900/30 dark:bg-sky-950/20 dark:text-sky-200">{kotRes}</div>}
          </div>
        )}
      </div>

      <div className="grid gap-4 xl:grid-cols-[300px_1fr_380px]">
        {/* floor */}
        <section className="flex flex-col rounded-2xl border border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-900">
          <div className="flex items-center justify-between border-b border-zinc-100 px-4 py-3 dark:border-zinc-800">
            <h2 className="text-sm font-semibold">Floor plan</h2><span className="rounded-full bg-zinc-100 px-2 py-0.5 font-mono text-xs text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300">{filteredTables.length}/{tables.length}</span>
          </div>
          <div className="p-3">
            <input value={tFilter} onChange={(e) => setTFilter(e.target.value)} placeholder="Filter T1, Hall…" className="w-full rounded-full border border-zinc-200 bg-zinc-50 px-3 py-2 text-sm outline-none placeholder:text-zinc-400 focus:border-zinc-900 focus:bg-white dark:border-zinc-700 dark:bg-zinc-800" />
          </div>
          <div className="flex-1 overflow-auto px-3 pb-3">
            {tLoading ? <div className="rounded-xl border border-dashed border-zinc-300 p-6 text-center text-sm text-zinc-500">Loading tables…</div>
              : tErr ? <div className="rounded-xl border border-red-200 bg-red-50 p-3 text-sm text-red-700 dark:bg-red-950/30">{tErr}<button onClick={fetchTables} className="ml-2 underline">retry</button></div>
                : filteredTables.length === 0 ? <div className="rounded-xl border border-dashed border-zinc-300 bg-zinc-50 p-6 text-center text-sm text-zinc-500 dark:border-zinc-700 dark:bg-zinc-800/50">No tables{tFilter ? ` for “${tFilter}”` : " — try with ?room (API returns empty without room)."} <button onClick={fetchTables} className="mt-2 block w-full rounded-full bg-zinc-900 py-2 text-xs font-semibold text-white">Retry</button></div>
                  : (
                    <div className="grid grid-cols-2 gap-2">
                      {filteredTables.map((t) => {
                        const sel = selTable?.name === t.name;
                        const merged = t.merged_with.length > 0;
                        return (
                          <button key={t.name} onClick={() => setSelTable(t)} className={`group relative flex flex-col items-start gap-1 rounded-xl border p-3 text-left transition ${sel ? "border-zinc-900 bg-zinc-900 text-white shadow-md dark:border-white dark:bg-white dark:text-zinc-900" : t.occupied ? "border-amber-200 bg-amber-50 hover:bg-amber-100 dark:border-amber-900/40 dark:bg-amber-950/20" : "border-zinc-200 bg-zinc-50 hover:bg-white dark:border-zinc-700 dark:bg-zinc-800"}`}>
                            <span className="flex w-full items-center justify-between"><span className={`font-mono text-sm font-semibold ${sel ? "" : "text-zinc-900 dark:text-zinc-100"}`}>{t.name}</span><span className={`h-2 w-2 rounded-full ${t.occupied ? "bg-amber-500" : "bg-emerald-500"}`} title={t.occupied ? "Occupied" : "Free"} /></span>
                            <span className={`text-xs ${sel ? "text-white/70 dark:text-zinc-500" : "text-zinc-500"}`}>{t.restaurant_room} · {t.no_of_seats} seats{t.is_take_away ? " · Takeaway" : ""}</span>
                            {merged && <span className={`mt-1 inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium ${sel ? "bg-white/20 text-white dark:bg-zinc-900 dark:text-white" : "bg-violet-100 text-violet-700 dark:bg-violet-900/30 dark:text-violet-200"}`}>⧉ {t.merged_with.length + 1} merged</span>}
                            {merged && <span className={`line-clamp-1 text-[10px] ${sel ? "text-white/60" : "text-zinc-400"}`}>{[t.name, ...t.merged_with].join(" · ")}</span>}
                            {t.table_shape && <span className={`text-[10px] uppercase tracking-widest ${sel ? "text-white/50" : "text-zinc-400"}`}>{t.table_shape}</span>}
                          </button>
                        );
                      })}
                    </div>
                  )}
          </div>
          {selTable && (
            <div className="border-t border-zinc-100 bg-zinc-50 p-3 dark:border-zinc-800 dark:bg-zinc-800/50">
              <div className="rounded-xl bg-white p-3 dark:bg-zinc-900">
                <div className="font-mono text-sm font-semibold">{selTable.name} <span className="font-sans text-xs font-normal text-zinc-500">· {selTable.restaurant_room} · {selTable.branch}</span></div>
                <div className="mt-1 flex flex-wrap gap-1.5 text-xs">
                  <span className="rounded-full bg-zinc-100 px-2 py-0.5 dark:bg-zinc-800">{selTable.no_of_seats} seats (min {selTable.minimum_seating})</span>
                  <span className={`rounded-full px-2 py-0.5 ${selTable.occupied ? "bg-amber-100 text-amber-800 dark:bg-amber-900/30 dark:text-amber-200" : "bg-emerald-100 text-emerald-800 dark:bg-emerald-900/30 dark:text-emerald-200"}`}>{selTable.occupied ? "Occupied" : "Free"}</span>
                  {selTable.merged_with.length > 0 && <span className="rounded-full bg-violet-100 px-2 py-0.5 text-violet-700 dark:bg-violet-900/30 dark:text-violet-200">Cluster: {[selTable.name, ...selTable.merged_with].join(", ")}</span>}
                </div>
                <button onClick={() => setSelTable(null)} className="mt-2 w-full rounded-full border border-zinc-200 bg-white py-1.5 text-xs font-medium hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800">Clear selection</button>
              </div>
            </div>
          )}
        </section>

        {/* menu */}
        <section className="flex flex-col rounded-2xl border border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-900">
          <div className="border-b border-zinc-100 p-4 dark:border-zinc-800">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h2 className="text-sm font-semibold">Menu {menuMeta ? <span className="font-normal text-zinc-500">· {menuMeta.menu} · {menuMeta.strategy}{menuMeta.fellBack ? " (fallback)" : ""}</span> : null}</h2>
              <span className="rounded-full bg-zinc-100 px-2.5 py-1 font-mono text-xs text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300">{filteredMenu.length}/{menuItems.length}</span>
            </div>
            <div className="mt-3 flex gap-2">
              <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search dish or code…" className="flex-1 rounded-full border border-zinc-200 bg-zinc-50 px-4 py-2 text-sm outline-none placeholder:text-zinc-400 focus:border-zinc-900 focus:bg-white dark:border-zinc-700 dark:bg-zinc-800" />
              <select value={course} onChange={(e) => setCourse(e.target.value)} className="rounded-full border border-zinc-200 bg-white px-3 py-2 text-sm dark:border-zinc-700 dark:bg-zinc-800">{courses.map((c) => <option key={c} value={c}>{c}</option>)}</select>
            </div>
            {menuMeta?.fellBack && <div className="mt-2 rounded-xl bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:bg-amber-950/30 dark:text-amber-200">Fallback menu — no exact match for room <span className="font-mono">{selTable?.restaurant_room ?? "—"}</span></div>}
          </div>
          <div className="flex-1 overflow-auto p-3">
            {mLoading ? <div className="rounded-xl border border-dashed border-zinc-300 p-8 text-center text-sm text-zinc-500">Loading menu…</div>
              : mErr ? <div className="rounded-xl border border-red-200 bg-red-50 p-3 text-sm text-red-700 dark:bg-red-950/30">{mErr} <button onClick={fetchMenu} className="ml-2 underline">retry</button></div>
                : filteredMenu.length === 0 ? <div className="rounded-xl border border-dashed border-zinc-300 bg-zinc-50 p-8 text-center text-sm text-zinc-500 dark:border-zinc-700 dark:bg-zinc-800/50">No dishes{q ? ` for “${q}”` : ""}{course !== "All" ? ` in ${course}` : ""}. Set X-Restaurant and pick a table room.</div>
                  : (
                    <div className="grid gap-2 sm:grid-cols-2">
                      {filteredMenu.map((it) => (
                        <div key={it.item_code} className="group flex flex-col justify-between rounded-xl border border-zinc-200 bg-zinc-50 p-3 transition hover:bg-white hover:shadow-sm dark:border-zinc-700 dark:bg-zinc-800/60 dark:hover:bg-zinc-800">
                          <div>
                            <div className="flex items-start justify-between gap-2">
                              <span className="line-clamp-2 text-sm font-medium leading-tight text-zinc-900 dark:text-zinc-100">{it.item_name}</span>
                              {it.special_dish && <span className="shrink-0 rounded-full bg-amber-100 px-1.5 py-0.5 text-[10px] font-bold uppercase tracking-widest text-amber-800 dark:bg-amber-900/30 dark:text-amber-200">★ Special</span>}
                            </div>
                            <div className="mt-1 flex flex-wrap items-center gap-1.5 text-xs text-zinc-500">
                              <span className="font-mono text-zinc-600 dark:text-zinc-300">{it.item_code}</span>
                              {it.course && <span className="rounded-full bg-white px-2 py-0.5 text-[11px] dark:bg-zinc-900">{it.course}{it.course_sequence ? ` #${it.course_sequence}` : ""}</span>}
                            </div>
                            <div className="mt-2 font-mono text-sm font-semibold text-zinc-900 dark:text-zinc-100">{formatMoney(it.rate)}</div>
                            {priceInfo[it.item_code] && <div className="mt-1 rounded-lg bg-sky-50 px-2 py-1 font-mono text-xs text-sky-700 dark:bg-sky-950/30 dark:text-sky-200">Pricelist: {priceInfo[it.item_code].startsWith("₹") || priceInfo[it.item_code].includes("HTTP") || priceInfo[it.item_code].length > 20 ? priceInfo[it.item_code] : formatMoney(priceInfo[it.item_code])}</div>}
                          </div>
                          <div className="mt-3 flex gap-1.5">
                            <button onClick={() => addToCart(it)} className="flex-1 rounded-full bg-zinc-900 px-3 py-1.5 text-xs font-semibold text-white hover:bg-zinc-800 dark:bg-white dark:text-zinc-900">Add +</button>
                            <button onClick={() => checkPrice(it.item_code)} disabled={priceBusy === it.item_code} className="rounded-full border border-zinc-300 bg-white px-3 py-1.5 text-xs font-medium hover:bg-zinc-50 disabled:opacity-50 dark:border-zinc-600 dark:bg-zinc-900">{priceBusy === it.item_code ? "…" : "Price"}</button>
                          </div>
                        </div>
                      ))}
                    </div>
                  )}
          </div>
          <div className="border-t border-zinc-100 bg-zinc-50 px-4 py-2 text-center text-[11px] text-zinc-500 dark:border-zinc-800 dark:bg-zinc-800/30">GET /api/menu + GET /api/menu/:id/items + GET /api/items/:code/price · money via formatMoney</div>
        </section>

        {/* cart */}
        <section className="flex flex-col rounded-2xl border border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-900">
          <div className="border-b border-zinc-100 px-4 py-3 dark:border-zinc-800">
            <div className="flex items-center justify-between"><h2 className="text-sm font-semibold">Cart</h2><span className="rounded-full bg-zinc-900 px-2.5 py-1 font-mono text-xs font-semibold text-white dark:bg-white dark:text-zinc-900">{cart.length} items</span></div>
            <div className="mt-1 text-xs text-zinc-500">{selTable ? <>Table <span className="font-mono font-semibold">{selTable.name}</span> · {selTable.restaurant_room}</> : "No table — takeaway"} · <span className="font-mono">{formatMoney(cartTotal)}</span></div>
          </div>
          <div className="flex-1 overflow-auto">
            {cart.length === 0 ? <div className="m-3 rounded-xl border border-dashed border-zinc-300 bg-zinc-50 p-8 text-center text-sm text-zinc-500 dark:border-zinc-700 dark:bg-zinc-800/50">Cart empty — add dishes from menu. Prices are strings via <span className="font-mono">formatMoney</span>.</div>
              : (
                <div className="divide-y divide-zinc-100 dark:divide-zinc-800">
                  {cart.map((l) => (
                    <div key={l.code} className="flex gap-3 px-4 py-3">
                      <div className="min-w-0 flex-1">
                        <div className="truncate text-sm font-medium text-zinc-900 dark:text-zinc-100">{l.name}</div>
                        <div className="flex flex-wrap items-center gap-1.5 text-xs text-zinc-500"><span className="font-mono">{l.code}</span>{l.course && <span className="rounded bg-zinc-100 px-1 py-0.5 dark:bg-zinc-800">{l.course}</span>}<span className="font-mono font-medium text-zinc-700 dark:text-zinc-300">{formatMoney(l.rate)} × {l.qty} = {formatMoney(mulMoney(l.rate, String(l.qty)))}</span></div>
                      </div>
                      <div className="flex shrink-0 items-center gap-1">
                        <button onClick={() => changeQty(l.code, -1)} className="h-7 w-7 rounded-full border border-zinc-200 bg-white text-sm hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800">−</button>
                        <span className="w-6 text-center font-mono text-sm font-semibold">{l.qty}</span>
                        <button onClick={() => changeQty(l.code, 1)} className="h-7 w-7 rounded-full border border-zinc-200 bg-white text-sm hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800">+</button>
                        <button onClick={() => removeLine(l.code)} className="ml-1 rounded-full bg-red-50 px-2 py-1 text-xs font-medium text-red-700 hover:bg-red-100 dark:bg-red-950/30 dark:text-red-300">✕</button>
                      </div>
                    </div>
                  ))}
                </div>
              )}
          </div>
          <div className="border-t border-zinc-100 bg-zinc-50 p-4 dark:border-zinc-800 dark:bg-zinc-800/30">
            <div className="space-y-1.5 text-sm">
              <div className="flex justify-between text-zinc-500"><span>Subtotal ({cart.reduce((a, b) => a + b.qty, 0)} pcs)</span><span className="font-mono font-semibold text-zinc-900 dark:text-zinc-100">{formatMoney(cartTotal)}</span></div>
              {order && <div className="flex justify-between text-xs text-zinc-500"><span>Order {order.id.slice(0, 8)} · v{order.version} · {order.status}</span><span className="font-mono">{formatMoney(order.grand_total)}</span></div>}
              {invoiceId && <div className="flex justify-between text-xs text-violet-700 dark:text-violet-300"><span>Invoice</span><span className="font-mono">{invoiceId}</span></div>}
            </div>
            <div className="mt-3 grid grid-cols-2 gap-2">
              <button onClick={handleSaveOrder} disabled={!!busy || !cart.length} className="rounded-full bg-zinc-900 px-4 py-2.5 text-sm font-semibold text-white hover:bg-zinc-800 disabled:opacity-50 dark:bg-white dark:text-zinc-900">{busy === "order" ? "Saving…" : order ? "Add to order" : "Create order"}</button>
              <button onClick={handleKot} disabled={!!busy || !cart.length} className="rounded-full bg-amber-600 px-4 py-2.5 text-sm font-semibold text-white hover:bg-amber-700 disabled:opacity-50 dark:bg-amber-500">{busy === "kot" ? "Sending…" : "Send to kitchen"}</button>
              <button onClick={handleInvoice} disabled={!!busy || !order} className="rounded-full border border-zinc-300 bg-white px-4 py-2.5 text-sm font-semibold hover:bg-zinc-50 disabled:opacity-50 dark:border-zinc-600 dark:bg-zinc-800 dark:text-zinc-100">{busy === "invoice" ? "…" : "Create invoice"}</button>
              <button onClick={handlePay} disabled={!!busy || !invoiceId} className="rounded-full bg-emerald-600 px-4 py-2.5 text-sm font-semibold text-white hover:bg-emerald-700 disabled:opacity-50 dark:bg-emerald-500">{busy === "pay" ? "Paying…" : `Pay ${formatMoney(cartTotal)}`}</button>
            </div>
            <button onClick={() => { setCart([]); setOrder(null); setInvoiceId(null); setKotRes(null); setOk(null); setErr(null); }} className="mt-2 w-full rounded-full border border-zinc-200 bg-white py-2 text-xs font-medium hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-900">Clear cart</button>
            <p className="mt-2 text-center text-[11px] leading-4 text-zinc-500">POST /api/orders · PATCH /api/orders/:id · POST /api/kot/generate · POST /api/invoices · POST /api/invoices/:id/pay</p>
          </div>
        </section>
      </div>
    </div>
  );
}
