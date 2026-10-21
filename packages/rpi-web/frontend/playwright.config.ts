import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  workers: process.env.CI ? 2 : undefined,
  testDir: "./e2e",
  timeout: 30_000,
  expect: { timeout: 5_000 },
  fullyParallel: true,
  reporter: "list",
  use: {
    baseURL: "http://127.0.0.1:4191",
    trace: "retain-on-failure",
  },
  webServer: {
    command: "npm run dev -- --port 4191",
    url: "http://127.0.0.1:4191",
    reuseExistingServer: false,
    timeout: 120_000,
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
