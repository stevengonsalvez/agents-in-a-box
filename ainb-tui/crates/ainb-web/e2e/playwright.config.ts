import { defineConfig, devices } from "@playwright/test";

// The runner script (scripts/hangar/run_web_e2e.sh) provisions the daemon +
// `ainb web` server and exports WEB_URL / WEB_TOKEN / TARGET_SESSION /
// HANGAR_HOME before invoking this suite. The suite never starts servers
// itself — it drives a real browser against the already-live dashboard.
const webUrl = process.env.WEB_URL;
if (!webUrl) {
  throw new Error(
    "WEB_URL is not set. Run this suite via scripts/hangar/run_web_e2e.sh, " +
      "which stands up the daemon + `ainb web` server and exports the journey env.",
  );
}

export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  // The journey polls a real tmux last-mile delivery + a SQLite flip, so give
  // each test room without masking a genuine hang.
  timeout: 60_000,
  expect: { timeout: 15_000 },
  reporter: [["list"]],
  use: {
    baseURL: webUrl,
    headless: true,
    trace: "off",
    screenshot: "only-on-failure",
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
  ],
});
