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

// Write the seeded attention row directly, to stand in for another surface
// winning the answer race. The daemon's first-answer-wins guard reads this row,
// so flipping it here is what makes the browser's POST lose.
function setAttentionRow(state: string, answeredBy: string | null, answer: string | null): void {
  const col = (v: string | null) => (v === null ? "NULL" : `'${v}'`);
  execFileSync("sqlite3", [
    HANGAR_DB,
    `UPDATE attention SET state='${state}', answered_by=${col(answeredBy)}, ` +
      `answer=${col(answer)} WHERE id='${ASK_ID}';`,
  ]);
}

// S-C, the loser's half of first-answer-wins. Declared FIRST so it runs against
// the still-open seeded row (this file runs single-worker, in declaration
// order), and it puts the row back before the delivering journey below.
//
// Both data routes are pinned, because the dashboard has two render drivers and
// only one of them is a fetch: `/api/snapshot` on boot and after an answer, and
// the SSE stream on `/api/events`, whose every `snapshot` frame calls `render`.
// Left live, the stream would drop the answered row and take the card with it,
// and this test would be asserting about a card that is not there. Pinned to a
// frozen frame instead, the row keeps arriving as open, which is exactly the
// condition the retirement has to survive: without it the next render puts live
// option buttons back on a row the daemon has already resolved.
test("web dashboard retires a card the daemon says another surface answered", async ({
  page,
}) => {
  let frozen: string | null = null;

  await page.route("**/api/snapshot*", async (route) => {
    if (frozen === null) {
      const res = await route.fetch();
      frozen = await res.text();
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: frozen,
    });
  });

  // One `snapshot` frame per connection, then EOF. EventSource reconnects on
  // its own, so this is a render every few seconds off the real code path
  // rather than a stream the test has to keep open.
  await page.route("**/api/events*", async (route) => {
    await poll("frozen snapshot captured", 15_000, () => frozen !== null);
    await route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      headers: { "cache-control": "no-cache" },
      body: `event: snapshot\ndata: ${frozen}\n\n`,
    });
  });

  await page.goto(`/?token=${encodeURIComponent(WEB_TOKEN)}`);

  const askCard = page.locator(".need", { hasText: ASK_QUESTION });
  await expect(askCard).toBeVisible();
  const pickButton = page.getByRole("button", { name: ANSWER_BUTTON });
  await expect(pickButton).toBeVisible();

  try {
    // Another surface wins while this browser is still looking at the card.
    // Inside the try, because from here on the seeded row is not the one the
    // delivering journey below expects: anything that throws between the flip
    // and the restore has to reach the finally.
    setAttentionRow("answered", "tui@e2e", "1");
    expect(attentionRow()).toBe("answered|tui@e2e|1");

    await pickButton.click();

    // The controls are gone and the winner is named, without waiting for any
    // snapshot to drop the row.
    await expect(askCard.locator(".need-actions")).toHaveCount(0);
    await expect(askCard.locator(".need-outcome")).toHaveText("answered by tui@e2e");
    await expect(askCard).toHaveAttribute("data-outcome", "already_answered");

    // Past a full 2s snapshot cycle, with the frozen frame still listing the
    // row: the card must stay retired across those renders.
    await page.waitForTimeout(2_500);
    await expect(askCard.locator(".need-actions")).toHaveCount(0);
    await expect(page.getByRole("button", { name: ANSWER_BUTTON })).toHaveCount(0);

    // And the retirement is a hint, not a lock. The daemon may put the same id
    // back: a winner whose delivery fails is reverted to `open` and re-raised
    // (`answer.rs::reopen_on_failed_delivery`), so once the hint has outlived
    // two snapshot cycles the daemon's view wins again and the row a frozen
    // frame still calls open becomes answerable. Holding it forever would
    // strand a reopened card with no controls for the life of the tab.
    await expect(page.getByRole("button", { name: ANSWER_BUTTON })).toBeVisible({
      timeout: 20_000,
    });
  } finally {
    // Hand the delivering journey below the open row it expects.
    setAttentionRow("open", null, null);
    await page.unroute("**/api/events*");
    await page.unroute("**/api/snapshot*");
  }

  expect(attentionRow()).toBe("open||");
});

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
