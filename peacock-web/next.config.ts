import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  async rewrites() {
    // hard-coded Hetzner API — same-origin proxy avoids https→http mixed-content + CORS
    const api = "http://2.28.30.22:8080";
    return [
      { source: "/api/:path*", destination: `${api}/api/:path*` },
      { source: "/health", destination: `${api}/health` },
      { source: "/health/:path*", destination: `${api}/health/:path*` },
    ];
  },
};

export default nextConfig;
