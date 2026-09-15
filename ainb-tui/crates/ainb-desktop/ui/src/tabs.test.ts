// The terminal focus rules: which keys stay with the shell, Esc Esc, tab steps.

import assert from "node:assert/strict";
import { test } from "node:test";
import { accelerator, escEsc, stepTab, type Tab } from "./tabs.ts";

const key = (code: string, mods: Partial<{ meta: boolean; ctrl: boolean; shift: boolean; alt: boolean }> = {}) => ({
  code,
  metaKey: mods.meta ?? false,
  ctrlKey: mods.ctrl ?? false,
  shiftKey: mods.shift ?? false,
  altKey: mods.alt ?? false,
});

test("on macOS only cmd chords stay with the shell", () => {
  assert.deepEqual(accelerator(key("Digit3", { meta: true }), true), { kind: "tab", index: 2 });
  assert.deepEqual(accelerator(key("KeyW", { meta: true }), true), { kind: "close" });
  assert.deepEqual(accelerator(key("BracketLeft", { meta: true }), true), { kind: "prev" });
  assert.deepEqual(accelerator(key("KeyK", { meta: true }), true), { kind: "palette" });
  assert.deepEqual(accelerator(key("KeyH", { meta: true, shift: true }), true), { kind: "hosts" });
  // The pane keeps ctrl+c, ctrl+b, arrows, and cmd chords the spec does not list.
  assert.equal(accelerator(key("KeyC", { ctrl: true }), true), null);
  assert.equal(accelerator(key("KeyB", { ctrl: true }), true), null);
  assert.equal(accelerator(key("ArrowUp"), true), null);
  assert.equal(accelerator(key("KeyH", { meta: true }), true), null);
  assert.equal(accelerator(key("KeyW", { meta: true, shift: true }), true), null);
});

test("elsewhere the shell's mod is ctrl+shift and plain ctrl reaches the pane", () => {
  assert.deepEqual(accelerator(key("Digit1", { ctrl: true, shift: true }), false), { kind: "tab", index: 0 });
  assert.deepEqual(accelerator(key("BracketRight", { ctrl: true, shift: true }), false), { kind: "next" });
  assert.deepEqual(accelerator(key("KeyH", { ctrl: true, shift: true }), false), { kind: "hosts" });
  assert.equal(accelerator(key("KeyW", { ctrl: true }), false), null);
  assert.equal(accelerator(key("KeyC", { ctrl: true }), false), null);
  assert.equal(accelerator(key("Digit1", { meta: true }), false), null);
});

test("Esc twice within 300 ms leaves the terminal; a slower pair does not", () => {
  const esc = escEsc();
  assert.equal(esc(1000), false);
  assert.equal(esc(1250), true);
  assert.equal(esc(1300), false, "a third Esc starts a new pair");
  assert.equal(esc(1700), false);
});

test("tab steps wrap in both directions", () => {
  const tabs = ["a", "b", "c"].map((k) => ({ key: k, target: { kind: "tmux", tmux: k }, state: "attached" }) as Tab);
  assert.equal(stepTab(tabs, "c", 1), "a");
  assert.equal(stepTab(tabs, "a", -1), "c");
  assert.equal(stepTab([], null, 1), null);
});
