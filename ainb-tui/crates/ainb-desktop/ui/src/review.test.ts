// The review tab draws the reducer's rows and says what the frame left out.

import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import solid from "vite-plugin-solid";
import type {
  DiffRow_Serialize,
  GitViewFrame_Serialize,
  GitViewView_Serialize,
  HunkFrame_Serialize,
  ReviewFileFrame_Serialize,
} from "../../../ainb-app/bindings/AppState";
import {
  bodyLines,
  fileCut,
  fileRows,
  keyRows,
  ROW_PX,
  scrollIntent,
  sectionCut,
  segments,
  selectFileIntent,
  wheelRows,
  windowLines,
} from "./review.ts";

const row = (n: number, text: string): DiffRow_Serialize => ({
  kind: "Added",
  old_lineno: null,
  new_lineno: n,
  raw: text,
  emphasis: [],
});

const hunk = (start: number, rows: DiffRow_Serialize[], gap = 0): HunkFrame_Serialize => ({
  old_start: start,
  new_start: start,
  gap_before: gap,
  gap_after: 0,
  expanded_before: 0,
  expanded_after: 0,
  rows,
});

const file = (
  path: string,
  hunks: HunkFrame_Serialize[],
  over: Partial<ReviewFileFrame_Serialize> = {},
): ReviewFileFrame_Serialize => ({
  path,
  status: "Modified",
  insertions: 1,
  deletions: 0,
  language: null,
  collapsed: false,
  binary: false,
  hunks,
  rows_cut: 0,
  hunks_cut: 0,
  ...over,
});

/** A git view section carrying `files`, with `selected` open. */
function section(
  files: ReviewFileFrame_Serialize[],
  selected = 0,
  over: Partial<GitViewFrame_Serialize> = {},
): GitViewView_Serialize {
  const state: GitViewFrame_Serialize = {
    active_tab: "Review",
    changed_files: [],
    files_cut: 0,
    selected_file_index: 0,
    diff_content: [],
    diff_lines_cut: 0,
    diff_scroll_offset: 0,
    worktree_name: "repo",
    is_dirty: true,
    can_push: false,
    commit_message_len: null,
    commit_message_cursor: 0,
    expanded_folders: [],
    expanded_folders_cut: 0,
    file_tree_items: [],
    tree_items_cut: 0,
    selected_tree_index: 0,
    markdown_content: [],
    markdown_lines_cut: 0,
    markdown_scroll_offset: 0,
    commits: [],
    commits_cut: 0,
    selected_commit_index: 0,
    selected_commit_cut: false,
    review: { files, files_cut: 0 },
    review_ui: {
      selected_file: selected,
      sidebar_selected: 0,
      collapsed_dirs: [],
      collapsed_dirs_cut: 0,
      scroll: 0,
      scroll_cut: false,
      current_hunk: 0,
    },
    ...over,
  };
  return {
    git_view_state: state,
    quick_commit_message_len: null,
    quick_commit_cursor: 0,
    is_current_dir_git_repo: true,
  };
}

test("a hunk renders its rows, under a header naming the lines it skipped", () => {
  const body = section([file("src/lib.rs", [hunk(10, [row(10, "one"), row(11, "two")], 8)])]);

  const lines = bodyLines(body);

  assert.deepEqual(
    lines.map((line) => line.kind),
    ["file", "hunk", "expand", "row", "row"],
  );
  assert.equal(lines[1].kind === "hunk" && lines[1].header, "@@ -10 +10 @@");
  assert.equal(lines[2].kind === "expand" && lines[2].hidden, 8);
  assert.deepEqual(
    lines.flatMap((line) => (line.kind === "row" ? [line.row.raw] : [])),
    ["one", "two"],
  );
});

test("a file with no hunks is still named, with nothing under it", () => {
  const body = section([file("assets/logo.png", [], { binary: true })]);

  const lines = bodyLines(body);
  assert.deepEqual(
    lines.map((line) => line.kind),
    ["file"],
    "the file is drawn; it simply has no hunks",
  );
  assert.equal(lines[0].kind === "file" && lines[0].file.binary, true);
  assert.equal(lines[0].kind === "file" && lines[0].file.path, "assets/logo.png");
});

