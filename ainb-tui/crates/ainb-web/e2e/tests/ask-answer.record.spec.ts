import { test, expect } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { join } from "node:path";

// S1 — WEB ANSWER, recorded (verify-converged-goal.md journey catalogue).
//
// A headed + video-recorded mirror of `ask-answer.spec.ts` (the CI-gated CC18
// leg, which proves this exact fixture headless). Same fixture, same daemon,
// same real tmux delivery target — the only difference is this run is ON
// FILM: `playwright.config.record.ts` sets `headless: false, video: "on"`, and
// this spec additionally drops PNG stills at the three moments the journey
// catalogue calls out, plus explicit PASS console lines quoting the frame text
// each assertion is based on (so the recording's proof doesn't rely on
// trusting a green exit code alone).
//
// NOT CI-gated — a local recording aid only, run via
// scripts/hangar/run_web_e2e_record.sh. The authoritative, CI-safe assertion
// suite remains ask-answer.spec.ts / CC18.
const TARGET_SESSION = requireEnv("TARGET_SESSION");
const HANGAR_HOME = requireEnv("HANGAR_HOME");
const WEB_TOKEN = requireEnv("WEB_TOKEN");
const SCREENSHOT_DIR = requireEnv("SCREENSHOT_DIR");

const ASK_ID = "att-ask-1";
const ASK_QUESTION = "Ship to which env?";
const PICK_LABEL = "prod";
const PICK_ANSWER = "2";
const ANSWER_BUTTON = `${PICK_ANSWER}. ${PICK_LABEL}`;

const HANGAR_DB = join(HANGAR_HOME, ".agents-in-a-box", "hangar.db");

function requireEnv(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`${name} is not set (run via scripts/hangar/run_web_e2e_record.sh)`);
  return v;
}

function capturePane(): string {
  return execFileSync("tmux", ["capture-pane", "-t", TARGET_SESSION, "-p"], {
    encoding: "utf8",
  });
}

function attentionRow(): string {
  try {
    return execFileSync(
      "sqlite3",
      [
        HANGAR_DB,
        `SELECT state || '|' || COALESCE(answered_by,'') || '|' || COALESCE(answer,'') ` +
          `FROM attention WHERE id='${ASK_ID}';`,
      ],
      { encoding: "utf8" },
    ).trim();
  } catch {
    return "";
  }
}

async function poll(
  label: string,
  deadlineMs: number,
  pred: () => boolean,
): Promise<void> {
  const end = Date.now() + deadlineMs;
  for (;;) {
    if (pred()) return;
    if (Date.now() >= end) throw new Error(`timed out waiting for: ${label}`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

test("RECORDING: web dashboard answers a seeded ASK: render → click ② → delivered + answered(by=web)", async ({
  page,
}) => {
  await page.goto(`/?token=${encodeURIComponent(WEB_TOKEN)}`);

  // 1) RENDER: the ASK card and its three inline options come from the daemon.
  const askCard = page.locator(".need", { hasText: ASK_QUESTION });
  await expect(askCard).toBeVisible();
  await expect(askCard.locator(".need-kind")).toHaveText("ASK");
  await expect(page.getByRole("button", { name: "1. staging" })).toBeVisible();
  const pickButton = page.getByRole("button", { name: ANSWER_BUTTON });
  await expect(pickButton).toBeVisible();
  await expect(page.getByRole("button", { name: "3. canary" })).toBeVisible();
  const cardText = (await askCard.textContent()) ?? "";
  console.log(`PASS render: ASK card visible with options — frame text: ${JSON.stringify(cardText.trim())}`);

  expect(attentionRow()).toBe("open||");
  const paneBefore = capturePane();

  // Pause on the rendered card long enough for the recording to show it at
  // human-readable speed before the click.
  await page.waitForTimeout(1500);
  await page.screenshot({ path: join(SCREENSHOT_DIR, "s1-1-ask-card-rendered.png") });

  // 2) CLICK ②: routes POST /api/answer → daemon verified send.
  await pickButton.click();

  // 3) RPC-level flip: the answered row leaves the open inbox, so the ASK card
  //    (and its option buttons) disappear from the live needs panel.
  await expect(page.getByRole("button", { name: ANSWER_BUTTON })).toHaveCount(0);
  await expect(page.locator(".need", { hasText: ASK_QUESTION })).toHaveCount(0);
  console.log("PASS flip: ASK card removed from the open-needs panel after clicking option 2");
  await page.screenshot({ path: join(SCREENSHOT_DIR, "s1-2-card-flipped-gone.png") });

  // 4) LAST MILE: the picked keystroke actually landed in the target tmux pane.
  await poll(
    `keystroke "${PICK_ANSWER}" delivered into ${TARGET_SESSION}`,
    15_000,
    () => {
      const now = capturePane();
      return now !== paneBefore && now.includes(PICK_ANSWER);
    },
  );
  const paneAfter = capturePane();
  console.log(
    `PASS delivery: target tmux pane changed and contains "${PICK_ANSWER}" — ` +
      `frame text: ${JSON.stringify(paneAfter.trim().slice(-200))}`,
  );

  // 5) STORE TRUTH: the daemon recorded the row answered, by web, with the pick.
  let row = "";
  await poll("attention row flips to answered(by=web) in hangar.db", 15_000, () => {
    row = attentionRow();
    return row.startsWith("answered|");
  });
  expect(row).toBe(`answered|web|${PICK_ANSWER}`);
  console.log(`PASS store: attention row = ${JSON.stringify(row)}`);

  await page.waitForTimeout(1000);
  await page.screenshot({ path: join(SCREENSHOT_DIR, "s1-3-store-answered.png") });
});
