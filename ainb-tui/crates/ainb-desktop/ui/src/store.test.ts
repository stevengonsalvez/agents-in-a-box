// The frame store's D15 invariants, run under node's own test runner against
// Solid's browser build: `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";
import { createEffect, createRoot } from "solid-js";
import type { FrameBatch_Serialize, Frame_Serialize } from "../../../ainb-app/bindings/AppState";
import { createFrameStore, type FrameStore, type SectionName } from "./store.ts";

function frame(host: string, section: SectionName, epoch: number, version: number, body: unknown): Frame_Serialize {
  return { section, version, epoch, host_id: host, body };
}

function sessions(...names: string[]) {
  return {
    workspaces: [{ name: "repo", path: "/repo", sessions: names.map((name) => ({ id: name, name })) }],
  };
}

function names(store: FrameStore, host: string): string[] | undefined {
  return store.section(host, "sessions")?.workspaces[0]?.sessions.map((session) => session.name);
}

// The store is created in a root and driven from outside it, as the window's
// channel drives it: a root defers its effects until its own function returns.
function withStore(subscribed: SectionName[], run: (store: FrameStore) => void) {
  const { store, dispose } = createRoot((dispose) => ({ store: createFrameStore(subscribed), dispose }));
  try {
    run(store);
  } finally {
    dispose();
  }
}

const drain = (...frames: Frame_Serialize[]): FrameBatch_Serialize[] => [{ frames }];

test("two hosts' Sessions sections do not merge", () => {
  withStore(["sessions"], (store) => {
    store.applyDrain(drain(frame("local", "sessions", 1, 1, sessions("a"))));
    store.applyDrain(drain(frame("peer", "sessions", 7, 1, sessions("b", "c"))));

    assert.deepEqual(names(store, "local"), ["a"]);
    assert.deepEqual(names(store, "peer"), ["b", "c"]);
    assert.equal(store.hostCount(), 2);
  });
});

test("a larger epoch drops that host's held sections and no other host's", () => {
  withStore(["sessions", "shell"], (store) => {
    store.applyDrain(
      drain(
        frame("local", "sessions", 1, 9, sessions("a")),
        frame("local", "shell", 1, 4, { current_screen: "Sessions" }),
        frame("peer", "sessions", 1, 2, sessions("b")),
      ),
    );

    // The host restarted: its version count starts again below the held one.
    store.applyDrain(drain(frame("local", "sessions", 2, 1, sessions("fresh"))));

    assert.equal(store.state.hosts.local.epoch, 2);
    assert.deepEqual(names(store, "local"), ["fresh"]);
    assert.equal(store.section("local", "shell"), undefined, "the old process's shell frame is gone");
    assert.deepEqual(names(store, "peer"), ["b"]);

    // A straggler from the old process applies nothing.
    store.applyDrain(drain(frame("local", "shell", 1, 5, { current_screen: "Git" })));
    assert.equal(store.section("local", "shell"), undefined);
  });
});

test("an older or equal version within an epoch applies nothing", () => {
  withStore(["sessions"], (store) => {
    store.applyDrain(drain(frame("local", "sessions", 1, 3, sessions("a"))));
    store.applyDrain(drain(frame("local", "sessions", 1, 3, sessions("same version"))));
    store.applyDrain(drain(frame("local", "sessions", 1, 2, sessions("older"))));
    assert.deepEqual(names(store, "local"), ["a"]);
  });
});

test("an unsubscribed section applies nothing", () => {
  withStore(["sessions"], (store) => {
    store.applyDrain(drain(frame("local", "logs", 1, 1, { lines: ["secret"] })));
    assert.equal(store.state.hosts.local, undefined);
    assert.equal(store.hostCount(), 0);
  });
});

test("one drain is one commit: an effect reading two hosts runs once", () => {
  withStore(["sessions"], (store) => {
    const seen: string[] = [];
    createRoot(() =>
      createEffect(() => seen.push(`${names(store, "local") ?? "-"}|${names(store, "peer") ?? "-"}`)),
    );
    store.applyDrain([
      { frames: [frame("local", "sessions", 1, 1, sessions("a"))] },
      { frames: [frame("peer", "sessions", 3, 1, sessions("b"))] },
      { frames: [frame("local", "sessions", 1, 2, sessions("a", "c"))] },
    ]);
    assert.deepEqual(seen, ["-|-", "a,c|b"]);
  });
});

test("an oversize section is stale until the host frames it again", () => {
  withStore(["sessions"], (store) => {
    store.applyDrain([{ frames: [], oversize: [{ section: "sessions", version: 2, bytes: 9_000_000 }] }]);
    assert.deepEqual(store.state.stale, ["sessions"]);
    store.applyDrain(drain(frame("local", "sessions", 1, 3, sessions("a"))));
    assert.deepEqual(store.state.stale, []);
  });
});
