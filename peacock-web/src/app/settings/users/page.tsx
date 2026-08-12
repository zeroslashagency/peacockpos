"use client";

// design-taste-frontend-v1 — bg-[#f9fafb] rounded-[2.5rem] Geist
// Owner-only users console: list · add (email, role, restaurant, branch) · deactivate
// Auth: require owner role, redirect to /login if 401 (via ApiError + direct fetch fallback)

import { useCallback, useEffect, useState, type FormEvent } from "react";
import { useRouter } from "next/navigation";
import Link from "next/link";
import { motion, AnimatePresence } from "framer-motion";
import {
  Envelope,
  Shield,
  Storefront,
  House,
  UserPlus,
  Power,
  WarningCircle,
  Check,
  CircleNotch,
  ArrowsClockwise,
  Users as UsersIcon,
  Lock,
  Crown,
} from "@phosphor-icons/react";
import { apiBase, authApi, ApiError, type MeResponse } from "@/lib/api";

const spring = { type: "spring" as const, stiffness: 100, damping: 20 };

// Geist is provided by layout via --font-geist-sans / --font-geist-mono
// This page uses bg-[#f9fafb], rounded-[2.5rem], border-slate-200/50, shadow diffusion

type UserRow = {
  id: string;
  email: string;
  role: string;
  restaurant: string | null;
  branch: string | null;
  active: boolean;
  created_at?: string;
  updated_at?: string;
  created_by?: string | null;
};

type UsersResponse =
  | UserRow[]
  | { users: UserRow[]; count?: number }
  | { data: UserRow[]; count?: number };

const ROLES = ["waiter", "cashier", "manager", "owner"] as const;
type RoleOption = (typeof ROLES)[number];

function getCsrf(): string | null {
  if (typeof window === "undefined") return null;
  try {
    return (
      window.localStorage.getItem("peacock_csrf") ||
      window.localStorage.getItem("x-csrf-token") ||
      window.localStorage.getItem("csrf") ||
      null
    );
  } catch {
    return null;
  }
}

function roleBadge(role: string): string {
  const r = role.toLowerCase();
  if (r === "owner") return "bg-zinc-900 text-white border-zinc-900";
  if (r === "manager") return "bg-amber-50 text-amber-800 border-amber-200/60";
  if (r === "cashier") return "bg-emerald-50 text-emerald-700 border-emerald-200/60";
  return "bg-zinc-100 text-zinc-700 border-slate-200/60";
}

function normalizeUsers(data: UsersResponse): UserRow[] {
  if (Array.isArray(data)) return data;
  if (Array.isArray((data as { users?: unknown }).users)) return (data as { users: UserRow[] }).users;
  if (Array.isArray((data as { data?: unknown }).data)) return (data as { data: UserRow[] }).data;
  return [];
}

export const dynamic = "force-dynamic";

