import { defineConfig } from "vite";

// Tauri expects a fixed dev port and no screen clearing so Rust errors stay visible.
export default defineConfig({
  root: "ui",
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { outDir: "dist", emptyOutDir: true, target: "es2021", sourcemap: false },
});
