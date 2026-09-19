// The subscription list matches what the shell chrome reads (#1132): every
// section the sidebar and header read is subscribed, and every subscribed
// section is either read or declared ahead of its reader.

import assert from "node:assert/strict";
import { test } from "node:test";
import { createRoot } from "solid-js";
import { ROOT_SELECTORS } from "./selectors.ts";
import { createFrameStore, type FrameStore, type SectionName } from "./store.ts";
import {
  AHEAD_OF_READERS,
  shellAgentStatus,
  shellFleet,
  shellGitView,
  shellSessions,
  shellUsage,
  SUBSCRIBED,
} from "./subscription.ts";

/**
 * A store that records which sections a reader asks for, through
 * `section(...)` and through the per-host `stale` map.
 */
function recording(store: FrameStore, read: Set<string>): FrameStore {
  const stale = new Proxy(store.state.stale, {
    get(target, host, receiver) {
      const held = Reflect.get(target, host, receiver);
      return new Proxy(held ?? {}, {
        get(sections, name, inner) {
          if (typeof name === "string") read.add(name);
          return Reflect.get(sections, name, inner);
        },
      });
    },
  });
  const state = new Proxy(store.state, {
    get(target, key, receiver) {
      return key === "stale" ? stale : Reflect.get(target, key, receiver);
    },
  });
  return {
    ...store,
    state,
    section(host, name) {
      read.add(name);
      return store.section(host, name);
    },
  };
}

test("the shell chrome reads only subscribed sections, and the list names no unread one", () => {
  const read = new Set<string>();
  createRoot((dispose) => {
    const store = recording(createFrameStore(SUBSCRIBED), read);
    for (const select of Object.values(ROOT_SELECTORS)) select(store, "local");
    shellSessions(store, "local");
    shellAgentStatus(store, "local");
    shellFleet(store, "local");
    shellGitView(store, "local");
    shellUsage(store, "local");
    dispose();
  });

  assert.ok(read.has("sessions"), "the recorder saw the chrome's reads");
  assert.ok(read.has("usage"), "the stats tab reads section 21");
  const subscribed = new Set<string>(SUBSCRIBED);
  for (const name of read) {
    assert.ok(subscribed.has(name), `the chrome reads "${name}", which is not subscribed`);
  }
  for (const name of AHEAD_OF_READERS) {
    assert.ok(subscribed.has(name), `"${name}" is declared ahead of its reader but not subscribed`);
    assert.ok(!read.has(name), `"${name}" has a reader now: take it out of AHEAD_OF_READERS`);
  }
  const accounted = new Set<string>([...read, ...AHEAD_OF_READERS]);
  const unread = SUBSCRIBED.filter((name: SectionName) => !accounted.has(name));
  assert.deepEqual(unread, [], "subscribed with no reader and not declared ahead of one");
  assert.equal(new Set(SUBSCRIBED).size, SUBSCRIBED.length, "no section is subscribed twice");
});

test("reading without a host reads no section", () => {
  const read = new Set<string>();
  createRoot((dispose) => {
    const store = recording(createFrameStore(SUBSCRIBED), read);
    for (const select of Object.values(ROOT_SELECTORS)) select(store, undefined);
    assert.equal(shellSessions(store, undefined), undefined);
    assert.equal(shellAgentStatus(store, undefined), undefined);
    assert.equal(shellFleet(store, undefined), undefined);
    assert.equal(shellGitView(store, undefined), undefined);
    dispose();
  });
  assert.deepEqual([...read], []);
});
