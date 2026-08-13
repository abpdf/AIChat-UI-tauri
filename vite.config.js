import { defineConfig } from "vite";
import { resolve, dirname } from "path";
import { fileURLToPath } from "url";
import process from "node:process";

// 补充 ESM 下的 __dirname 定义
const __dirname = dirname(fileURLToPath(import.meta.url));

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  // 源码根目录（相对于项目根目录）
  root: "src",

  // 显式指定 public 目录（默认就是 "public"，但显式声明更明确）
  publicDir: "public",

  // 清屏（保持终端整洁）
  clearScreen: false,

  // 开发服务器配置
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

  // 构建配置
  build: {
    // 输出目录（相对于项目根目录）
    outDir: "../build",
    // 构建前清空输出目录
    emptyOutDir: true,
    // 显式启用 public 目录复制（默认为 true，这里显式声明）
    copyPublicDir: true,
    // 多入口配置
    rollupOptions: {
      input: {
        index: resolve(__dirname, "src/index.html"),
        audiochat: resolve(__dirname, "src/audiochat.html"),
        desktopwork: resolve(__dirname, "src/desktopwork.html"),
      },
    },
  },
});