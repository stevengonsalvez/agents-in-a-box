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
  /** When the entry was made, as the viewer's local time. */
  when: string;
}

/** The inbox page's picture of section 16. */
export interface InboxPage {
  state: InboxState;
  /** One line on where the read stands, or `null` while it is live. */
  status: string | null;
  rows: InboxRow[];
  /** What the fold cut, as one line, or undefined. */
  cut: string | undefined;
  /** Whether the sweep is worth offering: the daemon counts something unread. */
  canMarkAllRead: boolean;
}

/** The inbox's one write: every entry read, as the daemon's sweep. */
export const MARK_ALL_READ: RendererIntent = { Command: ["inbox.mark_all_read", null] };

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
    return { state: "waiting", status: "Reading the inbox from the daemon", rows: [], cut: undefined, canMarkAllRead: false };
  }
  const lost: string[] = [];
  if (inbox.rows_cut > 0) lost.push(`${plural(inbox.rows_cut, "older entry", "older entries")} not sent`);
  if (inbox.summaries_cut > 0) lost.push(`${plural(inbox.summaries_cut, "summary", "summaries")} shortened`);
  const rows = inbox.entries.map((entry) => ({
    id: entry.id,
    label: `${entry.kind} · ${entry.event}`,
    summary: entry.summary,
    unread: entry.read_at === null || entry.read_at === undefined,
    when: new Date(entry.created_at).toLocaleString(),
  }));
  const cut = lost.length === 0 ? undefined : lost.join(", ");
  if (inbox.absent !== null) {
    return { state: "absent", status: inbox.absent, rows, cut, canMarkAllRead: false };
  }
  if (inbox.unreachable !== null) {
    return {
      state: "unreachable",
      status: `The daemon could not be read: ${inbox.unreachable}; these are the last entries that landed`,
      rows,
      cut,
      canMarkAllRead: inbox.unread > 0,
    };
  }
  return {
    state: rows.length === 0 ? "empty" : "live",
    status: null,
    rows,
    cut,
    canMarkAllRead: inbox.unread > 0,
  };
}
