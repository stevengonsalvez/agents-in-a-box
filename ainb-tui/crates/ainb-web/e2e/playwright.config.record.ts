import { defineConfig, devices } from "@playwright/test";

// S1 recording variant of playwright.config.ts (verify-converged-goal.md
// journey catalogue): the same CC18 fixture, driven HEADED with video "on" so
// the browser answering the seeded ASK is captured on film, not just asserted
// headless. Only `tests/ask-answer.record.spec.ts` matches — the CI-gated
// `ask-answer.spec.ts` / `playwright.config.ts` pair is untouched.
const webUrl = process.env.WEB_URL;
if (!webUrl) {
  throw new Error(
    "WEB_URL is not set. Run this suite via scripts/hangar/run_web_e2e_record.sh, " +
      "which stands up the daemon + `ainb web` server and exports the journey env.",
  );
}

export default defineConfig({
  testDir: "./tests",
  testMatch: /.*\.record\.spec\.ts/,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 60_000,
  expect: { timeout: 15_000 },
  reporter: [["list"]],
  use: {
    baseURL: webUrl,
    headless: false,
    viewport: { width: 1280, height: 800 },
    trace: "off",
    video: "on",
    screenshot: "off",
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
  ],
});
