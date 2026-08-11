"use client";

import { memo } from "react";
import { motion, AnimatePresence } from "framer-motion";

export const BreathingDot = memo(function BreathingDot({ active }: { active: boolean }) {
  return (
    <span className="relative inline-flex h-3 w-3 shrink-0">
      <motion.span
        aria-hidden
        className="absolute inline-flex h-full w-full rounded-full bg-emerald-500 opacity-60"
        animate={{ scale: active ? [1, 1.9, 1] : 1, opacity: active ? [0.55, 0, 0.55] : 0.35 }}
        transition={{ duration: 2.2, repeat: active ? Infinity : 0, ease: "easeInOut" }}
      />
      <span className={`relative inline-flex h-3 w-3 rounded-full ${active ? "bg-emerald-500" : "bg-zinc-300"}`} />
    </span>
  );
});

export const PopBadge = memo(function PopBadge({ count, show }: { count: number; show: boolean }) {
  return (
    <AnimatePresence>
      {show && (
        <motion.span
          key={`pop-${count}`}
          initial={{ scale: 0, y: -10, opacity: 0 }}
          animate={{ scale: 1, y: 0, opacity: 1 }}
          exit={{ scale: 0.8, opacity: 0, transition: { duration: 0.2 } }}
          transition={{ type: "spring", stiffness: 420, damping: 12 }}
          className="inline-flex items-center gap-1 rounded-full bg-emerald-600 px-2.5 py-1 text-xs font-semibold tracking-tight text-white shadow-sm"
        >
          <span className="h-1.5 w-1.5 rounded-full bg-white" />
          {count} new
        </motion.span>
      )}
    </AnimatePresence>
  );
});
