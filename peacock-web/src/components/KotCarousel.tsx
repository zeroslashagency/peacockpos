"use client";

import { memo } from "react";
import { motion } from "framer-motion";

// Isolated perpetual motion — never re-render parent layout
function KotCarousel({ children, enabled }: { children: React.ReactNode; enabled: boolean }) {
  if (!enabled) {
    return <div className="flex gap-4 overflow-x-auto pb-2 scrollbar-thin">{children}</div>;
  }
  // Duplicate track for seamless -100%
  return (
    <div className="relative overflow-hidden">
      <motion.div
        className="flex w-max gap-4 will-change-transform"
        animate={{ x: ["0%", "-50%"] }}
        transition={{ duration: 28, ease: "linear", repeat: Infinity }}
        style={{ willChange: "transform" }}
      >
        {/* first set */}
        <div className="flex gap-4 pr-4">{children}</div>
        {/* duplicate for loop */}
        <div aria-hidden className="flex gap-4 pr-4">
          {children}
        </div>
      </motion.div>
    </div>
  );
}

export default memo(KotCarousel);
