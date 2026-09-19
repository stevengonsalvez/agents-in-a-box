// The desktop inbox draws section 16 as the host framed it: the rows, which
// are unread, the unread count as one scalar, why there is nothing when there
// is nothing, and what the fold cut. It keeps no state of its own and has one
// write, the whole-inbox sweep, sent as a command and nothing else.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { InboxRowFrame_Serialize, InboxView_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  age,
  CLOSE_INBOX,
  inboxView,
  MARK_ALL_READ,
  OPEN_INBOX,
  scrollIntents,
  unreadCount,
} from "./inbox.ts";

const row = (n: number, read: number | null = null): InboxRowFrame_Serialize => ({
  id: `01J0INBOX00000000000000000${n}`,
  kind: "issue",
  event: "issue_created",
  subject_id: `issue-${n}`,
  summary: `issue ${n} opened [cut]`,
  recipient: "operator",
  created_at: Date.UTC(2026, 8, 19, 12, n),
  read_at: read,
});

const view = (over: Partial<InboxView_Serialize> = {}): InboxView_Serialize => ({
  entries: [row(2), row(1, Date.UTC(2026, 8, 19, 13, 0))],
  unread: 1,
  recipient: "operator",
  absent: null,
  unreachable: null,
  rows_cut: 0,
  summaries_cut: 0,
  received_at_ms: 1,
  scroll: 0,
  ...over,
});

test("a row's age is the daemon's clock, in the terminal's words", () => {
  // `ainb-core/src/components/inbox.rs` age(): the largest unit that is at
  // least one, `now` for a stamp at or past the clock, `?` with no read yet.
  const at = (created: number, now: number) => age(created, now);
  assert.equal(at(1_000, 0), "?", "no read has landed");
  assert.equal(at(5_000, 4_000), "now", "skew reads now, never a negative age");
  assert.equal(at(4_000, 5_000), "1s");
  assert.equal(at(0, 59_000), "59s");
  assert.equal(at(0, 60_000), "1m");
  assert.equal(at(0, 3_599_000), "59m");
  assert.equal(at(0, 3_600_000), "1h");
  assert.equal(at(0, 86_399_000), "23h");
  assert.equal(at(0, 86_400_000), "1d");
});

test("the rows draw newest first as framed, each marked read or unread", () => {
  const drawn = inboxView(view());
  // The frame's own clock, so both surfaces print the same text.
  assert.deepEqual(
    drawn.rows.map((entry) => entry.when),
    [age(row(2).created_at, 1), age(row(1).created_at, 1)],
  );
  assert.deepEqual(
    drawn.rows.map((entry) => [entry.id, entry.unread, entry.summary, entry.label]),
    [
      [row(2).id, true, "issue 2 opened [cut]", "issue · issue_created"],
      [row(1).id, false, "issue 1 opened [cut]", "issue · issue_created"],
    ],
    "the frame's order and summaries, the fold's cut marker kept",
  );
  assert.equal(drawn.status, null);
  assert.equal(drawn.cut, undefined);
});

test("the unread count is one scalar, the daemon's, and zero with no section", () => {
  assert.equal(unreadCount(view({ unread: 7 })), 7, "the daemon's count, not the rows counted");
  assert.equal(unreadCount(undefined), 0);
  assert.equal(typeof unreadCount(view()), "number");
});

test("mark all read is offered only while something is unread, and is a command", () => {
  assert.equal(inboxView(view({ unread: 3 })).canMarkAllRead, true);
  assert.equal(inboxView(view({ unread: 0 })).canMarkAllRead, false);
  assert.deepEqual(MARK_ALL_READ, { Command: ["inbox.mark_all_read", null] });
});

test("an absent inbox and an unreachable one each say why, in the host's words", () => {
  const absent = inboxView(view({ entries: [], unread: 0, absent: "the daemon serves no hangar/inbox_list" }));
  assert.equal(absent.state, "absent");
  assert.equal(absent.status, "the daemon serves no hangar/inbox_list");
  assert.equal(absent.canMarkAllRead, false, "no sweep against an inbox the daemon does not serve");

  const stale = inboxView(view({ unreachable: "daemon not reachable" }));
  assert.equal(stale.state, "unreachable");
  assert.match(stale.status ?? "", /daemon not reachable/);
  assert.equal(stale.rows.length, 2, "the last rows that landed still draw");

  assert.equal(inboxView(undefined).state, "waiting");
  assert.equal(inboxView(view({ entries: [], unread: 0 })).state, "empty");
});

test("what the fold cut is one line of its own counters", () => {
  assert.equal(
    inboxView(view({ rows_cut: 4, summaries_cut: 1 })).cut,
    "4 older entries not sent, 1 summary shortened",
  );
  assert.equal(inboxView(view({ rows_cut: 1 })).cut, "1 older entry not sent");
});

test("the page opens and closes the reducer's inbox screen, the rows the terminal uses", () => {
  // `tests/inbox_surface.rs` runs exactly these through the host.
  assert.deepEqual(OPEN_INBOX, [
    { Command: ["global.go_home", null] },
    { Command: ["home.inbox", null] },
  ]);
  // Back to the session list the window sits on, as closing settings does.
  assert.deepEqual(CLOSE_INBOX, [
    { Command: ["inbox.back", null] },
    { Command: ["home.sessions", null] },
  ]);
});

test("the page draws from the reducer's scroll, the window the terminal draws", () => {
  // `scroll` is an index into `entries`, framed so a mirrored surface draws
  // the same window (`wire/inbox.rs`, `components/inbox.rs`).
  const entries = [row(4), row(3), row(2), row(1)];
  const at = (scroll: number) => inboxView(view({ entries, scroll }));
  assert.deepEqual(
    at(0).rows.map((entry) => entry.summary),
    ["issue 4 opened [cut]", "issue 3 opened [cut]", "issue 2 opened [cut]", "issue 1 opened [cut]"],
  );
  assert.deepEqual(
    at(2).rows.map((entry) => entry.summary),
    ["issue 2 opened [cut]", "issue 1 opened [cut]"],
    "the rows above the offset are the ones the terminal has scrolled past",
  );
  assert.equal(at(2).scrolled, true, "and the page says it starts part way");
  assert.equal(at(0).scrolled, false);
  // A scroll past the end (a read that shrank the list) still draws a row.
  assert.equal(at(99).rows.length, 1);
});

test("the wheel asks the reducer for one command per row, bounded", () => {
  // The reducer moves one row per command (`inbox.scroll_up`/`_down`), so a
  // wheel of n rows is n commands, capped so one flick cannot flood the host.
  assert.deepEqual(scrollIntents(2), [
    { Command: ["inbox.scroll_down", null] },
    { Command: ["inbox.scroll_down", null] },
  ]);
  assert.deepEqual(scrollIntents(-1), [{ Command: ["inbox.scroll_up", null] }]);
  assert.deepEqual(scrollIntents(0), []);
  assert.equal(scrollIntents(500).length, 10, "capped");
  assert.equal(scrollIntents(-500).length, 10, "capped upward too");
});
