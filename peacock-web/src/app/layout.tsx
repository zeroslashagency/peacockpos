import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import Link from "next/link";
import "./globals.css";

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

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html
      lang="en"
      className={`${geistSans.variable} ${geistMono.variable} h-full antialiased`}
    >
      <body className="min-h-full flex flex-col bg-zinc-50 text-zinc-900 dark:bg-zinc-950 dark:text-zinc-50">
        <header className="sticky top-0 z-40 w-full border-b border-zinc-200 bg-white/80 backdrop-blur supports-[backdrop-filter]:bg-white/60 dark:border-zinc-800 dark:bg-zinc-900/80">
          <div className="mx-auto flex h-14 max-w-6xl items-center justify-between px-4 sm:px-6">
            <Link href="/" className="flex items-center gap-2 font-semibold tracking-tight">
              <span className="inline-flex h-7 w-7 items-center justify-center rounded-md bg-zinc-900 text-xs font-bold text-white dark:bg-white dark:text-zinc-900">
                P
              </span>
              <span>Peacock POS</span>
              <span className="ml-2 hidden rounded bg-amber-100 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-widest text-amber-800 sm:inline dark:bg-amber-900/40 dark:text-amber-200">
                W3-A foundation
              </span>
            </Link>
            <nav className="flex items-center gap-1 sm:gap-2">
              <Link
                href="/pos"
                className="rounded-full px-3 py-1.5 text-sm font-medium text-zinc-600 transition hover:bg-zinc-100 hover:text-zinc-900 dark:text-zinc-300 dark:hover:bg-zinc-800 dark:hover:text-white"
              >
                POS
              </Link>
              <Link
                href="/kds"
                className="rounded-full px-3 py-1.5 text-sm font-medium text-zinc-600 transition hover:bg-zinc-100 hover:text-zinc-900 dark:text-zinc-300 dark:hover:bg-zinc-800 dark:hover:text-white"
              >
                KDS
              </Link>
              <Link
                href="/shifts"
                className="rounded-full px-3 py-1.5 text-sm font-medium text-zinc-600 transition hover:bg-zinc-100 hover:text-zinc-900 dark:text-zinc-300 dark:hover:bg-zinc-800 dark:hover:text-white"
              >
                Shifts
              </Link>
              <a
                href={`${process.env.NEXT_PUBLIC_API_URL ?? "http://100.72.103.1:8080"}/health`}
                target="_blank"
                rel="noreferrer"
                className="ml-1 hidden rounded-full border border-zinc-200 px-3 py-1.5 text-xs font-medium text-zinc-500 transition hover:bg-zinc-50 sm:inline dark:border-zinc-700 dark:text-zinc-400"
              >
                API Health
              </a>
            </nav>
          </div>
        </header>
        <main className="flex flex-1 flex-col">{children}</main>
        <footer className="border-t border-zinc-200 bg-white px-4 py-4 text-center text-xs text-zinc-500 dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-400">
          Peacock POS — Next.js 15 · Money as string · SSE KDS · Auth-less (Wave 3)
        </footer>
      </body>
    </html>
  );
}
