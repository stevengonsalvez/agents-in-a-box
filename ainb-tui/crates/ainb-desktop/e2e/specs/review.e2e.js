// The review journey: a diff written by a separate process reaches the open
// window's review tab as the reducer's own rows, a selection sent from the
// window lands on the file the frame named, what the window costs to draw at
// that size is recorded rather than guessed (#1221), and the settings page
// draws the config section and takes a selection the same way.
//
// The diff is deliberately over a byte floor and over the frame's row bound
// (`MAX_ROWS_TOTAL`, `ainb-app/src/wire/git_view.rs`), so the run exercises the
// cut and its counters rather than a toy change. Both numbers are recorded in
// the report this writes, and the floor is asserted, so a repository that
// quietly shrank cannot leave the journey passing over nothing.

import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { click, intentsSent, selectedNode } from "../support.js";
import { run, seeded } from "../world.js";

const HERE = dirname(fileURLToPath(import.meta.url));

/** Where the recorded render figures land, beside the throughput report. */
const REPORT = process.env.AINB_E2E_REVIEW_REPORT ?? join(HERE, "..", "review-render.json");

/** The shell accelerator, as this platform spells it. */
const MOD = process.platform === "darwin" ? ["Meta"] : ["Control", "Shift"];

/**
 * Files and lines the journey writes. Together they are 10,800 rows, well past
 * the frame's MAX_ROWS_TOTAL of 4,000, and over half a MiB of text, which is
 * the floor below.
 */
const FILES = 12;
const LINES = 900;

/** The diff must be at least this large, or the bound is never reached. */
const BYTE_FLOOR = 512 * 1024;

/** One line of a changed file: long enough to be real, short of the cut. */
const line = (file, n) => `pub fn generated_${file}_${n}() -> usize { ${n} * ${file} + 7 }`;

