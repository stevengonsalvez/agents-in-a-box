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
  openFile,
  ROW_PX,
  scrollIntent,
  sectionCut,
  selectFileIntent,
  wheelRows,
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
    worktree_path: "/repo",
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
    review: { files, files_cut: 0 },
    review_ui: {
      selected_file: selected,
      sidebar_selected: 0,
      collapsed_dirs: [],
      collapsed_dirs_cut: 0,
      scroll: 0,
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
  assert.equal(openFile(body)?.path, "assets/logo.png");
});

test("the open file is the one the reducer says is open", () => {
  const body = section([file("a.rs", []), file("b.rs", [])], 1);

  assert.equal(openFile(body)?.path, "b.rs");
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

test("a section that has not arrived draws nothing rather than throwing", () => {
  assert.deepEqual(fileRows(undefined), []);
  assert.equal(openFile(undefined), undefined);
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
    const step = wheelRows(pending, { deltaY: ROW_PX / 5, deltaMode: 0 });
    pending = step.pending;
    sent += step.rows;
  }
  assert.equal(sent, 2, "ten tenths of a fifth of a row is two rows");

  // A line-mode mouse: three lines is three rows, whatever a row is in pixels.
  assert.deepEqual(wheelRows(0, { deltaY: 3, deltaMode: 1 }), { rows: 3, pending: 0 });
  // And up is up.
  assert.equal(wheelRows(0, { deltaY: -3, deltaMode: 1 }).rows, -3);
});

test("the row a frame's scroll names is the row the reducer means", async () => {
  const body = section(
    [
      file("src/lib.rs", [hunk(1, [row(1, "first"), row(2, "second")])]),
      file("src/next.rs", [hunk(1, [row(1, "third")])]),
    ],
    0,
    { review_ui: { selected_file: 0, sidebar_selected: 0, collapsed_dirs: [], collapsed_dirs_cut: 0, scroll: 3, current_hunk: 1 } },
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
