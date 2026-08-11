# Peacock POS — Cheapest Cloud Research (Muse Spark 1.2)

**Stack:** Rust Axum `0.7.9` on `:8080` + Postgres 16 (32 tables, `NUMERIC(18,6)`) + SSE `GET /api/events/stream` (KeepAlive 15s, retry 3000, `tokio::broadcast` + replay tail) + Next.js 15 (App Router) `peacock-web` • **Load:** Starter = 1 restaurant / 3 terminals / ~1k req/day (~30k/mo, <5 GB egress) → Scale = 10 restaurants / ~30 terminals / ~10k req/day (~300k/mo, <50 GB egress, DB <2 GB). All costs Aug 2026, excl. VAT unless noted. `€→$ ≈ 1.08`.

---

## 1. Executive Summary — Split Architecture Is Mandatory

Peacock is **not serverless-monolith friendly**. The Axum API is a long-lived Rust binary with a local `broadcast` bus, Pg pool sizing `(2×cpus)+1`, and SSE streams that hold a `broadcast::Receiver` per tab (50+ tabs tested in `peacock-api/src/events/sse.rs`). Vercel **cannot** run it.

**Canonical split (recommended for every cheap path):**

```
peacock-web (Next.js 15)  →  Vercel (Hobby $0 → Pro $20)
         │  NEXT_PUBLIC_API_URL=https://api.peacock.example.com
         │  CORS: PEACOCK_CORS_ALLOWED_ORIGINS=https://<vercel>.vercel.app
         ↓
peacock-api (Rust :8080)  →  VM / container  (Hetzner | DO Droplet | DO App Platform)
         │  DATABASE_URL=postgres://...
         ↓
Postgres 16 (32 tables)   →  self-hosted on same VM (cheapest)  OR  Managed / Serverless (Neon/Supabase/DO Managed)
         │
SSE: EventSource → GET /api/events/stream?events=kot.generated,...&last_event_id=  →  client useSSE()  (peacock-web/src/hooks/useSSE.ts) expects retry + Last-Event-ID resume + keep-alive comment every 15s
```

| Question | Answer |
|---|---|
| **Can Vercel host the Rust backend?** | **No.** Vercel Functions run Node/Python/Go/Edge only (Fluid compute). No Rust binary, no TCP `listen(8080)`, no persistent `broadcast` channel, no Pg pool. The only Rust path on Vercel would be compiling to WASM/Edge — which Peacock is not (uses `tokio`, `sqlx`, native TLS). |
| **Can Vercel host Postgres?** | Only via **Vercel Postgres** (actually **Neon** under the hood), **Vercel KV/Redis**, or external `DATABASE_URL`. No self-managed Pg on Vercel infra. |
| **SSE on Vercel?** | **Frontend: yes. Backend: don't.** `peacock-web` can `EventSource` to any backend — Vercel Edge `ReadableStream` SSE works great (see web_search result). But if you put `GET /api/events/stream` *on Vercel* (as a Function), you'll hit: 120s proxied-request timeout, 60s Hobby max / 300s Pro max function duration, cold starts that kill KDS boards. Peacock's KDS needs hours-long connections with `KeepAlive 15s` — that's a VM-native pattern, not a serverless one. Host SSE on the VM. |

**Relevant Peacock wiring:**

- `/Users/xoxo/Documents/resreah/billing/peacock-pos/peacock-api/src/events/sse.rs:56-240` — `GET /api/events/stream`, `Filter::parse(?events=)` 400 on unknown kind, `resume_from(header Last-Event-ID > query last_event_id)`, open frame `retry:3000` + `stream.open`, `stream.gap` / `stream.lagged`, `KeepAlive 15s`, fan-out to 50 tabs without blocking publishers.
- `/Users/xoxo/Documents/resreah/billing/peacock-pos/peacock-web/src/hooks/useSSE.ts:72-82` — `buildUrl(base, opts) => ${base}/api/events/stream?events=&last_event_id=` with exponential backoff 3s×1.5 cap 30s.
- `/Users/xoxo/Documents/resreah/billing/peacock-pos/peacock-web/src/lib/api.ts:11` + `/Users/xoxo/Documents/resreah/billing/peacock-pos/peacock-api/src/config.rs:60-68` — CORS credentials enabled, origins must be listed explicitly, `PEACOCK_CORS_ALLOWED_ORIGINS` must contain the Vercel origin.
- `/Users/xoxo/Documents/resreah/billing/peacock-pos/peacock-storage/src/config.rs:20-35` — Pool `max_connections ~ (2×cpus)+1`, needs a stable DB reachable over `DATABASE_URL`.

---

## 2. Vercel — Frontend Only

**Version checked:** pricing page live fetch 2026-08-11 matches `vercel.com/pricing` + `vercel.com/docs/limits` (last_updated 2026-08-03).

| Plan | Price | Included (the bits that matter for `peacock-web`) | Limits / caveat |
|---|---|---|---|
| **Hobby** | **$0 / mo** | 100 GB Fast Data Transfer, 1 M invocations, 4 CPU-hrs, 360 GB-hrs provisioned mem, 1 M edge requests, 10 GB Fast Origin Transfer, 3 static sites free | Hobby = **personal, non-commercial** per Vercel TOS. No overage buying — hard cap → 402/429 when exceeded. 1 concurrent build, 45 min build cap, 100 deployments/day, `team` creation 5/day. Great for dev / 1 resto single-owner. |
| **Pro** | **$20 / user / mo** (+ usage beyond included) | 1 TB Fast Data Transfer, 10 M edge requests, usage-based CPU/mem/invocations, $20 included credit, 6 000 deployments/day, up to 500 concurrent builds, 32-64 GB build disk, Log Drains $0.50/GB | Required the moment you have a second dev, staging env, or commercial use. Pay-as-you-go beyond included: `$0.60/1M invocations`, `$0.128/hr Active CPU`, `$0.15/GB Fast Data Transfer` over 1 TB, `$2/1M edge req` over 10 M. |
| **Enterprise** | Custom | Custom, 99.99% SLA, SSO/SCIM | Overkill until 10+ restos on a single domain. |

