import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  build: {
    outDir: "../assets",
    emptyOutDir: true,
    sourcemap: false,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (/node_modules[\\/](react|react-dom|scheduler)[\\/]/.test(id))
            return "react-runtime";
          if (
            id.includes("node_modules") &&
            (id.includes("highlight.js") || id.includes("lowlight"))
          )
            return "syntax";
        },
      },
    },
  },
  server: {
    host: "0.0.0.0",
    allowedHosts: true,
    port: 18087,
    strictPort: true,
    hmr: {
      host: "work-dev.bigfisharc.cn",
      protocol: "wss",
      clientPort: 443,
    },
    proxy: {
      "/api": {
        target: "http://127.0.0.1:18088",
        changeOrigin: true,
        configure(proxy) {
          proxy.on("proxyReq", (request) => {
            request.removeHeader("origin");
          });
        },
      },
    },
  },
});
