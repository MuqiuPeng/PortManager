import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri serves the app from a fixed port and needs a predictable failure when
// that port is taken — the runtime this app manages must not silently move it.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: "safari15",
    sourcemap: false,
  },
});