test("the open file is the one the reducer says is open", () => {
  const body = section([file("a.rs", []), file("b.rs", [])], 1);

  assert.deepEqual(
    fileRows(body).map((file) => [file.path, file.open]),
    [
      ["a.rs", false],
      ["b.rs", true],
    ],
  );
});

test("a selection names the path, never the index", () => {
  assert.deepEqual(selectFileIntent("src/deep/lib.rs"), {
    Command: ["git_view.select_review_row", { target: { file: "src/deep/lib.rs" } }],
  });
  assert.deepEqual(scrollIntent(-3), { Command: ["git_view.scroll", { lines: -3 }] });
});

test("every row drawn came from the frame, and none was minted here", () => {
  const rows = [row(1, "kept"), row(2, "also kept")];
  const body = section([file("src/lib.rs", [hunk(1, rows)])]);

  const drawn = bodyLines(body).flatMap((line) => (line.kind === "row" ? [line.row] : []));

  assert.equal(drawn.length, rows.length);
  for (const [index, row] of drawn.entries()) {
    assert.equal(row, rows[index], "the row object is the frame's own, not a copy built here");
  }
});

test("a cut file and a short file do not read the same", () => {
  const cut = file("src/lib.rs", [hunk(1, [row(1, "one")])], { rows_cut: 320, hunks_cut: 4 });

  assert.equal(fileCut(cut), "320 rows and 4 hunks not sent");
  assert.equal(fileCut(file("src/small.rs", [hunk(1, [row(1, "one")])])), undefined);
});

test("the section says what the budget cost it, or says nothing at all", () => {
  const whole = section([file("a.rs", [])]);
  assert.equal(sectionCut(whole), undefined);

  const shortened = section([file("a.rs", [])], 0, { files_cut: 12, diff_lines_cut: 900 });
  assert.equal(
    sectionCut(shortened),
    "Over the frame's budget: 12 changed paths, 900 diff lines not sent",
  );
});

test("rows the files lost are said at the top, not only file by file", () => {
  // The shape a real repository hits first: every file framed, each cut to the
  // per-file cap, nothing else cut at all. Without the sum the section says
  // nothing and a diff missing thousands of rows reads as a short one.
  const cut = section([
    file("a.rs", [hunk(1, [row(1, "one")])], { rows_cut: 3_600, hunks_cut: 2 }),
    file("b.rs", [hunk(1, [row(1, "two")])], { rows_cut: 1_400 }),
  ]);

  assert.equal(sectionCut(cut), "Over the frame's budget: 5000 rows, 2 hunks not sent");
});

test("a section that has not arrived draws nothing rather than throwing", () => {
  assert.deepEqual(fileRows(undefined), []);
  assert.deepEqual(bodyLines(undefined), []);
  assert.equal(sectionCut(undefined), undefined);
});

// The tests below render for real, through Vite's SSR loader and the Solid
// plugin, so a row invented in `review.tsx` rather than taken from the frame
// fails here.

/** Server-render `Review` with `props` and return its HTML. */
async function rendered(props: Record<string, unknown>): Promise<string> {
  const server = await createServer({
    configFile: false,
    root: fileURLToPath(new URL("..", import.meta.url)),
    plugins: [solid({ ssr: true })],
    server: { middlewareMode: true, hmr: false },
    appType: "custom",
    // As the sidebar's test: Vite resolves Solid itself, so the component and
    // `renderToString` share one server build.
    ssr: { noExternal: ["solid-js"] },
    logLevel: "silent",
  });
  try {
    const { Review } = await server.ssrLoadModule("/src/review.tsx");
    const { renderToString } = await server.ssrLoadModule("solid-js/web");
    return renderToString(() => Review(props));
  } finally {
    await server.close();
  }
}

