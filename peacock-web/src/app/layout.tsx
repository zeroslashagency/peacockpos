import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import Link from "next/link";
import "./globals.css";
import ShellNav from "@/components/ShellNav";

const geistSans = Geist({
  variable: "--font-geist-sans",
  subsets: ["latin"],
});

const geistMono = Geist_Mono({
  variable: "--font-geist-mono",
  subsets: ["latin"],
});

export const metadata: Metadata = {
  title: "Peacock POS",
  description: "Peacock POS — branch operations: orders, kitchen, invoices, shifts",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const apiBase = (
    process.env.NEXT_PUBLIC_API_URL || "http://2.28.30.22:8080"
  ).replace(/^=+/, "");
  return (
    <html
      lang="en"
      className={`${geistSans.variable} ${geistMono.variable} antialiased`}
    >
      <body className="flex min-h-[100dvh] flex-col bg-[#f9fafb] text-zinc-900 selection:bg-emerald-100 selection:text-emerald-900">
        <header className="sticky top-0 z-40 w-full border-b border-slate-200/50 bg-white/80 backdrop-blur supports-[backdrop-filter]:bg-white/70">
          <div className="mx-auto flex h-14 max-w-[1400px] items-center justify-between gap-4 px-4 sm:px-6">
            <Link
              href="/"
              className="flex items-center gap-3 shrink-0"
            >
              <span className="inline-flex h-7 w-7 items-center justify-center rounded-lg bg-zinc-900 text-xs font-bold tracking-tight text-white">
                P
              </span>
              <span className="text-[15px] font-semibold tracking-tighter text-zinc-900">
                Peacock
              </span>
              <span className="hidden items-center gap-2 sm:inline-flex">
                <span
                  className="h-1 w-1 rounded-full bg-slate-300"
                  aria-hidden
                />
                <span className="text-xs font-medium tracking-wide text-zinc-500">
                  Branch — Main
                </span>
              </span>
            </Link>

            <div className="flex items-center gap-2 sm:gap-3">
              <ShellNav />
              <a
                href={`${apiBase}/health`}
                target="_blank"
                rel="noreferrer"
                className="hidden items-center gap-2 rounded-full border border-slate-200/50 bg-white px-3 py-1.5 text-xs font-medium text-zinc-600 shadow-sm transition hover:bg-zinc-50 sm:inline-flex"
              >
                <span className="relative flex h-2 w-2">
                  <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-75" />
                  <span className="relative inline-flex h-2 w-2 rounded-full bg-emerald-500" />
                </span>
                <span className="hidden sm:inline">API</span>
                <span className="h-1 w-1 rounded-full bg-slate-300 hidden sm:inline-block" />
                Live
              </a>
            </div>
          </div>
        </header>

        <main className="flex flex-1 flex-col">{children}</main>

        <footer className="border-t border-slate-200/50 bg-white">
          <div className="mx-auto flex max-w-[1400px] flex-col items-center justify-between gap-2 px-4 py-4 text-xs text-zinc-500 sm:flex-row sm:px-6">
            <span className="tracking-tight">
              Peacock POS — Next.js 15 · Money as string · SSE KDS · Auth-less
              (Wave 3)
            </span>
            <span className="font-mono text-[11px] tracking-wide text-zinc-400">
              {apiBase}
            </span>
          </div>
        </footer>
      </body>
    </html>
  );
}
