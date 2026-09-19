import path from "node:path";
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./tests/setupGlobals.ts", "./tests/setupTests.ts"],
    globals: true,
    // Pi 表单等重型用例在 5s 默认值边缘：交互密集的用例在 2 核 Windows runner 上
    // 会被资源竞争推到 5s 以上而假失败（本地单跑 2~3s、串行全绿）。这里只放宽上限，
    // 不改变断言，真实挂死仍会失败。
    testTimeout: 15000,
    hookTimeout: 15000,
    coverage: {
      reporter: ["text", "lcov"],
    },
  },
});
