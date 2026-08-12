"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { useCallback, useEffect, useState } from "react";
import { SignOut } from "@phosphor-icons/react";
import { apiBase, authApi, type MeResponse } from "@/lib/api";

const NAV = [
  { href: "/pos", label: "POS" },
  { href: "/kds", label: "KDS" },
  { href: "/shifts", label: "Shifts" },
] as const;

const DEV_NAV = [
  { href: "/dashboard", label: "Dashboard" },
  { href: "/settings/users", label: "Users" },
] as const;

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

export default function ShellNav() {
  const pathname = usePathname();
  const router = useRouter();
  const [me, setMe] = useState<MeResponse | null>(null);
  const [meLoading, setMeLoading] = useState(true);
  const [logoutBusy, setLogoutBusy] = useState(false);

  const fetchMe = useCallback(async () => {
    setMeLoading(true);
    try {
      const url = `${apiBase()}/api/auth/me`;
      const csrf = getCsrf();
      const headers: Record<string, string> = { Accept: "application/json" };
      if (csrf) {
        headers["X-CSRF"] = csrf;
        headers["x-csrf-token"] = csrf;
      }
      const res = await fetch(url, { headers, credentials: "include" });
      if (res.status === 401 || res.status === 403) {
        setMe(null);
        return;
      }
      if (!res.ok) {
        setMe(null);
        return;
      }
      const data = (await res.json()) as MeResponse;
      setMe(data);
    } catch {
      setMe(null);
    } finally {
      setMeLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchMe();
    const onStorage = (e: StorageEvent) => {
      if (e.key === "peacock_csrf" || e.key === "x-csrf-token" || e.key === null) fetchMe();
    };
    const onAuth = () => fetchMe();
    const onFocus = () => fetchMe();
    window.addEventListener("storage", onStorage);
    window.addEventListener("peacock:auth", onAuth);
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("peacock:auth", onAuth);
      window.removeEventListener("focus", onFocus);
    };
  }, [fetchMe]);

  // re-check when path changes (login -> pos)
  useEffect(() => {
    fetchMe();
  }, [pathname, fetchMe]);

  const handleLogout = useCallback(async () => {
    if (logoutBusy) return;
    setLogoutBusy(true);
    try {
      await authApi.logout();
    } catch {
      // still clear local state even if server 401
      if (typeof window !== "undefined") {
        try {
          window.localStorage.removeItem("peacock_csrf");
          window.localStorage.removeItem("x-csrf-token");
          window.localStorage.removeItem("csrf");
        } catch {}
      }
    } finally {
      setMe(null);
      setLogoutBusy(false);
      router.push("/login");
      router.refresh();
      // ensure ShellNav re-evaluates
      window.dispatchEvent(new Event("peacock:auth"));
    }
  }, [logoutBusy, router]);

  const displayName = me ? (me.email || me.name || me.user || me.sub || "you") : null;
  const role = me?.role ?? null;
  const isOwner = (() => {
    const r = (role || "").toLowerCase();
    return r === "owner" || r === "dev";
  })();

  return (
    <div className="flex items-center gap-2 sm:gap-3">
      <nav aria-label="Primary" className="flex items-center gap-1 rounded-full bg-zinc-100 p-1">
        {NAV.map((item) => {
          const active = pathname === item.href || pathname?.startsWith(`${item.href}/`);
          return (
            <Link
              key={item.href}
              href={item.href}
              aria-current={active ? "page" : undefined}
              className={`rounded-full px-3.5 py-1.5 text-sm font-medium transition-all duration-300 ease-[cubic-bezier(0.16,1,0.3,1)] ${
                active ? "bg-zinc-900 text-white shadow-sm" : "text-zinc-600 hover:bg-white hover:text-zinc-900"
              }`}
            >
              {item.label}
            </Link>
          );
        })}
      </nav>
      {isOwner && (
        <nav aria-label="Developer" className="hidden items-center gap-1 rounded-full bg-violet-50 p-1 ring-1 ring-violet-200/50 sm:flex">
          {DEV_NAV.map((item) => {
            const active = pathname === item.href || pathname?.startsWith(`${item.href}/`);
            return (
              <Link
                key={item.href}
                href={item.href}
                aria-current={active ? "page" : undefined}
                className={`rounded-full px-3 py-1 text-xs font-medium tracking-tight transition-all duration-300 ease-[cubic-bezier(0.16,1,0.3,1)] ${
                  active ? "bg-violet-600 text-white shadow-sm" : "text-violet-700 hover:bg-white hover:text-violet-900"
                }`}
              >
                {item.label}
              </Link>
            );
          })}
        </nav>
      )}

      {/* auth chip — design-taste-frontend-v1: bg-[#f9fafb]/white, border-slate-200/50 */}
      {meLoading ? (
        <span className="hidden h-8 w-24 animate-pulse rounded-full border border-slate-200/50 bg-slate-100 sm:inline-flex" aria-hidden />
      ) : me && displayName && role ? (
        <>
          <span className="hidden items-center gap-2 rounded-full border border-slate-200/50 bg-white px-3 py-1.5 text-xs font-medium tracking-tight text-zinc-700 shadow-sm sm:inline-flex">
            <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" aria-hidden />
            <span className="max-w-[14ch] truncate font-mono text-xs tracking-tight text-zinc-900">{displayName}</span>
            <span className="text-zinc-300">·</span>
            <span className="capitalize tracking-wide text-zinc-600">{role}</span>
          </span>
          {/* mobile compact */}
          <span className="inline-flex items-center gap-1.5 rounded-full border border-slate-200/50 bg-white px-2.5 py-1 text-[11px] font-medium tracking-tight text-zinc-700 shadow-sm sm:hidden">
            <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
            <span className="capitalize">{role}</span>
          </span>
          <button
            type="button"
            onClick={handleLogout}
            disabled={logoutBusy}
            className="inline-flex items-center gap-1.5 rounded-full bg-zinc-900 px-3.5 py-1.5 text-xs font-semibold tracking-tight text-white shadow-sm transition hover:-translate-y-[1px] hover:bg-zinc-800 hover:shadow active:scale-[0.98] disabled:opacity-50 sm:px-4 sm:py-2 sm:text-sm"
          >
            <SignOut size={14} weight="regular" className="hidden sm:block" />
            {logoutBusy ? "…" : "Logout"}
          </button>
        </>
      ) : (
        <Link
          href="/login"
          className="inline-flex items-center justify-center gap-1.5 rounded-full bg-zinc-900 px-4 py-1.5 text-xs font-semibold tracking-tight text-white shadow-sm transition hover:-translate-y-[1px] hover:bg-zinc-800 hover:shadow active:scale-[0.98] sm:px-4 sm:py-2 sm:text-sm"
        >
          Login
        </Link>
      )}
    </div>
  );
}
