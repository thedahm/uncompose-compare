import { defineConfig, devices } from "@playwright/test";

// Three-engine matrix, pinned to ubuntu-latest in CI (issue #5). globalSetup
// launches the pip-installed binary once and records its tokened URL; every
// project drives that same served page.
export default defineConfig({
  testDir: ".",
  testMatch: "**/*.spec.mjs",
  timeout: 60_000,
  reporter: [["list"]],
  globalSetup: "./global-setup.mjs",
  globalTeardown: "./global-teardown.mjs",
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    { name: "firefox", use: { ...devices["Desktop Firefox"] } },
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
  ],
});
