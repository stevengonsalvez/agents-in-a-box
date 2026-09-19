// Every root selector returns a scalar, and a memo over one does not wake when
// a drain leaves its value alone.

import assert from "node:assert/strict";
import { test } from "node:test";
import { createEffect, createMemo, createRoot } from "solid-js";
import type { Frame_Serialize } from "../../../ainb-app/bindings/AppState";
import { ROOT_SELECTORS } from "./selectors.ts";
import { createFrameStore, type SectionName } from "./store.ts";

const SECTIONS: SectionName[] = ["sessions", "workspace_load", "fleet"];

function frame(section: SectionName, version: number, body: unknown): Frame_Serialize {
  return { section, version, epoch: 1, host_id: "local", body };
}

function sessions(...statuses: string[]) {
  return {
    workspaces: [
      { name: "repo", path: "/repo", sessions: statuses.map((status, i) => ({ id: `s${i}`, name: `s${i}`, status })) },
    ],
  };
}

test("every root selector returns a scalar, with and without a host", () => {
  createRoot((dispose) => {
    const store = createFrameStore(SECTIONS);
    store.applyDrain("local", [
      {
        frames: [
          frame("sessions", 1, sessions("Idle", "Running")),
          frame("workspace_load", 1, { is_loading_workspaces: true, workspace_load_error: null }),
          frame("fleet", 1, { attention_elsewhere: 2, fleet_metadata: {}, daemon_attention: { by_session_id: {} } }),
        ],
      },
    ]);
    for (const [name, select] of Object.entries(ROOT_SELECTORS)) {
      for (const host of ["local", undefined]) {
        const value: unknown = select(store, host);
        assert.ok(typeof value === "number" || typeof value === "boolean", `${name} for ${host} gave ${typeof value}`);
      }
    }
    assert.equal(ROOT_SELECTORS.idleCount(store, "local"), 1);
    assert.equal(ROOT_SELECTORS.workspacesLoading(store, "local"), true);
    assert.equal(ROOT_SELECTORS.attentionElsewhere(store, "local"), 2, "read off a populated Fleet frame");
    dispose();
  });
});

test("a memo over a root selector stays quiet when a drain keeps its value", () => {
  const { store, runs, dispose } = createRoot((dispose) => {
    const store = createFrameStore(SECTIONS);
    const runs = { idle: 0 };
    const idle = createMemo(() => ROOT_SELECTORS.idleCount(store, "local"));
    createEffect(() => {
      idle();
      runs.idle += 1;
    });
    return { store, runs, dispose };
  });

  store.applyDrain("local", [{ frames: [frame("sessions", 1, sessions("Idle", "Running"))] }]);
  const afterFirst = runs.idle;
  // A different session changes, the idle count does not.
  store.applyDrain("local", [{ frames: [frame("sessions", 2, sessions("Idle", "Stopped"))] }]);
  assert.equal(runs.idle, afterFirst, "the idle count's readers did not re-run");
  store.applyDrain("local", [{ frames: [frame("sessions", 3, sessions("Idle", "Idle"))] }]);
  assert.equal(runs.idle, afterFirst + 1);
  dispose();
});

test("usageStale reads the stale mark on section 21, not on any other", () => {
  createRoot((dispose) => {
    const store = createFrameStore(["usage", "git_view"]);
    assert.equal(ROOT_SELECTORS.usageStale(store, "local"), false);
    store.applyDrain("local", [{ frames: [], oversize: [{ section: "usage", version: 2, bytes: 5_000_000 }] }]);
    assert.equal(ROOT_SELECTORS.usageStale(store, "local"), true);
    assert.equal(ROOT_SELECTORS.gitViewStale(store, "local"), false);
    dispose();
  });
});

test("inboxUnread is the daemon's unread count for the header, 0 with no section", () => {
  createRoot((dispose) => {
    const store = createFrameStore(["inbox"]);
    assert.equal(ROOT_SELECTORS.inboxUnread(store, "local"), 0);
    store.applyDrain("local", [
      {
        frames: [
          frame("inbox", 1, {
            entries: [],
            unread: 4,
            recipient: "operator",
            absent: null,
            unreachable: null,
            rows_cut: 0,
            summaries_cut: 0,
            received_at_ms: 1,
          }),
        ],
      },
    ]);
    assert.equal(ROOT_SELECTORS.inboxUnread(store, "local"), 4);
    assert.equal(ROOT_SELECTORS.inboxUnread(store, undefined), 0);
    dispose();
  });
});
