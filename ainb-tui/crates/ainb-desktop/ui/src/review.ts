// What the review tab draws, projected from the framed git view, and the two
// intents it sends back.
//
// Every row here comes from the reducer's own model
// (`ainb-app/src/components/code_review/model.rs`): the webview splits no
// diff, parses no hunk and mints no row id. A click names the PATH it hit
// (`ainb-app/src/app/pointer.rs:201`), so a click resolved against one frame
// acts on the same file after the tree moved, and a click on a file that is
// gone does nothing.
//
// The frame is bounded (`ainb-app/src/wire/git_view.rs`), so what arrives is a
// window with counters saying what it left out. Those counters are drawn, not
// swallowed: a cut diff and a short diff must not read the same.

import type {
  DiffRow_Serialize,
  GitViewView_Serialize,
  HunkFrame_Serialize,
  ReviewFileFrame_Serialize,
} from "../../../ainb-app/bindings/AppState";
import type { RendererIntent } from "./tabs.ts";

/** One row of the sidebar's file list. */
export interface FileRow {
  path: string;
  status: string;
  insertions: number;
  deletions: number;
  /** Whether the reducer has this file open. */
  open: boolean;
  /** What the frame left out of this file, drawn as a note under it. */
  cut: string | undefined;
}

/** One line of the diff body: a hunk's header, or one of its rows. */
export type BodyLine =
  | { kind: "hunk"; key: string; header: string; hidden: number }
  | { kind: "row"; key: string; row: DiffRow_Serialize };

/** The framed git view, or undefined when the section has not arrived. */
export function gitView(section: GitViewView_Serialize | undefined) {
  return section?.git_view_state ?? undefined;
}

/** The sidebar's rows, in the frame's own order. */
export function fileRows(section: GitViewView_Serialize | undefined): FileRow[] {
  const view = gitView(section);
  if (view === undefined) return [];
  return view.review.files.map((file, index) => ({
    path: file.path,
    status: file.status,
    insertions: file.insertions,
    deletions: file.deletions,
    open: index === view.review_ui.selected_file,
    cut: fileCut(file),
  }));
}

/** The file the reducer has open, if the frame carries one. */
export function openFile(
  section: GitViewView_Serialize | undefined,
): ReviewFileFrame_Serialize | undefined {
  const view = gitView(section);
  return view?.review.files[view.review_ui.selected_file];
}

/**
 * The open file's body: every hunk's header followed by its rows.
 *
 * The header says what the hunk skipped (`gap_before`), because a diff drawn
 * without its gaps reads as a file with nothing between its changes.
 */
export function bodyLines(file: ReviewFileFrame_Serialize | undefined): BodyLine[] {
  if (file === undefined) return [];
  const lines: BodyLine[] = [];
  file.hunks.forEach((hunk, index) => {
    lines.push({
      kind: "hunk",
      key: `${file.path}:hunk:${index}`,
      header: hunkHeader(hunk),
      hidden: hunk.gap_before - hunk.expanded_before,
    });
    hunk.rows.forEach((row, rowIndex) => {
      lines.push({ kind: "row", key: `${file.path}:${index}:${rowIndex}`, row });
    });
  });
  return lines;
}

/** `@@ -old +new @@`, as a diff names a hunk. */
export function hunkHeader(hunk: HunkFrame_Serialize): string {
  return `@@ -${hunk.old_start} +${hunk.new_start} @@`;
}

/** What a file says it left out, or undefined when it carries all of itself. */
export function fileCut(file: ReviewFileFrame_Serialize): string | undefined {
  const parts: string[] = [];
  if (file.rows_cut > 0) parts.push(`${file.rows_cut} rows`);
  if (file.hunks_cut > 0) parts.push(`${file.hunks_cut} hunks`);
  return parts.length === 0 ? undefined : `${parts.join(" and ")} not sent`;
}

/**
 * What the whole section left out, as one line, or undefined when it carries
 * everything.
 *
 * The counters are the frame's own: a bounded projection degrades and says so
 * rather than being withheld whole.
 */
export function sectionCut(section: GitViewView_Serialize | undefined): string | undefined {
  const view = gitView(section);
  if (view === undefined) return undefined;
  const parts: string[] = [];
  if (view.review.files_cut > 0) parts.push(`${view.review.files_cut} files`);
  if (view.files_cut > 0) parts.push(`${view.files_cut} changed paths`);
  if (view.diff_lines_cut > 0) parts.push(`${view.diff_lines_cut} diff lines`);
  if (view.commits_cut > 0) parts.push(`${view.commits_cut} commits`);
  return parts.length === 0 ? undefined : `Over the frame's budget: ${parts.join(", ")} not sent`;
}

/**
 * What the tab says when the section was withheld whole.
 *
 * The store keeps the body it last held (`store.ts:148`), so the tab goes on
 * drawing a diff the host has already moved past; saying which section and why
 * is the difference between stale and silently stale.
 */
export const WITHHELD =
  "The git view was too large to send, so this is the last diff that fitted.";

/** Select the file at `path`, by path rather than by index. */
export function selectFileIntent(path: string): RendererIntent {
  return { Command: ["git_view.select_review_row", { target: { file: path } }] };
}

/** Select the directory at `path` in the sidebar tree. */
export function selectDirIntent(path: string): RendererIntent {
  return { Command: ["git_view.select_review_row", { target: { dir: path } }] };
}

/** Scroll the review by `lines`, down when positive; the reducer owns the offset. */
export function scrollIntent(lines: number): RendererIntent {
  return { Command: ["git_view.scroll", { lines }] };
}
