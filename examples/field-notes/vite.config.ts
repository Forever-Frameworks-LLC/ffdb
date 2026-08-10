import react from "@vitejs/plugin-react";
import { loadEnv } from "vite";
import { defineConfig } from "vitest/config";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const proxyTarget = env.FFDB_PROXY_TARGET ?? env.FFDB_API_URL;
  const proxy = proxyTarget === undefined
    ? undefined
    : Object.fromEntries(
        ["/v1", "/healthz", "/readyz"].map((path) => [path, {
          target: proxyTarget,
          changeOrigin: true,
          secure: true,
        }]),
      );

  return {
    plugins: [react()],
    server: { port: 5180, strictPort: true, ...(proxy === undefined ? {} : { proxy }) },
    preview: { port: 4180, strictPort: true },
    test: { environment: "node" },
  };
});
