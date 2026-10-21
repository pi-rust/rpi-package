import { defineConfig } from "@playwright/test";
import base from "./playwright.config";

export default defineConfig({
  ...base,
  webServer: {
    command: "npx vite preview --host 127.0.0.1 --port 4191",
    url: "http://127.0.0.1:4191",
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