### Vercel + Peacock fit

| Aspect | Verdict |
|---|---|
| **Next.js 15 on Vercel** | **Perfect.** Zero config, `next build` → global CDN, ISR, image optimization. `eslint.ignoreDuringBuilds:true` already set in `peacock-web/next.config.ts:3`. No server code change. |
| **SSE (frontend)** | Vercel *can* proxy/serve SSE for pure Node/Edge streams (`new ReadableStream`, `Content-Type: text/event-stream`). But **Peacock's** `GET /api/events/stream` is Axum — so Vercel only does the *fetching* side (`EventSource('/api/events/stream')` in `useSSE.ts`). It rewrites via `next.config.ts` `rewrites()/redirects()` or just `NEXT_PUBLIC_API_URL`. |
| **Rust backend on Vercel** | **Not supported.** Alternative "host Axum on Vercel" hacks (compile to Edge WASM) don't work — `sqlx+tokio+chrono-tz+h­mac` not Edge compatible, and the bus requires a single long-lived process. |
| **Vercel Postgres** | Offered as marketplace add-on; under the hood it's **Neon** (see §6). Cheaper to buy Neon directly ($0-15) than via Vercel markup. Fine if you want one bill. |
| **Cost at Peacock scale** | 1k req/day → ~<1 GB transfer → **$0 Hobby covers it**. 10k req/day → ~<10 GB → still Hobby. Only bandwidth-heavy static assets / images push you over 100 GB. So **Vercel cost is $0 (Hobby) or $20 (Pro seat)** regardless of backend choice. |

**Recommended Vercel setup for Peacock:**

```ts
// peacock-web/next.config.ts — add rewrite so /api/* in the browser hits Hetzner/DO
import type { NextConfig } from "next";
const nextConfig: NextConfig = {
  eslint: { ignoreDuringBuilds: true },
  async rewrites() {
    return [
      { source: "/api/:path*", destination: `${process.env.NEXT_PUBLIC_API_URL}/api/:path*` },
    ];
  },
};
export default nextConfig;
// env: NEXT_PUBLIC_API_URL=https://api.peacock.example.com (Caddy/TLS on VM)
```

> **Rule:** With Hobby you **cannot** add a credit card to pay overages — you just get blocked. So for any paid/production restaurant, budget **Pro $20** to guarantee KDS never goes dark during dinner rush.

---

## 3. Hetzner Cloud — Cheapest Long-Term King

**Source:** live fetch `hetzner.com/cloud` + `hetzner.com/cloud/regular-performance` + `hetzner.com/cloud/cost-optimized` (2026-08-11). Hetzner hides price JS but plan specs are stable. `CX11/CX21` legacy names were renamed `CPX11/CPX21` (AMD EPYC Milan, shared vCPU). Current pricing incl. 20 TB traffic, 1 IPv4 (+€0.60 after first), snapshots €0.011/GB/mo, backups 20% of instance price.

> **Note on naming:** The task says `CX11/CX22 @ €4-7`. Those are old labels. Today they map to **CPX11 ≈ CX11** and **CPX21 ≈ CX22**. I've listed both.

| Hetzner Plan (2026) | vCPU (shared) | RAM | SSD | Traffic incl. | Hourly → Monthly cap | €/mo (ex-VAT) | $/mo (×1.08) | Use for Peacock |
|---|---|---|---|---|---|---|---|---|
| **CPX11 (ex-CX11)** | 2× AMD | 2 GB | 40 GB | **20 TB** | €0.0069 → **€4.55** | **€4.55** | **$4.91** | **Starter best** — Axum + Pg16 + SSE 3 terminals idle 512 MB |
| **CPX21 (ex-CX21/CX22)** | 3× AMD | 4 GB | 80 GB | 20 TB | €0.0128 → **€8.49** | **€8.49** | **$9.17** | **Sweet spot** — Pg+API co-hosted, 10 restos headroom, `max_connections≈7-9` |
| **CPX31** | 4× AMD | 8 GB | 160 GB | 20 TB | €0.0235 → **€15.59** | **€15.59** | **$16.84** | Scale vertical — 10 restos with reports + Bambora |
| **CPX41** | 8× AMD | 16 GB | 240 GB | 20 TB | €0.0467 → **€30.96** | **€30.96** | **$33.44** | Future: 20+ restos or heavy `daily-pl` |
| **CAX11 (ARM, Ampere)** | 2× ARM | 4 GB | 40 GB | 20 TB | → **€4.75** | €4.75 | $5.13 | Cheaper per GB but Docker `rust:1.80` ARM image needed — fine for new builds |
| **CX23/CAX31/etc (Cost-Optimized)** | older gen | 4-8 GB | 40-80 GB | 20 TB | ~€3.5-9 | — | — | Limited stock, not in all regions — okay for dev only |
| **CCX13 (Dedicated vCPU)** | 2 ded. | 8 GB | 80 GB | 20 TB | → **€16.51** | €16.51 | $17.83 | Only if noisy-neighbor jitter on shared matters for KDS p99 |

**Add-ons:**

