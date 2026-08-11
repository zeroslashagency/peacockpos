# Peacock POS — Redesign Plan (design-taste-frontend-v1)

> **Trigger:** screenshot `/pos` on dark `bg-zinc-950` — 3 equal cards, `0/0` everywhere, red error as hero, `W3-A FOUNDATION` chip, pure black, no hierarchy. **Goal:** premium, clean, light, bento, asymmetric, money-first.

## 1. Audit — why UI looks not ok

| Area | Current | Tell (forbidden) | Fix (taste-v1) |
|---|---|---|---|
| **Base** | `bg-zinc-950` `#000` pure black, dark on dark cards, `border-zinc-800` low contrast | `NO Pure Black`, `NO Neon`, cockpit density | `bg-[#f9fafb]` `border-slate-200/50` `rounded-[2.5rem]` diffusion shadow `shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]`, density 4 Daily App |
| **Typography** | `text-xs text-zinc-500` everywhere, no display, default sans, labels inside/below mixed | `NO Inter`, `NO Oversized H1`, `Serif BANNED` for dashboard | `Geist` + `Geist Mono` (already) `tracking-tighter leading-none` `text-4xl md:text-6xl` for KPI, `font-mono` for money only |
| **Color** | Zinc + red error + amber + violet + sky + emerald — 5 accents, saturated | `Max 1 Accent`, `LILA BAN`, `Saturation <80` | **Zinc/Slate neutral + single Emerald** `#059669` (cash/paid), desaturated, `*` not allowed |
| **Layout** | `xl:grid-cols-[300px_1fr_380px]` **3 equal cards** horizontally, centered header, `h-screen` risk | `NO 3-Column Card Layouts BANNED`, `ANTI-CENTER BIAS` `DESIGN_VARIANCE 8` | **Asymmetric bento:** `grid-cols-12` `2fr 1fr 1fr` masonry, left 8 cols = Menu hero, right 4 cols = stacked Floor+Cart, `max-w-[1400px] mx-auto` `min-h-[100dvh]` |
| **Cards** | Every panel `rounded-2xl border shadow-sm` boxed, even empty states | `Anti-Card Overuse` `VISUAL_DENSITY 4` | Cards only for elevation hierarchy — else `border-t`/`divide-y` negative space, labels **outside/below** cards |
| **States** | `Loading…` text, `No tables — try with ?room` dashed, red error block, no skeleton, no stagger | `Mandatory Loading/Empty/Error` | Skeleton loaders matching layout, beautiful empty (illustration + CTA), inline error below input, tactile `-translate-y-[1px]` |
| **Motion** | Static, no hover, no stagger, no spring | `MOTION 6` Fluid CSS `staggerChildren` | `transition-all 0.3s cubic-bezier(0.16,1,0.3,1)` `stiffness 100 damping 20`, `layout` `layoutId`, staggered waterfall `delay calc(var(--index)*100ms)` |
| **Icons** | `⧉` unicode, `✕` text, no stroke control | `Icons MUST phosphor/radix stroke 1.5` | `phosphor-icons` `House`, `CookingPot`, `Receipt` `stroke 1.5` |
| **Content** | `Walk-in` generic, `Peacock Restaurant` hard-coded, `0/0` fake numbers, `W3-A FOUNDATION` slop | `NO Jane Doe`, `NO Fake Numbers`, `NO Startup Slop` | Realistic `Arjun Patel · 4 pax · Table T7 Hall`, `₹1,240.00` organic, no chip, no `W3-A` |

## 2. Baseline (taste-v1)

* `DESIGN_VARIANCE: 8` Asymmetric
* `MOTION_INTENSITY: 6` Fluid CSS + spring, no GSAP/ThreeJS
* `VISUAL_DENSITY: 4` Airy, `p-8` inside cards, huge gaps `gap-6` `py-10`
* Stack: Next.js 15.4.6 RSC + Tailwind v4 + `phosphor-icons` + `framer-motion` (check `package.json`, install if missing) + `decimal.js` for money

