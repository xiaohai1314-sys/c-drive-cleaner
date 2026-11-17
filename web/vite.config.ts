import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

export default defineConfig({
  // Tauri 通过自定义协议加载本地文件，资源必须用相对路径。
  base: "./",
  plugins: [vue()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "chrome105",
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});