test("the tab draws the frame's files and the open file's rows", async () => {
  const body = section(
    [
      file("src/lib.rs", [hunk(1, [row(1, "kept one"), row(2, "kept two")])]),
      file("README.md", [hunk(1, [row(1, "also drawn")])]),
    ],
    0,
  );

  const html = await rendered({ gitView: body, stale: false, onChoose() {} });

  const drawn = [...html.matchAll(/data-file="([^"]+)"/g)].map((match) => match[1]);
  assert.deepEqual(drawn, ["src/lib.rs", "README.md"], html);
  assert.equal(html.match(/aria-current="true"/g)?.length, 1, html);
  assert.match(html, /kept one/, html);
  assert.match(html, /kept two/, html);
  // The body runs over every file, because the reducer's scroll offset and
  // hunk cursor count across the whole review, not within the open file.
  assert.match(html, /also drawn/, html);
});

test("a withheld section says so rather than drawing a stale diff silently", async () => {
  const body = section([file("src/lib.rs", [hunk(1, [row(1, "last one that fitted")])])]);

  const html = await rendered({ gitView: body, stale: true, onChoose() {} });

  assert.match(html, /review-withheld/, html);
  assert.match(html, /too large to send/, html);
});

test("a binary file says why it has nothing under its heading", async () => {
  const body = section([file("assets/logo.png", [], { binary: true })]);

  const html = await rendered({ gitView: body, stale: false, onChoose() {} });

  assert.match(html, /binary, nothing to show/, html);
});

test("a section that never arrived draws its own loading line", async () => {
  const html = await rendered({ gitView: undefined, stale: false, onChoose() {} });

  assert.match(html, /Loading the review/, html);
});

test("the body's virtual rows are the reducer's own", () => {
  const body = section([
    file("src/lib.rs", [hunk(1, [row(1, "one"), row(2, "two")], 4)]),
    file("assets/logo.png", [], { binary: true }),
    file("src/quiet.rs", [hunk(1, [row(1, "three")])], { collapsed: true }),
    file("src/last.rs", [hunk(1, [row(1, "four")])]),
  ]);

  const lines = bodyLines(body);
  const numbered = lines.filter((line) => line.index !== undefined);

  // `flatten` (components/code_review/render.rs:112) counts a row per file
  // heading, a row per hidden gap and a row per code line, and gives a
  // collapsed or binary file nothing beyond its heading. The `@@` header is
  // this window's own decoration and carries no index.
  assert.deepEqual(
    numbered.map((line) => line.kind),
    ["file", "expand", "row", "row", "file", "file", "file", "row"],
  );
  assert.deepEqual(
    numbered.map((line) => line.index),
    [0, 1, 2, 3, 4, 5, 6, 7],
  );
  assert.ok(
    lines.some((line) => line.kind === "hunk" && line.index === undefined),
    "the hunk header is drawn but not counted",
  );
});

test("many small wheel deltas add up to whole rows, and lines are rows", () => {
  // A trackpad's deltas: each one truncates to nothing on its own.
  let pending = 0;
  let sent = 0;
  for (let tick = 0; tick < 10; tick += 1) {
    const step = wheelRows(pending, { deltaY: ROW_PX / 5, deltaMode: 0 }, 30);
    pending = step.pending;
    sent += step.rows;
  }
  assert.equal(sent, 2, "ten tenths of a fifth of a row is two rows");

  // A line-mode mouse: three lines is three rows, whatever a row is in pixels.
  assert.deepEqual(wheelRows(0, { deltaY: 3, deltaMode: 1 }, 30), { rows: 3, pending: 0 });
  // And up is up.
  assert.equal(wheelRows(0, { deltaY: -3, deltaMode: 1 }, 30).rows, -3);
  // A page is what the body can show, not a number picked in the code.
  assert.equal(wheelRows(0, { deltaY: 1, deltaMode: 2 }, 30).rows, 30);
});

