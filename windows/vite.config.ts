import { defineConfig } from "vite";
import preact from "@preact/preset-vite";

// Vite dev/build config for the Tauri frontend. The dev server is only used by
// `tauri dev`; production builds land in dist/ and are embedded by tauri build.
export default defineConfig({
  plugins: [preact()],
  clearScreen: false,
  server: {
    port: 51420,
    strictPort: true,
  },
  build: {
    target: "es2021",
    outDir: "dist",
    sourcemap: false,
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
