import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The Tauri config points `frontendDist` at ./ui/dist, so the build must land
// there. `base: "./"` keeps asset URLs relative — the WebView loads over
// tauri://localhost and the CSP is `default-src 'self'`.
export default defineConfig({
  plugins: [react()],
  base: "./",
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "safari15",
    sourcemap: false,
    // One chunk. A search tool that code-splits its only screen has traded
    // cold-start budget (GUI §7: < 800 ms to usable search) for nothing.
    chunkSizeWarningLimit: 900,
  },
});
