import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Daemon to proxy gRPC-Web calls to during `vite dev`. Override with
// PAWRLY_CONSOLE_DAEMON when your daemon listens elsewhere.
const daemon = process.env.PAWRLY_CONSOLE_DAEMON ?? "http://127.0.0.1:8787";

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    port: 5173,
    // Proxy gRPC-Web RPCs to the daemon so `vite dev` is same-origin — no CORS
    // flag and no manual Endpoint needed (the SPA defaults baseUrl to :5173).
    // The embedded production build is unaffected (it's served by the daemon
    // itself, already same-origin).
    proxy: {
      "/pawrly.v1.": {
        target: daemon,
        changeOrigin: true,
      },
    },
  },
});
