// The three states a plugin screen shows with nothing to paint (D3p-f), decided
// from the framed plugins_host section alone, as the terminal decides them in
// `build_placeholder_for_unloaded_plugin`.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { PluginsHostView_Serialize } from "../../../ainb-app/bindings/AppState";
import { placeholderFor } from "./plugin_placeholder.ts";

function host(over: Partial<PluginsHostView_Serialize> = {}): PluginsHostView_Serialize {
  return {
    plugin_captures_text: {},
    plugin_presence: {},
    plugin_render_errors: {},
    ...over,
  } as PluginsHostView_Serialize;
}

const registered = { registered: true, wedged: false, abi: 1 };

test("a plugin the host has not registered says it is not loaded", () => {
  assert.deepEqual(placeholderFor("witr", host()), { kind: "not_registered", screen: "witr", plugin: "witr" });
});

test("a registered plugin with no frame yet is connecting", () => {
  assert.deepEqual(placeholderFor("hangar", host({ plugin_presence: { hangar: registered } })), {
    kind: "no_frame",
    screen: "hangar",
    plugin: "hangar-tui",
  });
});

test("a recorded render error outranks connecting, as it does on the terminal", () => {
  const view = host({ plugin_presence: { witr: registered }, plugin_render_errors: { witr: "spawn failed" } });
  assert.deepEqual(placeholderFor("witr", view), {
    kind: "render_error",
    screen: "witr",
    plugin: "witr",
    error: "spawn failed",
  });
});

test("a render error for a plugin that is not registered is not shown", () => {
  const view = host({ plugin_render_errors: { witr: "stale" } });
  assert.equal(placeholderFor("witr", view).kind, "not_registered");
});

test("each plugin screen names the plugin that owns it", () => {
  const owners = ["analytics", "witr", "learnings", "abtop", "hangar"].map(
    (screen) => placeholderFor(screen, host()).plugin,
  );
  assert.deepEqual(owners, ["burndown", "witr", "learnings", "abtop", "hangar-tui"]);
});
