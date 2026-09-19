// The sidebar's ring and the header's counts, from the Sessions frame's merged
// attention.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AttentionKind,
  SessionStatus,
  Session_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { idleCount, isSelected, label, LABEL_CHARS, ringCount, ringFor } from "./sessions.ts";

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

test("the selected row is named by id, not by its place in the list", () => {
  // The frame carries only the rows the filter shows, so an index into the
  // reducer's full list would name the wrong one (#1180).
  const sessions = {
    workspaces: [{ name: "repo", sessions: [session("live"), session("boss")] }],
    selected_session_id: "boss",
  } as unknown as SessionsView_Serialize;
  assert.equal(isSelected(sessions, "boss"), true);
  assert.equal(isSelected(sessions, "live"), false);
  assert.equal(isSelected(undefined, "boss"), false);
});
