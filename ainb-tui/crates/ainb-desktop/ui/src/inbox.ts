// What the desktop inbox draws from section 16, the daemon's inbox as the host
// folded it (D3p-c). Projections only: `inbox.tsx` draws what these return.
//
//   inbox section ──inboxView──▶ rows, status line, cut line, the sweep offer
//
// The window keeps no inbox state: the rows, which are unread, and the count
// are all the frame's. Its one write is the daemon's whole-inbox sweep
// (`hangar/inbox_mark_read`), sent as the `inbox.mark_all_read` command; the
// reducer mints the op id and folds the daemon's reply, so a count the daemon
// did not send is never drawn. There is no per-row control because the verb has
// none.

import type { InboxView_Serialize } from "../../../ainb-app/bindings/AppState";
import type { RendererIntent } from "./tabs.ts";

/** Where the inbox read stands, one word the page can style by. */
export type InboxState = "waiting" | "absent" | "unreachable" | "empty" | "live";

/** One entry as the page draws it. */
export interface InboxRow {
  /** The entry id, the key the row is drawn by. */
  id: string;
  /** `<kind> · <event>`. */
  label: string;
  /** The daemon's line, scrubbed and cut by the host, cut marker kept. */
  summary: string;
  unread: boolean;
  /** How long ago the entry was made, on the daemon's clock. */
  when: string;
}

/** The inbox page's picture of section 16. */
export interface InboxPage {
  /** Whether the window starts below the first entry the frame carries. */
  scrolled: boolean;
  state: InboxState;
  /** One line on where the read stands, or `null` while it is live. */
  status: string | null;
  rows: InboxRow[];
  /** What the fold cut, as one line, or undefined. */
  cut: string | undefined;
  /** Whether the sweep is worth offering: the daemon counts something unread. */
  canMarkAllRead: boolean;
}

/**
 * The inbox section as the renderer's telemetry line reports it, for the
 * proof harness: how many rows the frame holds and how many the daemon counts
 * unread. Counts only, never a body; `[0, 0]` until the section is framed.
 */
export function inboxCounts(inbox: InboxView_Serialize | undefined): [number, number] {
  return inbox === undefined ? [0, 0] : [inbox.entries.length, inbox.unread];
}

/**
 * The rows that put the reducer on its inbox screen, where the sweep is
 * active, the way the terminal opens it from home. Whether the page shows is
 * the reducer's: it shows while `shell.current_screen` is `inbox`.
 */
export const OPEN_INBOX: RendererIntent[] = [
  { Command: ["global.go_home", null] },
  { Command: ["home.inbox", null] },
];

/**
 * The rows that leave the inbox screen for home, where the page opened it
 * from, and put the reducer back on the session list, as closing settings does.
 */
export const CLOSE_INBOX: RendererIntent[] = [
  { Command: ["inbox.back", null] },
  { Command: ["home.sessions", null] },
];

/** Pixels a row is worth to the wheel, as the review tab counts them. */
export const ROW_PX = 24;

/** The most rows one wheel event asks the reducer for. */
export const MAX_WHEEL_ROWS = 10;

/**
 * The commands a scroll of `rows` sends, down when positive.
 *
 * The reducer moves one row per command (`InboxScrollUp` / `InboxScrollDown`,
 * the terminal's `j` and `k`), so a wheel of n rows is n commands. Capped, so
 * one flick of a trackpad cannot queue hundreds of them at the host.
 */
export function scrollIntents(rows: number): RendererIntent[] {
  const count = Math.min(Math.abs(rows), MAX_WHEEL_ROWS);
  const command: RendererIntent = rows > 0
    ? { Command: ["inbox.scroll_down", null] }
    : { Command: ["inbox.scroll_up", null] };
  return Array.from({ length: count }, () => command);
}

/** The inbox's one write: every entry read, as the daemon's sweep. */
export const MARK_ALL_READ: RendererIntent = { Command: ["inbox.mark_all_read", null] };

/**
 * `created_at` against the clock the read landed on, both epoch milliseconds,
 * in the largest unit that is at least one.
 *
 * The terminal's rule, `age()` in `ainb-core/src/components/inbox.rs`, so one
 * entry reads the same on both surfaces: a read that has not landed has no
 * clock and reads `?`, and a stamp past the clock (skew) reads `now`.
 */
export function age(createdAtMs: number, nowMs: number): string {
  if (nowMs <= 0) return "?";
  const secs = Math.floor((nowMs - createdAtMs) / 1000);
  if (secs <= 0) return "now";
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86_400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86_400)}d`;
}

/** The daemon's unread count, one scalar for the header; 0 with no section. */
export function unreadCount(inbox: InboxView_Serialize | undefined): number {
  return inbox?.unread ?? 0;
}

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** Section 16 as the inbox page draws it. */
export function inboxView(inbox: InboxView_Serialize | undefined): InboxPage {
  if (inbox === undefined) {
    return {
      state: "waiting",
      status: "Reading the inbox from the daemon",
      rows: [],
      cut: undefined,
      canMarkAllRead: false,
      scrolled: false,
    };
  }
  const lost: string[] = [];
  if (inbox.rows_cut > 0) lost.push(`${plural(inbox.rows_cut, "older entry", "older entries")} not sent`);
  if (inbox.summaries_cut > 0) lost.push(`${plural(inbox.summaries_cut, "summary", "summaries")} shortened`);
  // The window the terminal draws: `scroll` is an index into `entries`,
  // bounded by the reducer, and at least one row always draws.
  const scroll = Math.min(inbox.scroll, Math.max(inbox.entries.length - 1, 0));
  const scrolled = scroll > 0;
  const rows = inbox.entries.slice(scroll).map((entry) => ({
    id: entry.id,
    label: `${entry.kind} · ${entry.event}`,
    summary: entry.summary,
    unread: entry.read_at === null || entry.read_at === undefined,
    when: age(entry.created_at, inbox.received_at_ms),
  }));
  const cut = lost.length === 0 ? undefined : lost.join(", ");
  if (inbox.absent !== null) {
    return { state: "absent", status: inbox.absent, rows, cut, canMarkAllRead: false, scrolled };
  }
  if (inbox.unreachable !== null) {
    return {
      state: "unreachable",
      status: `The daemon could not be read: ${inbox.unreachable}; these are the last entries that landed`,
      rows,
      cut,
      canMarkAllRead: inbox.unread > 0,
      scrolled,
    };
  }
  return {
    state: rows.length === 0 ? "empty" : "live",
    status: null,
    rows,
    cut,
    canMarkAllRead: inbox.unread > 0,
    scrolled,
  };
}
