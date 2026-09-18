// The command palette's rows and the order it offers them in. Projections and
// ranking only: the overlay in `palette.tsx` draws what these return, and the
// intent each row carries is the one `dispatch` receives.
//
// The command rows come from the host's `palette` command, which builds them
// from the same refusal set the dispatch gate uses, so the palette cannot
// offer a row the shell would then refuse.

import type { Session_Serialize, SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { allSessions, label, rowStatus } from "./sessions.ts";
import type { PaletteEntry } from "../../bindings/Desktop.ts";
import { openRowIntent, type RendererIntent } from "./tabs.ts";

/** Generated from `ainb_desktop::host::PaletteEntry` (#1158). */
export type { PaletteEntry };

/** One offered row: what it draws, and the intent choosing it sends. */
export interface PaletteRow {
  kind: "command" | "session";
  /** Unique within a palette, so the list keys by it. */
  key: string;
  title: string;
  /** The line under the title: a command's doc, a session's branch. */
  detail: string;
  /** The key that also runs it, when it has one. */
  chord: string | null;
  /** A row the reducer would refuse now is still offered, greyed. */
  active: boolean;
  intent: RendererIntent;
}

/** The most rows the palette lists at once. */
export const PALETTE_ROWS = 60;

/** A run of matched characters scores this much more than a lone one. */
const RUN_BONUS = 3;

/** A match at the start of a word scores this much more than mid-word. */
const WORD_BONUS = 2;

const WORD_BREAK = /[^\p{L}\p{N}]/u;

/**
 * How well `text` matches `query`, or `null` when it does not.
 *
 * A subsequence match, case-insensitively: every query character appears in
 * order. Characters that land in a run, or at the start of a word, count for
 * more, so `sls` reaches `session_list.select_row` ahead of a row that merely
 * contains the three letters scattered.
 *
 * Each character takes the first position left, without backtracking to look
 * for a better-placed later one: a palette of a few hundred short rows does
 * not need the search, and the ranking it gives is stable and explainable.
 */
export function score(query: string, text: string): number | null {
  const needle = query.toLowerCase();
  const hay = text.toLowerCase();
  let total = 0;
  let at = 0;
  let previous = -2;
  for (const want of needle) {
    const found = hay.indexOf(want, at);
    if (found < 0) return null;
    const start = found === 0 || WORD_BREAK.test(hay[found - 1]);
    total += 1 + (found === previous + 1 ? RUN_BONUS : 0) + (start ? WORD_BONUS : 0);
    previous = found;
    at = found + 1;
  }
  return total;
}

/** The command rows, in the order the host listed them. */
export function commandRows(entries: readonly PaletteEntry[]): PaletteRow[] {
  return entries.map((entry) => ({
    kind: "command" as const,
    key: `command:${entry.id}`,
    title: label(entry.doc || entry.id),
    detail: label(`${entry.context} · ${entry.id}`),
    chord: entry.chord === null ? null : label(entry.chord),
    active: entry.active,
    intent: { Command: [entry.id, null] } as RendererIntent,
  }));
}

/** The live sessions, as rows that open the session's terminal. */
export function sessionRows(view: SessionsView_Serialize | undefined): PaletteRow[] {
  return allSessions(view).map((session: Session_Serialize) => ({
    kind: "session" as const,
    key: `session:${session.id}`,
    title: label(session.name),
    detail: label(`${rowStatus(session.status)} · ${session.branch_name}`),
    chord: null,
    active: true,
    intent: openRowIntent({ session: session.id }),
  }));
}

/**
 * The rows `query` selects, best first. An empty query keeps the given order,
 * so the palette opens on the full list rather than on an arbitrary ranking.
 *
 * A row a key already runs, and a row the reducer would run now, sort ahead of
 * an equal match without one: the palette's first row is the likely one.
 */
export function rank(rows: readonly PaletteRow[], query: string, limit = PALETTE_ROWS): PaletteRow[] {
  const trimmed = query.trim();
  if (trimmed === "") return rows.slice(0, limit);
  const scored: { row: PaletteRow; score: number }[] = [];
  for (const row of rows) {
    const hit = score(trimmed, `${row.title} ${row.detail}`);
    if (hit !== null) scored.push({ row, score: hit });
  }
  scored.sort(
    (a, b) =>
      b.score - a.score ||
      Number(b.row.active) - Number(a.row.active) ||
      Number(b.row.chord !== null) - Number(a.row.chord !== null) ||
      a.row.title.localeCompare(b.row.title),
  );
  return scored.slice(0, limit).map((entry) => entry.row);
}

/** Where `step` lands from `at` over `count` rows, wrapping. */
export function stepRow(count: number, at: number, step: number): number {
  return count === 0 ? 0 : (at + step + count) % count;
}
