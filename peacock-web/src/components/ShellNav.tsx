"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

const NAV = [
  { href: "/pos", label: "POS" },
  { href: "/kds", label: "KDS" },
  { href: "/shifts", label: "Shifts" },
] as const;

export default function ShellNav() {
  const pathname = usePathname();
  return (
    <nav
      aria-label="Primary"
      className="flex items-center gap-1 rounded-full bg-zinc-100 p-1"
    >
      {NAV.map((item) => {
        const active =
          pathname === item.href || pathname?.startsWith(`${item.href}/`);
        return (
          <Link
            key={item.href}
            href={item.href}
            aria-current={active ? "page" : undefined}
            className={`rounded-full px-3.5 py-1.5 text-sm font-medium transition-all duration-300 ease-[cubic-bezier(0.16,1,0.3,1)] ${
              active
                ? "bg-zinc-900 text-white shadow-sm"
                : "text-zinc-600 hover:bg-white hover:text-zinc-900"
            }`}
          >
            {item.label}
          </Link>
        );
      })}
    </nav>
  );
}