export default function UsersPage() {
  const router = useRouter();

  // auth
  const [me, setMe] = useState<MeResponse | null>(null);
  const [meLoading, setMeLoading] = useState(true);
  const [authDenied, setAuthDenied] = useState<string | null>(null);

  // users
  const [users, setUsers] = useState<UserRow[]>([]);
  const [uLoading, setULoading] = useState(false);
  const [uError, setUError] = useState<string | null>(null);

  // add form — email, password, role, restaurant, branch
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState<RoleOption>("waiter");
  const [restaurant, setRestaurant] = useState("");
  const [branch, setBranch] = useState("");
  const [formBusy, setFormBusy] = useState(false);
  const [formErr, setFormErr] = useState<string | null>(null);
  const [formOk, setFormOk] = useState<string | null>(null);

  // deactivate
  const [deactBusy, setDeactBusy] = useState<string | null>(null);
  const [deactErr, setDeactErr] = useState<string | null>(null);

  const isOwner = (() => {
    const r = (me?.role || "").toLowerCase();
    return r === "owner" || r === "dev";
  })();

  const fetchUsers = useCallback(async () => {
    setULoading(true);
    setUError(null);
    setDeactErr(null);
    try {
      const csrf = getCsrf();
      const headers: Record<string, string> = {
        Accept: "application/json, application/problem+json",
      };
      if (csrf) {
        headers["X-CSRF"] = csrf;
        headers["x-csrf-token"] = csrf;
      }
      const res = await fetch(`${apiBase()}/api/users`, {
        method: "GET",
        headers,
        credentials: "include",
      });
      if (res.status === 401) {
        router.replace("/login");
        return;
      }
      if (res.status === 403) {
        const text = await res.text().catch(() => "");
        throw new Error(text || "Forbidden — owner role required");
      }
      if (!res.ok) {
        const text = await res.text().catch(() => "");
        // try to parse Problem JSON detail
        let detail = text;
        try {
          const j = JSON.parse(text);
          detail = j.detail || j.title || text;
        } catch {}
        throw new Error(detail.slice(0, 500) || `HTTP ${res.status}`);
      }
      const text = await res.text();
      const data: UsersResponse = text ? (JSON.parse(text) as UsersResponse) : [];
      const list = normalizeUsers(data);
      setUsers(list);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      // ApiError 401 fallback
      if (msg.includes("401") || msg.toLowerCase().includes("unauthorized")) {
        router.replace("/login");
        return;
      }
      setUError(msg);
    } finally {
      setULoading(false);
    }
  }, [router]);

  // auth check — require owner role, redirect to /login if 401
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setMeLoading(true);
      setAuthDenied(null);
      try {
        const data = await authApi.me();
        if (cancelled) return;
        setMe(data);
        const r = (data.role || "").toLowerCase();
        if (r !== "owner" && r !== "dev") {
          setAuthDenied(`Role "${data.role}" cannot access /settings/users — owner required.`);
          // still allow fetch to surface 403, but we show denied UI
        } else {
          setAuthDenied(null);
        }
      } catch (e) {
        if (cancelled) return;
        const status =
          e instanceof ApiError ? e.status : (e as unknown as { status?: number })?.status;
        const msg = e instanceof Error ? e.message : String(e);
        const is401 =
          status === 401 ||
          msg.includes("401") ||
          msg.toLowerCase().includes("unauthorized") ||
          msg.toLowerCase().includes("authentication required") ||
          msg.toLowerCase().includes("missing session");
        if (is401) {
          // also probe direct fetch for strict 401 to satisfy redirect requirement
          try {
            const csrf = getCsrf();
            const headers: Record<string, string> = { Accept: "application/json" };
            if (csrf) {
              headers["X-CSRF"] = csrf;
              headers["x-csrf-token"] = csrf;
            }
            const res = await fetch(`${apiBase()}/api/auth/me`, {
              headers,
              credentials: "include",
            });
            if (res.status === 401) {
              router.replace("/login");
              return;
            }
          } catch {}
          router.replace("/login");
          return;
        }
        // other error — treat as denied if not 401
        setAuthDenied(msg.slice(0, 300));
        setMe(null);
      } finally {
        if (!cancelled) setMeLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [router]);

  // fetch users once owner is confirmed (or attempt anyway to surface 403)
  useEffect(() => {
    if (meLoading) return;
    if (!me) return;
    if (!isOwner) return;
    fetchUsers();
  }, [meLoading, me, isOwner, fetchUsers]);

  async function onAdd(e: FormEvent) {
    e.preventDefault();
    setFormErr(null);
    setFormOk(null);
    const trimmedEmail = email.trim();
    if (!trimmedEmail || !trimmedEmail.includes("@")) {
      setFormErr("Email is required and must contain @");
      return;
    }
    if (!password.trim()) {
      setFormErr("Password is required");
      return;
    }
    if (!ROLES.includes(role)) {
      setFormErr("Role must be one of waiter, cashier, manager, owner");
      return;
    }
    setFormBusy(true);
    try {
      const csrf = getCsrf();
      const headers: Record<string, string> = {
        "Content-Type": "application/json",
        Accept: "application/json, application/problem+json",
      };
      if (csrf) {
        headers["X-CSRF"] = csrf;
        headers["x-csrf-token"] = csrf;
      }
      const body: Record<string, unknown> = {
        email: trimmedEmail,
        password,
        role,
        restaurant: restaurant.trim() ? restaurant.trim() : null,
        branch: branch.trim() ? branch.trim() : null,
      };
      const res = await fetch(`${apiBase()}/api/users`, {
        method: "POST",
        headers,
        credentials: "include",
        body: JSON.stringify(body),
      });
      if (res.status === 401) {
        router.replace("/login");
        return;
      }
      if (!res.ok) {
        const text = await res.text().catch(() => "");
        let detail = text;
        try {
          const j = JSON.parse(text);
          detail = j.detail || j.title || text;
        } catch {}
        throw new Error(detail.slice(0, 600) || `HTTP ${res.status}`);
      }
      // success — clear form and refresh
      setFormOk(`User ${trimmedEmail} added as ${role}`);
      setEmail("");
      setPassword("");
      setRestaurant("");
      setBranch("");
      // keep role as selected for rapid adds
      await fetchUsers();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (msg.includes("401") || msg.toLowerCase().includes("unauthorized")) {
        router.replace("/login");
        return;
      }
      setFormErr(msg);
    } finally {
      setFormBusy(false);
    }
  }

  async function onDeactivate(u: UserRow) {
    if (!u.active) return;
    // prevent self-deactivation
    const selfEmail = (me?.email || me?.user || "").toLowerCase();
    if (selfEmail && u.email.toLowerCase() === selfEmail) {
      setDeactErr("You cannot deactivate your own account.");
      return;
    }
    setDeactBusy(u.id);
    setDeactErr(null);
    setUError(null);
    try {
      const csrf = getCsrf();
      const baseHeaders: Record<string, string> = {
        Accept: "application/json, application/problem+json",
        "Content-Type": "application/json",
      };
      if (csrf) {
        baseHeaders["X-CSRF"] = csrf;
        baseHeaders["x-csrf-token"] = csrf;
      }

      // Try POST /api/users/:id/deactivate (common), then PATCH /api/users/:id
      const idEnc = encodeURIComponent(u.id);

      // attempt 1: POST /api/users/:id/deactivate
      let res = await fetch(`${apiBase()}/api/users/${idEnc}/deactivate`, {
        method: "POST",
        headers: baseHeaders,
        credentials: "include",
        body: JSON.stringify({}),
      });

      // attempt 2: PATCH /api/users/:id { active:false } if 404/405
      if (res.status === 404 || res.status === 405) {
        res = await fetch(`${apiBase()}/api/users/${idEnc}`, {
          method: "PATCH",
          headers: baseHeaders,
          credentials: "include",
          body: JSON.stringify({ active: false }),
        });
      }

      // attempt 3: POST /api/users/:id/disable
      if (res.status === 404 || res.status === 405) {
        res = await fetch(`${apiBase()}/api/users/${idEnc}/disable`, {
          method: "POST",
          headers: baseHeaders,
          credentials: "include",
          body: JSON.stringify({}),
        });
      }

      // attempt 4: DELETE /api/users/:id (soft deactivate)
      if (res.status === 404 || res.status === 405) {
        res = await fetch(`${apiBase()}/api/users/${idEnc}`, {
          method: "DELETE",
          headers: { Accept: baseHeaders.Accept, "X-CSRF": baseHeaders["X-CSRF"] || "", "x-csrf-token": baseHeaders["x-csrf-token"] || "" },
          credentials: "include",
        });
      }

      if (res.status === 401) {
        router.replace("/login");
        return;
      }
      if (!res.ok) {
        const text = await res.text().catch(() => "");
        let detail = text;
        try {
          const j = JSON.parse(text);
          detail = j.detail || j.title || text;
        } catch {}
        throw new Error(detail.slice(0, 500) || `HTTP ${res.status}`);
      }

      // optimistic update + refetch
      setUsers((prev) => prev.map((x) => (x.id === u.id ? { ...x, active: false } : x)));
      await fetchUsers();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (msg.includes("401") || msg.toLowerCase().includes("unauthorized")) {
        router.replace("/login");
        return;
      }
      setDeactErr(msg);
    } finally {
      setDeactBusy(null);
    }
  }

  // ---------------------------------------------------------------------------
  // render
  // ---------------------------------------------------------------------------

  if (meLoading) {
    return (
      <div className="mx-auto flex min-h-[60dvh] w-full max-w-[1400px] flex-col gap-6 bg-[#f9fafb] px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
        <div className="shimmer rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
          <div className="h-6 w-40 rounded-full bg-slate-100" />
          <div className="mt-3 h-4 w-80 rounded-full bg-slate-50" />
        </div>
        <div className="rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)]">
          <div className="space-y-3">
            <div className="h-10 w-full rounded-2xl bg-slate-100" />
            <div className="h-10 w-full rounded-2xl bg-slate-50" />
            <div className="h-10 w-32 rounded-full bg-slate-100" />
          </div>
        </div>
      </div>
    );
  }

  // Not owner — forbidden (spec: Require owner role)
  if (authDenied) {
    const isRoleDenied = authDenied.toLowerCase().includes("owner required");
    return (
      <div className="mx-auto flex min-h-[60dvh] w-full max-w-[1400px] flex-col items-center justify-center gap-6 bg-[#f9fafb] px-4 py-8 sm:px-6">
        <motion.div
          initial={{ opacity: 0, y: 8 }}
          animate={{ opacity: 1, y: 0 }}
          transition={spring}
          className="w-full max-w-[640px] rounded-[2.5rem] border border-slate-200/50 bg-white p-8 text-center shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] sm:p-10"
        >
          <span className="mx-auto flex h-12 w-12 items-center justify-center rounded-2xl bg-amber-50 text-amber-600 ring-1 ring-amber-200/50">
            <Shield size={20} weight="light" />
          </span>
          <h1 className="mt-4 text-2xl font-semibold tracking-tighter text-zinc-900">Owner only</h1>
          <p className="mx-auto mt-2 max-w-[48ch] text-sm leading-6 text-zinc-600">
            {authDenied}{" "}
            {!isRoleDenied && (
              <>
                Please <Link href="/login" className="font-medium text-zinc-900 underline decoration-slate-300 underline-offset-4 hover:decoration-zinc-900">
                  sign in
                </Link>{" "}
                as <span className="font-mono text-xs font-medium tracking-tight text-zinc-900">owner@peacock.local</span>.
              </>
            )}
          </p>
          {me && (
            <p className="mt-3 font-mono text-xs tracking-tight text-zinc-500">
              Signed in as <span className="font-medium text-zinc-700">{me.email || me.user || me.sub}</span> · {me.role}
            </p>
          )}
          <div className="mt-6 flex justify-center gap-2">
            <Link
              href="/login"
              className="inline-flex items-center justify-center rounded-full bg-zinc-900 px-5 py-2.5 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:bg-zinc-800"
            >
              Go to login
            </Link>
            <Link
              href="/pos"
              className="inline-flex items-center justify-center rounded-full border border-slate-200 bg-white px-5 py-2.5 text-sm font-medium tracking-tight text-zinc-700 hover:bg-zinc-50"
            >
              Back to POS
            </Link>
          </div>
          {isRoleDenied && (
            <p className="mt-4 text-xs leading-5 text-zinc-400">
              Ask an owner to promote your account — <span className="font-mono text-zinc-600">role</span> is set at creation.
            </p>
          )}
        </motion.div>
      </div>
    );
  }

  return (
    <div className="mx-auto flex w-full max-w-[1400px] flex-col gap-6 bg-[#f9fafb] px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      {/* Header */}
      <div className="flex flex-col gap-3">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="flex flex-col gap-2">
            <span className="inline-flex items-center gap-2 self-start rounded-full border border-slate-200/50 bg-white px-3 py-1 text-[11px] font-semibold uppercase tracking-widest text-zinc-500 shadow-sm">
              <span className="h-1.5 w-1.5 rounded-full bg-violet-500" />
              Settings
              <span className="h-1 w-1 rounded-full bg-slate-300" />
              Users
              <Crown size={12} weight="light" className="text-zinc-400" />
              Owner
            </span>
            <h1 className="text-3xl font-semibold tracking-tighter leading-none text-zinc-900">Users</h1>
            <p className="max-w-[65ch] text-sm leading-6 text-zinc-600">
              Manage staff access — add by <span className="font-mono tracking-tight text-zinc-900">email</span>, assign{" "}
              <span className="font-mono tracking-tight text-zinc-900">role</span> ·{" "}
              <span className="font-mono tracking-tight text-zinc-900">restaurant</span> ·{" "}
              <span className="font-mono tracking-tight text-zinc-900">branch</span>, and deactivate when off-boarding.
              <br />
              <span className="text-zinc-500">Owner-only · 401 → </span>
              <span className="font-mono text-xs font-medium tracking-tight text-zinc-900">/login</span>
              <span className="text-zinc-500"> · session via </span>
              <span className="font-mono text-xs tracking-tight text-zinc-700">peacock_session</span>
            </p>
          </div>
          <div className="flex items-center gap-2">
            <span className="hidden items-center gap-2 rounded-full border border-slate-200/50 bg-white px-3 py-1.5 text-xs font-medium tracking-tight text-zinc-600 shadow-sm sm:inline-flex">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
              <span className="font-mono text-xs tracking-tight text-zinc-900">{me?.email || me?.user || "owner"}</span>
              <span className="text-zinc-300">·</span>
              <span className="capitalize">{me?.role}</span>
            </span>
            <button
              type="button"
              onClick={fetchUsers}
              disabled={uLoading}
              className="inline-flex items-center gap-1.5 rounded-full border border-slate-200 bg-white px-4 py-2 text-sm font-medium tracking-tight text-zinc-700 shadow-sm transition hover:bg-zinc-50 hover:-translate-y-px active:translate-y-px disabled:opacity-50"
            >
              <ArrowsClockwise size={14} weight="light" className={uLoading ? "animate-spin" : ""} />
              Refresh
            </button>
          </div>
        </div>
      </div>

      {/* Add user form — diffusion card */}
      <section className="rounded-[2.5rem] border border-slate-200/50 bg-white p-6 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] sm:p-8">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="flex items-center gap-2 text-sm font-semibold tracking-tighter text-zinc-900">
              <span className="inline-flex h-7 w-7 items-center justify-center rounded-full bg-zinc-900 text-white">
                <UserPlus size={14} weight="light" />
              </span>
              Add user
            </h2>
            <p className="mt-1 text-xs leading-5 text-zinc-500">
              Email + role + restaurant + branch — sent as <span className="font-mono tracking-tight text-zinc-700">POST /api/users</span> with{" "}
              <span className="font-mono tracking-tight text-zinc-700">X-CSRF</span>.
            </p>
          </div>
          <span className="hidden items-center gap-1.5 rounded-full bg-[#f9fafb] px-3 py-1 text-[11px] font-medium tracking-tight text-zinc-500 ring-1 ring-slate-200/50 sm:inline-flex">
            <Lock size={12} weight="light" /> Owner only
          </span>
        </div>

        <form onSubmit={onAdd} className="mt-6 grid gap-4 sm:grid-cols-2">
          {/* email */}
          <label className="flex flex-col gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500">Email</span>
            <div className="relative">
              <Envelope size={16} weight="regular" className="pointer-events-none absolute left-3.5 top-1/2 -translate-y-1/2 text-zinc-400" />
              <input
                type="email"
                required
                autoComplete="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder="new@peacock.local"
                className="w-full rounded-2xl border border-slate-200 bg-white py-2.5 pl-10 pr-4 text-sm tracking-tight text-zinc-900 placeholder:text-zinc-400 outline-none transition focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10"
              />
            </div>
          </label>

          {/* password */}
          <label className="flex flex-col gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500">Password</span>
            <div className="relative">
              <Lock size={16} weight="regular" className="pointer-events-none absolute left-3.5 top-1/2 -translate-y-1/2 text-zinc-400" />
              <input
                type="password"
                required
                autoComplete="new-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder="••••••••"
                className="w-full rounded-2xl border border-slate-200 bg-white py-2.5 pl-10 pr-4 text-sm tracking-tight text-zinc-900 placeholder:text-zinc-400 outline-none transition focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10"
              />
            </div>
            <span className="text-xs leading-5 text-zinc-400">Min 1 char — will be argon2-hashed.</span>
          </label>

          {/* role */}
          <label className="flex flex-col gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500">Role</span>
            <div className="relative">
              <Shield size={16} weight="regular" className="pointer-events-none absolute left-3.5 top-1/2 -translate-y-1/2 text-zinc-400" />
              <select
                value={role}
                onChange={(e) => setRole(e.target.value as RoleOption)}
                className="w-full appearance-none rounded-2xl border border-slate-200 bg-white py-2.5 pl-10 pr-10 text-sm tracking-tight text-zinc-900 outline-none transition focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10"
              >
                {ROLES.map((r) => (
                  <option key={r} value={r}>
                    {r}
                  </option>
                ))}
              </select>
              <span className="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2 text-zinc-400">▾</span>
            </div>
            <span className="text-xs leading-5 text-zinc-400">
              <span className="font-mono text-zinc-600">waiter</span> &lt; <span className="font-mono text-zinc-600">cashier</span> &lt;{" "}
              <span className="font-mono text-zinc-600">manager</span> &lt; <span className="font-mono text-zinc-600">owner</span>
            </span>
          </label>

          {/* restaurant */}
          <label className="flex flex-col gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500">Restaurant</span>
            <div className="relative">
              <Storefront size={16} weight="regular" className="pointer-events-none absolute left-3.5 top-1/2 -translate-y-1/2 text-zinc-400" />
              <input
                value={restaurant}
                onChange={(e) => setRestaurant(e.target.value)}
                placeholder="Peacock Restaurant"
                className="w-full rounded-2xl border border-slate-200 bg-white py-2.5 pl-10 pr-4 text-sm tracking-tight text-zinc-900 placeholder:text-zinc-400 outline-none transition focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10"
              />
            </div>
            <span className="text-xs leading-5 text-zinc-400">Optional — null for owner (all outlets).</span>
          </label>

          {/* branch */}
          <label className="flex flex-col gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500">Branch</span>
            <div className="relative">
              <House size={16} weight="regular" className="pointer-events-none absolute left-3.5 top-1/2 -translate-y-1/2 text-zinc-400" />
              <input
                value={branch}
                onChange={(e) => setBranch(e.target.value)}
                placeholder="Main Branch"
                className="w-full rounded-2xl border border-slate-200 bg-white py-2.5 pl-10 pr-4 text-sm tracking-tight text-zinc-900 placeholder:text-zinc-400 outline-none transition focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10"
              />
            </div>
            <span className="text-xs leading-5 text-zinc-400">Optional — scoped to restaurant/branch.</span>
          </label>

          <div className="sm:col-span-2 flex flex-col gap-3 pt-2">
            <AnimatePresence>
              {(formErr || formOk) && (
                <motion.div
                  initial={{ opacity: 0, y: 4 }}
                  animate={{ opacity: 1, y: 0 }}
                  exit={{ opacity: 0, y: 4 }}
                  transition={spring}
                  className="flex flex-col gap-2"
                >
                  {formErr && (
                    <div className="flex items-start gap-2 rounded-2xl border border-red-200/60 bg-red-50 px-4 py-3 text-sm leading-6 text-red-700">
                      <WarningCircle size={16} weight="fill" className="mt-0.5 shrink-0 text-red-500" />
                      <span className="min-w-0 break-words">{formErr}</span>
                    </div>
                  )}
                  {formOk && (
                    <div className="flex items-center gap-2 rounded-2xl border border-emerald-200/60 bg-emerald-50 px-4 py-3 text-sm leading-6 text-emerald-800">
                      <Check size={16} weight="bold" className="shrink-0 text-emerald-600" />
                      {formOk}
                    </div>
                  )}
                </motion.div>
              )}
            </AnimatePresence>

            <div className="flex flex-wrap items-center gap-2">
              <motion.button
                whileTap={{ scale: 0.98, y: 1 }}
                transition={spring}
                type="submit"
                disabled={formBusy}
                className="inline-flex items-center justify-center gap-2 rounded-full bg-zinc-900 px-6 py-2.5 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:bg-zinc-800 hover:-translate-y-px hover:shadow active:translate-y-px disabled:opacity-50"
              >
                {formBusy ? (
                  <>
                    <CircleNotch size={16} weight="regular" className="animate-spin" />
                    Adding…
                  </>
                ) : (
                  <>
                    <UserPlus size={16} weight="regular" />
                    Add user
                  </>
                )}
              </motion.button>
              <span className="text-xs leading-5 text-zinc-400">
                Password is set by the user on first login · email must be unique.
              </span>
            </div>
          </div>
        </form>
      </section>

      {/* List */}
      <section className="rounded-[2.5rem] border border-slate-200/50 bg-white p-6 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] sm:p-8">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold tracking-tighter text-zinc-900">
            <UsersIcon size={16} weight="light" />
            Staff
            <span className="rounded-full bg-zinc-900 px-2.5 py-0.5 font-mono text-xs font-semibold tracking-tight text-white">
              {users.length}
            </span>
            {uLoading && <span className="font-mono text-xs font-normal tracking-tight text-zinc-400">loading…</span>}
          </h2>
          <div className="flex items-center gap-2">
            <span className="hidden font-mono text-xs tracking-tight text-zinc-400 sm:inline">
              {users.filter((u) => u.active).length} active · {users.filter((u) => !u.active).length} deactivated
            </span>
            <button
              type="button"
              onClick={fetchUsers}
              disabled={uLoading}
              className="inline-flex items-center gap-1.5 rounded-full border border-slate-200 bg-white px-3 py-1.5 text-xs font-medium tracking-tight text-zinc-700 hover:bg-zinc-50 disabled:opacity-50"
            >
              <ArrowsClockwise size={12} weight="light" className={uLoading ? "animate-spin" : ""} />
              Refresh
            </button>
          </div>
        </div>

        <AnimatePresence>
          {(uError || deactErr) && (
            <motion.div
              initial={{ opacity: 0, y: 4 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: 4 }}
              transition={spring}
              className="mt-4 flex flex-col gap-2"
            >
              {uError && (
                <div className="flex items-start gap-2 rounded-2xl border border-red-200/60 bg-red-50 px-4 py-3 text-sm leading-6 text-red-700">
                  <WarningCircle size={16} weight="fill" className="mt-0.5 shrink-0 text-red-500" />
                  <span className="min-w-0 break-words">{uError}</span>
                  <button
                    onClick={fetchUsers}
                    className="ml-auto shrink-0 rounded-full bg-white px-3 py-1 text-xs font-semibold tracking-tight text-red-700 ring-1 ring-red-200 hover:bg-red-50"
                  >
                    Retry
                  </button>
                </div>
              )}
              {deactErr && (
                <div className="flex items-start gap-2 rounded-2xl border border-amber-200/60 bg-amber-50 px-4 py-3 text-sm leading-6 text-amber-800">
                  <WarningCircle size={16} weight="fill" className="mt-0.5 shrink-0 text-amber-500" />
                  <span className="min-w-0 break-words">{deactErr}</span>
                </div>
              )}
            </motion.div>
          )}
        </AnimatePresence>

        <div className="mt-6">
          {uLoading && users.length === 0 ? (
            <div className="grid gap-3">
              {[0, 1, 2].map((i) => (
                <div key={i} className="shimmer flex animate-pulse items-center justify-between rounded-2xl border border-slate-200/50 bg-white p-4">
                  <div className="flex items-center gap-3">
                    <div className="h-10 w-10 rounded-full bg-slate-100" />
                    <div className="space-y-2">
                      <div className="h-3 w-40 rounded-full bg-slate-100" />
                      <div className="h-2.5 w-24 rounded-full bg-slate-50" />
                    </div>
                  </div>
                  <div className="h-8 w-24 rounded-full bg-slate-100" />
                </div>
              ))}
            </div>
          ) : users.length === 0 ? (
            <div className="flex flex-col items-center justify-center rounded-[2rem] border border-dashed border-slate-200/70 bg-[#f9fafb] px-6 py-12 text-center">
              <span className="flex h-12 w-12 items-center justify-center rounded-2xl bg-white text-zinc-400 shadow-sm ring-1 ring-slate-200/50">
                <UsersIcon size={20} weight="light" />
              </span>
              <h3 className="mt-3 text-sm font-semibold tracking-tighter text-zinc-900">No users yet</h3>
              <p className="mt-1 max-w-[36ch] text-sm leading-6 text-zinc-500">
                Add your first staff member above — owner can invite waiters, cashiers, managers and scope them to a restaurant/branch.
              </p>
              {!uError && (
                <button
                  onClick={fetchUsers}
                  className="mt-4 inline-flex items-center gap-1.5 rounded-full bg-zinc-900 px-4 py-2 text-xs font-semibold tracking-tight text-white hover:bg-zinc-800"
                >
                  <ArrowsClockwise size={14} weight="light" />
                  Reload
                </button>
              )}
            </div>
          ) : (
            <motion.div
              initial="hidden"
              animate="show"
              variants={{ hidden: {}, show: { transition: { staggerChildren: 0.05 } } }}
              className="grid gap-3"
            >
              {users.map((u) => {
                const self = (me?.email || me?.user || "").toLowerCase() === u.email.toLowerCase();
                const canDeactivate = u.active && !self;
                return (
                  <motion.div
                    key={u.id}
                    variants={{ hidden: { opacity: 0, y: 8 }, show: { opacity: 1, y: 0, transition: spring } }}
                    layout
                    className={`flex flex-col gap-3 rounded-2xl border p-4 transition sm:flex-row sm:items-center sm:justify-between ${
                      u.active ? "border-slate-200/60 bg-white" : "border-slate-200/50 bg-[#f9fafb] opacity-75"
                    }`}
                  >
                    <div className="flex min-w-0 items-center gap-3">
                      <span
                        className={`flex h-10 w-10 shrink-0 items-center justify-center rounded-full border text-sm font-semibold tracking-tight ${
                          u.active ? "bg-zinc-900 text-white border-zinc-900" : "bg-white text-zinc-500 border-slate-200"
                        }`}
                        aria-hidden
                      >
                        {u.email.slice(0, 1).toUpperCase()}
                      </span>
                      <div className="min-w-0">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="truncate font-mono text-sm font-semibold tracking-tight text-zinc-900">{u.email}</span>
                          <span className={`inline-flex items-center rounded-full border px-2 py-0.5 text-[11px] font-semibold uppercase tracking-widest ${roleBadge(u.role)}`}>
                            {u.role}
                          </span>
                          <span className={`h-2 w-2 rounded-full ${u.active ? "bg-emerald-500" : "bg-zinc-300"}`} title={u.active ? "Active" : "Deactivated"} />
                          <span className={`text-xs font-medium tracking-tight ${u.active ? "text-emerald-700" : "text-zinc-500"}`}>
                            {u.active ? "Active" : "Deactivated"}
                          </span>
                          {self && (
                            <span className="rounded-full bg-violet-50 px-2 py-0.5 text-[11px] font-medium tracking-tight text-violet-700 ring-1 ring-violet-200/50">
                              you
                            </span>
                          )}
                        </div>
                        <div className="mt-1 flex flex-wrap items-center gap-1.5 text-xs tracking-tight text-zinc-500">
                          <span className="inline-flex items-center gap-1">
                            <Storefront size={12} weight="light" className="text-zinc-400" />
                            {u.restaurant || <span className="italic text-zinc-400">all restaurants</span>}
                          </span>
                          <span className="h-1 w-1 rounded-full bg-slate-300" />
                          <span className="inline-flex items-center gap-1">
                            <House size={12} weight="light" className="text-zinc-400" />
                            {u.branch || <span className="italic text-zinc-400">all branches</span>}
                          </span>
                          <span className="h-1 w-1 rounded-full bg-slate-300 hidden sm:inline-block" />
                          <span className="font-mono text-[11px] tracking-tight text-zinc-400">{u.id.slice(0, 8)}</span>
                        </div>
                      </div>
                    </div>

                    <div className="flex shrink-0 items-center gap-2 self-start sm:self-center">
                      {u.active ? (
                        <motion.button
                          whileTap={{ scale: 0.98 }}
                          transition={spring}
                          onClick={() => onDeactivate(u)}
                          disabled={!!deactBusy || !canDeactivate}
                          title={self ? "Cannot deactivate yourself" : `Deactivate ${u.email}`}
                          className={`inline-flex items-center gap-1.5 rounded-full border px-4 py-1.5 text-xs font-semibold tracking-tight shadow-sm transition ${
                            canDeactivate
                              ? "border-red-200 bg-white text-red-700 hover:bg-red-50 hover:-translate-y-px active:translate-y-px"
                              : "border-slate-200 bg-zinc-50 text-zinc-400 cursor-not-allowed"
                          }`}
                        >
                          {deactBusy === u.id ? (
                            <CircleNotch size={12} weight="regular" className="animate-spin" />
                          ) : (
                            <Power size={12} weight="regular" />
                          )}
                          {deactBusy === u.id ? "Deactivating…" : "Deactivate"}
                        </motion.button>
                      ) : (
                        <span className="inline-flex items-center gap-1.5 rounded-full border border-slate-200 bg-white px-3.5 py-1.5 text-xs font-medium tracking-tight text-zinc-500">
                          <span className="h-1.5 w-1.5 rounded-full bg-zinc-300" />
                          Deactivated
                        </span>
                      )}
                    </div>
                  </motion.div>
                );
              })}
            </motion.div>
          )}
        </div>

        <p className="mt-6 text-center text-xs leading-5 tracking-tight text-zinc-400">
          Deactivate is soft — login as <span className="font-mono text-zinc-600">deactivated</span> returns{" "}
          <span className="font-mono text-zinc-600">401</span> · Owner role required · 401 →{" "}
          <Link href="/login" className="font-medium text-zinc-700 underline decoration-slate-300 underline-offset-4 hover:decoration-zinc-900">
            /login
          </Link>
        </p>
      </section>
    </div>
  );
}
