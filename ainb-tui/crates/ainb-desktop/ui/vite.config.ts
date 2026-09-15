import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// Tauri serves the build output from `ui/dist`; the dev server port is the
// `devUrl` in `tauri.conf.json`.
export default defineConfig({
  plugins: [solid()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: "es2022", outDir: "dist", emptyOutDir: true },
});
