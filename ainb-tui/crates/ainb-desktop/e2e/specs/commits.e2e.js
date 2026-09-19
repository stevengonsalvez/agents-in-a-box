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

    // The reducer builds the git view for the selected session, through the
    // palette, which is also what reads the branch's commits.
    await browser.keys([...MOD, "k"]);
    await setPaletteQuery("session_list.git");
    await $('.palette-row[data-row="command:session_list.git"]').waitForExist({ timeout: 30_000 });
    await browser.waitUntil(
      async () =>
        (await browser.execute(
          () => document.querySelector('.palette-row[aria-selected="true"]')?.getAttribute("data-row") ?? "",
        )) === "command:session_list.git",
      { timeout: 30_000, timeoutMsg: "the palette never put session_list.git under the cursor" },
    );
    await browser.keys(["Enter"]);

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

/** The commit the frame says is selected, as the window draws it. */
async function selectedCommit() {
  const current = await $('.commit-row[aria-current="true"]');
  return (await current.isExisting()) ? current.getAttribute("data-sha") : null;
}
