// The desktop journey, in one run against a real daemon and real tmux
// sessions: the shell launches, the sidebar lists the seeded sessions grouped
// by workspace, a row opens a terminal tab that paints its pane and takes a
// typed line, the palette opens on the shell accelerator and runs a named
// command, and a session created by a separate CLI process appears without a
// restart.

import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { env, paneText, run, seed, seeded } from "../world.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const MIB = 1024 * 1024;

/** The bytes the recorded read pushes through tmux, the PTY and the pane. */
const BULK_BYTES = 50 * MIB;

/** Where the recorded throughput figure lands, for CI to upload. */
const REPORT = process.env.AINB_E2E_REPORT ?? join(HERE, "..", "throughput.json");

/** The shell accelerator, as this platform spells it. */
const MOD = process.platform === "darwin" ? ["Meta"] : ["Control", "Shift"];

/** The session whose tab the journey opened, for the recorded read to use. */
let attached = null;

const paintedBy = async (key) => Number(await $(`.terminal[data-tab="${key}"]`).getAttribute("data-painted"));

describe("the desktop shell", () => {
  it("opens a seeded session, runs a palette command and sees a new one arrive", async () => {
    const sessions = seeded();

    // The window is up and the sidecar connected: a banner stays up while it
    // is starting, reconnecting or degraded.
    await $(".sidebar").waitForExist({ timeout: 90_000 });
    await browser.waitUntil(async () => !(await $(".banner").isExisting()), {
      timeout: 90_000,
      timeoutMsg: async () => `the sidecar never connected: ${await $(".banner").getText()}`,
    });

    // The sidebar lists the seeded sessions, grouped by workspace.
    await browser.waitUntil(async () => (await $$(".session-row")).length >= sessions.length, {
      timeout: 60_000,
      timeoutMsg: `the sidebar never listed the ${sessions.length} seeded sessions`,
    });
    assert.ok((await $$(".workspace")).length >= 1, "the rows are grouped by workspace");

    // The sidebar's own first row opens a terminal tab, which paints the
    // pane's own output. It is taken in the order the sidebar draws, not the
    // order the world seeded, so the palette's "next" has somewhere to go.
    const firstRow = (await $$(".session-row"))[0];
    const firstId = await firstRow.getAttribute("data-session");
    const first = sessions.find((session) => session.id === firstId);
    assert.ok(first, `the sidebar's first row ${firstId} is one of the seeded sessions`);
    attached = first;
    await firstRow.click();
    const tab = await $(".terminal[data-tab]");
    await tab.waitForExist({ timeout: 60_000 });
    const key = await tab.getAttribute("data-tab");
    await browser.waitUntil(async () => (await paintedBy(key)) > 0, {
      timeout: 60_000,
      timeoutMsg: "the tab painted no bytes from its pane",
    });
    // Past the fixed tabs, which always have titles: this is the session's.
    // Both have to be excluded by name; with only the board excluded this
    // matched the Review tab, read "Review" and proved nothing.
    assert.ok(
      (await $(".tab:not(.board-tab):not(.review-tab) .tab-title").getText()).length > 0,
      "the tab strip names the session",
    );

    // A typed line reaches the pane. The agent on the other end is a separate
    // process on a real tmux server and echoes what it reads, so the pane's
    // own capture is the proof that the keys crossed the whole path.
    const typed = `e2e-${Date.now()}`;
    await $(`.terminal[data-tab="${key}"] .xterm`).click();
    // The characters go in through the webview's own input event, and Enter
    // through the driver. WebKitGTK's driver synthesises a character keydown
    // whose keyCode is the character code, which the terminal reads as a
    // function key, so a line typed that way reaches the pane mangled; Enter
    // carries its own key code and is unaffected. Everything after the key
    // event, the terminal's data path included, is the product's own.
    await browser.execute((text) => {
      const area = document.querySelector(".xterm-helper-textarea");
      area.focus();
      area.value = text;
      area.dispatchEvent(new InputEvent("input", { data: text, inputType: "insertText", bubbles: true }));
    }, typed);
    await browser.keys("Enter");
    await browser.waitUntil(() => paneText(first.tmux).includes(`agent read: ${typed}`), {
      timeout: 30_000,
      timeoutMsg: `the typed line never reached the agent in ${first.tmux}`,
    });

    // The palette opens on the shell accelerator, offers the command rows the
    // host built, and runs what Enter lands on. Two rows are run here: a
    // command, whose effect is the session list's own selection moving, and a
    // live session, whose effect is a tab.
    const selected = async () => await $(".session-row.selected").getAttribute("data-session");
    await (await $$(".session-row"))[0].click();
    const wasSelected = await selected();

    await browser.keys([...MOD, "k"]);
    await $(".palette-query").waitForExist({ timeout: 30_000 });
    await browser.keys("Select next session");
    await browser.waitUntil(async () => (await $$(".palette-row")).length > 0, {
      timeout: 15_000,
      timeoutMsg: "the palette offered no row for the command",
    });
    assert.equal(
      await $(".palette-row .palette-title").getText(),
      "Select next session",
      "the query's tightest match is the row Enter runs",
    );
    await browser.keys("Enter");
    await browser.waitUntil(async () => (await selected()) !== wasSelected, {
      timeout: 30_000,
      timeoutMsg: `the palette's command did not move the selection from ${wasSelected}`,
    });

    // The palette's other half: a live session, which opens its terminal tab.
    const other = sessions.find((session) => session.id !== first.id);
    assert.ok(other, "the world seeded a second session");
    const tabsNow = (await $$(".tab")).length;
    await browser.keys([...MOD, "k"]);
    await $(".palette-query").waitForExist({ timeout: 30_000 });
    await browser.execute((text) => {
      const query = document.querySelector(".palette-query");
      query.value = text;
      query.dispatchEvent(new Event("input", { bubbles: true }));
    }, other.branch);
    await browser.waitUntil(
      async () =>
        (await $$(".palette-row")).length > 0 &&
        (await $(".palette-row").getAttribute("data-row")).startsWith("session:"),
      { timeout: 15_000, timeoutMsg: `the palette offered no session row for ${other.id}` },
    );
    await browser.keys("Enter");
    await browser.waitUntil(async () => (await $$(".tab")).length > tabsNow, {
      timeout: 60_000,
      timeoutMsg: "the palette's session row opened no tab",
    });

    // A session another process creates arrives on the open window.
    const fresh = seed();
    await browser.waitUntil(async () => await $(`.session-row[data-session="${fresh.id}"]`).isExisting(), {
      timeout: 120_000,
      timeoutMsg: `the session ${fresh.id} created by the CLI never reached the sidebar`,
    });
  });

  // Recorded, never a pass condition: the spec still calls the number an open
  // spike, so the run reports what this runner reached and CI keeps the file.
  //
  // What is measured is a 50 MiB read through tmux, the PTY and the pane, to
  // the point where the shell takes its prompt back. The bytes the tab paints
  // are fewer than the bytes read on purpose: an attached tmux client is sent
  // rendered screen updates, not a replay of the pane's output, so the figure
  // to read is the time the read took with a live window attached.
  it("records what a 50 MiB read costs the window", async () => {
    const first = attached ?? seeded()[0];
    const key = await $(".terminal[data-tab]").getAttribute("data-tab");
    const path = `${env().HOME}/bulk.txt`;
    run("bash", ["-c", `head -c ${BULK_BYTES} /dev/urandom | base64 | head -c ${BULK_BYTES} > ${path}`]);

    // The agent reads the file on request and says when it is done, so the
    // read runs in the pane the tab is already showing.
    const before = await paintedBy(key);
    const started = Date.now();
    run("tmux", ["send-keys", "-t", `=${first.tmux}:`, `bulk ${path}`, "Enter"]);

    let finished = false;
    try {
      await browser.waitUntil(
        async () => {
          finished = paneText(first.tmux).includes("BULK DONE");
          return finished;
        },
        { timeout: 240_000, interval: 500 },
      );
    } catch {
      // Recorded as what it reached, not failed: the gap is the figure.
    }
    const seconds = (Date.now() - started) / 1000;
    const painted = (await paintedBy(key)) - before;
    const report = {
      platform: process.platform,
      read_bytes: BULK_BYTES,
      finished,
      seconds,
      painted_bytes: painted,
      mib_per_second: Number((BULK_BYTES / MIB / seconds).toFixed(2)),
      note: "an attached tmux client is sent rendered screen updates, not a replay of the pane's bytes",
    };
    writeFileSync(REPORT, `${JSON.stringify(report, null, 2)}\n`);
    console.log(`50 MiB read: ${JSON.stringify(report)}`);
  });
});
