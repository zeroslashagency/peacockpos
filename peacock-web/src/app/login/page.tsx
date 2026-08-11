"use client";

import { useEffect, useState, type FormEvent } from "react";
import { useRouter } from "next/navigation";
import Link from "next/link";
import { motion, AnimatePresence } from "framer-motion";
import { Envelope, Lock, ArrowRight, CircleNotch, Check, WarningCircle } from "@phosphor-icons/react";
import { authApi, apiBase } from "@/lib/api";

const spring = { type: "spring" as const, stiffness: 100, damping: 20 };

export default function LoginPage() {
  const router = useRouter();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [ok, setOk] = useState<string | null>(null);

  // if already authenticated, bounce to /pos
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const url = `${apiBase()}/api/auth/me`;
        const csrf =
          typeof window !== "undefined"
            ? window.localStorage.getItem("peacock_csrf") || window.localStorage.getItem("x-csrf-token")
            : null;
        const headers: Record<string, string> = { Accept: "application/json" };
        if (csrf) {
          headers["X-CSRF"] = csrf;
          headers["x-csrf-token"] = csrf;
        }
        const res = await fetch(url, { headers, credentials: "include" });
        if (!cancelled && res.ok) {
          router.replace("/pos");
        }
      } catch {
        /* remain on login */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [router]);

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setErr(null);
    setOk(null);
    const trimmedEmail = email.trim();
    if (!trimmedEmail || !password) {
      setErr("Email and password are required.");
      return;
    }
    setBusy(true);
    try {
      const res = await authApi.login({ email: trimmedEmail, password });
      // store already done in authApi.login; ensure header fallback stored
      const csrf = (res as unknown as { _csrf?: string })._csrf;
      if (csrf && typeof window !== "undefined") {
        try {
          window.localStorage.setItem("peacock_csrf", csrf);
        } catch {}
        // notify ShellNav in same tab
        window.dispatchEvent(new StorageEvent("storage", { key: "peacock_csrf", newValue: csrf }));
        window.dispatchEvent(new Event("peacock:auth"));
      }
      setOk("Signed in — redirecting to POS…");
      // small delay for tactile feedback, then hard redirect
      setTimeout(() => {
        router.push("/pos");
        router.refresh();
      }, 250);
    } catch (e2) {
      const msg = e2 instanceof Error ? e2.message : String(e2);
      // ApiError carries "title: detail"; strip to detail when possible
      setErr(msg);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="mx-auto flex w-full max-w-[1400px] flex-1 flex-col items-center justify-center bg-[#f9fafb] px-4 py-8 sm:px-6 sm:py-10">
      {/* subtle ambient dot */}
      <motion.div
        initial={{ opacity: 0, y: 8 }}
        animate={{ opacity: 1, y: 0 }}
        transition={spring}
        className="w-full max-w-[440px]"
      >
        {/* brand row */}
        <div className="mb-6 flex items-center justify-center gap-3">
          <span className="inline-flex h-8 w-8 items-center justify-center rounded-xl bg-zinc-900 text-xs font-bold tracking-tight text-white">
            P
          </span>
          <span className="text-sm font-semibold tracking-tighter text-zinc-900">Peacock</span>
          <span className="h-1 w-1 rounded-full bg-slate-300" aria-hidden />
          <span className="text-xs font-medium tracking-wide text-zinc-500">Branch — Main</span>
          <span className="ml-1 inline-flex items-center gap-1.5 rounded-full border border-slate-200/50 bg-white px-2.5 py-1 text-[11px] font-medium tracking-wide text-zinc-500 shadow-sm">
            <span className="h-1.5 w-1.5 rounded-full bg-emerald-500 animate-pulse" />
            Secure
          </span>
        </div>

        {/* card */}
        <div className="rounded-[2.5rem] border border-slate-200/50 bg-white p-8 shadow-[0_20px_40px_-15px_rgba(0,0,0,0.05)] sm:p-10">
          <div className="inline-flex items-center gap-2 rounded-full border border-slate-200/50 bg-[#f9fafb] px-3 py-1 text-[11px] font-semibold uppercase tracking-widest text-zinc-500">
            <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
            Sign in
          </div>
          <h1 className="mt-4 text-3xl font-semibold tracking-tighter leading-none text-zinc-900">Welcome back</h1>
          <p className="mt-2 max-w-[65ch] text-sm leading-6 text-zinc-600">
            Use your Peacock account. Session is <span className="font-medium text-zinc-900">HttpOnly</span> and CSRF-protected — you’ll land on{" "}
            <span className="font-mono text-xs font-medium tracking-tight text-zinc-900">/pos</span>.
          </p>

          <form onSubmit={onSubmit} className="mt-8 flex flex-col gap-5">
            {/* email */}
            <label className="flex flex-col gap-2">
              <span className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500">Email</span>
              <div className="relative">
                <Envelope
                  size={16}
                  weight="regular"
                  className="pointer-events-none absolute left-3.5 top-1/2 -translate-y-1/2 text-zinc-400"
                />
                <input
                  type="email"
                  autoComplete="email"
                  required
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="you@peacock.local"
                  className="w-full rounded-2xl border border-slate-200 bg-white py-2.5 pl-10 pr-4 text-sm tracking-tight text-zinc-900 placeholder:text-zinc-400 outline-none transition focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10"
                />
              </div>
            </label>

            {/* password */}
            <label className="flex flex-col gap-2">
              <span className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500">Password</span>
              <div className="relative">
                <Lock
                  size={16}
                  weight="regular"
                  className="pointer-events-none absolute left-3.5 top-1/2 -translate-y-1/2 text-zinc-400"
                />
                <input
                  type="password"
                  autoComplete="current-password"
                  required
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder="••••••••"
                  className="w-full rounded-2xl border border-slate-200 bg-white py-2.5 pl-10 pr-4 text-sm tracking-tight text-zinc-900 placeholder:text-zinc-400 outline-none transition focus:border-zinc-900 focus:ring-2 focus:ring-zinc-900/10"
                />
              </div>
              <span className="text-xs leading-5 text-zinc-400">HttpOnly cookie + <span className="font-mono text-zinc-600">X-CSRF</span> header stored locally.</span>
            </label>

            <AnimatePresence>
              {(err || ok) && (
                <motion.div
                  initial={{ opacity: 0, y: 4 }}
                  animate={{ opacity: 1, y: 0 }}
                  exit={{ opacity: 0, y: 4 }}
                  transition={spring}
                  className="flex flex-col gap-2"
                >
                  {err && (
                    <div className="flex items-start gap-2 rounded-2xl border border-red-200/60 bg-red-50 px-4 py-3 text-sm leading-6 text-red-700">
                      <WarningCircle size={16} weight="fill" className="mt-0.5 shrink-0 text-red-500" />
                      <span className="min-w-0 break-words">{err}</span>
                    </div>
                  )}
                  {ok && (
                    <div className="flex items-center gap-2 rounded-2xl border border-emerald-200/60 bg-emerald-50 px-4 py-3 text-sm leading-6 text-emerald-800">
                      <Check size={16} weight="bold" className="shrink-0 text-emerald-600" />
                      {ok}
                    </div>
                  )}
                </motion.div>
              )}
            </AnimatePresence>

            <motion.button
              whileTap={{ scale: 0.98, y: 1 }}
              transition={spring}
              type="submit"
              disabled={busy}
              className="inline-flex w-full items-center justify-center gap-2 rounded-full bg-zinc-900 px-6 py-3 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:-translate-y-[1px] hover:bg-zinc-800 hover:shadow-md active:scale-[0.98] disabled:opacity-50 disabled:hover:translate-y-0"
            >
              {busy ? (
                <>
                  <CircleNotch size={16} weight="regular" className="animate-spin" />
                  Signing in…
                </>
              ) : (
                <>
                  Sign in
                  <ArrowRight size={14} weight="bold" />
                </>
              )}
            </motion.button>

            <div className="flex items-center justify-between gap-3 pt-1 text-xs">
              <span className="tracking-tight text-zinc-500">
                No account? <span className="font-medium text-zinc-700">Contact owner</span>
              </span>
              <Link
                href="/pos"
                className="font-medium tracking-tight text-zinc-900 underline decoration-slate-300 underline-offset-4 hover:decoration-zinc-900"
              >
                Go to POS →
              </Link>
            </div>
          </form>
        </div>

        {/* foot */}
        <p className="mt-6 px-2 text-center text-xs leading-5 text-zinc-400">
          Secured by <span className="font-mono text-zinc-600">peacock_session</span> cookie (SameSite=Lax) · CSRF token persisted as{" "}
          <span className="font-mono text-zinc-600">peacock_csrf</span> and sent as{" "}
          <span className="font-mono text-zinc-600">X-CSRF</span>.
        </p>
      </motion.div>
    </div>
  );
}