test("the row a frame's scroll names is the row the reducer means", async () => {
  const body = section(
    [
      file("src/lib.rs", [hunk(1, [row(1, "first"), row(2, "second")])]),
      file("src/next.rs", [hunk(1, [row(1, "third")])]),
    ],
    0,
    {
      review_ui: {
        selected_file: 0,
        sidebar_selected: 0,
        collapsed_dirs: [],
        collapsed_dirs_cut: 0,
        scroll: 3,
        scroll_cut: false,
        current_hunk: 1,
      },
    },
  );

  const html = await rendered({ gitView: body, stale: false, onChoose() {} });

  // 0 the first file's heading, 1 and 2 its rows, 3 the second file's heading.
  const rows = [...html.matchAll(/data-vrow="(\d+)"[^>]*>(.*?)<\/p>/gs)].map((match) => [
    match[1],
    match[2].replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim(),
  ]);
  assert.deepEqual(
    rows.map(([index]) => index),
    ["0", "1", "2", "3", "4"],
    html,
  );
  assert.match(rows[3][1], /src\/next\.rs/, "row 3 is the second file's heading");
  assert.match(rows[1][1], /first/, "and row 1 is the first file's first line");
});

test("the banners say what the frame left out", async () => {
  const body = section(
    [file("src/lib.rs", [hunk(1, [row(1, "one")])], { rows_cut: 40, hunks_cut: 2 })],
    0,
    { files_cut: 7, diff_lines_cut: 120, commits_cut: 3 },
  );

  const html = await rendered({ gitView: body, stale: true, onChoose() {} });
  const drawn = html.replace(/<[^>]*>/g, " ").replace(/\s+/g, " ");

  assert.match(drawn, /too large to send/, "the withheld banner");
  assert.match(drawn, /7 changed paths, 120 diff lines, 3 commits not sent/, drawn);
  assert.match(drawn, /40 rows and 2 hunks not sent/, "the file says what it lost");
});

test("emphasis ranges are byte offsets, and the window draws them where they are", () => {
  const plain = { ...row(1, "let token = value;"), emphasis: [[4, 9] as [number, number]] };
  assert.deepEqual(segments(plain), [
    { text: "let ", emphasis: false },
    { text: "token", emphasis: true },
    { text: " = value;", emphasis: false },
  ]);

  // "héllo" is six BYTES and five UTF-16 units: a window that sliced by the
  // byte offsets would cut a character short.
  const accented = { ...row(1, "héllo world"), emphasis: [[0, 6] as [number, number]] };
  assert.deepEqual(segments(accented), [
    { text: "héllo", emphasis: true },
    { text: " world", emphasis: false },
  ]);

  // An emoji is four bytes and two units.
  const emoji = { ...row(1, "🔐 key"), emphasis: [[0, 4] as [number, number]] };
  assert.deepEqual(segments(emoji), [
    { text: "🔐", emphasis: true },
    { text: " key", emphasis: false },
  ]);

  // The common case: the projection drops the ranges whenever it changed the
  // text, so most rows are one run and no work.
  assert.deepEqual(segments(row(1, "untouched")), [{ text: "untouched", emphasis: false }]);
  // And a range past the end of the text takes what there is, not a crash.
  const over = { ...row(1, "short"), emphasis: [[2, 99] as [number, number]] };
  assert.deepEqual(segments(over), [
    { text: "sh", emphasis: false },
    { text: "ort", emphasis: true },
  ]);
});

test("the keys the terminal answers move the reducer's offset, not the body", () => {
  // Thirty rows in view.
  assert.equal(keyRows("ArrowDown", 30), 1);
  assert.equal(keyRows("ArrowUp", 30), -1);
  assert.equal(keyRows("PageDown", 30), 30);
  assert.equal(keyRows("PageUp", 30), -30);
  // Home and End ask for an end, not a distance: this window counts the
  // FRAME's rows and the reducer applies the delta to the MODEL's, so any
  // arithmetic from a frame offset would be in the wrong space. The reducer
  // saturates at both ends.
  assert.ok(keyRows("Home", 30)! < -1_000_000, "Home asks for the top, whatever is above");
  assert.ok(keyRows("End", 30)! > 1_000_000, "End asks for the bottom");
  assert.equal(keyRows("a", 30), null, "an ordinary key is not ours");
});

test("the body takes focus and refuses the browser's own scrolling", async () => {
  const body = section([file("src/lib.rs", [hunk(1, [row(1, "one"), row(2, "two")])])]);

  const html = await rendered({ gitView: body, stale: false, onChoose() {} });

  assert.match(html, /class="review-body"[^>]*tabindex="0"/, html);
  assert.match(html, /role="region"/, html);
});

