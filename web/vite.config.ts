import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The console is served by the Rust binary at `/`. `base: "/"` makes the built
// index.html reference assets as /assets/…, which is the path the server's
// embedder answers.
//
// The bundle must be self-contained: `h5i ui` binds loopback and the page is
// meant to work on a machine with no route out, so nothing here may reach a CDN
// at runtime. Fonts are system-stack (see theme.css) for the same reason.
export default defineConfig({
  plugins: [react()],
  base: "/",
  server: {
    port: 5173,
    // `npm run dev` proxies the API to a running `h5i ui`, so the frontend can
    // be iterated on against real boxes. The dev server has no token, so start
    // the console with the same one the browser already holds — or just reload
    // the printed URL once and let the cookie carry it.
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
