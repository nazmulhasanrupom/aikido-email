import { existsSync } from "node:fs";
import { defineConfig, devices } from "@playwright/test";

// The tests run against a real server with an empty database. `e2e/run.sh` makes one and starts it.
// Set AIKIDO_E2E_URL to use a server you started yourself.
// The browser: CHROME_PATH, else the Chrome of this machine, else the one Playwright installed.
const chrome = process.env.CHROME_PATH ?? "/usr/bin/google-chrome-stable";

export default defineConfig({
  testDir: "e2e",
  // The tests build on each other (the owner is made in the first one).
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: process.env.AIKIDO_E2E_URL ?? "http://localhost:8080",
    trace: "retain-on-failure",
    ...(existsSync(chrome) ? { launchOptions: { executablePath: chrome } } : {}),
  },
  projects: [{ name: "chrome", use: { ...devices["Desktop Chrome"] } }],
});
