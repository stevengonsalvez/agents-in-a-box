// ABOUTME: The DOM half of the parity enumeration: every screen the window
// builds a surface for (`surfaces.ts`) has a row in
// `ainb-app/tests/parity/screens.txt`, and every row the ratatui half draws
// or the DOM half alone draws has the frames the DOM half reads. The Rust
// half (`ainb-app/tests/parity_registry.rs`) walks the same file against the
// screen ids and the terminal's registry, so one file is the definition.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { SURFACE_SCREENS } from "./surfaces.ts";

const parityDir = new URL("../../../ainb-app/tests/parity/", import.meta.url);

interface Row {
  screen: string;
  coverage: string;
  detail: string;
}

function rows(): Row[] {
  return readFileSync(new URL("screens.txt", parityDir), "utf8")
    .split("\n")
    .filter((line) => line.trim() !== "" && !line.startsWith("#"))
    .map((line) => {
      const [screen, coverage, ...rest] = line.split("\t");
      return { screen, coverage, detail: rest.join("\t") };
    });
}

test("every screen the window builds a surface for has a row", () => {
  const named = new Set(rows().map((row) => row.screen));
  const unlisted = SURFACE_SCREENS.filter((screen) => !named.has(screen));
  assert.deepEqual(unlisted, [], "surfaces with no row in screens.txt");
});

test("every row the DOM half draws has the frames it draws from", () => {
  for (const row of rows()) {
    if (row.coverage !== "both" && row.coverage !== "dom") continue;
    for (const fixture of row.detail.split(/\s+/)) {
      const frames = fileURLToPath(new URL(`frames/${fixture}.json`, parityDir));
      assert.ok(existsSync(frames), `${row.screen}: no frames for ${fixture}`);
    }
  }
});

test("every drawn row has the facts list its renderers are checked against", () => {
  for (const row of rows()) {
    if (row.coverage !== "both" && row.coverage !== "dom") continue;
    for (const fixture of row.detail.split(/\s+/)) {
      const facts = fileURLToPath(new URL(`facts/${fixture}.txt`, parityDir));
      assert.ok(existsSync(facts), `${row.screen}: no facts list for ${fixture}`);
    }
  }
});
