// The sidebar's ring and the header's counts, from Sessions and Fleet bodies.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AttentionKind,
  FleetView_Serialize,
  SessionStatus,
  Session_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { idleCount, ringCount, ringFor } from "./sessions.ts";

function session(id: string, status: SessionStatus = "Running", is_attached = false): Session_Serialize {
  return { id, name: id, workspace_path: "/repo", status, is_attached } as unknown as Session_Serialize;
}

/** Fleet with `provider` ids recorded per session and daemon rows per provider id. */
function fleet(provider: Record<string, string>, rows: Record<string, AttentionKind[]>): FleetView_Serialize {
  return {
    fleet_metadata: Object.fromEntries(
      Object.entries(provider).map(([id, provider_session_id]) => [id, { provider_session_id }]),
    ),
    daemon_attention: {
      by_session_id: Object.fromEntries(
        Object.entries(rows).map(([id, kinds]) => [id, kinds.map((kind) => ({ kind }))]),
      ),
    },
  } as unknown as FleetView_Serialize;
}

test("a row rings with its tightest daemon kind, by exact provider id", () => {
  const view = fleet({ parent: "p-1", child: "p-2" }, { "p-1": ["Done", "Approve", "Ask"] });
  assert.equal(ringFor(session("parent"), view), "Ask");
  // Same cwd, different provider id: the sibling gets no copied ring.
  assert.equal(ringFor(session("child"), view), null);
  // No recorded provider id: nothing to correlate, no guess.
  assert.equal(ringFor(session("unknown"), view), null);
});

test("an attached row never rings; an errored row rings Err without a daemon row", () => {
  const view = fleet({ a: "p-1" }, { "p-1": ["Ask"] });
  assert.equal(ringFor(session("a", "Running", true), view), null);
  assert.equal(ringFor(session("b", { Error: "exited" }), undefined), "Err");
  assert.equal(ringFor(session("a", { Error: "exited" }), view), "Ask");
});

test("header counts are per ring kind and idle status", () => {
  const sessions = {
    workspaces: [
      { name: "one", sessions: [session("a"), session("b", "Idle")] },
      { name: "two", sessions: [session("c", "Idle"), session("d", { Error: "x" })] },
    ],
  } as unknown as SessionsView_Serialize;
  const view = fleet({ a: "p-a", c: "p-c" }, { "p-a": ["Ask"], "p-c": ["Ask", "Wait"] });
  assert.equal(ringCount(sessions, view, "Ask"), 2);
  assert.equal(ringCount(sessions, view, "Err"), 1);
  assert.equal(ringCount(sessions, view, "Wait"), 0);
  assert.equal(idleCount(sessions), 2);
});
