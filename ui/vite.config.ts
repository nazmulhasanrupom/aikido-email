import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// In development the server runs on 8080 and this dev server proxies the API to it,
// so the browser sees one origin and the session cookie works.
// Start the server with AIKIDO_PUBLIC_URL=http://localhost:5173 so its Origin check passes.
const server = "http://127.0.0.1:8080";

export default defineConfig({
  plugins: [tanstackRouter({ target: "react", autoCodeSplitting: true }), react(), tailwindcss()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  server: {
    port: 5173,
    strictPort: true, // the server checks the Origin against AIKIDO_PUBLIC_URL, so the port must not change
    proxy: {
      "/v1": server,
      "/docs": server,
      "/openapi.json": server,
      "/health": server,
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
  },
});