## 3. Design System

**Palette:**
* Bg `bg-[#f9fafb]` `text-zinc-900`
* Card `bg-white border-slate-200/50 rounded-[2.5rem] shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]`
* Accent Emerald `emerald-600` only — for `Paid`, `Free` dot, `Create invoice` primary
* Neutrals: `zinc-900` headings `zinc-600` body `zinc-400` muted `slate-200` borders
* No purple glows, no gradient text

**Typography:**
* Display: `Geist` `text-3xl tracking-tighter leading-none font-semibold`
* Mono: `Geist Mono` `font-mono` for `₹1,240.00` `T-07` `INV-0041`
* Body: `text-sm leading-6 text-zinc-600 max-w-[65ch]`

**Motion:**
* Spring `type: spring stiffness:100 damping:20`
* Stagger: parent `variants staggerChildren 0.08` children `initial opacity 0 y 8 → animate`
* Hover: `hover:-translate-y-[1px] hover:shadow-md active:scale-[0.98]`

## 4. Information Architecture

**Current:** `Header(logo+POS/KDS/Shifts+API Health) → Filters(Restaurant/Customer/Pax+Retry) → 3 cols Floor/Menu/Cart → Footer`

**New:**
* **Shell:** `Sticky header` `h-14` `max-w-[1400px]` left `P Peacock` (no chip) `· Branch` right `POS KDS Shifts` pill nav `bg-zinc-900 text-white` active, plus `API ● Live` dot breathing
* **POS Hero:** Split — left `60%` title `Service` `Floor → Menu → Fire` + KPI `₹0.00 · 0 items · No table` with `font-mono`; right `40%` contextual `Order` summary (if any) or `Empty` illustration
* **Bento:** `grid grid-cols-12 gap-6` `lg:grid-cols-[1.6fr_1fr]` or `12` with `col-span-7 Menu` `col-span-5 stack Floor(1fr) Cart(1.2fr)` — **not** 3 equal
* **Menu:** Bento cards with `p-8`, image placeholder `picsum.photos/seed/{code}/400/300` 4:3 vs 16:9 mix, course filter as pill `All · Starter · Main` horizontal scroll, search `rounded-full` with `magnifying-glass` icon
* **Floor:** Not boxed — `border-t` list or `grid grid-cols-2` with `divide-y` on mobile, `emerald dot` free / `amber dot` occupied, merged `⧉ n` as subtle badge, selection via `ring-2 ring-zinc-900`
* **Cart:** `sticky top-20` `rounded-[2.5rem]` white card with `divide-y`, qty stepper `− 2 +` tactile, totals outside card below, primary `Create order` `bg-zinc-900` + secondary `Send to kitchen` `bg-emerald-600` (single accent)

## 5. POS — screen specifics

**Header filters:** Labels **above** input (`gap-2`), helper `X-Restaurant → Peacock Restaurant (auto from first table)` inline error below, not red block. `Pax` stepper not number input. `Refresh` ghost, `Reload menu` not white pill → `bg-zinc-900`.

**Floor plan:**
* Empty: illustration (empty tables line art) + `No tables yet — sync from ERP or set room` + `Sync floor` CTA, not dashed
* Loading: skeleton `grid-cols-2` with `animate-pulse` matching card size
* Error: inline `Couldn’t load floor — {detail}` + `Retry` text button, not red card
* Cards: `rounded-2xl` `p-4` `border-slate-200/50` hover, `layoutId` for selection, breathing dot `animate-pulse` for occupied

**Menu:**
* Empty: `Set restaurant to load menu` with `Select restaurant` CTA + `Search` disabled state
* Loading: skeleton Bento `grid-cols-2` shimmer
* Error: `Restaurant Peacock Restaurant not found — did you seed?` with `Seed demo` link, not red slab
* Item card: Image `400x300` `rounded-2xl` `object-cover`, `★ Special` `emerald` pill top-right, `code` `course` `rate font-mono text-lg` outside below image, `Add +` `rounded-full` `hover:shadow` `active:translate-y`

