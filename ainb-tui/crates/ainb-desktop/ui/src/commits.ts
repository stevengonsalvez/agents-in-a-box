// What the Commits tab draws, projected from the framed git view, and the two
// intents it sends back.
//
// Every row is the reducer's own (`ainb-app/src/components/git_view.rs:41`,
// `CommitInfo`): the webview runs no git, reads no log and mints no row. A
// click names the commit's SHORT HASH, which is the id the frame carries
// (`ainb-app/src/app/pointer.rs`, `git_view.select_commit`), never an index:
// the list is cut to a byte budget on the wire and can move under a click.
//
// There is no commit scroll offset in the model. The terminal keeps the
// selected commit in view with ratatui's own list state, so the SELECTION is
// the scroll (`components/git_view.rs:258-276`), and this window does the
// same: the page it draws is the one the selection sits in.

import type { CommitInfo, GitViewView_Serialize } from "../../../ainb-app/bindings/AppState";
import { gitView, MAX_PAGE_ROWS, OVERSCAN } from "./review.ts";
import type { RendererIntent } from "./tabs.ts";

/** The command a click on a commit is sent as, `ids::GIT_VIEW_SELECT_COMMIT`. */
export const SELECT_COMMIT = "git_view.select_commit";

/** One row of the commit list, as the frame carries it. */
export interface CommitRow {
  /** `CommitInfo::hash_short`: what a click names and what keys the row. */
  sha: string;
  message: string;
  author: string;
  date: string;
  /** The commit the reducer is on. */
  selected: boolean;
  /** Where this row sits in the whole list, for a reader and for the window. */
  index: number;
}

/** Every commit the frame carries, in the reducer's order. */
export function commitRows(section: GitViewView_Serialize | undefined): CommitRow[] {
  const view = gitView(section);
  if (view === undefined) return [];
  return view.commits.map((commit: CommitInfo, index: number) => ({
    sha: commit.hash_short,
    message: commit.message,
    author: commit.author,
    date: commit.date,
    selected: index === view.selected_commit_index,
    index,
  }));
}

/**
 * The rows this window draws: the page the selection sits in, plus an
 * overscan on each side.
 *
 * `rowsPerPage` is the caller's measurement of its own box, held under the
 * same cap the review body uses, so a box that measures its content cannot
 * ask for the whole list (#1221).
 */
export function commitWindow(rows: CommitRow[], selected: number, rowsPerPage: number): CommitRow[] {
  const page = Math.min(Math.max(rowsPerPage, 1), MAX_PAGE_ROWS);
  const first = Math.max(Math.min(selected - Math.floor(page / 2), rows.length - page), 0);
  return rows.slice(Math.max(first - OVERSCAN, 0), first + page + OVERSCAN);
}

/**
 * What the frame left out of the commit list, or undefined when it carries
 * all of it.
 *
 * Two different things are said here, and a reader needs both: how many
 * commits were not sent (`commits_cut`), and whether the commit the reducer is
 * actually on is one of the ones that were not (`selected_commit_cut`, #1268).
 * The second is not implied by the first: a list cut at the end still shows
 * the right selection, and a selection past the cut does not.
 */
export function commitsCut(section: GitViewView_Serialize | undefined): string | undefined {
  const view = gitView(section);
  if (view === undefined) return undefined;
  const parts: string[] = [];
  if (view.commits_cut > 0) parts.push(`${view.commits_cut} commits not sent`);
  if (view.selected_commit_cut) {
    parts.push("the commit the terminal is on was not sent; this is the nearest one that was");
  }
  return parts.length === 0 ? undefined : parts.join("; ");
}

/** A click on the commit `sha`: the reducer decides, and the frame says so. */
export function selectCommitIntent(sha: string): RendererIntent {
  return { Command: [SELECT_COMMIT, { sha }] };
}