test("a frame whose row was cut says so", async () => {
  const body = section([file("src/lib.rs", [hunk(1, [row(1, "one")])])], 0, {
    review_ui: {
      selected_file: 0,
      sidebar_selected: 0,
      collapsed_dirs: [],
      collapsed_dirs_cut: 0,
      scroll: 1,
      scroll_cut: true,
      current_hunk: 0,
    },
  });

  const html = await rendered({ gitView: body, stale: false, onChoose() {} });

  assert.match(html.replace(/<[^>]*>/g, " "), /was not sent; this is the nearest one that was/, html);
});

test("emphasis ranges out of order are drawn, not dropped", () => {
  const jumbled = {
    ...row(1, "alpha beta gamma"),
    emphasis: [
      [11, 16] as [number, number],
      [0, 5] as [number, number],
    ],
  };

  assert.deepEqual(segments(jumbled), [
    { text: "alpha", emphasis: true },
    { text: " beta ", emphasis: false },
    { text: "gamma", emphasis: true },
  ]);
});

test("the highlighted hunk is the one the frame's cursor names, past a collapsed file", async () => {
  const body = section(
    [
      file("src/quiet.rs", [hunk(1, [row(1, "hidden one")]), hunk(9, [row(9, "hidden two")])], {
        collapsed: true,
      }),
      file("src/open.rs", [hunk(1, [row(1, "first")]), hunk(20, [row(20, "second")])]),
    ],
    1,
    {
      review_ui: {
        selected_file: 1,
        sidebar_selected: 0,
        collapsed_dirs: [],
        collapsed_dirs_cut: 0,
        scroll: 0,
        scroll_cut: false,
        current_hunk: 1,
      },
    },
  );

  const html = await rendered({ gitView: body, stale: false, onChoose() {} });

  // A collapsed file draws its heading and none of its hunks, so the cursor's
  // hunk 1 is the open file's SECOND hunk, and only that line is current.
  const hunks = [...html.matchAll(/class="review-hunk([^"]*)"[^>]*>([^<]*)/g)].map((match) => [
    match[1].includes("current"),
    match[2].trim(),
  ]);
  assert.deepEqual(hunks, [
    [false, "@@ -1 +1 @@"],
    [true, "@@ -20 +20 @@"],
  ], html);
});

test("the window draws the viewport's rows and an overscan on each side", () => {
  const rows = Array.from({ length: 300 }, (_, n) => row(n, `line ${n}`));
  const lines = bodyLines(section([file("a.rs", [hunk(1, rows)])]));
  const drawn = windowLines(lines, 100, 40, 10);
  const indexes = drawn.map((line) => line.index).filter((index) => index !== undefined);
  assert.equal(indexes[0], 90, "the window opens an overscan above the reducer's offset");
  assert.equal(indexes.at(-1), 149, "and closes an overscan below the last row on screen");
  assert.ok(drawn.length < lines.length, "the window is smaller than the body");
});

test("a window that opens inside a hunk still carries its header", () => {
  const rows = Array.from({ length: 200 }, (_, n) => row(n, `line ${n}`));
  const lines = bodyLines(section([file("a.rs", [hunk(1, rows)])]));
  const drawn = windowLines(lines, 100, 20, 0);
  assert.equal(drawn[0]?.kind, "hunk", "the header of the hunk the window opens in is drawn");
  assert.equal(drawn[1]?.index, 100, "and the first row is the reducer's own offset");
});

test("the window at the top of the body starts at the first line", () => {
  const rows = Array.from({ length: 50 }, (_, n) => row(n, `line ${n}`));
  const lines = bodyLines(section([file("a.rs", [hunk(1, rows)])]));
  const drawn = windowLines(lines, 0, 10, 20);
  assert.equal(drawn[0]?.index, 0, "there is nothing above row zero to overscan into");
  assert.deepEqual(drawn[0]?.kind, "file", "and the file heading is that row");
});
