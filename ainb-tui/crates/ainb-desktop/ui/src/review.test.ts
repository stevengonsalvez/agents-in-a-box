// The review tab draws the reducer's rows and says what the frame left out.

import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import solid from "vite-plugin-solid";
import type {
  DiffRow_Serialize,
  GitViewView_Serialize,
  HunkFrame_Serialize,
  ReviewFileFrame_Serialize,
} from "../../../ainb-app/bindings/AppState";
import {
  bodyLines,
  fileCut,
  fileRows,
  openFile,
  scrollIntent,
  sectionCut,
  selectFileIntent,
} from "./review.ts";

const row = (n: number, text: string): DiffRow_Serialize =>
  ({ kind: "Added", old_lineno: null, new_lineno: n, raw: text, emphasis: [] }) as DiffRow_Serialize;

const hunk = (start: number, rows: DiffRow_Serialize[], gap = 0): HunkFrame_Serialize =>
  ({
    old_start: start,
    new_start: start,
    gap_before: gap,
    gap_after: 0,
    expanded_before: 0,
    expanded_after: 0,
    rows,
  }) as HunkFrame_Serialize;

const file = (
  path: string,
  hunks: HunkFrame_Serialize[],
  over: Partial<ReviewFileFrame_Serialize> = {},
): ReviewFileFrame_Serialize =>
  ({
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
  }) as ReviewFileFrame_Serialize;

/** A git view section carrying `files`, with `selected` open. */
function section(
  files: ReviewFileFrame_Serialize[],
  selected = 0,
  over: Record<string, number> = {},
): GitViewView_Serialize {
  return {
    git_view_state: {
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
        scroll: 0,
        current_hunk: 0,
      },
      ...over,
    },
    quick_commit_message_len: null,
    quick_commit_cursor: 0,
    is_current_dir_git_repo: true,
  } as unknown as GitViewView_Serialize;
}

test("a hunk renders its rows, under a header naming the lines it skipped", () => {
  const body = section([file("src/lib.rs", [hunk(10, [row(10, "one"), row(11, "two")], 8)])]);

  const lines = bodyLines(openFile(body));

  assert.deepEqual(
    lines.map((line) => line.kind),
    ["hunk", "row", "row"],
  );
  assert.equal(lines[0].kind === "hunk" && lines[0].header, "@@ -10 +10 @@");
  assert.equal(lines[0].kind === "hunk" && lines[0].hidden, 8);
  assert.deepEqual(
    lines.flatMap((line) => (line.kind === "row" ? [line.row.raw] : [])),
    ["one", "two"],
  );
});

test("a file with no hunks draws no body at all", () => {
  const body = section([file("assets/logo.png", [], { binary: true })]);

  assert.deepEqual(bodyLines(openFile(body)), []);
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

  const drawn = bodyLines(openFile(body)).flatMap((line) => (line.kind === "row" ? [line.row] : []));

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
      file("README.md", [hunk(1, [row(1, "not drawn")])]),
    ],
    0,
  );

  const html = await rendered({ gitView: body, stale: false, onChoose() {} });

  const drawn = [...html.matchAll(/data-file="([^"]+)"/g)].map((match) => match[1]);
  assert.deepEqual(drawn, ["src/lib.rs", "README.md"], html);
  assert.equal(html.match(/aria-current="true"/g)?.length, 1, html);
  assert.match(html, /kept one/, html);
  assert.match(html, /kept two/, html);
  assert.doesNotMatch(html, /not drawn/, "only the open file's rows are drawn");
});

test("a withheld section says so rather than drawing a stale diff silently", async () => {
  const body = section([file("src/lib.rs", [hunk(1, [row(1, "last one that fitted")])])]);

  const html = await rendered({ gitView: body, stale: true, onChoose() {} });

  assert.match(html, /review-withheld/, html);
  assert.match(html, /too large to send/, html);
});

test("a binary file says why its body is empty", async () => {
  const body = section([file("assets/logo.png", [], { binary: true })]);

  const html = await rendered({ gitView: body, stale: false, onChoose() {} });

  assert.match(html, /Binary file/, html);
});

test("a section that never arrived draws its own loading line", async () => {
  const html = await rendered({ gitView: undefined, stale: false, onChoose() {} });

  assert.match(html, /Loading the review/, html);
});