describe("reviewing from the window", () => {
  it("draws the reducer's diff, takes a selection, and records what it costs", async () => {
    const session = seeded()[0];
    assert.ok(session?.cwd, "the world seeded a session with a worktree");

    // A separate process writes the diff, as an agent would: the window is
    // already open and never hears about it except through the reducer.
    let bytes = 0;
    for (let file = 0; file < FILES; file += 1) {
      const body = Array.from({ length: LINES }, (_, n) => line(file, n)).join("\n") + "\n";
      bytes += Buffer.byteLength(body);
      writeFileSync(join(session.cwd, `generated_${file}.rs`), body);
    }
    assert.ok(
      bytes >= BYTE_FLOOR,
      `the diff is ${bytes} bytes, under the ${BYTE_FLOOR} floor this journey exists to exercise`,
    );
    // The paths are the repository's, so the review model names them as git
    // does rather than as this spec spelled them.
    const status = run("git", ["-C", session.cwd, "status", "--porcelain"]);
    assert.equal(status.split("\n").filter(Boolean).length, FILES, status);

    await $(".sidebar").waitForExist({ timeout: 90_000 });
    await browser.waitUntil(async () => !(await $(".banner").isExisting()), {
      timeout: 90_000,
      timeoutMsg: "the sidecar never connected: the window still shows a banner",
    });
    await browser.waitUntil(async () => (await $$(".session-row")).length > 0, {
      timeout: 60_000,
      timeoutMsg: "the sidebar never listed a session",
    });

    // The session whose worktree the diff is in, selected the way a person
    // selects one.
    await $(`.session-row[data-session="${session.id}"]`).waitForExist({ timeout: 30_000 });
    await click(`.session-row[data-session="${session.id}"]`);

    // The reducer builds the review for the selected session, through the
    // palette rather than through anything this spec reaches into.
    await browser.keys([...MOD, "k"]);
    const query = await $(".palette-query");
    await query.waitForExist({ timeout: 30_000 });
    // The palette draws the first PALETTE_ROWS of the list until a query
    // narrows it, and this row is far down, so it is asked for by id: the
    // detail line a row carries is "<context> · <id>". setValue rather than
    // keys, because the query survives the palette being closed and reopened,
    // and another spec in this world has typed in it before now: typed keys
    // would land after whatever it still held.
    await query.setValue("session_list.git");
    // Clicked the way a person clicks it: found once, then clicked, with no
    // retry on a stale element. The palette keeps an unchanged row's node
    // across the frames the host keeps sending (#1267), so the row found is
    // still the row on screen when the click lands. A palette that rebuilt its
    // rows again would fail here with a stale element, not pass on a retry.
    const row = await $('.palette-row[data-row="command:session_list.git"]');
    await row.waitForExist({ timeout: 30_000 });
    await row.click();

    // The review tab, and the first row drawn: the wall clock across this is a
    // real window's first render of a diff at the bound, which is the figure
    // #1221 asks for and the server render in `review.bound.test.ts` cannot
    // give.
    const started = Date.now();
    await click(".review-tab .tab-title");
    // The default poll is 500 ms, which is larger than the figure being
    // measured; at 20 ms the reading is the render, not the polling.
    await browser.waitUntil(async () => (await $$(".review-row")).length > 0, {
      timeout: 120_000,
      interval: 20,
      timeoutMsg: "the review tab drew no rows",
    });
    const drawnMs = Date.now() - started;

    // Every path drawn is one the reducer framed, and every one of them is a
    // file this run wrote: nothing minted in the window.
    const drawn = [];
    for (const file of await $$(".review-file")) drawn.push(await file.getAttribute("data-file"));
    assert.ok(drawn.length > 0, "the sidebar of the review drew no file");
    for (const path of drawn) {
      assert.match(path, /^generated_\d+\.rs$/, `the window drew ${path}, which this run did not write`);
    }

    const rows = (await $$(".review-row")).length;
    const nodes = await browser.execute(() => document.querySelectorAll(".review *").length);
    // What the window thinks its page is. The body's own box decides how many
    // rows the window asks for, and a body that is not bounded by its grid row
    // measures the content it just drew instead of the viewport, which is how
    // a windowed body draws the whole diff again.
    const measured = await browser.execute(() => {
      const body = document.querySelector(".review-body");
      const panes = document.querySelector(".review-panes");
      return body === null
        ? null
        : {
            bodyHeight: body.clientHeight,
            bodyScrollHeight: body.scrollHeight,
            panesHeight: panes === null ? null : panes.clientHeight,
            drawnRows: body.querySelectorAll("[data-vrow]").length,
          };
    });
    const banners = await browser.execute(() =>
      [...document.querySelectorAll(".review-cut")].map((banner) => banner.textContent.trim()),
    );
    // The diff is past the frame's row bound, so the section says what it left
    // out rather than reading as a short diff. Two banners wear this class,
    // the section's counters and the scroll_cut note, so the assertion is on
    // the counters' own words: a regression that stopped summing the per-file
    // rows would otherwise stay green behind the other banner.
    const cut = banners.find((line) => line.startsWith("Over the frame's budget"));
    assert.ok(cut, `a diff of ${bytes} bytes framed no budget banner: ${rows} rows drawn, banners ${JSON.stringify(banners)}`);
    assert.match(cut, /^Over the frame's budget: \d+ rows(, \d+ hunks)?.* not sent$/);

    // A selection sent from the window lands on the file the frame names: the
    // click carries the path, the reducer decides, and the next frame says so.
    const already = await openFile();
    const wanted = drawn.find((path) => path !== already);
    assert.ok(wanted, `every drawn file is already the open one: ${drawn}`);
    await click(`.review-file[data-file="${wanted}"]`, 30_000);
    await browser.waitUntil(async () => (await openFile()) === wanted, {
      timeout: 30_000,
      timeoutMsg: `the frame never named ${wanted} as the open file`,
    });

    // The first figure is the whole path: the reducer projecting the section,
    // the frame crossing the channel, and the window building the DOM. This
    // second one is the window alone, with the frame already in its store: the
    // tab is left and taken again, so the components mount over a section that
    // has not changed. The gap between the two is what #1221 has to split.
    await click(".board-tab .tab-title");
    await $(".board").waitForExist({ timeout: 60_000 });
    const remountStarted = Date.now();
    await click(".review-tab .tab-title");
    await browser.waitUntil(async () => (await $$(".review-row")).length > 0, {
      timeout: 120_000,
      interval: 20,
      timeoutMsg: "the review tab drew no rows the second time",
    });
    const remountMs = Date.now() - remountStarted;

    writeFileSync(
      REPORT,
      `${JSON.stringify({ files: FILES, lines: LINES, bytes, rows, nodes, ...measured, drawnMs, remountMs }, null, 2)}\n`,
    );
    console.log(
      `review at ${bytes} bytes: ${rows} rows, ${nodes} nodes, first render ${drawnMs} ms, redraw ${remountMs} ms, cut banner ${JSON.stringify(cut)}`,
    );
    // The settings page, last, because it takes the pane over: it draws the
    // config section the window already subscribes to, and a click on a
    // category goes to the reducer and comes back in the frame, the same round
    // trip the file selection above makes.
    await click("button.settings");
    try {
      await $(".settings-page").waitForExist({ timeout: 30_000 });
    } catch (error) {
      throw new Error(`${error}; the host's last intents: ${intentsSent().slice(-6).join(", ") || "none"}`);
    }
    await browser.waitUntil(async () => (await $$(".settings-row")).length > 0, {
      timeout: 60_000,
      timeoutMsg: "the settings page drew no row of the config section",
    });
    for (const row of await $$(".settings-row")) {
      const key = await row.getAttribute("data-key");
      assert.ok(key, "a settings row was drawn with no key of the reducer's");
    }
    const categories = [];
    for (const node of await $$(".settings-node")) categories.push(await node.getAttribute("data-node"));
    const selected = await selectedNode();
    const other = categories.find((id) => id !== selected);
    assert.ok(other, `the tree drew nothing else to select: ${categories}`);
    await click(`.settings-node[data-node="${other}"] button:not(.chevron)`, 30_000);
    await browser.waitUntil(async () => (await selectedNode()) === other, {
      timeout: 30_000,
      timeoutMsg: `the frame never named ${other} as the selected category`,
    });
    await click(".settings-head .close", 30_000);

    // #1221's line, asserted rather than only recorded: the window draws the
    // rows around the reducer's offset, so neither figure and neither DOM may
    // grow with the diff. The redraw is the window alone, with the frame
    // already in the store; the first render also carries the reducer reading
    // the diff and the frame crossing the channel.
    console.log(`review window: ${JSON.stringify(measured)}`);
    assert.ok(
      nodes < 2_000,
      `the review tab built ${nodes} nodes for ${rows} rows: a DOM that grows with the diff, not with the viewport`,
    );
    // The redraw is the window alone, with the frame already in the store, so
    // it is held to #1221's line exactly.
    assert.ok(remountMs < 200, `the window redrew in ${remountMs} ms, over the 200 ms line #1221 set`);
    // The first render is not the same measurement: it also carries the
    // reducer reading a half-megabyte diff from git and the frame crossing the
    // channel, neither of which windowing touches. It is held to a stated
    // second, which still fails loudly if the whole body comes back.
    assert.ok(
      drawnMs < 1_000,
      `the first render took ${drawnMs} ms; the window draws a page, so this is the reducer's read, not the DOM`,
    );

  });
});

/** The file the frame says is open, as the window draws it. */
async function openFile() {
  const open = await $('.review-file[aria-current="true"]');
  return (await open.isExisting()) ? open.getAttribute("data-file") : null;
}
