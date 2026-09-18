// The frame store's D15 invariants, run under node's own test runner against
// Solid's browser build: `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";
import { createEffect, createRoot } from "solid-js";
import type { FrameBatch_Serialize, Frame_Serialize, SessionStatus } from "../../../ainb-app/bindings/AppState";
import { createFrameStore, MAX_HOSTS, type FrameStore, type SectionName } from "./store.ts";

function frame(host: string, section: SectionName, epoch: number, version: number, body: unknown): Frame_Serialize {
  return { section, version, epoch, host_id: host, body };
}

function sessions(...names: string[]) {
  return {
    workspaces: [
      { name: "repo", path: "/repo", sessions: names.map((name) => ({ id: name, name, status: "Idle" as SessionStatus })) },
    ],
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

/** One drain over `peer`'s channel carrying `frames` in one batch. */
function drain(store: FrameStore, peer: string, ...frames: Frame_Serialize[]) {
  store.applyDrain(peer, [{ frames }]);
}

const oversize: FrameBatch_Serialize = {
  frames: [],
  oversize: [{ section: "sessions", version: 2, bytes: 9_000_000 }],
};

test("two hosts' Sessions sections do not merge", () => {
  withStore(["sessions"], (store) => {
    drain(store, "local", frame("local", "sessions", 1, 1, sessions("a")));
    drain(store, "peer", frame("peer", "sessions", 7, 1, sessions("b", "c")));

    assert.deepEqual(names(store, "local"), ["a"]);
    assert.deepEqual(names(store, "peer"), ["b", "c"]);
    assert.equal(store.hostCount(), 2);
  });
});

test("a frame naming a host other than the channel's peer applies nothing", () => {
  withStore(["sessions"], (store) => {
    drain(store, "local", frame("impostor", "sessions", 1, 1, sessions("forged")));
    assert.equal(store.section("impostor", "sessions"), undefined);
    assert.equal(store.section("local", "sessions"), undefined);
    assert.equal(store.hostCount(), 0);
  });
});

test("a larger epoch drops that host's held sections and no other host's", () => {
  withStore(["sessions", "shell"], (store) => {
    drain(
      store,
      "local",
      frame("local", "sessions", 1, 9, sessions("a")),
      frame("local", "shell", 1, 4, { current_screen: "Sessions" }),
    );
    drain(store, "peer", frame("peer", "sessions", 1, 2, sessions("b")));

    // The host restarted: its version count starts again below the held one.
    drain(store, "local", frame("local", "sessions", 2, 1, sessions("fresh")));

    assert.equal(store.state.hosts.local.epoch, 2);
    assert.deepEqual(names(store, "local"), ["fresh"]);
    assert.equal(store.section("local", "shell"), undefined, "the old process's shell frame is gone");
    assert.deepEqual(names(store, "peer"), ["b"]);

    // A straggler from the old process applies nothing.
    drain(store, "local", frame("local", "shell", 1, 5, { current_screen: "Git" }));
    assert.equal(store.section("local", "shell"), undefined);
  });
});

test("an older or equal version within an epoch applies nothing", () => {
  withStore(["sessions"], (store) => {
    drain(store, "local", frame("local", "sessions", 1, 3, sessions("a")));
    drain(store, "local", frame("local", "sessions", 1, 3, sessions("same version")));
    drain(store, "local", frame("local", "sessions", 1, 2, sessions("older")));
    assert.deepEqual(names(store, "local"), ["a"]);
  });
});

test("an unsubscribed section applies nothing", () => {
  withStore(["sessions"], (store) => {
    drain(store, "local", frame("local", "logs", 1, 1, { lines: ["secret"] }));
    assert.equal(store.state.hosts.local, undefined);
    assert.equal(store.hostCount(), 0);
  });
});

test("the 65th host applies nothing", () => {
  withStore(["sessions"], (store) => {
    for (let i = 0; i < MAX_HOSTS; i++) drain(store, `h${i}`, frame(`h${i}`, "sessions", 1, 1, sessions("a")));
    drain(store, "late", frame("late", "sessions", 1, 1, sessions("b")));

    assert.equal(store.hostCount(), MAX_HOSTS);
    assert.equal(store.section("late", "sessions"), undefined);
    // A host already held still applies.
    drain(store, "h0", frame("h0", "sessions", 1, 2, sessions("moved")));
    assert.deepEqual(names(store, "h0"), ["moved"]);
  });
});

test("one drain is one commit: an effect runs once for many frames", () => {
  withStore(["sessions", "shell"], (store) => {
    const seen: string[] = [];
    createRoot(() =>
      createEffect(() => {
        const screen = (store.section("local", "shell") as { current_screen?: string } | undefined)?.current_screen;
        seen.push(`${names(store, "local") ?? "-"}|${screen ?? "-"}`);
      }),
    );
    store.applyDrain("local", [
      { frames: [frame("local", "sessions", 1, 1, sessions("a"))] },
      { frames: [frame("local", "shell", 1, 1, { current_screen: "Sessions" })] },
      { frames: [frame("local", "sessions", 1, 2, sessions("a", "c"))] },
    ]);
    assert.deepEqual(seen, ["-|-", "a,c|Sessions"]);
  });
});

test("an update in place wakes only the readers of what changed", () => {
  withStore(["sessions"], (store) => {
    drain(store, "local", frame("local", "sessions", 1, 1, sessions("a", "b")));
    const statusOfB: SessionStatus[] = [];
    createRoot(() =>
      createEffect(() => {
        const b = store.section("local", "sessions")?.workspaces[0]?.sessions[1];
        if (b) statusOfB.push(b.status);
      }),
    );

    const moved = sessions("a", "b");
    moved.workspaces[0].sessions[0].status = "Running";
    drain(store, "local", frame("local", "sessions", 1, 2, moved));

    assert.equal(store.section("local", "sessions")?.workspaces[0]?.sessions[0]?.status, "Running");
    assert.equal(store.state.hosts.local.sections.sessions?.version, 2);
    assert.deepEqual(statusOfB, ["Idle"], "a change to session a does not re-run a reader of session b");
  });
});

test("the daemon read is held in both the fresh and the in-place write", () => {
  withStore(["agent_status"], (store) => {
    const read = (revision: number): Frame_Serialize => ({
      ...frame("local", "agent_status", 1, revision, { absent: null, head_revision: revision, view: null }),
      daemon_read: { revision, clock_ms: revision * 1000 },
    });
    drain(store, "local", read(1));
    assert.deepEqual(store.state.hosts.local.sections.agent_status?.daemon_read, { revision: 1, clock_ms: 1000 });
    drain(store, "local", read(2));
    assert.deepEqual(store.state.hosts.local.sections.agent_status?.daemon_read, { revision: 2, clock_ms: 2000 });
  });
});

test("evicting a host drops its sections and stale marks and no other host's", () => {
  withStore(["sessions"], (store) => {
    drain(store, "local", frame("local", "sessions", 5, 1, sessions("a")));
    store.applyDrain("local", [oversize]);
    drain(store, "peer", frame("peer", "sessions", 1, 1, sessions("b")));
    assert.equal(store.hostCount(), 2);

    store.evictHost("local");

    assert.equal("local" in store.state.hosts, false, "the host entry is gone");
    assert.equal("local" in store.state.stale, false, "its stale marks are gone");
    assert.equal(store.section("local", "sessions"), undefined);
    assert.equal(store.hostCount(), 1);
    assert.deepEqual(names(store, "peer"), ["b"]);

    // Evicting a host the store does not hold changes nothing.
    store.evictHost("absent");
    assert.equal(store.hostCount(), 1);

    // A later frame from the evicted host starts it over, even at an epoch
    // older than the one it was evicted at.
    drain(store, "local", frame("local", "sessions", 1, 1, sessions("again")));
    assert.deepEqual(names(store, "local"), ["again"]);
  });
});

test("an evicted host does not consume a MAX_HOSTS slot", () => {
  withStore(["sessions"], (store) => {
    for (let i = 0; i < MAX_HOSTS; i++) drain(store, `h${i}`, frame(`h${i}`, "sessions", 1, 1, sessions("a")));
    // `h0` stands in for the stale `local` entry a re-pinned window evicts.
    store.evictHost("h0");
    assert.equal(store.hostCount(), MAX_HOSTS - 1);

    drain(store, "ulid", frame("ulid", "sessions", 1, 1, sessions("b")));

    assert.equal(store.hostCount(), MAX_HOSTS);
    assert.deepEqual(names(store, "ulid"), ["b"], "the freed slot is taken");
  });
});

test("framesIgnored counts every frame that applied nothing, and nothing else", () => {
  withStore(["sessions"], (store) => {
    assert.equal(store.framesIgnored(), 0);

    // Applied: one fresh frame, and a same-section frame it replaces in one drain.
    drain(store, "local", frame("local", "sessions", 5, 1, sessions("a")), frame("local", "sessions", 5, 2, sessions("b")));
    assert.equal(store.framesIgnored(), 0, "a frame replaced within a drain is not ignored");

    drain(store, "local", frame("local", "shell", 5, 3, {}));
    assert.equal(store.framesIgnored(), 1, "an unsubscribed section");
    drain(store, "local", frame("peer", "sessions", 5, 3, sessions("c")));
    assert.equal(store.framesIgnored(), 2, "a host other than the peer");
    drain(store, "local", frame("local", "sessions", 4, 9, sessions("d")));
    assert.equal(store.framesIgnored(), 3, "an older epoch");
    drain(store, "local", frame("local", "sessions", 5, 2, sessions("e")));
    assert.equal(store.framesIgnored(), 4, "a version already held");
    assert.deepEqual(names(store, "local"), ["b"]);

    for (let i = 1; i < MAX_HOSTS; i++) drain(store, `h${i}`, frame(`h${i}`, "sessions", 1, 1, sessions("x")));
    assert.equal(store.framesIgnored(), 4);
    store.applyDrain("late", [
      { frames: [frame("late", "sessions", 1, 1, sessions("y")), frame("late", "sessions", 1, 2, sessions("z"))] },
    ]);
    assert.equal(store.framesIgnored(), 6, "every frame of a drain past MAX_HOSTS");
  });
});

test("framesIgnored wakes its reader once per drain", () => {
  withStore(["sessions"], (store) => {
    const seen: number[] = [];
    const dispose = createRoot((dispose) => {
      createEffect(() => seen.push(store.framesIgnored()));
      return dispose;
    });
    try {
      drain(store, "local", frame("local", "shell", 1, 1, {}), frame("peer", "sessions", 1, 1, sessions("a")));
      assert.deepEqual(seen, [0, 2]);
    } finally {
      dispose();
    }
  });
});

test("an oversize section is stale until the host frames it again, and keeps its body", () => {
  withStore(["sessions"], (store) => {
    drain(store, "local", frame("local", "sessions", 1, 1, sessions("a")));
    store.applyDrain("local", [oversize]);

    assert.equal(store.state.stale.local?.sessions, true);
    assert.deepEqual(names(store, "local"), ["a"], "the held body survives the notice");

    drain(store, "local", frame("local", "sessions", 1, 3, sessions("b")));
    assert.equal(store.state.stale.local?.sessions, undefined);
  });
});

test("an oversize notice marks only its own host's copy stale", () => {
  withStore(["sessions"], (store) => {
    drain(store, "local", frame("local", "sessions", 1, 1, sessions("a")));
    drain(store, "peer", frame("peer", "sessions", 1, 1, sessions("b")));
    // One drain each: the oversize notice over local's channel, a fresh
    // frame of the same section over peer's.
    store.applyDrain("local", [oversize]);
    drain(store, "peer", frame("peer", "sessions", 1, 2, sessions("b", "c")));

    assert.equal(store.state.stale.local?.sessions, true);
    assert.equal(store.state.stale.peer?.sessions, undefined);
    assert.deepEqual(names(store, "peer"), ["b", "c"]);
  });
});
