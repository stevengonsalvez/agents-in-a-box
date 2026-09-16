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
import { env, paneText, run, seed, seeded, shellReady } from "../world.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const MIB = 1024 * 1024;

/** The bytes the recorded read pushes through tmux, the PTY and the pane. */
const BULK_BYTES = 50 * MIB;

/** Where the recorded throughput figure lands, for CI to upload. */
const REPORT = process.env.AINB_E2E_REPORT ?? join(HERE, "..", "throughput.json");

/** The shell accelerator, as this platform spells it. */
const MOD = process.platform === "darwin" ? ["Meta"] : ["Control", "Shift"];

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

    // A row opens a terminal tab, which paints the pane's own output.
    const [first, second] = sessions;
    await $(`.session-row[data-session="${first.id}"]`).click();
    const tab = await $(".terminal[data-tab]");
    await tab.waitForExist({ timeout: 60_000 });
    const key = await tab.getAttribute("data-tab");
    await browser.waitUntil(async () => (await paintedBy(key)) > 0, {
      timeout: 60_000,
      timeoutMsg: "the tab painted no bytes from its pane",
    });
    assert.ok((await $(".tab .tab-title").getText()).length > 0, "the tab strip names the session");

    // A typed line reaches the pane, which is a second process on a real tmux
    // server, so the pane's own capture is the proof. It is typed into a
    // window of its own, where a shell echoes it back: the session's first
    // pane belongs to the agent, which reads its input and prints over it.
    run("tmux", ["new-window", "-t", `=${first.tmux}:`]);
    await shellReady(first.tmux);
    const typed = `e2e-${Date.now()}`;
    await $(`.terminal[data-tab="${key}"] .xterm`).click();
    await browser.keys(`echo ${typed}\n`);
    await browser.waitUntil(() => paneText(first.tmux).includes(typed), {
      timeout: 30_000,
      timeoutMsg: `the typed line never reached ${first.tmux}`,
    });

    // The palette opens on the shell accelerator and runs a named command:
    // `session_list.next` moves the session list's selection.
    const selected = async () => await $(".session-row.selected").getAttribute("data-session");
    assert.equal(await selected(), first.id);
    await browser.keys([...MOD, "k"]);
    await $(".palette-query").waitForExist({ timeout: 30_000 });
    await browser.keys("Select next session");
    await browser.waitUntil(async () => (await $$(".palette-row")).length > 0, {
      timeout: 15_000,
      timeoutMsg: "the palette offered no row for the command",
    });
    await browser.keys("Enter");
    await browser.waitUntil(async () => (await selected()) === second.id, {
      timeout: 30_000,
      timeoutMsg: "the palette's command did not move the selection",
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
  it("records what a 50 MiB read paints", async () => {
    const [first] = seeded();
    const key = await $(".terminal[data-tab]").getAttribute("data-tab");
    const path = `${env().HOME}/bulk.txt`;
    run("bash", ["-c", `head -c ${BULK_BYTES} /dev/urandom | base64 | head -c ${BULK_BYTES} > ${path}`]);

    // A window of its own, so the read runs in a shell rather than in the
    // agent that owns the session's first pane.
    run("tmux", ["new-window", "-t", `=${first.tmux}:`]);
    await shellReady(first.tmux);
    const before = await paintedBy(key);
    const started = Date.now();
    run("tmux", ["send-keys", "-t", `=${first.tmux}:`, `cat ${path}`, "Enter"]);

    let painted = before;
    try {
      await browser.waitUntil(
        async () => {
          painted = await paintedBy(key);
          return painted - before >= BULK_BYTES;
        },
        { timeout: 240_000, interval: 500 },
      );
    } catch {
      // Recorded as what it reached, not failed: the gap is the figure.
    }
    const seconds = (Date.now() - started) / 1000;
    const report = {
      platform: process.platform,
      requested_bytes: BULK_BYTES,
      painted_bytes: painted - before,
      seconds,
      mib_per_second: Number((((painted - before) / MIB) / seconds).toFixed(2)),
    };
    writeFileSync(REPORT, `${JSON.stringify(report, null, 2)}\n`);
    console.log(`50 MiB read: ${JSON.stringify(report)}`);
  });
});
