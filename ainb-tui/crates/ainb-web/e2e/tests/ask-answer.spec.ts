import { test, expect } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { join } from "node:path";

// C2 web ASK-answer journey (spec P8 / verify-converged CC-web leg).
//
// Proves the whole converged answer path through a REAL browser:
//   1. the dashboard renders the seeded 3-option ASK card via the daemon's
//      attention inbox (GET /api/snapshot → daemon attention/list, D18);
//   2. clicking option ② POSTs /api/answer → the daemon's ONE verified send
//      path delivers into the raising session's live tmux pane;
//   3. the answered row drops off the open inbox, so the ASK card disappears on
//      the re-pull (the RPC-level proof the flip happened);
//   4. the picked keystroke actually lands in the seeded target tmux pane
//      (capture-pane — the last mile the UI cannot itself show);
//   5. the daemon store records the row `answered` by `web` carrying the pick
//      (the by-surface attribution that is the entire point of C2).
//
// The runner script exports every coordinate this test needs.
const TARGET_SESSION = requireEnv("TARGET_SESSION");
const HANGAR_HOME = requireEnv("HANGAR_HOME");
const WEB_TOKEN = requireEnv("WEB_TOKEN");

// Seeded fixture identity (mirrors seed_control_center.rs / tripwire_p4_common).
const ASK_ID = "att-ask-1";
const ASK_QUESTION = "Ship to which env?";
// The frontend answers with the option's 1-based number ("reply N" contract),
// so both the delivered keystroke and the stored answer are the digit "2".
const PICK_LABEL = "prod";
const PICK_ANSWER = "2";
const ANSWER_BUTTON = `${PICK_ANSWER}. ${PICK_LABEL}`;

const HANGAR_DB = join(HANGAR_HOME, ".agents-in-a-box", "hangar.db");

function requireEnv(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`${name} is not set (run via scripts/hangar/run_web_e2e.sh)`);
  return v;
}

// Capture the visible contents of the seeded delivery-target tmux pane.
function capturePane(): string {
  return execFileSync("tmux", ["capture-pane", "-t", TARGET_SESSION, "-p"], {
    encoding: "utf8",
  });
}

// Read the seeded attention row straight from the daemon's SQLite store — the
// single source of truth for state + answered_by + answer. Returns "" on a
// transient read race so the caller can keep polling.
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

test("web dashboard answers a seeded ASK: render → click ② → delivered + answered(by=web)", async ({
  page,
}) => {
  // The dashboard requires the bearer token; the query param is consumed on boot
  // and stashed in sessionStorage, then sent on every /api/* call.
  await page.goto(`/?token=${encodeURIComponent(WEB_TOKEN)}`);

  // 1) RENDER: the ASK card and its three inline options come from the daemon.
  const askCard = page.locator(".need", { hasText: ASK_QUESTION });
  await expect(askCard).toBeVisible();
  await expect(askCard.locator(".need-kind")).toHaveText("ASK");
  await expect(page.getByRole("button", { name: "1. staging" })).toBeVisible();
  const pickButton = page.getByRole("button", { name: ANSWER_BUTTON });
  await expect(pickButton).toBeVisible();
  await expect(page.getByRole("button", { name: "3. canary" })).toBeVisible();

  // Baseline: the store still holds the row open (nobody has answered), and we
  // snapshot the target pane so the delivery assertion can prove it CHANGED
  // (rather than trusting an incidental digit already on the prompt line).
  expect(attentionRow()).toBe("open||");
  const paneBefore = capturePane();

  // 2) CLICK ②: routes POST /api/answer → daemon verified send.
  await pickButton.click();

  // 3) RPC-level flip: the answered row leaves the open inbox, so the ASK card
  //    (and its option buttons) disappear from the live needs panel.
  await expect(page.getByRole("button", { name: ANSWER_BUTTON })).toHaveCount(0);
  await expect(page.locator(".need", { hasText: ASK_QUESTION })).toHaveCount(0);

  // 4) LAST MILE: the picked keystroke actually landed in the target tmux pane.
  //    The daemon only keeps the row answered on a CONFIRMED tmux delivery, so a
  //    changed pane carrying the pick is the visible proof of the verified send.
  await poll(
    `keystroke "${PICK_ANSWER}" delivered into ${TARGET_SESSION}`,
    15_000,
    () => {
      const now = capturePane();
      return now !== paneBefore && now.includes(PICK_ANSWER);
    },
  );

  // 5) STORE TRUTH: the daemon recorded the row answered, by web, with the pick.
  let row = "";
  await poll("attention row flips to answered(by=web) in hangar.db", 15_000, () => {
    row = attentionRow();
    return row.startsWith("answered|");
  });
  expect(row).toBe(`answered|web|${PICK_ANSWER}`);
});
