import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  async rewrites() {
    const api = process.env.NEXT_PUBLIC_API_URL || "http://2.28.30.22:8080";
    return [
      { source: "/api/:path*", destination: `${api}/api/:path*` },
      { source: "/health", destination: `${api}/health` },
      { source: "/health/:path*", destination: `${api}/health/:path*` },
    ];
  },
};

export default nextConfig;
