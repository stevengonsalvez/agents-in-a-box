// What the review tab costs to draw at the frame's bound, measured rather than
// assumed: the window has no virtualisation anywhere, every list is a plain
// `<For>` under `overflow-y: auto`, and the bound allows 4,000 rows across the
// section (`ainb-app/src/wire/git_view.rs`, MAX_ROWS_TOTAL) at 2,000
// characters each.
//
// This is a server render, so it is the component tree's own work without a
// browser's layout or paint: a floor for what the window does, not a ceiling.
// The number a real window takes, on these 16,094 nodes and 8.7 MB of text, is
// #1221, and windowing the rows against `review_ui.scroll` is its call to
// make. The assertion here is deliberately generous; what it catches is the
// tab growing a cost per row that is not linear, not a slow runner.

import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import solid from "vite-plugin-solid";
import type {
  DiffRow_Serialize,
  GitViewView_Serialize,
  ReviewFileFrame_Serialize,
} from "../../../ainb-app/bindings/AppState";

/** The bound the host frames to: rows across the section, characters a row. */
const MAX_ROWS_TOTAL = 4_000;
const MAX_ROWS_PER_FILE = 400;
const MAX_LINE_CHARS = 2_000;

/** A section at the bound: ten files of four hundred rows, each row full. */
function atTheBound(): GitViewView_Serialize {
  const text = "x".repeat(MAX_LINE_CHARS);
  const files: ReviewFileFrame_Serialize[] = [];
  for (let file = 0; file < MAX_ROWS_TOTAL / MAX_ROWS_PER_FILE; file += 1) {
    const rows: DiffRow_Serialize[] = [];
    for (let row = 0; row < MAX_ROWS_PER_FILE; row += 1) {
      rows.push({
        kind: row % 3 === 0 ? "Added" : row % 3 === 1 ? "Removed" : "Context",
        old_lineno: row,
        new_lineno: row,
        raw: text,
        emphasis: [],
      } as DiffRow_Serialize);
    }
    files.push({
      path: `src/module${file}/file${file}.rs`,
      status: "Modified",
      insertions: MAX_ROWS_PER_FILE,
      deletions: 0,
      language: "rust",
      collapsed: false,
      binary: false,
      hunks: [
        {
          old_start: 1,
          new_start: 1,
          gap_before: 0,
          gap_after: 0,
          expanded_before: 0,
          expanded_after: 0,
          rows,
        },
      ],
      rows_cut: 0,
      hunks_cut: 0,
    } as ReviewFileFrame_Serialize);
  }
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
        selected_file: 0,
        sidebar_selected: 0,
        collapsed_dirs: [],
        collapsed_dirs_cut: 0,
        scroll: 0,
        current_hunk: 0,
      },
    },
    quick_commit_message_len: null,
    quick_commit_cursor: 0,
    is_current_dir_git_repo: true,
  } as unknown as GitViewView_Serialize;
}

test("the review tab at the frame's bound, measured", async () => {
  const server = await createServer({
    configFile: false,
    root: fileURLToPath(new URL("..", import.meta.url)),
    plugins: [solid({ ssr: true })],
    server: { middlewareMode: true, hmr: false },
    appType: "custom",
    ssr: { noExternal: ["solid-js"] },
    logLevel: "silent",
  });
  try {
    const { Review } = await server.ssrLoadModule("/src/review.tsx");
    const { renderToString } = await server.ssrLoadModule("solid-js/web");
    const gitView = atTheBound();

    // Once to warm the module and the JIT, then the measurement.
    renderToString(() => Review({ gitView, stale: false, onChoose() {} }));
    const start = performance.now();
    const html: string = renderToString(() => Review({ gitView, stale: false, onChoose() {} }));
    const took = performance.now() - start;

    const elements = (html.match(/<[a-z]/g) ?? []).length;
    const rows = (html.match(/class="review-row"/g) ?? []).length;
    console.log(
      `review at the bound: ${rows} rows, ${elements} elements, ${html.length} bytes, first render ${took.toFixed(0)} ms`,
    );

    assert.equal(rows, MAX_ROWS_TOTAL, "every row the frame allows is drawn");
    assert.ok(
      took < 5_000,
      `the bound rendered in ${took.toFixed(0)} ms, which is a cost per row that is no longer linear`,
    );
  } finally {
    await server.close();
  }
});