**Cart:**
* Empty: `Your cart is empty — tap a dish to fire` with arrow, not `Prices are strings via formatMoney`
* List: `divide-y divide-zinc-100` `p-4` `gap-3`, `−/+/✕` with `stroke 1.5` phosphor, amount `font-mono`
* Actions: `Subtotal 4 pcs ₹1,240.00` `tracking-tight` outside card, buttons `grid grid-cols-2` with spring, `Clear cart` text link not pill, footnotes `POST …` removed from UI (move to `docs/API.md` link)

## 6. KDS + Shifts (lanes)

**KDS:** `Pending/Preparing/Prepared` 3-col → **Asymmetric 1 + 2**: left `Live Status` breathing `12 pending` + `pop notification` spring, right `Wide Data Stream` infinite carousel `x 0% → -100%` for `KOT` cards, each `KOT` with `layout` and `stagger`, `Mark prepared` tactile

**Shifts:** Not boxed — `border-t` timeline, `ZReport` with `diffusion shadow`, `cash_threshold_warning` `emerald` inline, not card soup

## 7. Motion & Bento archetypes

* **Intelligent List:** Menu `course` filter reorders with `layoutId`
* **Command Input:** Search typewriter cycling `Search biryani…` → `paneer tikka`
* **Live Status:** Floor `occupied` breathing dots + KDS `12` badge pop
* **Wide Data Stream:** KDS horizontal carousel
* **Contextual UI:** Cart `sticky` float toolbar on hover

## 8. Pre-flight (taste-v1 §10)

- [ ] `Geist` not Inter, `tracking-tighter`, `font-mono` numbers
- [ ] `max-w-[1400px] mx-auto` `min-h-[100dvh]` not `h-screen`
- [ ] `grid` not flex-math, `max-w-7xl` mobile `w-full px-4`
- [ ] Empty/loading/error for Floor/Menu/Cart
- [ ] Cards only for elevation, else `divide-y`/`border-t`
- [ ] `phosphor-icons` `stroke 1.5` not emoji/unicode
- [ ] Spring `stiffness 100 damping 20` `layout` `staggerChildren`
- [ ] Isolated Client Components for motion, `AnimatePresence`
- [ ] No pure black, no purple glow, no 3 equal cards, no centered hero

## 9. Execution — /agent-team-orchestration

| Lane | Scope | Files | Model | Depends |
|---|---|---|---|---|
| **Shell** | `layout.tsx` header/nav, `globals.css` tokens, `max-w-[1400px]` light `bg-[#f9fafb]` | `src/app/layout.tsx` `src/app/globals.css` `src/app/page.tsx` | spark | — |
| **POS** | `src/app/pos/page.tsx` bento asymmetric, floor/menu/cart refinement, skeletons, empty, error, motion | `src/app/pos/page.tsx` | spark | Shell tokens |
| **KDS** | `src/app/kds/page.tsx` live status + carousel, shifts timeline | `src/app/kds/page.tsx` `src/app/shifts/page.tsx` `src/components/ShiftPanel.tsx` | spark | Shell tokens |

**Verify:** `npm run build` 0, `npx tsc --noEmit` 0, `grep -r "W3-A" src` 0, `grep -r "#000000" src` 0, `grep -r "Inter" src` 0, responsive `375,768,1400` no horiz scroll, no `h-screen`.

## 10. Clean

Remove `W3-A FOUNDATION` chip, `money as string · ₹0.00` header, `POST /api/...` footnotes, `No table — takeaway · ₹0.00` clutter, `Pure black` `dark:` overrides (keep light only or subtle dark), `0/0` counters replaced with `—` or hidden until data.
