import { defineConfig } from "vite";
import { resolve } from "path";
import process from "node:process";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  root: "src",
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || "127.0.0.1",
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    outDir: "../build",
    emptyOutDir: true,
    rollupOptions: {
      input: {
        index: resolve(__dirname, "src/index.html"),
        audiochat: resolve(__dirname, "src/audiochat.html"),
        desktopwork: resolve(__dirname, "src/desktopwork.html"),
      },
    },
  },
});