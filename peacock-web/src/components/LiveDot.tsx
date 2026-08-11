"use client";

import { motion } from "framer-motion";

export default function LiveDot({ live }: { live: boolean }) {
  return (
    <span className="hidden items-center gap-2 rounded-full border border-slate-200/70 bg-white px-3 py-1.5 sm:inline-flex">
      <span className="relative flex h-2 w-2">
        <motion.span
          aria-hidden
          className="absolute inline-flex h-full w-full rounded-full bg-emerald-500 opacity-75"
          animate={{ scale: live ? [1, 1.8, 1] : 1, opacity: live ? [0.7, 0, 0.7] : 0.35 }}
          transition={{ duration: 1.8, repeat: live ? Infinity : 0, ease: "easeInOut" }}
        />
        <span className={`relative inline-flex h-2 w-2 rounded-full ${live ? "bg-emerald-500" : "bg-zinc-400"}`} />
      </span>
      <span className="text-xs font-medium tracking-tight text-zinc-600">
        API <span className={live ? "text-emerald-600" : "text-zinc-400"}>{live ? "● Live" : "● Idle"}</span>
      </span>
    </span>
  );
}
