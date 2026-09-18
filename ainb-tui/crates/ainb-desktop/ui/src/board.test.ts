// The board's columns and the attention list, from the frames the window holds.

import assert from "node:assert/strict";
import { test } from "node:test";
import type {
  AgentCardFrame,
  AgentStatusView,
  FleetView_Serialize,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { attentionRows, boardColumns, boardHealth, COLUMNS, elsewhereCount, showIntents } from "./board.ts";

function card(sessionKey: string, over: Partial<AgentCardFrame> = {}): AgentCardFrame {
  return {
    session_key: sessionKey,
    provider: "claude",
    lifecycle: "RUNNING",
    transport_health: "HEALTHY",
    state: "working",
    has_open_request: false,
    wait_kind: null,
    ...over,
  } as AgentCardFrame;
}

function status(...cards: AgentCardFrame[]): AgentStatusView {
  return {
    absent: null,
    head_revision: 3,
    view: {
      host_id: "local",
      read_revision: 3,
      received_at_ms: 1,
      head_revision: 3,
      health: { kind: "live" },
      cards,
    },
  };
}

/** A sessions frame with one row per `[id, name, provider id]`, and the Fleet
 * metadata that correlates each row to its provider session. */
function world(...rows: [string, string, string][]): {
  sessions: SessionsView_Serialize;
  fleet: FleetView_Serialize;
} {
  return {
    sessions: {
      workspaces: [{ name: "repo", sessions: rows.map(([id, name]) => ({ id, name, attention: [] })) }],
    } as unknown as SessionsView_Serialize,
    fleet: {
      fleet_metadata: Object.fromEntries(rows.map(([id, , provider]) => [id, { provider_session_id: provider }])),
      fleet_snapshot: [{ session_key: "claude:p-1", model: "opus" }],
      daemon_attention: { by_session_id: {}, all: {}, reachable: true, error: null, not_running: false },
      attention_elsewhere: 0,
    } as unknown as FleetView_Serialize,
  };
}

test("the columns are the host's states, whatever the lifecycle says", () => {
  // A card the host calls idle stays idle even though its lifecycle reads
  // RUNNING: the renderer never re-derives a state.
  const { sessions, fleet } = world(["u-1", "api", "p-1"]);
  const columns = boardColumns(
    status(card("claude:p-1", { state: "idle", lifecycle: "RUNNING" }), card("codex:p-2", { state: "waiting" })),
    fleet,
    sessions,
  );
  assert.deepEqual(
    columns.map((column) => column.state),
    COLUMNS,
  );
  const by = Object.fromEntries(columns.map((column) => [column.state, column.cards.map((c) => c.key)]));
  assert.deepEqual(by.idle, ["claude:p-1"]);
  assert.deepEqual(by.working, []);
  assert.deepEqual(by.waiting, ["codex:p-2"]);
});

test("a card takes its row's name and the fleet's model, and one with no row still draws", () => {
  const { sessions, fleet } = world(["u-1", "api", "p-1"]);
  const [working] = boardColumns(status(card("claude:p-1"), card("claude:p-9")), fleet, sessions).filter(
    (column) => column.state === "working",
  );
  const known = working.cards.find((c) => c.key === "claude:p-1")!;
  assert.equal(known.title, "api");
  assert.equal(known.sessionId, "u-1");
  assert.equal(known.model, "opus");
  const stray = working.cards.find((c) => c.key === "claude:p-9");
  assert.ok(stray, "an agent the sidebar has not listed is still on the board");
  assert.equal(stray.sessionId, null);
  assert.equal(stray.title, "claude:p-9");
});

test("an agent with something open floats to the top of its column", () => {
  const { sessions, fleet } = world();
  const [waiting] = boardColumns(
    status(card("a:1", { state: "waiting" }), card("b:2", { state: "waiting", has_open_request: true })),
    fleet,
    sessions,
  );
  assert.deepEqual(
    waiting.cards.map((c) => c.key),
    ["b:2", "a:1"],
  );
});

test("an absent, stale or unreachable status draws its health, not an empty board", () => {
  assert.equal(boardHealth(undefined).kind, "absent");
  assert.deepEqual(boardHealth({ absent: "no daemon", head_revision: 0, view: null }), {
    kind: "absent",
    detail: "no daemon",
  });
  const stale = status();
  stale.view!.health = { kind: "stale", read_revision: 2, head_revision: 5 };
  assert.deepEqual(boardHealth(stale), { kind: "stale", behind: 3 });
  const gone = status();
  gone.view!.health = { kind: "unreachable", stale_since_ms: 1, reason: "socket closed" };
  assert.deepEqual(boardHealth(gone), { kind: "unreachable", reason: "socket closed" });
  assert.deepEqual(boardHealth(status()), { kind: "live" });
});

test("the attention list is the daemon's open rows, tightest first, titled by their session", () => {
  const { sessions, fleet } = world(["u-1", "api", "p-1"]);
  fleet.daemon_attention.by_session_id = {
    "p-1": [{ kind: "Err", detail: "exited 1" }],
    "p-unknown": [{ kind: "Ask", detail: "which branch?" }],
  } as unknown as FleetView_Serialize["daemon_attention"]["by_session_id"];
  const rows = attentionRows(fleet, sessions);
  assert.deepEqual(
    rows.map((row) => [row.kind, row.title, row.sessionId]),
    [
      ["Ask", "p-unknown", null],
      ["Err", "api", "u-1"],
    ],
  );
});

test("rows waiting elsewhere are counted, never swallowed", () => {
  const { fleet } = world();
  fleet.attention_elsewhere = 2;
  assert.equal(elsewhereCount(fleet), 2);
  assert.equal(elsewhereCount(undefined), 0);
});

test("a click selects the row without attaching it, then shows the pane it is about", () => {
  assert.deepEqual(showIntents("u-1", true), [
    { Command: ["session_list.select_row", { target: { session: "u-1" }, open: false }] },
    { Command: ["session_list.select_tab", { tab: "Ask" }] },
  ]);
  assert.deepEqual(showIntents("u-1", false)[1], { Command: ["session_list.select_tab", { tab: "Preview" }] });
});
