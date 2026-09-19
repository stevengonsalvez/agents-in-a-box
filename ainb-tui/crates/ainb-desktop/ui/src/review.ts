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

/**
 * One line of the diff body: a file's heading, a hunk's header, an expand
 * affordance, or a row.
 *
 * `index` is the reducer's OWN virtual-row index (`flatten`,
 * `ainb-app/src/components/code_review/render.rs:112`), which is what
 * `review_ui.scroll` counts: a row per file heading, a row per hidden gap, a
 * row per code line, and nothing for a collapsed or binary file beyond its
 * heading. A hunk's `@@` header has no index because the terminal does not
 * draw one; it is decoration here, and counting it would put this window's
 * offsets half a screen away from the terminal's.
 */
export type BodyLine =
  | { kind: "file"; key: string; index: number; file: ReviewFileFrame_Serialize; open: boolean }
  | { kind: "hunk"; key: string; index: undefined; header: string; current: boolean }
  | { kind: "expand"; key: string; index: number; hidden: number }
  | { kind: "row"; key: string; index: number; row: DiffRow_Serialize };

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

/**
 * The whole review's body: each file's heading, then its hunks and their rows.
 *
 * A hunk's header says what it skipped (`gap_before`), because a diff drawn
 * without its gaps reads as a file with nothing between its changes.
 */
export function bodyLines(section: GitViewView_Serialize | undefined): BodyLine[] {
  const view = gitView(section);
  if (view === undefined) return [];
  const lines: BodyLine[] = [];
  let index = 0;
  let hunkOrdinal = 0;
  view.review.files.forEach((file, fileIndex) => {
    lines.push({
      kind: "file",
      key: `${file.path}:file`,
      index: index++,
      file,
      open: fileIndex === view.review_ui.selected_file,
    });
    // A collapsed or binary file is its heading and nothing else, as the
    // reducer flattens it.
    if (file.collapsed || file.binary) return;
    file.hunks.forEach((hunk, hunkIndex) => {
      lines.push({
        kind: "hunk",
        key: `${file.path}:hunk:${hunkIndex}`,
        index: undefined,
        header: hunkHeader(hunk),
        current: hunkOrdinal === view.review_ui.current_hunk,
      });
      hunkOrdinal += 1;
      const before = hunk.gap_before - hunk.expanded_before;
      if (before > 0) {
        lines.push({
          kind: "expand",
          key: `${file.path}:${hunkIndex}:before`,
          index: index++,
          hidden: before,
        });
      }
      hunk.rows.forEach((row, rowIndex) => {
        lines.push({
          kind: "row",
          key: `${file.path}:${hunkIndex}:${rowIndex}`,
          index: index++,
          row,
        });
      });
      const after = hunk.gap_after - hunk.expanded_after;
      if (after > 0) {
        lines.push({
          kind: "expand",
          key: `${file.path}:${hunkIndex}:after`,
          index: index++,
          hidden: after,
        });
      }
    });
  });
  return lines;
}

/**
 * The height of one body row in pixels, which is what a wheel delta is
 * measured against before it becomes a row count for the reducer.
 *
 * It matches `.review-row`'s line box in `shell.css`. A few pixels out only
 * changes how far one flick of a wheel travels, never what the reducer and
 * this window agree the offset is: the reducer owns that, and the window draws
 * what comes back.
 */
export const ROW_PX = 20;

/** A wheel event's deltas, as the browser reports them. */
export interface Wheel {
  deltaY: number;
  /** 0 pixels, 1 lines, 2 pages, as `WheelEvent.deltaMode` gives it. */
  deltaMode: number;
}

/**
 * How many whole rows `wheel` asks for, given the pixels left over from the
 * wheels before it, and what is left over after.
 *
 * Accumulated rather than truncated per event: a trackpad sends deltas of a
 * pixel or two and a line-mode mouse sends 3, and truncating each one on its
 * own rounds every single one of them to no rows at all.
 */
export function wheelRows(pending: number, wheel: Wheel): { rows: number; pending: number } {
  const pixels =
    wheel.deltaMode === 1
      ? wheel.deltaY * ROW_PX
      : wheel.deltaMode === 2
        ? wheel.deltaY * ROW_PX * 20
        : wheel.deltaY;
  const total = pending + pixels;
  const rows = Math.trunc(total / ROW_PX);
  return { rows, pending: total - rows * ROW_PX };
}

/** A run of a row's text, and whether the reducer marked it as changed. */
export interface Segment {
  text: string;
  emphasis: boolean;
}

/**
 * The UTF-16 index `byte` names in `text`.
 *
 * A row's emphasis ranges are BYTE offsets, because the reducer measured them
 * in Rust where a string is UTF-8; a JavaScript string is indexed in UTF-16
 * code units. For ASCII the two agree, and for everything else they do not:
 * one accented letter is two bytes and one unit, one emoji four bytes and two.
 */
function utf16Index(text: string, byte: number): number {
  if (byte <= 0) return 0;
  let bytes = 0;
  let index = 0;
  while (index < text.length) {
    if (bytes >= byte) return index;
    const code = text.codePointAt(index) ?? 0;
    bytes += code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4;
    index += code > 0xffff ? 2 : 1;
  }
  return text.length;
}

/**
 * `row`'s text split into the runs the reducer marked and the runs it did not,
 * so the window draws the word-level emphasis the terminal draws.
 *
 * A row whose text the frame changed carries no ranges at all (the projection
 * drops them when it scrubs or cuts), so the common case is one run and no
 * work.
 */
export function segments(row: DiffRow_Serialize): Segment[] {
  if (row.emphasis.length === 0) return [{ text: row.raw, emphasis: false }];
  const runs: Segment[] = [];
  let at = 0;
  for (const [from, to] of row.emphasis) {
    const start = utf16Index(row.raw, from);
    const end = utf16Index(row.raw, to);
    if (end <= start || start < at) continue;
    if (start > at) runs.push({ text: row.raw.slice(at, start), emphasis: false });
    runs.push({ text: row.raw.slice(start, end), emphasis: true });
    at = end;
  }
  if (at < row.raw.length) runs.push({ text: row.raw.slice(at), emphasis: false });
  return runs;
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

/** Scroll the review by `lines`, down when positive; the reducer owns the offset. */
export function scrollIntent(lines: number): RendererIntent {
  return { Command: ["git_view.scroll", { lines }] };
}