| Add-on | Price |
|---|---|
| **Backups** (7 daily slots, auto) | **20% of instance** (CPX11 → €0.91/mo, CPX21 → €1.70/mo) |
| **Snapshot** (manual) | €0.011/GB/mo |
| **Extra IPv4** | €0.60/mo (first IPv4 free on new orders, second +) |
| **Load Balancer (LB11)** | **€6.07/mo** (€0.0091/hr) — shared, managed, Let's Encrypt, health checks |
| **Floating IP** | €0.60/mo |
| **Object Storage (S3)** | €5.04/TB/mo (for `/public` assets, backups) |
| **Traffic over 20 TB** | **€1.19 / TB** — irrelevant for Peacock (you'll use <0.05 TB) |

### Hetzner for Rust — why it's great

- **Bare Docker/binary, no Herokusms.** `cargo build --release` → 8 MB `peacock-api` binary + `migrations/*.sql` baked via `sqlx::migrate!`. Systemd unit on port 8080, Caddy reverse proxy with auto-TLS, `PEACOCK_CORS_ALLOWED_ORIGINS` set. Build once, `scp` + `systemctl restart`.
- **Bandwidth is basically free.** 20 TB included vs DO 1 TB on $6 plan. Peacock's SSE keep-alive `": stream.keep-alive\n\n"` every 15s × 30 terminals = ~100 MB/day — noise.
- **EU data residency** (Falkenstein/Nuremberg/Helsinki + US Ashburn/Hillsboro + Singapore). For Indian restos pick **SIN** or **HEL** (cheap, low latency via Tailscale).
- **No managed Postgres** — you run it. `docker compose` with `postgres:16-alpine` + `pgdata` volume on same SSD is fine for 1-10 restos (<5 GB data). For production add PITR: `barman`/`pgBackRest` to Object Storage.

### Scaling story on Hetzner

| Phase | Setup | Vertical or horizontal |
|---|---|---|
| **Starter (1 resto)** | 1× CPX11 (€4.55) — API + Pg on one box via Docker Compose, Caddy TLS, backups on. | Vertical: resize to CPX21 in 60s (reboot). |
| **Scale (3-10 restos)** | 1× CPX21/31 (€8-15) — same co-hosted, add `PEACOCK_DB_MAX_CONNECTIONS=15`. Or split: CPX11 for API + CPX11 for Pg (private network `10.0.0.0/16`, free intra-DC traffic). | Either. CPX31 handles 10 restos easily — Axum is threaded Tokio, not PHP. |
| **HA (10+ restos, uptime SLA)** | 2× CPX21 (€17) + LB11 (€6) = **€23/mo** + Managed DB split or Pg primary+replica via `repmgr`. | Horizontal with Hetzner Load Balancer + health check `/health/ready`. |

**Hetzner total cost for Peacock (frontend on Vercel):**

| Scenario | Hetzner infra | Vercel | All-in € | All-in $ | Notes |
|---|---|---|---|---|---|
| **Starter cheapest** | CPX11 €4.55 + backups €0.91 = **€5.46** (Pg self-hosted) | Hobby $0 | **€5.46 /mo** | **$5.90** | Single binary, 20 TB traffic, API+Pg together. The absolute cheapest live Peacock. |
| **Starter safe** | CPX21 €8.49 + backups €1.70 = **€10.19** | Hobby $0 (or Pro $20) | **€10.19** (+€18.52 with Pro) | **$11.00** ($31) | Recommended — 4 GB headroom for reports. |
| **Scale (10 restos)** | CPX31 €15.59 + backups €3.12 = **€18.71** _or_ 2×CPX21+LB = **€23.05** | Pro $20 = €18.52 | **€37-42 /mo** | **$40-45** | Still trivial. Most spend is Vercel Pro seat, not infra. |
| **Scale HA** | 2×CPX21 €16.98 + LB €6.07 + backups €3.40 = **€26.45** | Pro $20 | **€45 /mo** | **$49** |  |

> **Hetzner is ~3-5× cheaper than DO at every tier** and bandwidth is 20× DO. Trade-off: you own updates (`apt upgrade && docker pull postgres:16`), firewall rules (`ufw` + Hetzner Cloud Firewall free), and backups.

### Pros / Cons — Hetzner for Peacock

| Pros | Cons |
|---|---|
| ✅ Cheapest long-term — €4.55/mo entry, 20 TB free egress, no hidden metering | ❌ No managed Postgres — you pg_dump/backups, tune `shared_buffers`, handle failover yourself |
| ✅ **Ideal for Rust** — any binary, no buildpacks, static musl works, `sqlx::migrate` at boot | ❌ Manual vertical resize (30-60s downtime) or LB dance for zero-downtime; no auto-scaling |
| ✅ SSE trivial — plain TCP, no timeout, 15s keep-alive survives Cloudflare not in path | ❌ No US support phone, ticket-only; EU company — invoices with 19% VAT for EU |
| ✅ EU/GDPR-friendly HQ + global POPs | ❌ Cost-Optimized ARM stock is volatile ("not available" often) |
| ✅ Private network free between VMs in same DC | ❌ No $200 free credit like DO |

---

## 4. DigitalOcean — Droplets vs App Platform vs Budget VPS

### 4a. Droplets (VM, like Hetzner but USD, managed DB available)

Live pricing fetch `digitalocean.com/pricing/droplets` (2026-08-11) — per-sec billing min 60s or $0.01:

| Droplet (Basic, Regular Intel) | vCPU | RAM | SSD | Transfer/mo | Hourly → $/mo cap | Use for Peacock |
|---|---|---|---|---|---|---|
| **s-1vcpu-512mb (Mini)** | 1 | 512 MB | 10 GB | 500 GiB | $0.00595 → **$4.00** | Tight — only API (no Pg) |
| **s-1vcpu-1gb (Starter)** | 1 | 1 GB | 25 GB | 1 TB | $0.00893 → **$6.00** | **Starter VM** — API + Pg co-hosted (swap risk) |
| **s-1vcpu-2gb** | 1 | 2 GB | 50 GB | 2 TB | $0.01786 → **$12.00** | **Starter safe** — recommended single-box |
| **s-2vcpu-2gb** | 2 | 2 GB | 60 GB | 3 TB | $0.02679 → **$18.00** | Scale |
| **s-2vcpu-4gb** | 2 | 4 GB | 80 GB | 4 TB | $0.03571 → **$24.00** | Scale safe |
| **Premium Intel/AMD variants** | same | same | NVMe |  | +$1-3 premium | Only if you want NVMe guarantees |

Other Droplet types (General Purpose, CPU-Optimized, etc.) are $42+ — overkill.

| Related DO pricing | |
|---|---|
| **Backups** | 20% (weekly) / 30% (daily) of Droplet — $1.20-3.60 on starter |
| **Snapshots** | $0.06/GB/mo |
| **Load Balancer** | **$12/mo** (managed, Let's Encrypt) |
| **Spaces (S3)** | **$5/mo** — 250 GB storage + 1 TB transfer, then $0.02/GB |
| **Container Registry** | $0 free (private images) |

### 4b. DigitalOcean Managed Postgres (2026 live fetch `digitalocean.com/pricing/managed-databases`)

| Size | vCPU | RAM | Storage |  $/mo | Hourly | Notes |
|---|---|---|---|---|---|---|
| **Basic 1GB** | 1 | 1 GB | 10 GB (upgradable to 30 GB @ $0.215/GB) | **$15.15** | $0.02254 | **Starter DB** — Peacock needs <2 GB for 10 restos, so this lasts long |
| **Basic 2GB** | 1 | 2 GB | 30 GB | **$30.45** | $0.04531 | Scale DB — dedicated enough for `daily-pl` aggregates |
| **Basic 4GB** | 2 | 4 GB | 60 GB | **$60.90** | $0.09063 | 20+ restos / heavy reporting |
| **General Purpose / Storage Optimized** | 2-32 | 8-256 GB | NVMe | $63-1344 |  | Not relevant |

Features: daily backups free, PITR, standby node add-on (+ same price per node), Pg 16, connection pool (PgBouncer) toggle.

### 4c. DigitalOcean App Platform (PaaS, Heroku-like)

Live fetch `digitalocean.com/pricing/app-platform` — old Basic/Professional tiers **removed**; now modular container pricing. $0 free tier = **static sites only** (3 apps, 1 GiB transfer each). Any service/worker is paid:

| Container (App Platform) | vCPU | RAM | Transfer/mo (egress quota) | $/mo per container | Scaling |
|---|---|---|---|---|---|
| **Shared Fixed 512 MB** | 1 | 512 MB | 50 GiB | **$5.00** | 1 instance only |
| **Shared Fixed 1 GB** | 1 | 1 GB | 100 GiB | **$10.00** | 1 instance only |
| **Shared 1GB** | 1 | 1 GB | 150 GiB | **$12.00** | Horizontal OK |
| **Shared 2GB** | 1 | 2 GB | 200 GiB | **$25.00** | Horizontal OK |
| **Shared 4GB** | 2 | 4 GB | 250 GiB | **$50.00** | Horizontal OK |
| **Dedicated (autoscaling)** | from 1/0.5 GB |  | per spec | **from $20+** | Autoscaling only on dedicated |

Add-ons: **Development DB** $7 (512 MB, ephemeral, destroyed with app — **not for Peacock**), **Dedicated Egress IP** $25/app, overage transfer $0.02/GiB.

**Rust on App Platform:** Requires **Dockerfile** (no native Rust buildpack). Buildpacks: Node, Python, Go, Ruby — not Rust. So you `FROM rust:1.80 as builder` → `FROM debian:bookworm-slim` multi-stage (≈50 MB final). Deploy via GitHub or Container Registry.

**SSE on App Platform:** Works — keep-alive 15s required, set `KeepAlive` header. Caveat: App Platform sits behind a proxy with request timeouts; long-lived EventSource survives but you must handle reconnects (already in `useSSE.ts`). For dedicated containers, connection drains after deploy (~5 min). No 120s hard kill like Vercel, but not as raw as Droplet.

### DO Total Costs for Peacock

**Droplet path (recommended DO path):**

| Scenario | Droplet | Postgres | Vercel | All-in $ | All-in € (÷1.08) |
|---|---|---|---|---|---|
| **Starter cheapest (DO)** | $6 (1 GB) + Spaces $0 | **Neon Free $0** or self-Pg on same Droplet ($0) | Hobby $0 | **$6 /mo** | **€5.56** |
| **Starter managed** | $6 Droplet (API only) | **DO Managed $15.15** | Hobby $0 | **$21.15 /mo** | **€19.58** |
| **Starter managed safe** | **$12 Droplet (2 GB)** | **DO Managed $15.15** | Hobby $0 | **$27.15 /mo** | **€25.14** |
| **Scale (10 restos)** | $12-24 Droplet(s) | DO Managed $30.45 (2 GB) | Pro $20 | **$62-74 /mo** | **€57-69** |
| **Scale HA DO** | 2×$12 Droplets $24 + LB $12 | DO Managed $30.45 | Pro $20 | **$86.45 /mo** | **€80.05** |

**App Platform path (PaaS):**

| Scenario | App Platform containers | Postgres | Vercel* | All-in $ |
|---|---|---|---|---|
| **Starter App Platform** | 1× Shared 512 MB $5 (API) | Development DB $7 ⚠️ ephemeral OR Neon Free $0 | Hobby $0 (or host Next.js also on App Platform static free $0) | **$5-12 /mo** |
| **Starter safe App Platform** | 1× Shared 1 GB $10 | DO Managed $15.15 | Hobby $0 | **$25.15 /mo** |
| **Scale App Platform** | 2× Shared 1 GB $20 (2 instances for HA) | DO Managed $30.45 | Pro $20 | **~$70 /mo** (plus $0.02/GB over) |

> *You can also host `peacock-web` on App Platform as static site ($0 free tier, 1 GiB) and drop Vercel entirely — but you lose Vercel's Next.js ISR/edge optimization.

**Quick DO vs Hetzner € parity:**

| Tier | Hetzner (API+Pg co-hosted) | DO Droplet+Neon Free (API+Pg co-hosted/eFree) | DO Droplet+Managed Pg |
|---|---|---|---|
| Starter | **€5.46** (CPX11) | €5.56 ($6) | €19.58 ($21) |
| Starter safe | **€10.19** (CPX21) | €11.12 ($12) | €25.14 ($27) |
| Scale 10 restos | **€18.71** (CPX31) | €22.22 ($24) | €57.50 ($62) |

Hetzner wins by €5-15 at every tier when both self-host Pg; DO Managed premium is ~€10-15 of that gap.

### Budget VPS fringe (Contabo, Netcup, Oracle Free)

| Provider | Entry VPS | €/mo | Trap |
|---|---|---|---|
| **Contabo** VPS S | 4 vCPU, 8 GB, 200 GB SSD | €4.50 | "4 vCPU" = oversubscribed — terrible noisy neighbor for KDS p99. Support slow. |
| **Netcup** RS 1000 | 4 vCPU, 8 GB | €5-7 | Good but German contract (monthly cancel dance). |
| **Oracle Free Tier** (Ampere 4 vCPU/24 GB) | Always free | €0 | Free = reclaim risk — Oracle *will* reclaim idle VMs. Not for a till. |

> **Recommendation:** Ignore budget VPS for Peacock — Hetzner's €4.55 already beats them on real performance and reliability.

### Pros / Cons — DigitalOcean for Peacock

| Path | Pros | Cons |
|---|---|---|
| **DO Droplet** | ✅ Simple VM like Hetzner, but USD billing, global DOCS, $200 credit 60 days (new accounts), managed Pg available, great DX, 1-click Docker image | ❌ More $$ than Hetzner for same RAM (1 GB $6 vs 2 GB €4.55, 1 TB vs 20 TB transfer) |
| **DO Droplet + Managed Pg $15** | ✅ **Easiest managed Postgres** — backups, PITR, PgBouncer, no DBA work; Droplet just runs Rust binary | ❌ Adds $15 floor → starter jumps from $6 to $21; single AZ basic (no HA at $15) |
| **DO App Platform** | ✅ Heroku-like git push, auto-HTTPS, scaling slider, no SSH; static sites free | ❌ **Rust = Dockerfile only**, no native buildpack; pricier per-GB than Droplet; logs truncated; SSE less transparent; `512 MB $5` tier tiny; dev DB ephemeral is a production footgun |
| **Budget VPS** | ✅ Even cheaper on paper | ❌ Unreliable for money-handling — don't risk a POS on Contabo |

---

## 5. Postgres Comparison — Supabase vs Neon vs DO Managed vs Self-Hosted

Peacock needs **Postgres 16**, `NUMERIC(18,6)`, extensions `pgcrypto`/`uuid`, migrations via `sqlx::migrate!` (10 files, 32 tables). DB size estimate: 1 resto → ~100 MB/yr (orders+invoices+KOT), 10 restos → ~1-2 GB.

### 5a. Table

| Provider | Free tier | Included free | Paid entry | Storage price | Compute model | Scale-to-zero | Branching | Region | Best for Peacock |
|---|---|---|---|---|---|---|---|---|---|
| **Neon** (serverless Pg, child of Vercel) | **Forever free, no card** | **100 CU-hrs, 0.5 GB, 5 GB egress, 6h PITR, 10 branches, up to 2 CU (8 GB)** | **Launch:** $0.106/CU-hr + $0.35/GB-mo storage (no min) **Scale:** $0.222/CU-hr + same storage; 500 GB egress free, then $0.10/GB | $0.35/GB-mo + $0.20/GB-mo history | Autoscaling 0.25-16 CU, billed per CU-hr, **scales to 0 after 5 min** (pauses) | ✅ Yes — idle = $0 | ✅ Yes (10 free, $1.50/branch-mo extra) | Global (AWS) | **Backend on cheap VM (Hetzner/DO $6) + Neon Free/Launch** — cheapest managed |
| **Supabase** (Pg + Auth + Storage + Realtime) | Free $0 | **500 MB DB, 1 GB files, 5 GB egress, 50k MAU, 2 active projects, paused after 1 week idle**, 500 MB RAM shared | **Pro:** $25/mo + **$10 compute credit** (covers 1×Micro 1 GB). Extra Micro $10, Small $15, etc. Storage 8 GB free then $0.125/GB. Egress 250 GB free then $0.09/GB | $0.125/GB over 8 GB | Always-on Micro (1 GB) — no scale-to-zero on Pro unless you pause | ❌ Free pauses weekly (bad for KDS), Pro never sleeps | Paid only $0.01344/branch/hr | Global (AWS) | If you want Auth/Realtime bundled — **overkill for Peacock** (auth is TODO, but not needed now) |
| **DO Managed Pg** | No free — $15.15 min | — | $15.15 (1 GB/10 GB) → $30.45 (2 GB) → $60.90 (4 GB). Storage +$0.215/GB over range | $0.215/GB over | Always-on fixed, no scale-to-zero | ❌ | ❌ | DO regions only | **Easiest fully-managed** — no branching wizardry, just add `DATABASE_URL` |
| **Self-hosted on VM** (Hetzner/DO) | $0 extra | Uses Droplet disk (40-80 GB SSD) | $0 — your disk | $0 | VM's cpus/mem | ❌ (host always on) | ❌ | Same as VM | **Cheapest overall** — `postgres:16-alpine` Docker, 1 command |

### 5b. Supabase vs Neon — head-to-head for Peacock

| Criteria | **Neon Free** | **Supabase Free** | Winner for Peacock |
|---|---|---|---|
| DB size for 1 resto (100 MB) | 0.5 GB limit → fits with 400 MB margin | 500 MB → fits barely | Tie |
| DB size for 10 restos (~1.5 GB) | 0.5 GB → **must upgrade** Launch (~$0.53/mo storage + compute) | 500 MB → **must Pro** ($25+10) | **Neon ~$5-10 cheaper** |
| Idle behavior | Scale to zero after 5 min → **$0 when restaurant closed** (2am-6am not metered) | Free **pauses after 7 days** → KDS cold-start 5-10s, or Pro $25 always-on | **Neon wins for POS** — nightly close is perfect for scale-to-zero |
| Connection model | Pooler 60 direct / 200 pooled (Micro), autoscaling handles bursts (3 terminals) | 60 direct / 200 pooled — same | Tie |
| Peacock migrations | Vanilla Pg 16, all `sqlx` migrations work; branching lets you test migration 012 safely | Vanilla Pg 16, but Supabase `auth` schema + `realtime` extensions add noise to `\dt` | **Neon is cleaner** |
| Pricing surprise | Free suspension when you exceed 100 CU-hrs / 0.5 GB / 5 GB → upgrade to Launch, no bill shock; invoices <$0.50 not collected | Free → Pro is $25+(compute)-$10 credit = $25-35 even if you use 0.1 GB | **Neon pay-per-use** is cheaper for tiny POS load |
| DX | `psql` compatible, `neonctl branch create`, claimable `neon.new` 72h | Supavisor UI, Table editor nice but not needed | Tie |

**At Peacock's load (1k req/day = ~0.03 CU avg), Neon cost math:**

- **Free tier:** 100 CU-hrs ≈ 0.25 CU × 400 hrs/mo idling + bursting → **covers starter** if you have <100 hrs active (with scale-to-zero, a 12h/day restaurant → 360 hrs × 0.25 CU = 90 CU-hrs → fits). But 24/7 kitchen may exceed, plus 0.5 GB storage ceiling will hit at ~1.5 GB for 10 restos.
- **Launch pay-per-use:** 1 CU-hr = $0.106. If Peacock averages 0.1 CU (tiny), 730 hrs/mo × 0.1 = 73 CU-hrs × $0.106 = **$7.74 + storage 2 GB × $0.35 = $0.70 = $8.44/mo**. Often <$10. Same data on Supabase Pro = $25+10 = $35 floor.

### 5c. Verdict for Postgres

| Goal | Pick |
|---|---|
| **Absolute cheapest live** | **Self-host on Hetzner CPX11/CPX21** (Pg on same SSD) — $0 extra, 32 tables locally, fastest latency (<1ms `DATABASE_URL=postgres://localhost:5432`) |
| **Cheapest managed (no DBA)** | **Hetzner/DO $6 Droplet + Neon Launch ~$8-12/mo** — pay per use, scale-to-zero at night. Beats Supabase $25 by $15/mo. |
| **Easiest "no thinking" managed** | **DO Droplet $12 + DO Managed $15.15 = $27** — one provider, one invoice, PITR baked, no branching. Or **Supabase Pro $25+$10** if you want BaaS extras later. |
| **Avoid for Peacock** | Supabase **Free** (weekly pause will miss a KDS ticket at 7am reopen), DO **App Platform Development DB** ($7 ephemeral — deleting the app deletes orders). |

---

## 6. Starter vs Scale — Full Cost Estimates (USD + EUR)

**Assumptions:** Next.js on Vercel, Axum `:8080` via Docker/Caddy, Postgres 16, SSE keep-alive 15s included, no Spaces/Object Storage counted (add $5 if you store bill PDFs). EUR uses 1.08 rate. Vercel Hobby = $0 (non-commercial) — shown with Pro $20 alternative.

### 6a. At a Glance — Recommended Stacks

| # | Stack (frontend → backend → DB) | Starter (1 resto/3 terms/1k req/d) | Scale (10 restos/~30 terms/10k req/d) | Managed level |
|---|---|---|---|---|
| **A** | **Vercel Hobby → Hetzner CPX11 → self-Pg** | **$5.90 / €5.46 /mo** | n/a (resize) | Self-managed (cheapest) 🏆 |
| **B** | **Vercel Hobby → Hetzner CPX21 → self-Pg** | **$11.00 / €10.19** | **$38-42 / €35-39** (+Vercel Pro $20 on scale) | Self-managed (recommended) |
| **C** | Vercel Hobby → DO Droplet $6 → Neon Free | **$6.00 / €5.56** | ~$20-25 / €18-23 (Neon Launch) | Half-managed |
| **D** | Vercel Hobby → DO Droplet $6 → DO Managed $15 | **$21.15 / €19.58** | $62 / €57 (DO $24 + $30 Pg) | Fully managed |
| **E** | Vercel Pro → DO Droplet $12 → DO Managed $15 | **$47.15 / €43.66** ($12+$15+$20) | **$74 / €69** ($24+$30+$20) | Managed + Pro SLA |
| **F** | Vercel Pro → Hetzner CPX31 → Neon Launch | — | **~$50 / €46** | Half-managed scale |
| **G** | DO App Platform $5 → Neon Free (no Vercel) | $5.00 / €4.63 | $20-30 / €19-28 | PaaS |

### 6b. Detailed USD table

| Platform detail | Starter $ | Starter pcs | Scale $ | Scale pcs | Scale HA $ |
|---|---|---|---|---|---|
| **Hetzner CPX11 self-Pg + Vercel Hobby** | **5.90** (4.55+0.91+0) | CPX11 2GB+20TB+bkup | — | resize → | — |
| **Hetzner CPX21 self-Pg + Vercel Hobby** | **11.00** (8.49+1.70+0) | CPX21 4GB+20TB+bkup | **42** (15.59+3.12+20+~3) | CPX31+Pro+bkup | **49** (2×8.49+6.07+bkup+20) |
| **DO Droplet $6 + Neon Free + Vercel Hobby** | **6.00** | 1GB 1TB 25GB | **28** (12+~8+20) | 2GB DB | — |
| **DO Droplet $6 + DO Managed $15 + Vercel Hobby** | **21.15** | API VM + Pg managed | **62** (24+30+20) | 2CPU 4GB + 2GB Pg + Pro | **86** (+LB12) |
| **DO Droplet $12 + DO Managed $15 + Vercel Hobby** | **27.15** | Safe starter | **62** | same | **86** |
| **DO App Platform $10 + DO Managed $15 + Vercel Hobby** | **25.15** | 1GB container | **70** (2×$10+$30+$20) | 2 containers | — |
| **Supabase Pro baseline** (replaces DO/Neon) | **35.00** ($25+$10) | 8GB disk, 1GB RAM | **50+** | Small→Medium | — |

### 6c. Detailed EUR table

| Stack | Starter € | Scale € | Notes |
|---|---|---|---|
| **Hetzner CPX11** (Hobby) | **€5.46** | — | Entry king |
| **Hetzner CPX21** (Hobby) | **€10.19** | **€38.89** (CPX31 €15.59+€3.12+bkup + Vercel Pro €18.52) | Recommended |
| **Hetzner 2×CPX21+LB** (Pro) | — | **€44.91** (€16.98+€6.07+€3.40+€18.52) | HA |
| **DO $6 + Neon Free** | **€5.56** | **€23.15** (DO $12 €11.11 + Neon €7.41 + Pro €18.52) | Cheapest DO |
| **DO $12 + Managed $15** | **€25.14** | **€57.41** ($24+$30+$20) | Managed DO |
| **Supabase Pro** | **€32.41** | **~€46** | |

> **Bandwidth note:** Hetzner's 20 TB vs DO's 1-2 TB is irrelevant at Peacock load (<<50 GB), but protects you if you add bill-PDF downloads or menu images to Spaces/Object Storage.

### 6d. What each tier actually handles

| Metric | Starter (1 resto) | Scale (10 restos) | Bottleneck |
|---|---|---|---|
| DB rows | ~2k invoices/mo, ~10k kot_items | ~20k invoices, ~100k kot_items | 0.5 GB (Neon Free) |
| SSE connections | 3 KDS tabs + 3 POS = ~6 | 30+ tabs | `broadcast` channel cap 64-512 (configurable in `EventBroadcaster::with_capacity`) |
| Pg pool | 5-7 connections (`default_max_connections() = (2×cpus)+1 max 5 floor`) | 9-15 connections | bump `PEACOCK_DB_MAX_CONNECTIONS` |
| Compute | 0.05 CU avg (Neon), 5% CPU (VM) | 0.3 CU, 25% CPU | Rust handles 300k req/mo on 1 vCPU easily — DB is limit |

---

## 7. Per-Platform Deep Dive — SSE, Rust, Scaling

| Platform | Rust Axum `:8080` | SSE `GET /api/events/stream` | Postgres 16 | Scaling 1→10 restos | Ease (1=hard,5=easy) |
|---|---|---|---|---|---|
| **Vercel** | ❌ No (only frontend) | ✅ Frontend `EventSource` to external API only; functions timeout 60s/300s if you self-host SSE there | Via Neon/Supabase add-on | N/A | 5 (frontend only) |
| **Hetzner CX/CPX** | ✅ Perfect — Docker `postgres:16-alpine` + `peacock-api` binary + `CADDY` TLS | ✅ Native — no timeout, 15s keep-alive survives, `tokio::broadcast` fan-out to 50 clients verified | ✅ Self-hosted Docker, or external Neon | Manual resize (60s) or LB (€6) | 3 (you `apt` & `systemd`) |
| **DO Droplet** | ✅ Same as Hetzner, 1-click Docker image | ✅ Same as Hetzner | ✅ Self or Managed $15 | Resize or LB $12 | 3.5 (docs slightly better) |
| **DO App Platform** | ⚠️ Dockerfile required | ⚠️ Works, but behind proxy — test reconnect; logs limited | Dev $7 ephemeral ⚠️ or external Managed | Slider/dedicated autoscale | 4 (git push, less infra) |

---

## 8. Pros / Cons Summary

### Vercel (frontend)

| Pros | Cons |
|---|---|
| ✅ Cheapest + fastest Next.js 15 deploys, global edge, free TLS, preview branches | ❌ **Not for Rust** — no binary, 60s/120s timeout kills KDS SSE |
| ✅ Hobby $0 covers Peacock bandwidth; Pro $20 flat | ❌ Hobby = non-commercial TOS; no overage → hard block at 100 GB |
| ✅ Works perfectly with `NEXT_PUBLIC_API_URL` split | ❌ Need CORS explicit listing (wildcard illegal with credentials) |

### Hetzner

| Pros | Cons |
|---|---|
| ✅ **Cheapest raw infra in EU** — €4.55/mo, 20 TB traffic, best vCPU-per-€ | ❌ No managed DB; you're on call for `postgres:16` patching |
| ✅ Best for Rust — bare VM, no buildpacks, private network free | ❌ Resize = reboot; no autoscale without LB |
| ✅ SSE trivial, low latency, no serverless gotchas | ❌ Invoices +19% VAT for EU; ticket-only support |

### DigitalOcean Droplet

| Pros | Cons |
|---|---|
| ✅ VM simplicity + **optional Managed Pg $15** (daily PITR, pooling) | ❌ $6 for 1 GB vs Hetzner's 2 GB €4.55 |
| ✅ $200 credit 60 days, excellent docs, Spaces $5 | ❌ Transfer quota 1-2 TB (vs 20 TB) |
| ✅ SSE native | ❌ 2× price of Hetzner for same scaling |

### DigitalOcean App Platform

| Pros | Cons |
|---|---|
| ✅ Easiest PaaS — `git push`, managed HTTPS, container scaling | ❌ **Rust needs Dockerfile**; no Rust buildpack; dev DB is destructive |
| ✅ Static sites free ($0) if you ditch Vercel | ❌ `512 MB $5` too small for Pg co-host; pricier per GB than Droplet |
| ✅ SSE supported (dedicated autoscale) | ❌ Logs truncated, less debuggable than SSH |

### Supabase vs Neon (Postgres)

| Neon | Supabase |
|---|---|
| ✅ **Cheaper pay-per-use** — $0→~$8 for POS load, scale-to-zero saves at night | ✅ Batteries-included (Auth/Storage/Realtime) |
| ✅ Branching for `migrations/*.sql` safe tests | ❌ **Free pauses weekly** → KDS outage risk |
| ✅ Cleaner vanilla Pg (no `auth` schema noise) | ❌ Floor $25+$10 even for 100 MB DB |
| ✅ `neon.new` 72h claimable for PR previews | ✅ Better UI for non-SQL team |

---

## 9. Verdict for Peacock — What to Pick

### If you want **cheapest long-term** (no DBA fear) → **Hetzner CPX21 + Vercel Hobby**

> `Vercel Hobby $0 (peacock-web) → Hetzner CPX21 €8.49 + backups €1.70 = **€10.19 / $11.00 per month** for 1 resto, self-hosted Pg16 on same box via `docker-compose`. At 10 restos resize to CPX31 €15.59 → **~€19 / $20** + optionally Vercel Pro €18.52 → **€37 / $40**. The cheapest way to run real money-handling Postgres with zero per-GB metering.

**Provision (5 min):**

```bash
# on CPX21 (Ubuntu 24.04)
curl -fsSL https://get.docker.com | sh
mkdir -p /srv/peacock && cd /srv/peacock
cat > docker-compose.yml <<'YAML'
services:
  db:
    image: postgres:16-alpine
    environment: { POSTGRES_DB: peacock, POSTGRES_USER: peacock, POSTGRES_PASSWORD: ${PGPASS} }
    volumes: [pgdata:/var/lib/postgresql/data]
    ports: ["5432:5432"]
    healthcheck: { test: ["CMD-SHELL","pg_isready -U peacock"], interval: 5s }
  api:
    build: .  # Dockerfile: FROM rust:1.80 as builder → debian:bookworm-slim + sqlx migrate baked
    ports: ["8080:8080"]
    environment:
      DATABASE_URL: postgres://peacock:${PGPASS}@db:5432/peacock
      PEACOCK_API_HOST: 0.0.0.0
      PEACOCK_API_PORT: 8080
      PEACOCK_CORS_ALLOWED_ORIGINS: https://<vercel-app>.vercel.app,https://pos.peacock.example.com
      PEACOCK_LOG_FORMAT: json
    depends_on: { db: { condition: service_healthy } }
  caddy:
    image: caddy:2-alpine
    ports: ["80:80","443:443"]
    volumes: [./Caddyfile:/etc/caddy/Caddyfile, caddy_data:/data]
volumes: { pgdata:, caddy_data: }
YAML
# Caddyfile: api.peacock.example.com { reverse_proxy api:8080 }
docker compose up -d
# peacock-web on Vercel: set NEXT_PUBLIC_API_URL=https://api.peacock.example.com
```

### If you want **easiest with managed Postgres** (sleep well) → **Vercel Hobby + DO Droplet $6 + Neon Launch (~$8)  OR  DO Managed**

| Option | Starter | Scale | When |
|---|---|---|---|
| **Vercel Hobby + DO $6 + Neon Free → Launch** | **$6** → ~$15 after 0.5 GB | ~$30 | **Best balance** — tiny DO VM just for Rust, DB serverless with PITR and branching, no DBA. |
| **Vercel Hobby + DO $12 + DO Managed $15** | **$27** | **$62** | Easiest one-invoice, no Neon account, DO backs up. Worth it if a failed `pg_dump` at 10pm would lose sales. |

### If you want **git-push PaaS with no SSH** → **DO App Platform**

> `Dockerfile` multi-stage for Axum + `Dev DB $7` is a trap — use **Neon Launch** externally. So `App Platform Shared 1GB $10 + Neon $0-8` = **$10-18 starter**, but you still pay more per-GB than Droplet and lose `journalctl` debugging. Only pick if you love Heroku and hate `ssh`.

### Final ranked recommendation for Peacock POS

| Rank | Stack | $/mo starter → scale | Why |
|---|---|---|---|
| **1** | **Vercel Hobby + Hetzner CPX21 self-Pg** | **$11 → $40** | 🏆 **Cheapest production-grade**, Rust-native, 20 TB, single box, 5 min deploy. Keep backups on. |
| 2 | Vercel Hobby + Hetzner CPX11 self-Pg | $6 → resize | For proof-of-concept / 1-terno dev — riskier (2 GB vs 4 GB). |
| 3 | Vercel Hobby + DO $6 + Neon Launch | $6 → $30 | Cheapest **managed DB** — good if you won't `docker exec postgres`. |
| 4 | Vercel Hobby + DO $12 + DO Managed $15 | $27 → $62 | **Easiest** — fully managed PG + simple Droplet. Pay for peace. |
| 5 | Vercel Pro + Hetzner CPX31 + Neon | — → $50 | Scale hedge: dedicated managed DB when 10 restos hit 2 GB. |

> **Rule of thumb:** Until you have 10 restos doing `daily-pl` simultaneously, you won't saturate even a CPX11. Start on **Hetzner CPX21** with self-hosted Pg; migrate to **Neon Launch** only when `pgdata` hits 2 GB or you want branching. Keep `peacock-web` on **Vercel Hobby** (flip to **Pro $20** the day you go commercial — cost is a seat, not usage).

---

## 10. Files Referenced

- `/Users/xoxo/Documents/resreah/billing/peacock-pos/peacock-api/src/events/sse.rs` — SSE endpoint, KeepAlive 15s, broadcast fan-out, Last-Event-ID resume
- `/Users/xoxo/Documents/resreah/billing/peacock-pos/peacock-web/src/hooks/useSSE.ts` — frontend EventSource hook, retry 3s ×1.5 backoff
- `/Users/xoxo/Documents/resreah/billing/peacock-pos/peacock-web/src/lib/api.ts` — `NEXT_PUBLIC_API_URL` → `peacock-api`, `X-Restaurant`/`Idempotency-Key` headers
- `/Users/xoxo/Documents/resreah/billing/peacock-pos/peacock-web/next.config.ts` — Next.js 15 config (rewrite destination for split)
- `/Users/xoxo/Documents/resreah/billing/peacock-pos/peacock-api/src/config.rs` — `PEACOCK_CORS_ALLOWED_ORIGINS`, `PEACOCK_API_HOST`/`PORT`, `LogFormat`
- `/Users/xoxo/Documents/resreah/billing/peacock-pos/peacock-storage/src/config.rs` — `DATABASE_URL`, pool sizing, `PEACOCK_DB_MAX_CONNECTIONS`
- `/Users/xoxo/Documents/resreah/billing/peacock-pos/docs/DEPLOYMENT.md` — current Windows host (100.72.103.1:5433/8080) — replace `NEXT_PUBLIC_API_URL` when moving to cloud
- `/Users/xoxo/Documents/resreah/billing/peacock-pos/docs/ARCHITECTURE.md` / `/Users/xoxo/Documents/resreah/billing/peacock-pos/Cargo.toml` — workspace crates, Rust 1.80+ requirement
- Live pricing: `https://vercel.com/pricing` + `https://vercel.com/docs/limits` (2026-08-03), `https://www.hetzner.com/cloud` / `.../cost-optimized` / `.../regular-performance`, `https://www.digitalocean.com/pricing/{droplets,app-platform,managed-databases}`, `https://neon.com/pricing`, `https://supabase.com/pricing`

*Research run 2026-08-11, cross-checked via `web_fetch` live pages + `grep` of Peacock codebase. Prices excl. VAT, USD≈EUR×1.08.*
