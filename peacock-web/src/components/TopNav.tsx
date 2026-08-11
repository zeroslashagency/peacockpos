"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

const items = [
  { href: "/pos", label: "POS" },
  { href: "/kds", label: "KDS" },
  { href: "/shifts", label: "Shifts" },
] as const;

export default function TopNav() {
  const pathname = usePathname();
  return (
    <nav className="flex items-center gap-1 rounded-full border border-slate-200/60 bg-white p-1 shadow-[0_8px_24px_-12px_rgba(0,0,0,0.08)]">
      {items.map((it) => {
        const active = pathname === it.href || pathname.startsWith(it.href + "/");
        return (
          <Link
            key={it.href}
            href={it.href}
            className={
              active
                ? "rounded-full bg-zinc-900 px-4 py-1.5 text-sm font-medium tracking-tight text-white shadow-sm transition"
                : "rounded-full px-4 py-1.5 text-sm font-medium tracking-tight text-zinc-500 transition hover:bg-zinc-50 hover:text-zinc-900"
            }
          >
            {it.label}
          </Link>
        );
      })}
    </nav>
  );
}
