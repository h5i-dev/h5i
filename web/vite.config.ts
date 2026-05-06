import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// h5i workbench is served by the Rust binary at / (with /v2 as an alias).
// `base: "/"` makes the built index.html reference assets as /assets/...,
// which axum routes to rust-embed via the workbench_asset handler.
export default defineConfig({
  plugins: [react()],
  base: "/",
  server: {
    port: 5173,
    // During `npm run dev`, proxy /api requests to the running h5i serve
    // process so the frontend can talk to the same endpoints used in prod.
    proxy: {
      "/api": "http://127.0.0.1:8765",
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    target: "es2020",
  },
});
