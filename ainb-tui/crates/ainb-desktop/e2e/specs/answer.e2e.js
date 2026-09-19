// The answer journey, in one run against a real daemon and a real tmux
// session: a three-option question an agent's hook raises reaches the board
// and the attention banner, a person answers it from the window by picking
// option two, the daemon's one verified send types it into the agent's pane,
// and the daemon records the desktop as the surface that answered, which a
// second surface started against the same daemon then reads back.
//
//   hook line ──▶ events.jsonl ──▶ daemon ──▶ window: board card + banner
//                                    ▲                    │ option two
//                                    │ attention/answer   ▼
//   ainb web ── already answered ────┘◀──── session_list.ask.* (reducer)
//                                    │
//                                    ▼ tmux send-keys
//                              agent pane: "agent read: prod"
//
// Two hook lines, not one, and the reason is a product gap rather than the
// harness (#1049): a Claude session row never learns its provider session id,
// so the sidebar takes only a question raised with no session id (matched by
// its unique worktree), while the board's waiting card needs one raised with
// the id its Fleet session carries. One line per half, same question, same
// worktree. The row with the id is never answered, so its card is still
// waiting when the journey ends; it is asserted that way, not as answered,
// and #1049 is what lets one question do both.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { AINB_BIN, env, hook, paneText, run, seeded } from "../world.js";

const QUESTION = "Ship to which environment?";
const OPTIONS = ["staging", "prod", "canary"];
/** Option two, as a person picks it: the second button, answered by label. */
const PICK = 1;

/** A PreToolUse line announcing the question, as the Claude hook writes it. */
function askLine(eventId, sessionId, cwd) {
  return {
    event_id: eventId,
    ts: Date.now(),
    session_id: sessionId,
    cwd,
    event_type: "PreToolUse",
    matcher: "AskUserQuestion",
    agent: "claude",
    payload: {
      tool_name: "AskUserQuestion",
      tool_input: { questions: [{ question: QUESTION, options: OPTIONS.map((label) => ({ label })) }] },
    },
  };
}

/**
 * What the window asked the host to do and what became of it, in order: each
 * `renderer intent` line the desktop logged, as `{ command, outcome }`. A key
 * or a text reads `key` or `text(N chars)`, since their content is never
 * logged, and `outcome` is `dispatched` or `refused`.
 */
function intentsSent() {
  let lines = "";
  try {
    lines = run("sh", ["-c", 'cat "$1"/desktop.log* 2>/dev/null | grep "renderer intent"', "log", env().AINB_HANGAR_HOME]);
  } catch {
    return [];
  }
  return lines
    .split("\n")
    .filter(Boolean)
    .map((line) => ({
      command: line.match(/command="?([^"\s]+)/)?.[1] ?? "unknown",
      outcome: line.match(/outcome="?(\w+)/)?.[1] ?? "unknown",
    }));
}

/** The desktop's own log lines about answering, for a failure to show. */
function desktopLog() {
  try {
    return run("sh", ["-c", 'grep -hiE "answer|ask|attention" "$1"/desktop.log* 2>/dev/null | tail -15', "log", env().AINB_HANGAR_HOME]);
  } catch {
    return "(no desktop log)";
  }
}

/**
 * Click `selector` as a person does: through the driver, so the click passes
 * WebDriver's own actionability checks (displayed, enabled, not covered),
 * which a click the page runs on itself never proves. The banner's buttons
 * are reconciled by label across frames, so the element the driver found is
 * the one it clicks; should a frame replace it in between, the driver says
 * so as a stale element and the click is looked up again, a few times.
 */
async function clickAsPerson(selector, attempts = 5) {
  let last = null;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    try {
      await $(selector).click();
      return;
    } catch (error) {
      if (!/stale element/i.test(String(error))) throw error;
      last = error;
    }
  }
  throw last;
}

/** Wait for `condition`, failing with what `explain` says at the deadline. */
async function settle(condition, timeout, explain) {
  try {
    await browser.waitUntil(condition, { timeout, interval: 100 });
  } catch {
    throw new Error(explain());
  }
}

/** The web dashboard, the second surface, while this spec runs. */
let web = null;

