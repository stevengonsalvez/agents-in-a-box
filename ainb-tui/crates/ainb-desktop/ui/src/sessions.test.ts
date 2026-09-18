// The sidebar's ring and the header's counts, from the Sessions frame's merged
// attention.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AttentionKind,
  SessionFilter,
  SessionStatus,
  Session_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { idleCount, label, LABEL_CHARS, ringCount, ringFor, visibleRows } from "./sessions.ts";

function session(id: string, status: SessionStatus = "Running", marks: AttentionKind[] = []): Session_Serialize {
  return {
    id,
    name: id,
    workspace_path: "/repo",
    status,
    is_attached: false,
    attention: marks.map((kind) => ({ kind, detail: null })),
  } as unknown as Session_Serialize;
}

test("a row rings with the tightest kind of its merged attention", () => {
  assert.equal(ringFor(session("a", "Running", ["Done", "Approve", "Ask"])), "Ask");
  assert.equal(ringFor(session("b", "Running", ["Err", "Done"])), "Err");
  assert.equal(ringFor(session("c")), null, "no marks, no ring");
});

test("a row rings only from the frame: no status guess, no attention from another version", () => {
  // The host puts an error's ERR chip on the row itself; a status alone is not a ring.
  assert.equal(ringFor(session("d", { Error: "exited" })), null);
  const older = { id: "e", name: "e", status: "Idle" } as unknown as Session_Serialize;
  assert.equal(ringFor(older), null, "a host that sends no attention rings nothing");
});

test("header counts are per ring kind and idle status", () => {
  const sessions = {
    workspaces: [
      { name: "one", sessions: [session("a", "Running", ["Ask"]), session("b", "Idle")] },
      { name: "two", sessions: [session("c", "Idle", ["Ask", "Wait"]), session("d", { Error: "x" }, ["Err"])] },
    ],
  } as unknown as SessionsView_Serialize;
  assert.equal(ringCount(sessions, "Ask"), 2);
  assert.equal(ringCount(sessions, "Err"), 1);
  assert.equal(ringCount(sessions, "Wait"), 0);
  assert.equal(idleCount(sessions), 2);
});

test("a label drops control and format characters and stops at the cap", () => {
  assert.equal(label("feat/\u202Eevil\u001b[31m\u200Bx"), "feat/evil[31mx");
  assert.equal(label("\u{1F600}".repeat(100)), "\u{1F600}".repeat(LABEL_CHARS));
});

test("the sidebar draws only the rows the session list's filter keeps", () => {
  const rows = (filter: SessionFilter) => {
    const sessions = {
      workspaces: [
        {
          name: "repo",
          sessions: [
            { ...session("live"), mode: "Interactive" },
            { ...session("gone", "Stopped"), mode: "Interactive" },
            { ...session("boss", "Stopped"), mode: "Boss" },
          ],
        },
      ],
      session_filter: filter,
    } as unknown as SessionsView_Serialize;
    return visibleRows(sessions, sessions.workspaces[0]);
  };

  assert.deepEqual(
    rows("active_only").map((row) => [row.index, row.session.id]),
    [
      [0, "live"],
      // A Boss session is not the filter's business, as the reducer has it.
      [2, "boss"],
    ],
  );
  assert.deepEqual(
    rows("stopped_only").map((row) => [row.index, row.session.id]),
    [
      [1, "gone"],
      [2, "boss"],
    ],
  );
  assert.equal(rows("all").length, 3);
});
