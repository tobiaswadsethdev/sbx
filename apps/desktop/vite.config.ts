import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri drives this; the fixed port is what `devUrl` in tauri.conf.json expects,
// and `strictPort` makes a clash an error rather than a silently different port
// the window would then fail to load -- which is a failure with no error
// attached to it, so it is worth making loud here.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
});
