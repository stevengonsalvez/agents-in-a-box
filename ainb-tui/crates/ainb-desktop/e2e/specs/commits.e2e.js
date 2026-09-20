// The Commits journey: commits written by a separate process reach the open
// window's Commits tab as the reducer's own rows, and a click lands on the
// commit the frame names.
//
// Small on purpose. The parity halves already diff what the tab DRAWS against
// the terminal's own render over a committed fixture; what only a real window
// can prove is the wiring around it: the tab button, the mount, the section
// this window subscribes to, and a click crossing the seam into the reducer
// and coming back in a frame.

import assert from "node:assert/strict";
import { click, setPaletteQuery } from "../support.js";
import { run, seeded } from "../world.js";

/** The shell accelerator, as this platform spells it. */
const MOD = process.platform === "darwin" ? ["Meta"] : ["Control", "Shift"];

/** Commits this journey writes into the seeded session's branch. */
const COMMITS = 6;

describe("the commits tab", () => {
  it("draws the branch's own commits and takes a selection", async () => {
    const session = seeded()[0];
    assert.ok(session?.cwd, "the world seeded a session with a worktree");

    // Written by a separate process, as an agent committing would: the window
    // is already open and hears about them only through the reducer.
    // The identity is given per command: the world's HOME carries no git
    // config, which is the point of it being a world.
    const who = ["-c", "user.name=Journey", "-c", "user.email=journey@ainb.invalid"];
    for (let n = 0; n < COMMITS; n += 1) {
      run("git", ["-C", session.cwd, ...who, "commit", "--allow-empty", "-q", "-m", `journey commit ${n}`]);
    }
    const log = run("git", ["-C", session.cwd, "log", "--format=%h %s", `-${COMMITS}`])
      .split("\n")
      .filter(Boolean);
    assert.equal(log.length, COMMITS, log.join(" / "));
    const written = log.map((line) => line.split(" ")[0]);

    await $(".sidebar").waitForExist({ timeout: 90_000 });
    await browser.waitUntil(async () => !(await $(".banner").isExisting()), {
      timeout: 90_000,
      timeoutMsg: "the sidecar never connected: the window still shows a banner",
    });
    await $(`.session-row[data-session="${session.id}"]`).waitForExist({ timeout: 30_000 });
    await click(`.session-row[data-session="${session.id}"]`);

    // Another spec in this world may have left the reducer on the git view,
    // and `session_list.git` is a session-list row: out of context it is
    // refused, the view is never rebuilt, and the tab would draw the commits
    // as they were before this journey wrote any. So the view is closed first
    // when it is open, and only then opened again, which is what reads the
    // branch's commits (`show_git_view` builds the state from scratch).
    await tryCommand("git_view.back");
    assert.ok(await tryCommand("session_list.git"), "the palette never offered session_list.git");

    // The tab button, the mount and the subscription, which is the half the
    // parity tests cannot see: they render the component themselves.
    await click(".commits-tab .tab-title");
    await browser.waitUntil(async () => (await $$(".commit-row")).length > 0, {
      timeout: 60_000,
      timeoutMsg: "the commits tab drew no row",
    });

    // Every hash drawn is one this run wrote, so nothing was minted in the
    // window and nothing was read from some other repository.
    const drawn = [];
    for (const row of await $$(".commit-row")) drawn.push(await row.getAttribute("data-sha"));
    for (const sha of written) {
      assert.ok(drawn.includes(sha), `the window drew ${drawn}, which does not carry ${sha}`);
    }

    // A short list is not a cut list: the banner says nothing rather than
    // reading as a frame that left something out.
    assert.equal(
      await browser.execute(() => document.querySelector(".commits .review-cut") !== null),
      false,
      "a list of six commits framed a cut banner",
    );

    // A click names the commit, the reducer decides, and the next frame says
    // which commit it is on.
    const already = await selectedCommit();
    const wanted = drawn.find((sha) => sha !== already);
    assert.ok(wanted, `every drawn commit is already the selected one: ${drawn}`);
    await click(`.commit-row[data-sha="${wanted}"]`, 30_000);
    await browser.waitUntil(async () => (await selectedCommit()) === wanted, {
      timeout: 30_000,
      timeoutMsg: `the frame never named ${wanted} as the selected commit`,
    });
  });
});

/**
 * Run the command `id` from the palette, the way a person does, when the
 * palette offers it in the state the reducer is in now. `false` when it does
 * not: the palette is built from the reducer's own rows in context, so this is
 * also how the spec asks where the reducer is without reaching into it.
 */
async function tryCommand(id) {
  await openPalette();
  await setPaletteQuery(id);
  const row = await $(`.palette-row[data-row="command:${id}"]`);
  const drawn = await row.waitForExist({ timeout: 5_000 }).catch(() => false);
  // A row the reducer would not run now is still drawn, greyed and disabled,
  // so the list does not shift under a person (#1161). Drawn is therefore not
  // runnable: Enter on a greyed row does nothing and leaves the palette open,
  // which is exactly what this journey hit when it asked for `git_view.back`
  // from the session list.
  if (drawn === false || !(await row.isEnabled())) {
    await closePalette();
    return false;
  }
  await browser.waitUntil(
    async () =>
      (await browser.execute(
        () => document.querySelector('.palette-row[aria-selected="true"]')?.getAttribute("data-row") ?? "",
      )) === `command:${id}`,
    { timeout: 30_000, timeoutMsg: `the palette never put ${id} under the cursor` },
  );
  await browser.keys(["Enter"]);
  await browser.waitUntil(async () => !(await $(".palette-query").isExisting()), {
    timeout: 30_000,
    timeoutMsg: `the palette stayed open after running ${id}`,
  });
  return true;
}

/** Open the palette, or leave it open: the chord toggles it. */
async function openPalette() {
  if (await $(".palette-query").isExisting()) return;
  await browser.keys([...MOD, "k"]);
  await $(".palette-query").waitForExist({ timeout: 30_000 });
}

/** Close the palette, or leave it closed. */
async function closePalette() {
  if (!(await $(".palette-query").isExisting())) return;
  await browser.keys(["Escape"]);
  await browser.waitUntil(async () => !(await $(".palette-query").isExisting()), {
    timeout: 30_000,
    timeoutMsg: "the palette stayed open after Escape",
  });
}

/** The commit the frame says is selected, as the window draws it. */
async function selectedCommit() {
  const current = await $('.commit-row[aria-current="true"]');
  return (await current.isExisting()) ? current.getAttribute("data-sha") : null;
}
