// The Commits tab draws the reducer's commit list, says what the frame left
// out of it, and names a commit by its hash when one is clicked.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  CommitInfo,
  GitViewFrame_Serialize,
  GitViewView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import {
  commitCount,
  commitRows,
  commitsCut,
  commitWindow,
  scrollFor,
  selectCommitIntent,
} from "./commits.ts";

const commit = (n: number): CommitInfo => ({
  hash_short: `c${String(n).padStart(4, "0")}`,
  author: "Sample Dev",
  date: "2026-09-19",
  message: `commit ${n}`,
});

/** A git view section on the Commits tab carrying `commits`. */
function section(
  commits: CommitInfo[],
  selected = 0,
  over: Partial<GitViewFrame_Serialize> = {},
): GitViewView_Serialize {
  const state: GitViewFrame_Serialize = {
    active_tab: "Commits",
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
    commits,
    commits_cut: 0,
    selected_commit_index: selected,
    selected_commit_cut: false,
    review: { files: [], files_cut: 0 },
    review_ui: {
      selected_file: 0,
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

test("the rows are the frame's commits, in its order, with its selection", () => {
  const rows = commitRows(section([commit(0), commit(1), commit(2)], 1));

  assert.deepEqual(
    rows.map((row) => row.sha),
    ["c0000", "c0001", "c0002"],
  );
  assert.deepEqual(
    rows.map((row) => row.selected),
    [false, true, false],
  );
  assert.equal(rows[1]?.message, "commit 1");
  assert.equal(rows[1]?.author, "Sample Dev");
  assert.equal(rows[1]?.date, "2026-09-19");
});

test("a section that has not arrived draws no rows", () => {
  assert.deepEqual(commitRows(undefined), []);
});

test("the window is the page the selection sits in", () => {
  const rows = commitRows(section(Array.from({ length: 500 }, (_, n) => commit(n)), 300));
  const drawn = commitWindow(rows, 300, 20);

  assert.ok(drawn.length < rows.length, "the window is smaller than the list");
  assert.ok(
    drawn.some((row) => row.index === 300),
    "and it carries the commit the reducer is on",
  );
  assert.ok(drawn.every((row) => Math.abs(row.index - 300) < 100), "around it, not elsewhere");
});

test("the window at the top of the list starts at the first commit", () => {
  const rows = commitRows(section(Array.from({ length: 50 }, (_, n) => commit(n)), 0));
  assert.equal(commitWindow(rows, 0, 20)[0]?.index, 0);
});

test("a page larger than the list draws all of it", () => {
  const rows = commitRows(section([commit(0), commit(1)], 0));
  assert.equal(commitWindow(rows, 0, 40).length, 2);
});

test("the banner says how many commits were not sent", () => {
  const cut = commitsCut(section([commit(0)], 0, { commits_cut: 12 }));
  assert.equal(cut, "12 commits not sent");
});

test("the banner says when the commit the reducer is on was not sent", () => {
  const cut = commitsCut(section([commit(0)], 0, { selected_commit_cut: true }));
  assert.match(cut ?? "", /^the commit the terminal is on was not sent/);
});

test("both counters are said, because neither implies the other (#1268)", () => {
  const cut = commitsCut(
    section([commit(0)], 0, { commits_cut: 3, selected_commit_cut: true }),
  );
  assert.equal(
    cut,
    "3 commits not sent; the commit the terminal is on was not sent; this is the nearest one that was",
  );
});

test("a whole list says nothing", () => {
  assert.equal(commitsCut(section([commit(0)], 0)), undefined);
});

test("a click names the commit by the hash the frame carries", () => {
  assert.deepEqual(selectCommitIntent("c0007"), {
    Command: ["git_view.select_commit", { sha: "c0007" }],
  });
});

test("a selected row already in view does not move the list", () => {
  const box = { scrollTop: 100, clientHeight: 200 };
  assert.equal(scrollFor(box, { top: 120, height: 20 }), undefined);
  assert.equal(scrollFor(box, { top: 100, height: 20 }), undefined, "flush with the top is in view");
  assert.equal(scrollFor(box, { top: 280, height: 20 }), undefined, "and flush with the bottom");
});

test("a selected row above the view scrolls up to it, and no further", () => {
  assert.equal(scrollFor({ scrollTop: 100, clientHeight: 200 }, { top: 60, height: 20 }), 60);
});

test("a selected row below the view scrolls down by what it is short", () => {
  assert.equal(scrollFor({ scrollTop: 100, clientHeight: 200 }, { top: 310, height: 20 }), 130);
});

test("the count a row's place is given against includes what was cut", () => {
  assert.equal(commitCount(section([commit(0), commit(1)], 0, { commits_cut: 198 })), 200);
  assert.equal(commitCount(section([commit(0)], 0)), 1);
  assert.equal(commitCount(undefined), 0);
});