describe("answering from the window", () => {
  after(() => {
    // Its own child, stopped by its own pid.
    if (web !== null && web.exitCode === null) web.kill();
  });

  it("answers a daemon question from the banner and the daemon records the desktop", async () => {
    const target = seeded()[0];
    const stamp = Date.now();
    const provider = `e2e-provider-${stamp}`;

    await $(".sidebar").waitForExist({ timeout: 90_000 });
    await browser.waitUntil(async () => !(await $(".banner").isExisting()), {
      timeout: 90_000,
      timeoutMsg: async () => `the sidecar never connected: ${await $(".banner").getText()}`,
    });
    await $(`.session-row[data-session="${target.id}"]`).waitForExist({ timeout: 60_000 });

    // Raised while the window is open, by a process that is not the window.
    hook(askLine(`e2e-ask-board-${stamp}`, provider, target.cwd));
    hook(askLine(`e2e-ask-row-${stamp}`, "", target.cwd));

    // The board: the agent the id names sits in the waiting column.
    await $(".board-tab .tab-title").click();
    const card = `.board-column[data-state="waiting"] .board-card[data-card="claude:${provider}"]`;
    await $(card).waitForExist({
      timeout: 60_000,
      timeoutMsg: `no waiting card for claude:${provider} on the board`,
    });

    // The banner: the session's row carries the question, and selecting it
    // puts the question in front of the person.
    await $(`.session-row[data-session="${target.id}"]`).click();
    await $(".answer-banner[data-request]").waitForExist({
      timeout: 60_000,
      timeoutMsg: "the banner never showed the question",
    });
    // Queried again once it exists: an element looked up before it rendered
    // carries no id this driver can read an attribute through.
    const request = await $(".answer-banner[data-request]").getAttribute("data-request");
    assert.equal(await $(".answer-banner .answer-detail").getText(), QUESTION);
    const labels = await $$(".answer-banner .answer-option").map((option) => option.getText());
    assert.deepEqual(
      labels.map((text) => OPTIONS.find((label) => text.includes(label))),
      OPTIONS,
      "the banner offers the three options in order",
    );

    // Option two, clicked through the driver (#1192). The click sends the
    // reducer's own commands and nothing the window authored; the phase the
    // banner reads is the frame's `fleet.ask_state.phases` entry for this
    // request.
    const sentBefore = intentsSent().length;
    await clickAsPerson(`.answer-banner .answer-option[data-option="${PICK}"]`);
    let phase = "";
    await settle(
      async () => {
        phase = await browser.execute(
          (id) => document.querySelector(`.answer-banner[data-request="${id}"] .answer-phase`)?.dataset.phase ?? "",
          request,
        );
        return phase === "delivered";
      },
      60_000,
      () => `the banner never read delivered (last phase: ${phase || "none"}; pane: ${paneText(target.tmux).trim().split("\n").slice(-3).join(" / ")})\n${desktopLog()}`,
    );

    // What the window sent for that pick, from the host's own log: the
    // reducer's session list commands ending in one pick by label, no cursor
    // move and no Enter (#1191), and nothing it authored.
    const sent = intentsSent().slice(sentBefore);
    const shown = sent.map(({ command, outcome }) => `${command}:${outcome}`).join(", ");
    assert.deepEqual(
      sent.filter(({ command }) => !command.startsWith("session_list.")),
      [],
      `the pick sent only session list commands: ${shown}`,
    );
    assert.deepEqual(
      sent.filter(({ outcome }) => outcome !== "dispatched"),
      [],
      `the host applied every one of them: ${shown}`,
    );
    const picks = sent.filter(({ command }) => command === "session_list.ask.pick");
    assert.deepEqual(picks, [{ command: "session_list.ask.pick", outcome: "dispatched" }], `pick: ${shown}`);
    assert.equal(sent.at(-1)?.command, "session_list.ask.pick", `the pick is the last thing sent: ${shown}`);
    assert.deepEqual(
      sent.filter(({ command }) => /^session_list\.ask\.(next|previous|enter)$/.test(command)),
      [],
      `no cursor move and no Enter rode with the pick: ${shown}`,
    );

    // The last mile: the agent in the pane read the label.
    await browser.waitUntil(() => paneText(target.tmux).includes(`agent read: ${OPTIONS[PICK]}`), {
      timeout: 30_000,
      timeoutMsg: `the answer never reached the agent in ${target.tmux}`,
    });

    // The daemon's record, read through its own RPC by a second surface on
    // the same daemon (#1193): the web dashboard's answer to the same question
    // loses to the desktop's, and the refusal names the winner. The answer
    // text itself is the pane's business, proven above.
    const port = 20_000 + (stamp % 20_000);
    const token = `e2e-${stamp}`;
    web = spawn(AINB_BIN, ["web", "--listen", `127.0.0.1:${port}`, "--token", token], {
      env: env(),
      stdio: "ignore",
    });
    let second = null;
    await browser.waitUntil(
      async () => {
        try {
          const response = await fetch(`http://127.0.0.1:${port}/api/answer`, {
            method: "POST",
            headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
            body: JSON.stringify({ attentionId: request, answer: OPTIONS[0] }),
          });
          second = await response.json();
          return true;
        } catch {
          return false;
        }
      },
      { timeout: 60_000, interval: 500, timeoutMsg: "the web dashboard never answered" },
    );
    assert.equal(second?.outcome, "already_answered", JSON.stringify(second));
    // `<surface>@<host>`: the surface is what the person sat at.
    assert.match(second.by, /^desktop@/, `the desktop answered, not the terminal: ${JSON.stringify(second)}`);

    // The board's card is the row raised with the provider id, which no one
    // answered, so it is still waiting. With #1049 closed, one question would
    // be both the banner's and the card's, and this would read answered.
    // Selecting the row opened its terminal tab, so the board is brought back
    // to be read.
    await $(".board-tab .tab-title").click();
    await $(card).waitForExist({
      timeout: 30_000,
      timeoutMsg: "the card for the unanswered row left the waiting column (#1049 keeps it there)",
    });
  });
});
