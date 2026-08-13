"use client";

import { useEffect, useState, type FormEvent } from "react";
import { useRouter } from "next/navigation";
import Link from "next/link";
import { motion, AnimatePresence } from "framer-motion";
import { Envelope, Lock, ArrowRight, CircleNotch, Check, WarningCircle, Key, Hash } from "@phosphor-icons/react";
import { authApi, apiBase } from "@/lib/api";

const spring = { type: "spring" as const, stiffness: 100, damping: 20 };

export default function LoginPage() {
  const router = useRouter();
  const [mode, setMode] = useState<"password" | "pin">("pin");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [pin, setPin] = useState("");
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
      const csrf = (res as unknown as { _csrf?: string })._csrf;
      if (csrf && typeof window !== "undefined") {
        try {
          window.localStorage.setItem("peacock_csrf", csrf);
        } catch {}
        window.dispatchEvent(new StorageEvent("storage", { key: "peacock_csrf", newValue: csrf }));
        window.dispatchEvent(new Event("peacock:auth"));
      }
      setOk("Signed in — redirecting to POS…");
      setTimeout(() => {
        router.push("/pos");
        router.refresh();
      }, 250);
    } catch (e2) {
      const msg = e2 instanceof Error ? e2.message : String(e2);
      setErr(msg);
    } finally {
      setBusy(false);
    }
  }

  async function onPinSubmit(e: FormEvent) {
    e.preventDefault();
    setErr(null);
    setOk(null);
    const trimmedPin = pin.trim();
    if (!trimmedPin || trimmedPin.length < 4) {
      setErr("Enter 4-digit PIN.");
      return;
    }
    setBusy(true);
    try {
      const res = await authApi.pinLogin({ pin: trimmedPin });
      const csrf = (res as unknown as { _csrf?: string })._csrf;
      if (csrf && typeof window !== "undefined") {
        try {
          window.localStorage.setItem("peacock_csrf", csrf);
        } catch {}
        window.dispatchEvent(new StorageEvent("storage", { key: "peacock_csrf", newValue: csrf }));
        window.dispatchEvent(new Event("peacock:auth"));
      }
      setOk(`PIN ok — signed in as ${res.user?.email || "owner"} — redirecting…`);
      setTimeout(() => {
        router.push("/pos");
        router.refresh();
      }, 350);
    } catch (e2) {
      const msg = e2 instanceof Error ? e2.message : String(e2);
      setErr(msg.includes("pin") ? msg : `PIN failed: ${msg}`);
    } finally {
      setBusy(false);
    }
  }

  function pressPinDigit(d: string) {
    if (pin.length >= 6) return;
    setPin((p) => (p + d).slice(0, 6));
  }
  function clearPin() {
    setPin("");
    setErr(null);
  }
  function backspacePin() {
    setPin((p) => p.slice(0, -1));
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

          {/* Mode toggle — password vs PIN (demo) */}
          <div className="mt-6 flex rounded-full border border-slate-200 bg-[#f9fafb] p-1">
            <button
              type="button"
              onClick={() => {
                setMode("password");
                setErr(null);
              }}
              className={`flex flex-1 items-center justify-center gap-1.5 rounded-full px-4 py-2 text-xs font-semibold tracking-tight transition ${mode === "password" ? "bg-zinc-900 text-white shadow-sm" : "text-zinc-500 hover:text-zinc-700"}`}
            >
              <Lock size={12} weight="regular" /> Password
            </button>
            <button
              type="button"
              onClick={() => {
                setMode("pin");
                setErr(null);
              }}
              className={`flex flex-1 items-center justify-center gap-1.5 rounded-full px-4 py-2 text-xs font-semibold tracking-tight transition ${mode === "pin" ? "bg-zinc-900 text-white shadow-sm" : "text-zinc-500 hover:text-zinc-700"}`}
            >
              <Key size={12} weight="regular" /> PIN · Demo
            </button>
          </div>

          {mode === "password" ? (
            <form onSubmit={onSubmit} className="mt-6 flex flex-col gap-5">
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
          ) : (
            <form onSubmit={onPinSubmit} className="mt-6 flex flex-col gap-5">
              {/* PIN — demo / testing mode */}
              <div className="flex flex-col gap-3">
                <span className="text-[11px] font-semibold uppercase tracking-widest text-zinc-500">Demo PIN — testing mode</span>
                <div className="flex items-center justify-center gap-2.5 rounded-2xl border border-slate-200 bg-white py-4">
                  {[0, 1, 2, 3].map((i) => (
                    <span
                      key={i}
                      className={`flex h-12 w-11 items-center justify-center rounded-2xl border text-lg font-semibold tracking-tight transition ${pin[i] ? "border-zinc-900 bg-zinc-900 text-white shadow-sm" : "border-slate-200 bg-[#f9fafb] text-zinc-400"}`}
                    >
                      {pin[i] ? "•" : ""}
                    </span>
                  ))}
                </div>
                <div className="flex items-center justify-between gap-2 text-xs">
                  <span className="flex items-center gap-1.5 tracking-tight text-zinc-500">
                    <Hash size={12} weight="regular" className="text-zinc-400" /> 4 digits
                  </span>
                  <span className="font-mono text-[11px] tracking-tight text-zinc-400">Try 1234 · 0000 · 9999</span>
                </div>
                <div className="grid grid-cols-3 gap-2.5 pt-1">
                  {["1", "2", "3", "4", "5", "6", "7", "8", "9", "C", "0", "⌫"].map((k) => (
                    <button
                      key={k}
                      type="button"
                      onClick={() => {
                        if (k === "C") clearPin();
                        else if (k === "⌫") backspacePin();
                        else pressPinDigit(k);
                      }}
                      className={`flex h-12 items-center justify-center rounded-2xl border text-sm font-semibold tracking-tight transition active:scale-[0.98] ${k === "C" || k === "⌫" ? "border-slate-200 bg-[#f9fafb] text-zinc-600 hover:bg-white" : "border-slate-200 bg-white text-zinc-900 hover:bg-zinc-50 hover:border-zinc-900/20"}`}
                    >
                      {k}
                    </button>
                  ))}
                </div>
                <input
                  type="password"
                  inputMode="numeric"
                  autoComplete="off"
                  value={pin}
                  onChange={(e) => setPin(e.target.value.replace(/\D/g, "").slice(0, 6))}
                  placeholder="Enter PIN"
                  className="sr-only"
                  aria-hidden
                />
                <p className="text-xs leading-5 text-zinc-400">
                  PIN <span className="font-mono text-zinc-600">1234</span> logs in as <span className="font-mono text-zinc-600">owner@peacock.local</span> — demo/testing only. Also accepts <span className="font-mono text-zinc-600">0000</span>, <span className="font-mono text-zinc-600">9999</span>, <span className="font-mono text-zinc-600">1111</span>.
                </p>
              </div>

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
                disabled={busy || pin.length < 4}
                className="inline-flex w-full items-center justify-center gap-2 rounded-full bg-zinc-900 px-6 py-3 text-sm font-semibold tracking-tight text-white shadow-sm transition hover:-translate-y-[1px] hover:bg-zinc-800 hover:shadow-md active:scale-[0.98] disabled:opacity-50 disabled:hover:translate-y-0"
              >
                {busy ? (
                  <>
                    <CircleNotch size={16} weight="regular" className="animate-spin" />
                    Verifying PIN…
                  </>
                ) : (
                  <>
                    <Key size={14} weight="regular" />
                    Unlock with PIN
                  </>
                )}
              </motion.button>

              <div className="flex items-center justify-between gap-3 pt-1 text-xs">
                <button
                  type="button"
                  onClick={() => setMode("password")}
                  className="font-medium tracking-tight text-zinc-500 underline decoration-slate-300 underline-offset-4 hover:text-zinc-900 hover:decoration-zinc-900"
                >
                  Use password instead
                </button>
                <Link
                  href="/pos"
                  className="font-medium tracking-tight text-zinc-900 underline decoration-slate-300 underline-offset-4 hover:decoration-zinc-900"
                >
                  Go to POS →
                </Link>
              </div>
            </form>
          )}
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
